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

/// Stateful ANSI escape stripper that carries an in-progress escape across
/// chunk boundaries (R4-H3).
///
/// `strip_ansi` is stateless: each call treats input as a self-contained
/// buffer, so an ESC sequence split across two adjacent PTY reads is mis-
/// handled (the trailing bytes of the second chunk leak into the output as
/// raw text). `StripAnsiCarry` keeps any partial escape from the previous
/// call's tail in `carry`, and on the next `push` prepends it before
/// scanning, so the escape is correctly stripped end-to-end.
///
/// Memory bound: `carry` is capped at `MAX_CARRY` bytes (= longer escapes
/// are extremely rare; if exceeded we flush carry as raw bytes to avoid
/// unbounded growth from malformed streams).
#[derive(Debug, Default)]
pub struct StripAnsiCarry {
    /// Bytes from the previous call that started but did not finish an
    /// escape sequence. Always begins with `0x1b`.
    carry: Vec<u8>,
}

impl StripAnsiCarry {
    /// Maximum bytes to retain in `carry` between calls. Most escapes are
    /// <16 bytes; anything past this is treated as a malformed stream and
    /// flushed as raw output.
    const MAX_CARRY: usize = 4096;

    /// Create an empty carry-buffer stripper.
    #[must_use]
    pub fn new() -> Self {
        Self { carry: Vec::new() }
    }

    /// Process a chunk of bytes, returning the stripped output. Any
    /// in-progress escape at the tail is retained for the next call.
    pub fn push(&mut self, input: &[u8]) -> Vec<u8> {
        // Combine carry + new input, then scan. Concatenation is required
        // because the escape may start in `carry` (partial from prev call)
        // and complete several bytes into `input`.
        let buf: Vec<u8> = if self.carry.is_empty() {
            input.to_vec()
        } else {
            let mut v = std::mem::take(&mut self.carry);
            v.extend_from_slice(input);
            v
        };

        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                out.push(buf[i]);
                i += 1;
                continue;
            }
            // ESC at i. Try to consume a full escape; if the tail is
            // incomplete, stash it as carry and stop.
            let start = i;
            i += 1;
            if i >= buf.len() {
                // ESC alone at end → carry
                self.stash_carry(&buf, start);
                return out;
            }
            match buf[i] {
                b'[' => {
                    // CSI: params (0x30..=0x3f) → inter (0x20..=0x2f) → final (0x40..=0x7e)
                    i += 1;
                    while i < buf.len() && (0x30..=0x3f).contains(&buf[i]) {
                        i += 1;
                    }
                    while i < buf.len() && (0x20..=0x2f).contains(&buf[i]) {
                        i += 1;
                    }
                    if i >= buf.len() {
                        self.stash_carry(&buf, start);
                        return out;
                    }
                    if (0x40..=0x7e).contains(&buf[i]) {
                        i += 1;
                    } else {
                        // out-of-range terminator: treat as broken,
                        // advance one byte and continue (mirrors stateless
                        // strip_ansi: incomplete CSI at end is dropped).
                        // Here we have data after, so just resume.
                    }
                }
                b']' => {
                    // OSC: BEL or ST terminator
                    i += 1;
                    let mut terminated = false;
                    while i < buf.len() {
                        if buf[i] == 0x07 {
                            i += 1;
                            terminated = true;
                            break;
                        }
                        if buf[i] == 0x1b {
                            if i + 1 < buf.len() && buf[i + 1] == b'\\' {
                                i += 2;
                                terminated = true;
                                break;
                            }
                            // Partial ST (`ESC` then EOF) → carry from this ESC's start
                            self.stash_carry(&buf, start);
                            return out;
                        }
                        i += 1;
                    }
                    if !terminated {
                        // Reached end of buf without terminator → carry whole OSC
                        self.stash_carry(&buf, start);
                        return out;
                    }
                }
                b'P' | b'X' | b'^' | b'_' => {
                    // DCS / SOS / PM / APC: ST terminator
                    i += 1;
                    let mut terminated = false;
                    while i < buf.len() {
                        if buf[i] == 0x1b {
                            if i + 1 < buf.len() && buf[i + 1] == b'\\' {
                                i += 2;
                                terminated = true;
                                break;
                            }
                            self.stash_carry(&buf, start);
                            return out;
                        }
                        i += 1;
                    }
                    if !terminated {
                        self.stash_carry(&buf, start);
                        return out;
                    }
                }
                _ => {
                    // single char escape (ESC + 1 byte)
                    i += 1;
                }
            }
        }

        // Drained buf; carry empty (unless stashed above).
        // Carry is empty here because all sequences either completed or
        // returned early via stash_carry.
        out
    }

    /// Helper: stash `buf[start..]` as carry, falling back to flushing it
    /// as raw output if it would exceed `MAX_CARRY`. The fallback prevents
    /// unbounded growth from streams that emit a stray ESC without
    /// terminator (malformed / truncated).
    fn stash_carry(&mut self, buf: &[u8], start: usize) {
        let tail = &buf[start..];
        if tail.len() > Self::MAX_CARRY {
            // give up carrying; the stateless strip_ansi treats trailing
            // incomplete escapes as dropped. Match that semantically by
            // dropping (= do not output the partial escape bytes).
            self.carry.clear();
        } else {
            self.carry.clear();
            self.carry.extend_from_slice(tail);
        }
    }

    /// Drain any pending carry as raw output (e.g., on stream close).
    /// Currently unused but provided for completeness/symmetry with
    /// stateless `strip_ansi` (which silently drops trailing partial
    /// escapes).
    #[allow(dead_code)]
    pub fn finish(&mut self) -> Vec<u8> {
        let _ = std::mem::take(&mut self.carry);
        // strip_ansi drops trailing partial escapes; do the same.
        Vec::new()
    }
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

    // ---- StripAnsiCarry (R4-H3): chunk-boundary handling ----

    #[test]
    fn carry_no_escape_passes_through() {
        let mut s = StripAnsiCarry::new();
        assert_eq!(s.push(b"hello"), b"hello");
        assert_eq!(s.push(b" world"), b" world");
    }

    #[test]
    fn carry_csi_split_across_chunks_is_stripped() {
        // chunk1 ends with partial CSI, chunk2 finishes it. Without carry,
        // chunk2's `1m` and `ABC` would leak as raw text.
        let mut s = StripAnsiCarry::new();
        let out1 = s.push(b"pre\x1b[3");
        assert_eq!(out1, b"pre");
        let out2 = s.push(b"1mABC");
        assert_eq!(out2, b"ABC");
    }

    #[test]
    fn carry_esc_alone_at_chunk_boundary() {
        let mut s = StripAnsiCarry::new();
        assert_eq!(s.push(b"hi\x1b"), b"hi");
        assert_eq!(s.push(b"[31mX"), b"X");
    }

    #[test]
    fn carry_osc_split_across_chunks() {
        // OSC must terminate by BEL or ST; split before BEL → carry.
        let mut s = StripAnsiCarry::new();
        assert_eq!(s.push(b"a\x1b]0;tit"), b"a");
        assert_eq!(s.push(b"le\x07ok"), b"ok");
    }

    #[test]
    fn carry_osc_split_inside_st() {
        // ST is ESC \. Split between ESC and \.
        let mut s = StripAnsiCarry::new();
        assert_eq!(s.push(b"a\x1b]0;title\x1b"), b"a");
        assert_eq!(s.push(b"\\post"), b"post");
    }

    #[test]
    fn carry_dcs_split_across_chunks() {
        let mut s = StripAnsiCarry::new();
        assert_eq!(s.push(b"a\x1bP1$rdc"), b"a");
        assert_eq!(s.push(b"s\x1b\\post"), b"post");
    }

    #[test]
    fn carry_single_byte_escape_at_boundary() {
        // ESC alone in chunk1, single-char terminator in chunk2.
        let mut s = StripAnsiCarry::new();
        assert_eq!(s.push(b"a\x1b"), b"a");
        assert_eq!(s.push(b"=ok"), b"ok");
    }

    #[test]
    fn carry_needle_match_across_chunk_after_strip() {
        // The motivating scenario: needle "READY" arrives such that an
        // escape sequence spans the chunk boundary just before READY.
        let mut s = StripAnsiCarry::new();
        let mut acc = Vec::new();
        acc.extend(s.push(b"prefix\x1b[3"));
        acc.extend(s.push(b"1mREADY\n"));
        // After both pushes, accumulated stripped bytes contain "READY".
        let needle = b"READY";
        let found = acc.windows(needle.len()).any(|w| w == needle);
        assert!(
            found,
            "needle should match across split escape; got {acc:?}"
        );
    }

    #[test]
    fn carry_giant_unterminated_escape_does_not_grow_unbounded() {
        // A malformed stream emits ESC without terminator forever; carry
        // must not blow memory.
        let mut s = StripAnsiCarry::new();
        let big = vec![0u8; StripAnsiCarry::MAX_CARRY + 1];
        let mut input = vec![0x1b, b']'];
        input.extend_from_slice(&big);
        let out = s.push(&input);
        // Output is empty (escape opened, no terminator, dropped).
        assert!(out.is_empty());
        // Subsequent push should not be polluted by the giant carry.
        assert_eq!(s.push(b"plain"), b"plain");
    }
}
