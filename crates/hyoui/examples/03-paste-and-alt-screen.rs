//! PoC 03: bracketed paste + alternate screen 観測
//!
//! 子 program (default zsh) を forkpty で起動し、master fd から流れる bytes を一定時間記録、
//! escape sequence を抽出して以下を確認:
//! - bracketed paste enable (`ESC[?2004h`) — shell が自分から有効化するか
//! - alternate screen enable (`ESC[?1049h` / `ESC[?47h` / `ESC[?1047h`) — TUI app が使うか
//! - 出力 byte rate (= scrollback サイズ判断材料)
//!
//! 実行:
//!   cargo run --example 03-paste-and-alt-screen                     # default: zsh, 1500ms
//!   cargo run --example 03-paste-and-alt-screen -- zsh 2000
//!   cargo run --example 03-paste-and-alt-screen -- 'vi /tmp/foo' 2000
//!   cargo run --example 03-paste-and-alt-screen -- claude 5000      # 手動: claude 観測 (timeout 5s)

use hyoui::sys::Pty;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "zsh".to_string());
    let dur_ms: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500);

    // argv をスペース分割して直接 exec (sh -c 経由しない)
    let argv: Vec<&str> = cmd.split_whitespace().collect();
    let spawned = Pty::spawn(&argv, 80, 24).expect("spawn");
    let master = spawned.pty.into_master();
    fcntl(master.as_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).expect("nonblock");
    let master_raw = master.as_raw_fd();

    eprintln!(
        "=== observe '{cmd}' for {dur_ms}ms (child pid {}) ===",
        spawned.child
    );

    let mut buf = Vec::new();
    let start = Instant::now();
    let mut chunk_log: Vec<(Duration, usize)> = Vec::new();
    let mut zero_read_count = 0u64;
    let mut neg_read_count = 0u64;
    while start.elapsed() < Duration::from_millis(dur_ms) {
        let mut tmp = [0u8; 8192];
        // SAFETY: master_raw valid while owned
        let n = unsafe { libc::read(master_raw, tmp.as_mut_ptr() as *mut _, tmp.len()) };
        if n > 0 {
            chunk_log.push((start.elapsed(), n as usize));
            buf.extend_from_slice(&tmp[..n as usize]);
        } else if n == 0 {
            zero_read_count += 1;
            break; // EOF
        } else {
            neg_read_count += 1;
        }
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!(
        "  read counts: data={}, EOF={zero_read_count}, EAGAIN/errs={neg_read_count}",
        chunk_log.len()
    );
    let elapsed = start.elapsed();
    let total = buf.len();

    // 子 kill (best-effort)
    let _ = nix::sys::signal::kill(spawned.child, nix::sys::signal::Signal::SIGTERM);
    thread::sleep(Duration::from_millis(50));
    let _ = nix::sys::signal::kill(spawned.child, nix::sys::signal::Signal::SIGKILL);
    let _ = nix::sys::wait::waitpid(spawned.child, None);

    eprintln!("=== observed {total} bytes in {elapsed:?} ===");
    if elapsed.as_secs_f64() > 0.0 {
        eprintln!(
            "  byte rate: {:.1} B/s",
            total as f64 / elapsed.as_secs_f64()
        );
    }
    eprintln!("  chunks: {}", chunk_log.len());
    if !chunk_log.is_empty() {
        let max_chunk = chunk_log.iter().map(|(_, s)| *s).max().unwrap();
        let avg = total as f64 / chunk_log.len() as f64;
        eprintln!("  chunk size: avg {avg:.0} bytes, max {max_chunk} bytes");
    }

    // raw dump
    let dump = PathBuf::from(format!(
        "/tmp/hyoui-poc-03-{}.raw",
        cmd.split_whitespace().next().unwrap_or("cmd")
    ));
    std::fs::write(&dump, &buf).expect("dump");
    eprintln!("  raw bytes dumped to {}", dump.display());

    // 特定 escape の検出
    let has = |needle: &[u8]| buf.windows(needle.len()).any(|w| w == needle);
    let bracketed = has(b"\x1b[?2004h");
    let bracketed_off = has(b"\x1b[?2004l");
    let alt_1049 = has(b"\x1b[?1049h");
    let alt_1049_off = has(b"\x1b[?1049l");
    let alt_47 = has(b"\x1b[?47h");
    let alt_1047 = has(b"\x1b[?1047h");
    let alt_1048 = has(b"\x1b[?1048h");
    eprintln!("=== detected control sequences ===");
    eprintln!("  bracketed paste enable  (ESC[?2004h): {bracketed}");
    eprintln!("  bracketed paste disable (ESC[?2004l): {bracketed_off}");
    eprintln!("  alt screen enable       (ESC[?1049h): {alt_1049}");
    eprintln!("  alt screen disable      (ESC[?1049l): {alt_1049_off}");
    eprintln!("  alt screen (old ?47h):                  {alt_47}");
    eprintln!("  alt screen (old ?1047h):                {alt_1047}");
    eprintln!("  alt screen (old ?1048h):                {alt_1048}");

    // escape sequence の一覧 (簡易 parser)
    let escapes = extract_escapes(&buf);
    eprintln!(
        "=== escape sequences ({} total, unique {}) ===",
        escapes.len(),
        {
            let mut s: Vec<&String> = escapes.iter().collect();
            s.sort();
            s.dedup();
            s.len()
        }
    );
    let mut unique: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in &escapes {
        *unique.entry(s.clone()).or_default() += 1;
    }
    let mut sorted: Vec<(String, usize)> = unique.into_iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (s, n) in sorted.iter().take(40) {
        eprintln!("  {n:5} × {s:?}");
    }
    if sorted.len() > 40 {
        eprintln!("  ... ({} more unique)", sorted.len() - 40);
    }
}

/// 簡易 escape parser: CSI / OSC / single char ESC を切り出す。
fn extract_escapes(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        if i >= buf.len() {
            break;
        }
        match buf[i] {
            b'[' => {
                // CSI: parameter bytes (0x30..=0x3f) → intermediate (0x20..=0x2f) → final (0x40..=0x7e)
                i += 1;
                while i < buf.len() && (0x30..=0x3f).contains(&buf[i]) {
                    i += 1;
                }
                while i < buf.len() && (0x20..=0x2f).contains(&buf[i]) {
                    i += 1;
                }
                if i < buf.len() && (0x40..=0x7e).contains(&buf[i]) {
                    i += 1;
                }
            }
            b']' => {
                // OSC: terminate by BEL (0x07) or ST (ESC \)
                i += 1;
                while i < buf.len() {
                    if buf[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            b'P' | b'X' | b'^' | b'_' => {
                // DCS / SOS / PM / APC: terminate by ST
                i += 1;
                while i < buf.len() {
                    if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'\\' {
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
        let seq = &buf[start..i];
        out.push(escape_repr(seq));
    }
    out
}

fn escape_repr(seq: &[u8]) -> String {
    let mut s = String::from("ESC");
    for &b in &seq[1..] {
        match b {
            0x1b => s.push_str("ESC"),
            0x07 => s.push_str("BEL"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}
