//! PoC 08: ANSI escape strip
//!
//! CSI / OSC / DCS / single char escape を strip して raw text を取り出す。
//! PoC 03 で取得した /tmp/hyoui-poc-03-{bash,vi,less}.raw を入力にして動作確認。
//!
//! 実行:
//!   cargo run --example 08-ansi-strip                   # 全 sample
//!   cargo run --example 08-ansi-strip -- /path/to.raw   # 任意 file

use std::path::Path;

fn strip_ansi(input: &[u8]) -> Vec<u8> {
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
                // CSI: parameter bytes (0x30..=0x3f) → intermediate (0x20..=0x2f) → final (0x40..=0x7e)
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
                // single char escape (ESC + 1 char), e.g. ESC=, ESC>, ESCM (RI), etc.
                i += 1;
            }
        }
    }
    out
}

fn analyze(path: &Path) {
    let raw = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[skip] {}: {e}", path.display());
            return;
        }
    };
    let stripped = strip_ansi(&raw);
    eprintln!("=== {} ===", path.display());
    eprintln!("  raw size:      {}", raw.len());
    eprintln!(
        "  stripped size: {} ({:.1}% retained)",
        stripped.len(),
        100.0 * stripped.len() as f64 / raw.len().max(1) as f64
    );
    // ESC 残留チェック
    let esc_remain = stripped.iter().filter(|&&b| b == 0x1b).count();
    eprintln!("  ESC remaining in stripped: {esc_remain}");
    // 非印字文字の頻度
    let visible = stripped
        .iter()
        .filter(|&&b| (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t')
        .count();
    eprintln!(
        "  visible/text chars: {visible} ({:.1}%)",
        100.0 * visible as f64 / stripped.len().max(1) as f64
    );
    // 先頭 200 bytes プレビュー
    let preview: String = stripped
        .iter()
        .take(300)
        .map(|&b| {
            if b == b'\n' {
                '⏎'
            } else if b == b'\r' {
                '↩'
            } else if b == b'\t' {
                '⇥'
            } else if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '·'
            }
        })
        .collect();
    eprintln!("  preview (first 300 bytes): {preview}");
    eprintln!();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        // PoC 03 で取得した sample を全部試す
        let candidates = [
            "/tmp/hyoui-poc-03-bash.raw",
            "/tmp/hyoui-poc-03-vi.raw",
            "/tmp/hyoui-poc-03-less.raw",
            "/tmp/hyoui-poc-03-zsh.raw",
        ];
        for path in &candidates {
            let p = Path::new(path);
            if p.exists() {
                analyze(p);
            }
        }
    } else {
        for path in &args {
            analyze(Path::new(path));
        }
    }

    // 内蔵 unit test (synthetic)
    eprintln!("=== synthetic tests ===");
    let cases: &[(&[u8], &[u8], &str)] = &[
        (b"hello", b"hello", "no escape"),
        (b"\x1b[31mRed\x1b[0m text", b"Red text", "SGR color"),
        (b"a\x1b[2J\x1b[Hb", b"ab", "clear screen + cursor home"),
        (b"\x1b]0;title\x07ok", b"ok", "OSC title (BEL terminated)"),
        (b"\x1b]0;title\x1b\\ok", b"ok", "OSC title (ST terminated)"),
        (b"\x1bP1$rdcs\x1b\\post", b"post", "DCS"),
        (b"\x1b=ok", b"ok", "single char (keypad app mode)"),
        (
            b"\x1b[?2004hpaste\x1b[?2004l",
            b"paste",
            "bracketed paste enable/disable",
        ),
        (
            b"\x1b[?1049hAlt\x1b[?1049l",
            b"Alt",
            "alternate screen enable/disable",
        ),
        (b"\x1b[12;34Hcursor", b"cursor", "cursor positioning"),
        (b"\x1b[1;31;47mfancy\x1b[0m", b"fancy", "multi-param SGR"),
    ];
    let mut passed = 0;
    let mut failed = 0;
    for (input, expected, name) in cases {
        let result = strip_ansi(input);
        if result == *expected {
            eprintln!("  PASS  {name}");
            passed += 1;
        } else {
            eprintln!(
                "  FAIL  {name}\n        got:      {:?}\n        expected: {:?}",
                String::from_utf8_lossy(&result),
                String::from_utf8_lossy(expected)
            );
            failed += 1;
        }
    }
    eprintln!("synthetic tests: PASSED {passed}, FAILED {failed}");
    if failed > 0 {
        std::process::exit(1);
    }
}
