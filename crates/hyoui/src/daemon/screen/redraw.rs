//! attach 復元用の bytes sequence を組み立てるヘルパ群 (DR-0013 §4 Phase A)。
//!
//! `ScreenState::state_formatted()` は cell-level の状態を完全に保持するが、
//! PoC §2 で実証された通り **alt screen のフラグを復元しない**。本 module は
//! その欠落を 1 行の prepend で補い、`?1049h` / `?1049l` を被せて完全な復元
//! sequence を返す。
//!
//! 出力 sequence の構成 (= DR-0013 §4):
//!
//! 1. alt screen mode に応じて先頭に `\x1b[?1049h` (alt) または `\x1b[?1049l`
//!    (primary) を被せる。client terminal が detach 前と異なる buffer に居ても
//!    確実に切替えるため、primary 側でも明示する。
//! 2. `Screen::state_formatted()` の出力を結合する。これは内部で
//!    `ESC[?25h ESC[m ESC[H ESC[J <content> ESC[r;cH ESC> ESC[?1l ESC[?2004l`
//!    の順で「cursor 表示状態 / 色リセット / Home / clear / 本体描画 / cursor
//!    位置 / app keypad off / DECCKM off / bracketed paste off」を組み立てる。

use super::state::ScreenState;

/// attach 復元用の bytes sequence を 1 つの `Vec<u8>` で組み立てて返す。
///
/// client は本 bytes を stdout に書くだけで detach 時の画面が復元される
/// (DR-0013 §4 Phase A: push 型 redraw bytes)。
pub(crate) fn build_attach_redraw(state: &ScreenState) -> Vec<u8> {
    let mut out = Vec::new();
    // alt screen フラグの prepend (PoC §2 で発覚した state_formatted の欠落補完)。
    // primary 側でも `?1049l` を明示し、client が detach 前と別 buffer に居ても
    // 強制的に正しい buffer に揃える。
    if state.alternate_screen() {
        out.extend_from_slice(b"\x1b[?1049h");
    } else {
        out.extend_from_slice(b"\x1b[?1049l");
    }
    out.extend_from_slice(&state.state_formatted());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// primary buffer: redraw 出力の冒頭は `?1049l` で始まる。
    #[test]
    fn redraw_prepends_primary_off_for_primary_state() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process(b"hello");
        let out = build_attach_redraw(&s);
        assert!(out.starts_with(b"\x1b[?1049l"), "primary prepend");
    }

    /// alt screen: redraw 出力の冒頭は `?1049h` で始まる。
    #[test]
    fn redraw_prepends_alt_on_for_alt_state() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process(b"\x1b[?1049h");
        assert!(s.alternate_screen());
        let out = build_attach_redraw(&s);
        assert!(out.starts_with(b"\x1b[?1049h"), "alt prepend");
    }

    /// redraw を別 state に流し戻すと alt フラグまで含めて復元できる
    /// (= PoC §2 で ✗ だった alt round trip が wrapper で ✓ になる)。
    #[test]
    fn redraw_roundtrip_preserves_alt_flag() {
        let mut s1 = ScreenState::new(10, 40, 100);
        s1.process(b"primary\r\n\x1b[?1049h\x1b[2J\x1b[Halt-screen text");
        assert!(s1.alternate_screen());

        let out = build_attach_redraw(&s1);
        let mut s2 = ScreenState::new(10, 40, 100);
        s2.process(&out);

        assert!(s2.alternate_screen(), "alt flag preserved via wrapper");
        // cell content も拾える (= state_formatted の round trip)
        let scr2 = s2.screen();
        let c0 = scr2.cell(0, 0).unwrap();
        assert_eq!(c0.contents(), "a");
    }

    /// redraw を別 state に流し戻して cursor 位置と可視状態が一致する。
    #[test]
    fn redraw_roundtrip_preserves_cursor() {
        let mut s1 = ScreenState::new(10, 40, 100);
        s1.process(b"\x1b[5;10H@\x1b[?25l");
        let out = build_attach_redraw(&s1);

        let mut s2 = ScreenState::new(10, 40, 100);
        s2.process(&out);

        assert_eq!(s2.cursor_position(), s1.cursor_position());
        assert_eq!(s2.cursor_visible(), s1.cursor_visible());
    }
}
