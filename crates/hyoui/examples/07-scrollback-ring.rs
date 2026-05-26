//! PoC 07: scrollback ring buffer + last_evicted_ts + --since-strict
//!
//! VecDeque<OutputChunk{ts, bytes}> + size 制約 + last_evicted_ts 更新 +
//! since DUR フィルタ + since-strict 判定の synthetic test。
//!
//! 実行:
//!   cargo run --example 07-scrollback-ring

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct OutputChunk {
    ts: Instant,
    bytes: Vec<u8>,
}

struct Scrollback {
    chunks: VecDeque<OutputChunk>,
    last_evicted_ts: Option<Instant>,
    max_bytes: usize,
    total_bytes: usize,
}

#[derive(Debug)]
struct BufferInsufficient {
    last_evicted_ts: Instant,
    since_start: Instant,
}

impl Scrollback {
    fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            last_evicted_ts: None,
            max_bytes,
            total_bytes: 0,
        }
    }

    fn push(&mut self, ts: Instant, bytes: Vec<u8>) {
        self.total_bytes += bytes.len();
        self.chunks.push_back(OutputChunk { ts, bytes });
        while self.total_bytes > self.max_bytes {
            let Some(evicted) = self.chunks.pop_front() else {
                break;
            };
            self.total_bytes -= evicted.bytes.len();
            self.last_evicted_ts = Some(evicted.ts);
        }
    }

    fn oldest_ts(&self) -> Option<Instant> {
        self.chunks.front().map(|c| c.ts)
    }

    fn since(&self, now: Instant, dur: Duration) -> Vec<u8> {
        let cutoff = now - dur;
        let mut out = Vec::new();
        for c in &self.chunks {
            if c.ts >= cutoff {
                out.extend_from_slice(&c.bytes);
            }
        }
        out
    }

    fn since_strict(
        &self,
        now: Instant,
        dur: Duration,
    ) -> Result<Vec<u8>, BufferInsufficient> {
        let since_start = now - dur;
        if let Some(last_evict) = self.last_evicted_ts {
            if last_evict >= since_start {
                return Err(BufferInsufficient {
                    last_evicted_ts: last_evict,
                    since_start,
                });
            }
        }
        Ok(self.since(now, dur))
    }

    fn last_n_bytes(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n.min(self.total_bytes));
        for c in self.chunks.iter().rev() {
            if out.len() + c.bytes.len() <= n {
                let mut prepend = c.bytes.clone();
                prepend.extend(out);
                out = prepend;
            } else {
                let take = n - out.len();
                let start = c.bytes.len() - take;
                let mut prepend = c.bytes[start..].to_vec();
                prepend.extend(out);
                out = prepend;
                break;
            }
        }
        out
    }
}

fn main() {
    let mut passed = 0;
    let mut failed = 0;

    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond {
                eprintln!("  PASS  {}", $name);
                passed += 1;
            } else {
                eprintln!("  FAIL  {}", $name);
                failed += 1;
            }
        };
    }

    // === Test 1: 単純 push + since ===
    eprintln!("=== Test 1: 単純 push + since ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1024);
        sb.push(t0, b"hello".to_vec());
        sb.push(t0 + Duration::from_millis(100), b"world".to_vec());
        let all = sb.since(t0 + Duration::from_millis(200), Duration::from_secs(1));
        check!("since=1s で全取得", all == b"helloworld");
        check!("last_evicted_ts is None", sb.last_evicted_ts.is_none());
    }

    // === Test 2: size 超過で evict ===
    eprintln!("=== Test 2: size 超過で evict ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(20);
        sb.push(t0, vec![b'A'; 10]); // 10 bytes
        sb.push(t0 + Duration::from_millis(100), vec![b'B'; 10]); // 20 total OK
        check!("evict 前: total=20", sb.total_bytes == 20);
        check!("evict 前: last_evicted_ts is None", sb.last_evicted_ts.is_none());
        sb.push(t0 + Duration::from_millis(200), vec![b'C'; 5]); // 25 → A を evict、total=15
        check!("evict 後: total=15", sb.total_bytes == 15);
        check!("evict 後: last_evicted_ts is Some", sb.last_evicted_ts.is_some());
        check!("evict された chunk の ts は t0", sb.last_evicted_ts == Some(t0));
    }

    // === Test 3: since DUR フィルタ ===
    eprintln!("=== Test 3: since DUR フィルタ ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1024);
        for i in 0..10 {
            sb.push(t0 + Duration::from_millis(i * 100), vec![b'0' + i as u8]);
        }
        // now = t0 + 1000ms、since 500ms (= cutoff = t0+500ms)
        let now = t0 + Duration::from_millis(1000);
        let bytes = sb.since(now, Duration::from_millis(500));
        // chunk ts: 0,100,200,...,900。cutoff=500、>=500 のものは 500,600,700,800,900 = 5 chunks
        check!(
            "since 500ms で 5 chunks = '56789'",
            bytes == b"56789"
        );
    }

    // === Test 4: since-strict OK (last_evicted_ts < since_start) ===
    eprintln!("=== Test 4: since-strict OK ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(20);
        // t0 に 10 bytes、t0+100 に 10 bytes、t0+200 に 5 bytes → t0 chunk evict
        // last_evicted_ts = t0
        sb.push(t0, vec![b'A'; 10]);
        sb.push(t0 + Duration::from_millis(100), vec![b'B'; 10]);
        sb.push(t0 + Duration::from_millis(200), vec![b'C'; 5]);

        // now = t0 + 1000ms、since 500ms → since_start = t0 + 500ms
        // last_evicted_ts (t0) < since_start (t0+500) → 完全
        let now = t0 + Duration::from_millis(1000);
        let result = sb.since_strict(now, Duration::from_millis(500));
        check!("since-strict OK (= Ok)", result.is_ok());
    }

    // === Test 5: since-strict NG (last_evicted_ts >= since_start) ===
    eprintln!("=== Test 5: since-strict NG ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(20);
        // t0 に evict 候補、now=t0+300, since 1000ms → since_start = t0 - 700ms
        // last_evicted_ts (t0) >= since_start (t0-700ms) → 不完全
        sb.push(t0, vec![b'A'; 10]);
        sb.push(t0 + Duration::from_millis(100), vec![b'B'; 10]);
        sb.push(t0 + Duration::from_millis(200), vec![b'C'; 5]); // evict t0

        let now = t0 + Duration::from_millis(300);
        let result = sb.since_strict(now, Duration::from_secs(1));
        check!("since-strict NG (= Err)", result.is_err());
        if let Err(e) = result {
            check!(
                "BufferInsufficient.last_evicted_ts == t0",
                e.last_evicted_ts == t0
            );
        }
    }

    // === Test 6: ユーザ例 (DR 議論の具体ケース) ===
    eprintln!("=== Test 6: DR 議論の具体ケース (900KB + 2KB×60) ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1_000_000); // 1MB
        sb.push(t0, vec![b'X'; 900_000]); // 900KB@t=0
        for i in 1..=60u32 {
            sb.push(t0 + Duration::from_secs(i as u64), vec![b'X'; 2000]); // 2KB@t=1..60
        }
        // 900KB + 60*2KB = 1,020,000 → 1MB 超、t=0 (900KB) が evict
        // 残り = 60 chunks × 2KB = 120KB
        check!("total_bytes = 120000", sb.total_bytes == 120_000);
        check!(
            "last_evicted_ts = t0 (= 900KB chunk)",
            sb.last_evicted_ts == Some(t0)
        );
        check!("oldest = t0+1s", sb.oldest_ts() == Some(t0 + Duration::from_secs(1)));

        // now=t0+60s、since 70s → since_start = t0 - 10s
        // last_evicted_ts (t0) >= since_start (t0-10s) → NG
        let now = t0 + Duration::from_secs(60);
        let r1 = sb.since_strict(now, Duration::from_secs(70));
        check!("--since 70s strict → Err (期待通り)", r1.is_err());

        // since 50s → since_start = t0 + 10s、last_evicted (t0) < since_start (t0+10s) → OK
        let r2 = sb.since_strict(now, Duration::from_secs(50));
        check!("--since 50s strict → Ok (期待通り)", r2.is_ok());
        if let Ok(bytes) = r2 {
            // since 50s = t0+10s 以降の chunks = t=10..60 = 51 chunks × 2KB = 102KB
            check!("since 50s で 102KB 取得", bytes.len() == 102_000);
        }

        // since 70s (= 不完全だが取れる分だけ): 全 buffer 内容 = 120KB
        let bytes = sb.since(now, Duration::from_secs(70));
        check!("since 70s (non-strict) で 120KB", bytes.len() == 120_000);
    }

    // === Test 7: last_n_bytes ===
    eprintln!("=== Test 7: last N bytes ===");
    {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1024);
        sb.push(t0, b"abcdefghij".to_vec()); // 10 bytes
        sb.push(t0 + Duration::from_millis(100), b"klmnopqrst".to_vec()); // 20 total
        sb.push(t0 + Duration::from_millis(200), b"uvwxyz".to_vec()); // 26 total

        let last5 = sb.last_n_bytes(5);
        check!("last 5 = 'vwxyz'", last5 == b"vwxyz");
        let last15 = sb.last_n_bytes(15);
        check!("last 15 = 'lmnopqrstuvwxyz'", last15 == b"lmnopqrstuvwxyz");
        let last100 = sb.last_n_bytes(100);
        check!("last 100 = 全 26 bytes", last100.len() == 26);
    }

    eprintln!("=== Summary ===");
    eprintln!("PASSED: {passed}, FAILED: {failed}");
    if failed > 0 {
        std::process::exit(1);
    }
}
