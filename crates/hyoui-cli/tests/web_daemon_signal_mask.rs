//! DR-0043 決定 6 を web daemon にも当てる: SIGTERM / SIGINT を block した呼び出し元から手で
//! 起動した `hyoui web daemon run` / `supervise` も、`kill -TERM` で終わる (= 起動時に止める
//! 合図の signal の block を外す)。
//!
//! 終わりは、子 process の wait を別 thread で待って channel の期限付き受信で受ける。

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use nix::sys::signal::Signal;

/// 起動と終わりを待つ上限。
const DEADLINE: Duration = Duration::from_secs(10);

/// 呼び出し元の役: SIGTERM / SIGINT / SIGHUP を block して `@ARGV` を exec する perl。
/// SIGINT の無視 (非対話 shell の `&` で起動された時に付く) は既定に戻す。
const BLOCKING_CALLER: &str = r#"use POSIX qw(:signal_h);
$SIG{INT} = "DEFAULT";
sigprocmask(SIG_BLOCK, POSIX::SigSet->new(SIGTERM, SIGINT, SIGHUP)) or die "sigprocmask: $!";
exec { $ARGV[0] } @ARGV or die "exec: $!";"#;

fn spawn_blocked(root: &Path, args: &[&str]) -> Child {
    Command::new("perl")
        .args(["-e", BLOCKING_CALLER])
        .arg(PathBuf::from(env!("CARGO_BIN_EXE_hyoui")))
        .args(args)
        .env("HYOUI_STATE_DIR", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hyoui web daemon")
}

/// `child` の終わりを別 thread で待ち、期限内に終われば exit status を返す。期限を越えたら
/// SIGKILL で畳んで `None` を返す。
fn wait_exit(mut child: Child, deadline: Duration) -> Option<ExitStatus> {
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let status = match rx.recv_timeout(deadline) {
        Ok(status) => Some(status.expect("wait")),
        Err(_) => {
            let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
            None
        }
    };
    waiter.join().expect("waiter thread");
    status
}

fn send(child: &Child, signal: Signal) {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal)
        .expect("kill web daemon");
}

#[test]
fn web_daemon_run_started_with_blocked_sigterm_still_stops_on_sigterm() {
    let dir = SessionDir::new("hyoui-wmask-run-");
    let mut child = spawn_blocked(
        dir.path(),
        &[
            "web",
            "daemon",
            "run",
            "--no-config",
            "--listen",
            "127.0.0.1:0",
        ],
    );

    // listen し始めた (= 起動の処理を終えて serve に入った) のを stderr の行で受ける。
    let stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { return };
            if line.contains("listening on") && tx.send(()).is_err() {
                return;
            }
        }
    });
    if rx.recv_timeout(DEADLINE).is_err() {
        let _ = wait_exit(child, Duration::ZERO);
        panic!("web daemon run が listen し始めない");
    }

    send(&child, Signal::SIGTERM);
    let status = wait_exit(child, DEADLINE);
    // run は SIGTERM に handler を張らず既定の動作 (終了) で受ける。
    assert_eq!(
        status.and_then(|s| s.signal()),
        Some(Signal::SIGTERM as i32),
        "web daemon run (起動時に SIGTERM が block されていた) は kill -TERM で終わるはず: {status:?}"
    );
}

#[test]
fn web_daemon_supervise_started_with_blocked_sigterm_still_stops_on_sigterm() {
    let dir = SessionDir::new("hyoui-wmask-sup-");
    let child = spawn_blocked(dir.path(), &["web", "daemon", "supervise"]);

    // 制御 socket に繋がる (= 監督者が起動の処理を進めた) まで待つ。他 process の状態変化を
    // 通知で受ける手段が無いので、期限付きで繰り返し繋ぐ。
    let socket = dir.path().join("web").join("run").join("supervisor.sock");
    let deadline = Instant::now() + DEADLINE;
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        if Instant::now() >= deadline {
            let _ = wait_exit(child, Duration::ZERO);
            panic!("監督者の制御 socket に繋がらない: {}", socket.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    send(&child, Signal::SIGTERM);
    let status = wait_exit(child, DEADLINE);
    // socket を bind してから handler を張るまでの間に届けば既定の動作で終わり、張った後なら
    // 抱えた unit (ここでは 0 個) を止めて 0 で終わる。どちらでも期限内に終わる。
    assert!(
        status.is_some(),
        "web daemon supervise (起動時に SIGTERM が block されていた) は kill -TERM で終わるはず"
    );
}
