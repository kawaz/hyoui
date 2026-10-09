//! DR-0043 決定 5 e2e: daemon (anchor 経路) が起こした子は、呼び出し元の `RLIMIT_CORE`
//! (soft / hard) で exec する。daemon は R5-H12 で自分の soft を 0 にするが、hard は下げず、
//! 子は exec の前に soft を呼び出し元の値へ戻す。
//!
//! 呼び出し元の役の `sh` が soft を上げてから自分の値を 1 行出し、hyoui を exec する。子の
//! `sh` は同じ書式の 1 行を FIFO に書く。同じ `sh` の `ulimit` で表すので単位は揃う。FIFO は
//! test 側が子の起動前に O_RDWR で開いて持つので、子の open は読み手待ちで止まらず、`poll`
//! の期限で「報告が来なかった」を判定できる。
//!
//! 呼び出し元の hard が 0 の環境 (= hard 0 で動いている process の下で test を走らせた時)
//! では soft を上げられず、両者は `core=0,0` で一致するだけになる (= 子が戻したかを区別
//! できない)。

mod common;

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use nix::poll::{PollFd, PollFlags, PollTimeout};

/// 子の報告と `hyoui run` の戻りを待つ上限。
const DEADLINE: Duration = Duration::from_secs(10);

/// 呼び出し元の役: soft を 2048 block (hard がそれより小さければ hard) に上げ、自分の値を
/// 1 行出してから `"$@"` を exec する。
const CALLER: &str = r#"h=$(ulimit -H -c)
if [ "$h" = unlimited ] || [ "$h" -gt 2048 ]; then s=2048; else s=$h; fi
ulimit -S -c "$s" || exit 99
echo "core=$(ulimit -S -c),$(ulimit -H -c)"
exec "$@""#;

/// 子: 自分の値を `$1` (FIFO) に 1 行で書く。
const PROBE: &str = r#"echo "core=$(ulimit -S -c),$(ulimit -H -c)" > "$1""#;

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 子が FIFO に書いた報告の 1 行。期限内に来なければ `None`。
fn read_line(fifo: &std::fs::File) -> Option<String> {
    let deadline = Instant::now() + DEADLINE;
    let mut acc = Vec::new();
    while !acc.contains(&b'\n') {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
        let mut fds = [PollFd::new(fifo.as_fd(), PollFlags::POLLIN)];
        match nix::poll::poll(&mut fds, timeout) {
            Ok(n) if n > 0 => {
                let mut buf = [0u8; 256];
                let n = nix::unistd::read(fifo, &mut buf).expect("read fifo");
                acc.extend_from_slice(&buf[..n]);
            }
            _ => return None,
        }
    }
    Some(String::from_utf8_lossy(&acc).trim().to_string())
}

#[test]
fn child_starts_with_the_caller_core_limit() {
    let dir = SessionDir::new("hyoui-core-");
    let path = dir.path().join("report.fifo");
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).expect("mkfifo");
    let fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open fifo");

    let hyoui = hyoui_bin().display().to_string();
    let fifo_arg = path.display().to_string();
    let out = common::pty::capture_with_deadline(
        Command::new("sh")
            .args([
                "-c",
                CALLER,
                "sh",
                &hyoui,
                "run",
                "--detached",
                "--pty-stdin",
                "--",
                "sh",
                "-c",
                PROBE,
                "sh",
                &fifo_arg,
            ])
            .env("HYOUI_STATE_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env_remove("HYOUI_SESSION_ID")
            .env_remove("HYOUI_LOCK_TOKEN")
            .env_remove("HYOUI_ALLOW_CORE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        DEADLINE,
    )
    .expect("run hyoui");
    assert!(out.status.success(), "caller / hyoui run failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let caller = stdout
        .lines()
        .find(|l| l.starts_with("core="))
        .unwrap_or_else(|| panic!("caller did not report its limit: {stdout:?}"))
        .to_string();

    assert_eq!(read_line(&fifo), Some(caller));
}
