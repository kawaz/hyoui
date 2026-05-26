# Finding: scrollback ring buffer + last_evicted_ts + --since-strict

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/07-scrollback-ring.rs`
- 関連: [[DR-0006]] §11.6 (scrollback ring buffer)、§11.5 (tail)、§11 (wait)

## 判明した事実

PoC 21 ケース全 PASS (synthetic test、サブプロセス不要、純 in-memory):

1. **VecDeque + size 制約 + chunk 単位 evict** が素直に書ける (~50 行)
2. **`last_evicted_ts` を Option<Instant> で保持** → `--since-strict` の判定根拠として機能
3. **DR 議論のユーザ例 (1MB buffer, 900KB@t=0 + 2KB×60)** が期待通り:
   - 900KB chunk が t=51 で evict、last_evicted_ts = t0
   - `--since 70s strict` → `Err` (= since_start が last_evicted_ts より前)
   - `--since 50s strict` → `Ok` で 102KB (= t=10..60 の 51 chunks × 2KB)
   - `--since 70s` (non-strict) → 120KB (= buffer 内全部)
4. **`last_n_bytes(N)`** は VecDeque を逆順走査して N bytes 集めるシンプル実装で OK

## 実用的な示唆

### 本実装の Scrollback struct (PoC のままでほぼ使える)

```rust
pub struct Scrollback {
    chunks: VecDeque<OutputChunk>,
    last_evicted_ts: Option<Instant>,
    max_bytes: usize,
    total_bytes: usize,
}

pub struct OutputChunk {
    pub ts: Instant,
    pub bytes: Vec<u8>,
}

pub enum SinceError {
    /// since 範囲の一部が既に evict されてる (= 不完全)
    Insufficient { last_evicted_ts: Instant, since_start: Instant },
}

impl Scrollback {
    pub fn push(&mut self, ts: Instant, bytes: Vec<u8>);
    pub fn since(&self, now: Instant, dur: Duration) -> Vec<u8>;
    pub fn since_strict(&self, now: Instant, dur: Duration) -> Result<Vec<u8>, SinceError>;
    pub fn last_n_bytes(&self, n: usize) -> Vec<u8>;
    pub fn oldest_ts(&self) -> Option<Instant>;
    pub fn total_bytes(&self) -> usize { self.total_bytes }
}
```

### 細部の追加検討 (本実装で詰める)

- **chunk size の粒度**: pty read の 1 回分 = chunk 1 個。read buffer size (例: 8192 bytes) が chunk max。小さい read が連続すると chunk 数が増えて overhead 増加 → 隣接 chunk を merge する optimization (= 直前 chunk と同 ts (= 100ms 以内とか) なら append) は v0.1.0 後に検討
- **bytes ownership**: PoC は `Vec<u8>` で chunk ごとに alloc。本実装は arena/pool で alloc 削減も可、ただし v0.1.0 では Vec で十分
- **partial chunk eviction**: 「chunk 単位で全削除」が PoC 仕様、本実装も同じ。partial evict (= chunk の途中まで残す) は複雑度に対してメリット薄い

### `hyoui status` 出力 ([[DR-0006]] §11.6 で予告)

```
scrollback:
  size: 4.0 MB (used: 894 KB)
  oldest_age: 11.3s              # buffer 最古 chunk の経過時間
  last_evicted_age: 47.2s        # 最後に押し出されたデータの古さ (None なら "never evicted")
  chunks: 924
```

```rust
fn format_status(sb: &Scrollback, now: Instant) -> String {
    let used = sb.total_bytes();
    let oldest_age = sb.oldest_ts().map(|t| now.duration_since(t));
    let last_evicted_age = sb.last_evicted_ts.map(|t| now.duration_since(t));
    let chunks = sb.chunks.len();
    // ... format
}
```

## hyoui 本実装への反映

PoC のコードをそのまま本実装の `crates/hyoui/src/scrollback.rs` (新規 module) として置く。
ただし API 名は public 用に整える (`SinceError` 等)、doc コメント追加。

`hyoui run --scrollback-size=SIZE` で max_bytes を変更可、default 4MB (= [[DR-0006]] §11.6 確定値)。
0 指定で scrollback 無効化 (= max_bytes=0、push 即座に evict)。

## 検証の詳細

PoC の Test 6 (= DR 議論の具体ケース) が最も意義深い:

```rust
let mut sb = Scrollback::new(1_000_000); // 1MB
sb.push(t0, vec![b'X'; 900_000]); // 900KB@t=0
for i in 1..=60u32 {
    sb.push(t0 + Duration::from_secs(i as u64), vec![b'X'; 2000]); // 2KB@t=1..60
}
// 期待: total=120000、last_evicted_ts=t0、oldest=t0+1s
// since 70s strict → Err、since 50s strict → Ok (102KB)
```

期待通り全 PASS。設計議論の結論 ([[DR-0006]] §11.6) が PoC で実証された。

### 21 test 一覧

```
=== Test 1: 単純 push + since ===           PASS×2
=== Test 2: size 超過で evict ===            PASS×5
=== Test 3: since DUR フィルタ ===           PASS×1
=== Test 4: since-strict OK ===              PASS×1
=== Test 5: since-strict NG ===              PASS×2
=== Test 6: DR 議論の具体ケース ===          PASS×7
=== Test 7: last N bytes ===                 PASS×3
=== Summary === PASSED: 21, FAILED: 0
```
