//! `InputLog` — primary buffer 専用の bounded ring buffer (DR-0013 §7 Phase B)。
//!
//! resize 救済策。vt100 `Parser::set_size` は **truncate のみで真の reflow なし**
//! のため、primary buffer 中は子 PTY bytes を本 ring に保持しておき、resize 時に
//! 新 Parser を組み立てて log を再 feed する。
//!
//! ## 責務分離
//!
//! - scrollback (= vt100 内蔵 ring or `crates/hyoui/src/scrollback.rs`):
//!   過去 row へのアクセス + 描画 / since_ms 用。byte-base or row-base どちらも
//!   こちらの責務。
//! - `InputLog`: **resize 時の Parser 再構築 + 過去 bytes 再 feed 専用**。
//!   `state_formatted` の round trip では取りこぼせない bytes 列を保持する。
//!
//! ## alt screen との関係
//!
//! - **primary buffer 中のみ push**。alt screen 中の bytes は push しない
//!   (= alt は子側で再描画させる、§6 + §7)。
//! - `set_alt_mode(true)` で push が止まり、`set_alt_mode(false)` で再開する。
//! - 切替は `Screen::alternate_screen()` の値変化を ScreenState 側で検出して
//!   呼び出す。

use std::collections::VecDeque;

/// primary buffer 用の bounded byte ring。
///
/// `capacity` を超えると最古 byte から evict される (= `VecDeque::pop_front`)。
/// `set_alt_mode(true)` の間は `push` を skip する。
#[derive(Debug)]
pub(crate) struct InputLog {
    ring: VecDeque<u8>,
    capacity: usize,
    in_alt_screen: bool,
    /// 押し出された byte 数の累計 (= last_evicted_age の補完 counter、§8)。
    /// 0 のままなら一度も evict してない (= ring に全 bytes 残っている)。
    evicted_total: u64,
}

impl InputLog {
    /// `capacity` (byte 単位) の ring を作る。
    ///
    /// `capacity == 0` は退化型 (= 常に空、push は即 evict される)。caller が
    /// 0 を渡しても panic しない。primary buffer から開始 (= `in_alt_screen = false`)。
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            ring: VecDeque::with_capacity(capacity.min(64 * 1024)),
            capacity,
            in_alt_screen: false,
            evicted_total: 0,
        }
    }

    /// 現在 capacity (= byte 単位)。
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// 現在 ring に保持されている byte 数。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn len(&self) -> usize {
        self.ring.len()
    }

    /// ring が空か。
    pub(crate) fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// alt screen 中なら true (= push を skip 中)。
    #[cfg(test)]
    pub(crate) fn in_alt_screen(&self) -> bool {
        self.in_alt_screen
    }

    /// 累計 evict byte 数 (= ring から押し出された総 byte)。
    #[cfg(test)]
    pub(crate) fn evicted_total(&self) -> u64 {
        self.evicted_total
    }

    /// alt screen mode を更新する。
    ///
    /// `true` を渡すと以降の `push` は skip され、`false` で再開する。値が
    /// 変わったときのみ呼び出すのが想定だが、同値で呼んでも副作用なし。
    pub(crate) fn set_alt_mode(&mut self, in_alt: bool) {
        self.in_alt_screen = in_alt;
    }

    /// 新 chunk を末尾に push する。
    ///
    /// - alt screen 中は無視
    /// - `capacity == 0` は無視 (= push しても即 evict、ring に残らない)
    /// - 既存と新 bytes の合計が `capacity` を超える分は先頭から evict
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        if self.in_alt_screen || self.capacity == 0 || bytes.is_empty() {
            return;
        }

        // bytes が単体で capacity を超える場合は末尾だけ残すのが正しい
        // (= replay 時の最新 state を優先)。
        let to_append: &[u8] = if bytes.len() > self.capacity {
            let drop = bytes.len() - self.capacity;
            self.evicted_total = self.evicted_total.saturating_add(drop as u64);
            &bytes[drop..]
        } else {
            bytes
        };

        // 必要分だけ ring 先頭を evict してから extend する。
        if self.ring.len() + to_append.len() > self.capacity {
            let overflow = self.ring.len() + to_append.len() - self.capacity;
            self.evicted_total = self.evicted_total.saturating_add(overflow as u64);
            self.ring.drain(..overflow);
        }
        self.ring.extend(to_append.iter().copied());
    }

    /// resize 時の replay 用に ring の中身を copy out する。
    ///
    /// 内部 `VecDeque` は contiguous でないので `Vec<u8>` に集約して返す。
    /// ring 自体は破壊しない (= 同じ bytes を複数回 replay 可能、`clear` で消す)。
    pub(crate) fn drain_for_replay(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.ring.len());
        let (a, b) = self.ring.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        out
    }

    /// ring を空にする (= `reset` 用、`evicted_total` も 0 に戻す)。
    ///
    /// alt mode flag は保持する (= 呼出側が責任を持って制御)。
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.evicted_total = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_default_state() {
        let log = InputLog::new(1024);
        assert_eq!(log.capacity(), 1024);
        assert!(log.is_empty());
        assert!(!log.in_alt_screen());
        assert_eq!(log.evicted_total(), 0);
    }

    #[test]
    fn push_appends_bytes_under_capacity() {
        let mut log = InputLog::new(100);
        log.push(b"hello");
        log.push(b" world");
        assert_eq!(log.drain_for_replay(), b"hello world");
        assert_eq!(log.evicted_total(), 0);
    }

    #[test]
    fn push_evicts_oldest_on_overflow() {
        let mut log = InputLog::new(5);
        log.push(b"abcde");
        assert_eq!(log.drain_for_replay(), b"abcde");
        log.push(b"fg");
        // 2 byte overflow → "abcde" の先頭 2 byte が evict、残りは "cdefg"
        assert_eq!(log.drain_for_replay(), b"cdefg");
        assert_eq!(log.evicted_total(), 2);
    }

    #[test]
    fn push_chunk_larger_than_capacity_keeps_tail() {
        let mut log = InputLog::new(4);
        log.push(b"hello world"); // 11 byte > 4
        // 末尾 4 byte だけ残る
        assert_eq!(log.drain_for_replay(), b"orld");
        assert_eq!(log.evicted_total(), 7);
    }

    #[test]
    fn push_alt_mode_skips() {
        let mut log = InputLog::new(100);
        log.push(b"primary");
        log.set_alt_mode(true);
        log.push(b"-alt");
        log.set_alt_mode(false);
        log.push(b"-back");
        assert_eq!(log.drain_for_replay(), b"primary-back");
    }

    #[test]
    fn push_zero_capacity_never_stores() {
        let mut log = InputLog::new(0);
        log.push(b"hello");
        assert!(log.is_empty());
        // capacity 0 は退化型 (= 即 evict もせず保持もしない)
    }

    #[test]
    fn drain_for_replay_does_not_mutate() {
        let mut log = InputLog::new(10);
        log.push(b"hello");
        let _ = log.drain_for_replay();
        // 2 回目も同じ内容が取れる
        assert_eq!(log.drain_for_replay(), b"hello");
    }

    #[test]
    fn clear_resets_ring_and_evicted_total() {
        let mut log = InputLog::new(5);
        log.push(b"helloworld"); // overflow
        assert!(log.evicted_total() > 0);
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.evicted_total(), 0);
    }

    #[test]
    fn evicted_total_accumulates_across_pushes() {
        let mut log = InputLog::new(3);
        log.push(b"abc"); // 0 evict
        log.push(b"de"); // 2 evict
        log.push(b"fg"); // 2 evict
        assert_eq!(log.evicted_total(), 4);
        assert_eq!(log.drain_for_replay(), b"efg");
    }

    #[test]
    fn empty_push_is_noop() {
        let mut log = InputLog::new(10);
        log.push(b"");
        assert!(log.is_empty());
        assert_eq!(log.evicted_total(), 0);
    }
}
