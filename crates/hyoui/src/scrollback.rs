//! Scrollback ring buffer with timestamped chunks.
//!
//! daemon が子 pty 出力を bytes として蓄積する ring buffer。size 上限を超えた古い chunk は
//! `pop_front` で削除、削除時に `last_evicted_ts` を更新することで `tail --since-strict` で
//! 「since 範囲が完全に buffer 内に収まるか」を厳密判定可能にする。
//!
//! 参照: docs/decisions/DR-0006-cli-ground-rules.md §11.6 (scrollback ring buffer)、
//! docs/findings/2026-05-26-scrollback-ring-buffer.md (PoC 検証 21/21 PASS)

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 1 つの output chunk (1 回の pty read 単位)。
#[derive(Clone, Debug)]
pub struct OutputChunk {
    pub ts: Instant,
    pub bytes: Vec<u8>,
}

/// `--since-strict` で「since 範囲の一部が既に evict されてる」と判定された場合の error。
#[derive(Debug)]
pub struct BufferInsufficient {
    /// 最後に押し出された chunk の timestamp。
    pub last_evicted_ts: Instant,
    /// 要求された since 開始 timestamp (`now - dur`)。
    pub since_start: Instant,
}

impl std::fmt::Display for BufferInsufficient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scrollback buffer insufficient for --since: last evicted at {:?}, since start at {:?}",
            self.last_evicted_ts, self.since_start
        )
    }
}

impl std::error::Error for BufferInsufficient {}

/// Scrollback ring buffer。
///
/// `max_bytes` を超えると古い chunk から `pop_front`、削除時に `last_evicted_ts` を更新。
/// `since(now, dur)` で過去 `dur` 期間内の bytes をフィルタ取得、`since_strict` で
/// 「since 範囲が完全に buffer 内」を厳密判定。
#[derive(Debug)]
pub struct Scrollback {
    chunks: VecDeque<OutputChunk>,
    last_evicted_ts: Option<Instant>,
    max_bytes: usize,
    total_bytes: usize,
}

impl Scrollback {
    /// `max_bytes` size 上限の ring buffer を作成。0 を渡すと「常に空」(= push で即 evict)。
    pub fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            last_evicted_ts: None,
            max_bytes,
            total_bytes: 0,
        }
    }

    /// chunk を末尾に追加。size 超過なら古い chunk から削除し `last_evicted_ts` 更新。
    pub fn push(&mut self, ts: Instant, bytes: Vec<u8>) {
        self.total_bytes += bytes.len();
        self.chunks.push_back(OutputChunk { ts, bytes });
        while self.total_bytes > self.max_bytes {
            let Some(evicted) = self.chunks.pop_front() else {
                // chunks 空なのに total_bytes > max_bytes は ありえない (= 1 chunk が
                // max_bytes 超えてる場合のみ)。0 にして抜ける。
                self.total_bytes = 0;
                break;
            };
            self.total_bytes -= evicted.bytes.len();
            self.last_evicted_ts = Some(evicted.ts);
        }
    }

    /// buffer 最古 chunk の timestamp。空なら None。
    pub fn oldest_ts(&self) -> Option<Instant> {
        self.chunks.front().map(|c| c.ts)
    }

    /// 最後に押し出された chunk の timestamp。一度も evict してなければ None。
    pub fn last_evicted_ts(&self) -> Option<Instant> {
        self.last_evicted_ts
    }

    /// 現在の buffer 内 total bytes。
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// chunk 数。
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// 設定された size 上限。
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// `now - dur` 以降の chunk の bytes を連結して返す。buffer 外の古いデータは黙って捨てる
    /// (= silently truncate)。完全性を要求するなら [`since_strict`](Self::since_strict)。
    pub fn since(&self, now: Instant, dur: Duration) -> Vec<u8> {
        let cutoff = now.checked_sub(dur).unwrap_or(now);
        let mut out = Vec::new();
        for c in &self.chunks {
            if c.ts >= cutoff {
                out.extend_from_slice(&c.bytes);
            }
        }
        out
    }

    /// `since` の厳密版。`last_evicted_ts >= since_start` (= since 範囲の最古部分が既に
    /// evict されてた可能性) なら `Err(BufferInsufficient)`。完全に buffer 内なら `Ok`。
    pub fn since_strict(
        &self,
        now: Instant,
        dur: Duration,
    ) -> Result<Vec<u8>, BufferInsufficient> {
        let since_start = now.checked_sub(dur).unwrap_or(now);
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

    /// 末尾 `n` bytes を返す (= chunk を逆順走査、`n` bytes 揃ったら停止)。
    pub fn last_n_bytes(&self, n: usize) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_basics() {
        let sb = Scrollback::new(1024);
        assert_eq!(sb.total_bytes(), 0);
        assert_eq!(sb.chunk_count(), 0);
        assert!(sb.oldest_ts().is_none());
        assert!(sb.last_evicted_ts().is_none());
        assert_eq!(sb.since(Instant::now(), Duration::from_secs(1)), b"");
        assert_eq!(sb.last_n_bytes(100), b"");
    }

    #[test]
    fn push_then_since_no_eviction() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1024);
        sb.push(t0, b"hello".to_vec());
        sb.push(t0 + Duration::from_millis(100), b"world".to_vec());
        let bytes = sb.since(t0 + Duration::from_millis(200), Duration::from_secs(1));
        assert_eq!(bytes, b"helloworld");
        assert!(sb.last_evicted_ts().is_none());
    }

    #[test]
    fn push_evicts_oldest_on_size_overflow() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(20);
        sb.push(t0, vec![b'A'; 10]);
        sb.push(t0 + Duration::from_millis(100), vec![b'B'; 10]);
        assert_eq!(sb.total_bytes(), 20);
        assert!(sb.last_evicted_ts().is_none());

        sb.push(t0 + Duration::from_millis(200), vec![b'C'; 5]);
        assert_eq!(sb.total_bytes(), 15);
        assert_eq!(sb.last_evicted_ts(), Some(t0));
    }

    #[test]
    fn since_filters_by_timestamp() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1024);
        for i in 0..10u8 {
            sb.push(t0 + Duration::from_millis(i as u64 * 100), vec![b'0' + i]);
        }
        let now = t0 + Duration::from_millis(1000);
        // cutoff = now - 500ms = t0+500ms, chunks >= t0+500 are 5..9
        assert_eq!(sb.since(now, Duration::from_millis(500)), b"56789");
    }

    #[test]
    fn since_strict_returns_ok_when_buffer_complete() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(20);
        sb.push(t0, vec![b'A'; 10]);
        sb.push(t0 + Duration::from_millis(100), vec![b'B'; 10]);
        sb.push(t0 + Duration::from_millis(200), vec![b'C'; 5]); // evict t0
        let now = t0 + Duration::from_millis(1000);
        // last_evicted = t0、since_start = t0+500ms → since_start > last_evicted → OK
        assert!(sb.since_strict(now, Duration::from_millis(500)).is_ok());
    }

    #[test]
    fn since_strict_returns_err_when_buffer_insufficient() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(20);
        sb.push(t0, vec![b'A'; 10]);
        sb.push(t0 + Duration::from_millis(100), vec![b'B'; 10]);
        sb.push(t0 + Duration::from_millis(200), vec![b'C'; 5]); // evict t0
        let now = t0 + Duration::from_millis(300);
        // last_evicted = t0、since_start = t0 - 700ms → since_start < last_evicted → Err
        let result = sb.since_strict(now, Duration::from_secs(1));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.last_evicted_ts, t0);
    }

    #[test]
    fn dr_user_example_900kb_plus_2kb_x60() {
        // DR 議論で出した具体ケース (PoC 07 Test 6 と同じ)
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1_000_000); // 1MB
        sb.push(t0, vec![b'X'; 900_000]);
        for i in 1..=60u32 {
            sb.push(t0 + Duration::from_secs(i as u64), vec![b'X'; 2000]);
        }
        // 900KB + 60*2KB = 1,020,000 → t0 chunk evict、残り 120KB
        assert_eq!(sb.total_bytes(), 120_000);
        assert_eq!(sb.last_evicted_ts(), Some(t0));
        assert_eq!(sb.oldest_ts(), Some(t0 + Duration::from_secs(1)));

        let now = t0 + Duration::from_secs(60);

        // since 70s strict → Err
        assert!(sb.since_strict(now, Duration::from_secs(70)).is_err());

        // since 50s strict → Ok 102KB
        let ok = sb.since_strict(now, Duration::from_secs(50)).unwrap();
        assert_eq!(ok.len(), 102_000);

        // since 70s (non-strict) → buffer 内全部 120KB
        let all = sb.since(now, Duration::from_secs(70));
        assert_eq!(all.len(), 120_000);
    }

    #[test]
    fn last_n_bytes_basic() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(1024);
        sb.push(t0, b"abcdefghij".to_vec());
        sb.push(t0 + Duration::from_millis(100), b"klmnopqrst".to_vec());
        sb.push(t0 + Duration::from_millis(200), b"uvwxyz".to_vec());

        assert_eq!(sb.last_n_bytes(5), b"vwxyz");
        assert_eq!(sb.last_n_bytes(15), b"lmnopqrstuvwxyz");
        assert_eq!(sb.last_n_bytes(100), b"abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn zero_max_bytes_evicts_immediately() {
        let t0 = Instant::now();
        let mut sb = Scrollback::new(0);
        sb.push(t0, b"hello".to_vec());
        assert_eq!(sb.total_bytes(), 0);
        assert_eq!(sb.chunk_count(), 0);
        assert_eq!(sb.last_evicted_ts(), Some(t0));
    }
}
