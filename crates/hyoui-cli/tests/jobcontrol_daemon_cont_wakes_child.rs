//! issue 2026-06-11 優先2: daemon SIGCONT → stopped child 連動起こしの検証。
//!
//! 防衛策 (session.rs handle_suspend_signals): 「daemon が SIGCONT で再開した時、
//! child が STOPPED なら killpg(child, SIGCONT)」が実機で発火することを確認する。
//!
//! root cause だった「自前 waitpid(WUNTRACED) は SIGCHLD 経由で既に Stopped
//! transition が消費済のため StillAlive を返し、起こしが不発」を、latch 済の
//! `ChildLifecycle::is_stopped()` 参照に直して修正した。
//!
//! 経路:
//! ```
//! 子 PTY 内 self-stop (= sh -c 'kill -STOP $$; echo RESUMED')
//!   → daemon が SIGCHLD で stopped を観測・latch (notify policy なので起こさない)
//!   → daemon process に外部から kill -CONT
//!   → daemon の SIGCONT handler → handle_suspend_signals が is_stopped() を見て
//!     killpg(child, SIGCONT)
//!   → 子が復帰して RESUMED を出力
//! ```

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// self-stop してから marker を出力する子。
const SELF_STOP_THEN_ECHO: &[&str] = &["sh", "-c", "kill -STOP $$; echo RESUMED_BY_DAEMON_CONT"];

/// daemon に kill -CONT を送ると、latch 済の stopped child を killpg(SIGCONT) で
/// 起こす (= 修正後は発火する)。default(notify) policy なので daemon は self-stop
/// 観測時には起こさず、外部 CONT を契機に初めて起こす。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行"]
#[test]
fn daemon_sigcont_wakes_stopped_child() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "jobcontrol-daemon-cont",
        &[
            "run",
            "--",
            SELF_STOP_THEN_ECHO[0],
            SELF_STOP_THEN_ECHO[1],
            SELF_STOP_THEN_ECHO[2],
        ],
    );

    // leader 接続が成立するまで待つ (= daemon が稼働し socket 確立)。
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    // 子が self-stop して daemon が stopped を latch するまで少し待つ。
    // notify policy では起こされないので RESUMED はまだ出ない。
    std::thread::sleep(Duration::from_millis(500));
    let early = String::from_utf8_lossy(&h.drain_output()).to_string();
    assert!(
        !early.contains("RESUMED_BY_DAEMON_CONT"),
        "外部 CONT 前に marker が出てはいけない (= notify policy で起こさない): {early:?}"
    );

    // daemon process に kill -CONT を送る。
    let daemon = h.daemon_pid().expect("daemon process が見つからない");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGCONT,
    )
    .expect("kill -CONT daemon");

    // daemon の防衛策が latch 済 stopped child を killpg(SIGCONT) で起こす →
    // 子が復帰して marker を出力する。
    let out = h
        .wait_for("RESUMED_BY_DAEMON_CONT", Duration::from_secs(8))
        .expect("daemon CONT で stopped child が起きて marker を出すはず");
    assert!(
        out.contains("RESUMED_BY_DAEMON_CONT"),
        "daemon CONT 後の出力に marker が含まれない: {out:?}"
    );

    let _ = h.kill();
}
