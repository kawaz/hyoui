//! `ScreenState` — vt100 Parser を内包し、daemon が保持する screen state の正本。
//!
//! DR-0013 §3 で確定した wrapper module の中核。`Parser::process` で子 PTY bytes を
//! state に流し込み、`Screen` 経由で cell / cursor / mode 等を expose する。
//!
//! Phase A の責務は最小限:
//!
//! 1. 子 PTY bytes を 1 度だけ feed する経路を提供する (= `process`)
//! 2. attach 復元用の sequence を組み立てるための primitive を expose (= `redraw_sequence`,
//!    `alternate_screen`, `cursor_position`, `cursor_visible`, `size`)
//! 3. DEC sync update (`?2026h` / `?2026l`) の同期中フラグを保持し、redraw の deferred
//!    判定に使う (= `sync_in_progress`)
//! 4. stalled sequence 検出用に `last_feed_at: Instant` を保持し、5 秒経過判定 (`is_stalled`)
//!    と内部 buffer reset (`reset_stalled`) を提供する
//!
//! Phase B での追加予定 (= 本 module の責務には含めない):
//!
//! - input bytes log (= resize 救済策、§7)
//! - structured snapshot (= §11)
//! - last_evicted_age 補完 counter (= §8)
//! - per-line SequenceNo (= §4 Phase B)

use std::time::{Duration, Instant};

/// stalled sequence reset の閾値。tmux `input.c` 標準と揃える (= 5 秒)。
///
/// `last_feed_at` から本値経過しても新規 bytes が来なければ、parser 内部 buffer に
/// 取り残された partial escape sequence を捨てて整合を取る。broken byte stream で
/// parser が永久に partial 状態に閉じ込められるのを防ぐ (DR-0013 §5)。
pub(crate) const STALLED_RESET_TIMEOUT: Duration = Duration::from_secs(5);

/// daemon が保持する screen state の正本 wrapper。
///
/// vt100 `Parser` (= `Screen` を内包) をそのまま正本にし、hyoui 側の責務 (= sync
/// flag / stalled timer / 補完 hook) を追加 layer として持つ。`process` は 1 度
/// だけ呼び、子 PTY bytes は本 wrapper を経由してから broadcast / wait / tail に
/// 流れる (= DR-0013 §1「raw byte の直接 broadcast はしない」)。
pub(crate) struct ScreenState {
    parser: vt100::Parser,
    /// 最後に `process` で bytes を feed した時刻。stalled 判定に使う。
    last_feed_at: Instant,
    /// DEC sync update (`\x1b[?2026h` ... `\x1b[?2026l`) の同期中フラグ。
    ///
    /// vt100 は本 mode を内部処理しないため (vt100 0.16 時点)、hyoui wrapper で
    /// process 時に検出する。同期中は `redraw_sequence` の戻り値を blocking する
    /// 設計とし、中途半端な state の redraw を send しない (= DR-0013 §6 + alacritty
    /// `event_loop.rs:166` pattern)。
    sync_in_progress: bool,
}

impl ScreenState {
    /// rows × cols viewport、scrollback_len 行 ring の new state を作る。
    ///
    /// `vt100::Parser::new` をそのまま呼ぶ薄い factory。`last_feed_at` は now で
    /// 初期化し、初期状態では sync flag は off。
    pub(crate) fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, scrollback_len),
            last_feed_at: Instant::now(),
            sync_in_progress: false,
        }
    }

    /// 子 PTY 出力 bytes を vt100 parser に流し込む。
    ///
    /// DEC sync update (`?2026h`/`l`) の検出は本関数内で行う (= bytes を走査して
    /// 該当 sequence の出現で `sync_in_progress` を更新する)。vt100 0.16 は本 mode を
    /// 内部処理しないため、wrapper 側で hook する必要がある。
    pub(crate) fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.last_feed_at = Instant::now();
        update_sync_flag(&mut self.sync_in_progress, bytes);
    }

    /// 現在 alt screen (`?1049h` / `?47h` / `?1047h`) に居るか。
    pub(crate) fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// 0-origin cursor position `(row, col)`。
    ///
    /// Phase A 時点では caller は無いが、Phase B の structured snapshot / debug
    /// inspection (DR-0013 §9) で expose 予定のため API として保持する。
    #[allow(dead_code)]
    pub(crate) fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// cursor が可視か (`?25h` で true、`?25l` で false)。
    ///
    /// Phase B の structured snapshot 用 (DR-0013 §9)。
    #[allow(dead_code)]
    pub(crate) fn cursor_visible(&self) -> bool {
        !self.parser.screen().hide_cursor()
    }

    /// viewport size `(rows, cols)`。
    ///
    /// Phase A では `reset` で内部利用、Phase B の snapshot でも expose 予定。
    pub(crate) fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// DEC sync update 同期中なら true。同期中は attach 復元 redraw を送らない。
    pub(crate) fn sync_in_progress(&self) -> bool {
        self.sync_in_progress
    }

    /// attach 復元用の primary sequence (= `Screen::state_formatted()`)。
    ///
    /// alt screen mode の prepend は本関数では行わず、caller (= `redraw.rs`) が
    /// 組み立てる。本関数は vt100 が出す raw bytes をそのまま返すだけ。
    pub(crate) fn state_formatted(&self) -> Vec<u8> {
        self.parser.screen().state_formatted()
    }

    /// `last_feed_at` から `STALLED_RESET_TIMEOUT` 経過していれば true。
    pub(crate) fn is_stalled(&self, now: Instant) -> bool {
        now.duration_since(self.last_feed_at) >= STALLED_RESET_TIMEOUT
    }

    /// 内部 parser を新規構築して partial sequence buffer を捨てる。
    ///
    /// 現在の `Screen` state (= cells / cursor / mode 等) は失われるため、Phase A
    /// では「呼出側が判断したときに使う最終手段」として提供する。Phase A 既定の
    /// health check は warn log のみで state は保持する保守的方針 (= DR-0013 §5
    /// + DR-0013 task A-8)。
    ///
    /// reset 後の `last_feed_at` は now、`sync_in_progress` は false に戻る。
    ///
    /// Phase A の health check は detect only (= warn のみ) で reset を呼ばない。
    /// Phase B で stalled 時の挙動を再検討する際に呼出側を実装する予定。
    #[allow(dead_code)]
    pub(crate) fn reset(&mut self) {
        let (rows, cols) = self.size();
        // scrollback_len は vt100 0.16 では Parser::new 時に決定し、Parser から
        // 引き出す公開 API が無いため、現実装では既定値 (= 0) を渡す。Phase B で
        // scrollback 統合する際は DaemonConfig 経由で揃える設計に切替える。
        self.parser = vt100::Parser::new(rows, cols, 0);
        self.last_feed_at = Instant::now();
        self.sync_in_progress = false;
    }

    /// テスト用に内部 `Screen` を直接覗く。本 module 外には公開しない。
    #[cfg(test)]
    pub(crate) fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}

/// `bytes` 中の DEC sync update (`\x1b[?2026h` / `\x1b[?2026l`) を検出し、
/// `flag` を更新する。最後に検出した状態を flag に反映する (= 1 chunk 内に複数
/// 出現があれば最終結果を採用)。
///
/// 完全な CSI parser を書かず、`\x1b[?2026h` / `\x1b[?2026l` の固定 byte 列を
/// substring search するだけの素朴実装。partial sequence が chunk 境界に跨ると
/// 検出を取りこぼす可能性があるが、Phase A では「同期中の attach は次の sync 終了
/// まで blocking」程度のシンプル実装で OK な範囲 (= DR-0013 task A-7)。
fn update_sync_flag(flag: &mut bool, bytes: &[u8]) {
    const ON: &[u8] = b"\x1b[?2026h";
    const OFF: &[u8] = b"\x1b[?2026l";
    // 最終出現を採用するため、bytes を 1 度走査して on/off の最後の位置を比較する。
    let last_on = find_last_subseq(bytes, ON);
    let last_off = find_last_subseq(bytes, OFF);
    match (last_on, last_off) {
        (None, None) => {}
        (Some(_), None) => *flag = true,
        (None, Some(_)) => *flag = false,
        (Some(a), Some(b)) => *flag = a > b,
    }
}

/// `haystack` 中の `needle` の最後の出現位置を返す素朴 search。`needle` が空なら
/// None。bytes len は典型 8 KiB 以下、needle は 8 byte 固定で性能影響なし。
fn find_last_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut last = None;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            last = Some(i);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PoC §1: 基本構築 + process / size getter。
    #[test]
    fn new_and_size() {
        let s = ScreenState::new(24, 80, 100);
        assert_eq!(s.size(), (24, 80));
        assert!(!s.alternate_screen());
        assert_eq!(s.cursor_position(), (0, 0));
    }

    /// PoC §2: 簡易 process → screen content 反映。
    #[test]
    fn process_writes_cells() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process(b"hello");
        let scr = s.screen();
        let c = scr.cell(0, 0).unwrap();
        assert_eq!(c.contents(), "h");
        let c = scr.cell(0, 4).unwrap();
        assert_eq!(c.contents(), "o");
    }

    /// PoC §2: state_formatted の roundtrip (= 同じ Parser に流し戻して内容一致)。
    #[test]
    fn state_formatted_roundtrip_simple() {
        let mut s1 = ScreenState::new(5, 40, 100);
        s1.process(b"hello\x1b[1;31mRED\x1b[0m world\r\nline2");
        let formatted = s1.state_formatted();

        let mut s2 = ScreenState::new(5, 40, 100);
        s2.process(&formatted);

        // 主要 cell の中身が一致すること。
        for r in 0..5 {
            for c in 0..40 {
                let c1 = s1.screen().cell(r, c).unwrap();
                let c2 = s2.screen().cell(r, c).unwrap();
                assert_eq!(c1.contents(), c2.contents(), "char mismatch at ({r},{c})");
                assert_eq!(c1.fgcolor(), c2.fgcolor(), "fg mismatch at ({r},{c})");
                assert_eq!(c1.bold(), c2.bold(), "bold mismatch at ({r},{c})");
            }
        }
    }

    /// PoC §4: alt screen 切替の判定。
    #[test]
    fn alternate_screen_toggles() {
        let mut s = ScreenState::new(10, 40, 100);
        s.process(b"primary text");
        assert!(!s.alternate_screen());
        s.process(b"\x1b[?1049h");
        assert!(s.alternate_screen());
        s.process(b"alt text");
        assert!(s.alternate_screen());
        s.process(b"\x1b[?1049l");
        assert!(!s.alternate_screen());
    }

    /// PoC §7: cursor visibility hook。
    #[test]
    fn cursor_visible_toggles() {
        let mut s = ScreenState::new(5, 40, 100);
        assert!(s.cursor_visible(), "default visible");
        s.process(b"\x1b[?25l");
        assert!(!s.cursor_visible());
        s.process(b"\x1b[?25h");
        assert!(s.cursor_visible());
    }

    /// PoC §7: cursor 位置の取得。
    #[test]
    fn cursor_position_after_move() {
        let mut s = ScreenState::new(10, 40, 100);
        s.process(b"\x1b[5;10H");
        // 1-origin ANSI → 0-origin vt100
        assert_eq!(s.cursor_position(), (4, 9));
    }

    /// PoC §8: wide char (= 日本語) が 2 cell に保持される。
    #[test]
    fn wide_char_occupies_two_cells() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process("あ".as_bytes());
        let scr = s.screen();
        let c0 = scr.cell(0, 0).unwrap();
        let c1 = scr.cell(0, 1).unwrap();
        assert_eq!(c0.contents(), "あ");
        assert!(c0.is_wide());
        assert!(c1.is_wide_continuation());
        // 次の cursor 位置は col=2
        assert_eq!(s.cursor_position(), (0, 2));
    }

    /// DEC sync update mode の hook: `?2026h` で sync_in_progress = true、
    /// `?2026l` で false。
    #[test]
    fn sync_update_flag_tracks_dec_sync() {
        let mut s = ScreenState::new(5, 40, 100);
        assert!(!s.sync_in_progress());
        s.process(b"\x1b[?2026h");
        assert!(s.sync_in_progress(), "sync should be ON");
        s.process(b"some draw");
        assert!(s.sync_in_progress(), "sync stays ON during draws");
        s.process(b"\x1b[?2026l");
        assert!(!s.sync_in_progress(), "sync should be OFF after l");
    }

    /// 1 chunk 内に sync on/off が両方ある場合は「最終出現」を採用する。
    #[test]
    fn sync_update_flag_uses_last_occurrence_in_chunk() {
        let mut s = ScreenState::new(5, 40, 100);
        // off → on の順で同 chunk
        s.process(b"\x1b[?2026l\x1b[?2026h");
        assert!(s.sync_in_progress(), "last is ON");
        // on → off の順で同 chunk
        s.process(b"\x1b[?2026h\x1b[?2026l");
        assert!(!s.sync_in_progress(), "last is OFF");
    }

    /// stalled 判定: 5 秒経過判定の境界を Instant 操作なしで確認するため、
    /// `STALLED_RESET_TIMEOUT` を直接超過させた `Instant` を渡す。
    #[test]
    fn is_stalled_after_timeout() {
        let s = ScreenState::new(5, 40, 100);
        // 直後は stalled でない
        assert!(!s.is_stalled(Instant::now()));
        // 6 秒先の Instant を渡せば stalled
        let future = Instant::now() + STALLED_RESET_TIMEOUT + Duration::from_secs(1);
        assert!(s.is_stalled(future));
    }

    /// reset で parser が新規構築され、過去の cell 内容が消える。
    #[test]
    fn reset_clears_screen() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process(b"hello");
        assert_eq!(s.screen().cell(0, 0).unwrap().contents(), "h");
        s.reset();
        // 新 Parser は cell が空
        let c = s.screen().cell(0, 0).unwrap();
        assert_eq!(c.contents(), "");
        assert!(!s.sync_in_progress());
    }
}
