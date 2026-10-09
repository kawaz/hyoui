//! DR-0043 決定 6 e2e: SIGTERM / SIGHUP を block した呼び出し元から起動した daemon も、
//! `kill -TERM <daemon>` で graceful shutdown する (= daemon は起動時に、自分が handler を張る
//! signal の block を外す)。
//!
//! daemon の終わりは、ro で handshake した client の socket が閉じることで受ける (= 他人の
//! process の exit を通知で受ける portable な手段が無いので、daemon が持つ接続の EOF を
//! `poll` の期限付きで待つ)。handshake 済みの接続は daemon が終わるまで閉じられない。

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use nix::poll::{PollFd, PollFlags, PollTimeout};

/// `hyoui run` / `status` の戻りと、daemon の終わりを待つ上限。
const DEADLINE: Duration = Duration::from_secs(10);

/// 呼び出し元の役: SIGTERM / SIGHUP を block して `@ARGV` を exec する perl。
const BLOCKING_CALLER: &str = r#"use POSIX qw(:signal_h);
sigprocmask(SIG_BLOCK, POSIX::SigSet->new(SIGTERM, SIGHUP)) or die "sigprocmask: $!";
exec { $ARGV[0] } @ARGV or die "exec: $!";"#;

/// `program` を、状態の root を `root` にした hyoui の環境で動かす Command。
fn in_root(root: &std::path::Path, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.env("HYOUI_STATE_DIR", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

#[test]
fn daemon_started_with_blocked_sigterm_still_shuts_down_on_sigterm() {
    use hyoui::client::{AttachOptions, ClientConnection};

    let dir = SessionDir::new("hyoui-dmask-");
    let out = common::pty::capture_with_deadline(
        in_root(dir.path(), "perl")
            .args(["-e", BLOCKING_CALLER])
            .arg(hyoui_bin())
            .args(["run", "--detached", "--pty-stdin", "--", "cat"]),
        DEADLINE,
    )
    .expect("run hyoui");
    assert!(out.status.success(), "hyoui run --detached failed: {out:?}");
    let sid = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let status = common::pty::capture_with_deadline(
        in_root(dir.path(), hyoui_bin()).args(["status", &sid, "--format=json"]),
        DEADLINE,
    )
    .expect("hyoui status");
    let v: serde_json::Value = serde_json::from_slice(&status.stdout).expect("status json");
    let daemon = v["daemon_pid"].as_i64().expect("daemon_pid") as i32;

    let sock = dir.path().join("sessions").join(format!("{sid}.sock"));
    let conn = ClientConnection::connect(
        &sock,
        AttachOptions {
            mode: hyoui::protocol::Mode::Ro,
            ..AttachOptions::default()
        },
    )
    .expect("connect ro");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("kill -TERM daemon");

    // daemon の終わり = 接続の EOF。届いた bytes (session.exit.notify 等) は frame として
    // 解かずに読み捨てる (= recv_control は raw_data を読み飛ばして次の control を待ち続ける
    // ので、何も来ない daemon で期限を越えて止まる)。
    let deadline = Instant::now() + DEADLINE;
    let closed = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        let timeout = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
        let mut fds = [PollFd::new(conn.reader_fd(), PollFlags::POLLIN)];
        match nix::poll::poll(&mut fds, timeout) {
            Ok(n) if n > 0 => {
                let mut buf = [0u8; 4096];
                match nix::unistd::read(conn.reader_fd(), &mut buf) {
                    Ok(0) | Err(_) => break true,
                    Ok(_) => {}
                }
            }
            Ok(_) => break false,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => panic!("poll: {e}"),
        }
    };
    assert!(
        closed,
        "daemon (起動時に SIGTERM が block されていた) は kill -TERM で終わるはず (daemon pid {daemon})"
    );
}
