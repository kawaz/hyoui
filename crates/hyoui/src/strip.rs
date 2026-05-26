//! ANSI escape stripper (CSI / OSC / DCS / single char escape を strip して raw text を返す)。
//!
//! 装飾除去は wait/match で `--strip-escapes` default ON、`--raw` で opt-out する用途。
//! C0/C1 制御文字 (BEL/BS/TAB 等) は **strip しない** (= text として有意のため、L0 デフォルト)。
//! Cursor 移動による「同じ cell 上書き」は扱えない (= L1 emulator が必要、bytes 順と実画面の差は v0.2.0 で対応)。
//!
//! 参照: docs/decisions/DR-0006-cli-ground-rules.md §11 装飾除去、
//! docs/findings/2026-05-26-ansi-strip.md (PoC 検証 synthetic 11/11 + 実 sample ESC 残留 0)

/// 入力 bytes から ANSI escape を除去した bytes を返す。
///
/// 対応 escape:
/// - **CSI** (`ESC [ ...`): parameter bytes (0x30..=0x3f) → intermediate (0x20..=0x2f) → final (0x40..=0x7e)
/// - **OSC** (`ESC ] ...`): BEL (0x07) or ST (`ESC \`) で終端
/// - **DCS / SOS / PM / APC** (`ESC P/X/^/_`): ST (`ESC \`) で終端
/// - **single char** (`ESC <1 byte>`): 例 `ESC=` (keypad app)、`ESC M` (RI)、`ESC >` 等
///
/// C0/C1 制御文字 (BEL/BS/CR/LF/TAB 等) は残す。完全な C0 除去が欲しければ別途処理。
pub fn strip_ansi(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] != 0x1b {
            out.push(input[i]);
            i += 1;
            continue;
        }
        // ESC sequence: skip until terminator
        i += 1;
        if i >= input.len() {
            break;
        }
        match input[i] {
            b'[' => {
                // CSI: param (0x30..=0x3f) → intermediate (0x20..=0x2f) → final (0x40..=0x7e)
                i += 1;
                while i < input.len() && (0x30..=0x3f).contains(&input[i]) {
                    i += 1;
                }
                while i < input.len() && (0x20..=0x2f).contains(&input[i]) {
                    i += 1;
                }
                if i < input.len() && (0x40..=0x7e).contains(&input[i]) {
                    i += 1;
                }
            }
            b']' => {
                // OSC: terminate by BEL (0x07) or ST (ESC \)
                i += 1;
                while i < input.len() {
                    if input[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            b'P' | b'X' | b'^' | b'_' => {
                // DCS / SOS / PM / APC: terminate by ST
                i += 1;
                while i < input.len() {
                    if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                // single char escape (ESC + 1 char)
                i += 1;
            }
        }
    }
    out
}

/// CRLF / CR を LF に正規化する (装飾除去とは別レイヤ)。
///
/// 用途: pty の `ONLCR` (= cooked mode) で `\n → \r\n` 変換された出力を text として
/// 安定的に match したい時。`--newline-convert=lf` で利用 (default `preserve`)。
///
/// `\r\n` → `\n`、`\r` 単独 → `\n`。
pub fn normalize_lf(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\r' {
            if i + 1 < input.len() && input[i + 1] == b'\n' {
                out.push(b'\n');
                i += 2;
            } else {
                out.push(b'\n');
                i += 1;
            }
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

/// LF を CRLF に正規化する (Windows 系の出力先用)。
///
/// 既存の `\r\n` は変更しない (= 二重変換しない)、単独 `\n` のみを `\r\n` 化。
pub fn normalize_crlf(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + input.len() / 80);
    let mut prev_cr = false;
    for &b in input {
        if b == b'\n' && !prev_cr {
            out.push(b'\r');
        }
        out.push(b);
        prev_cr = b == b'\r';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_escape_passes_through() {
        assert_eq!(strip_ansi(b"hello"), b"hello");
        assert_eq!(strip_ansi(b""), b"");
    }

    #[test]
    fn sgr_color() {
        assert_eq!(strip_ansi(b"\x1b[31mRed\x1b[0m text"), b"Red text");
    }

    #[test]
    fn clear_screen_and_cursor() {
        assert_eq!(strip_ansi(b"a\x1b[2J\x1b[Hb"), b"ab");
    }

    #[test]
    fn osc_bel_terminated() {
        assert_eq!(strip_ansi(b"\x1b]0;title\x07ok"), b"ok");
    }

    #[test]
    fn osc_st_terminated() {
        assert_eq!(strip_ansi(b"\x1b]0;title\x1b\\ok"), b"ok");
    }

    #[test]
    fn dcs() {
        assert_eq!(strip_ansi(b"\x1bP1$rdcs\x1b\\post"), b"post");
    }

    #[test]
    fn single_char_escape() {
        assert_eq!(strip_ansi(b"\x1b=ok"), b"ok");
        assert_eq!(strip_ansi(b"\x1bMtest"), b"test");
    }

    #[test]
    fn bracketed_paste_enable_disable() {
        assert_eq!(strip_ansi(b"\x1b[?2004hpaste\x1b[?2004l"), b"paste");
    }

    #[test]
    fn alternate_screen_enable_disable() {
        assert_eq!(strip_ansi(b"\x1b[?1049hAlt\x1b[?1049l"), b"Alt");
    }

    #[test]
    fn cursor_positioning() {
        assert_eq!(strip_ansi(b"\x1b[12;34Hcursor"), b"cursor");
    }

    #[test]
    fn multi_param_sgr() {
        assert_eq!(strip_ansi(b"\x1b[1;31;47mfancy\x1b[0m"), b"fancy");
    }

    #[test]
    fn c0_control_chars_are_preserved() {
        assert_eq!(strip_ansi(b"a\x07b\x08c\td\re\nf"), b"a\x07b\x08c\td\re\nf");
    }

    #[test]
    fn truncated_escape_at_end() {
        assert_eq!(strip_ansi(b"hello\x1b"), b"hello");
        assert_eq!(strip_ansi(b"hello\x1b["), b"hello");
        assert_eq!(strip_ansi(b"hello\x1b[31"), b"hello");
    }

    #[test]
    fn normalize_lf_crlf_to_lf() {
        assert_eq!(normalize_lf(b"a\r\nb\r\nc"), b"a\nb\nc");
    }

    #[test]
    fn normalize_lf_cr_to_lf() {
        assert_eq!(normalize_lf(b"a\rb\rc"), b"a\nb\nc");
    }

    #[test]
    fn normalize_lf_mixed() {
        assert_eq!(normalize_lf(b"a\nb\r\nc\rd"), b"a\nb\nc\nd");
    }

    #[test]
    fn normalize_crlf_adds_cr_to_lone_lf() {
        assert_eq!(normalize_crlf(b"a\nb\nc"), b"a\r\nb\r\nc");
    }

    #[test]
    fn normalize_crlf_preserves_existing_crlf() {
        assert_eq!(normalize_crlf(b"a\r\nb\r\nc"), b"a\r\nb\r\nc");
    }
}
