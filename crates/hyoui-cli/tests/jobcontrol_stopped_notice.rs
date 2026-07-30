//! DR-0029 §1: 子が停止しても attach client は停止せず、覗き窓を維持する e2e。
//!
//! 経路:
//!
//! ```text
//! 子 PTY process に SIGSTOP
//!   → daemon が waitpid(WUNTRACED) で観測
//!   → SessionChildStoppedNotify を rw attach client に送信
//!   → attach client は停止せず、画面最下行に停止通知を描画
//! ```
//!
//! DR-0030 の default では rw attach が stopped child を即 resume するため、この test
//! だけ `on_child_suspend = "show_child_action_menu"` (= DR-0032 §1 の「起こさない」値)
//! にして停止表示の経路を観測する。この設定では停止表示が child action menu になり、
//! その第 1 行が DR-0029 §1 の通知と同じ「子プロセスが停止中」を含む (DR-0032 §4 第 1 段:
//! menu は通知行の N 行拡張)。

mod common;

use std::time::Duration;

use common::pty::{HyouiTestRunner, find_descendants, process_state_of};
use nix::sys::signal::Signal;

/// 子が停止しても attach は走行を続け、「子プロセスが停止中」を表示する。
///
/// DR-0029 §1 は子 stopped への client follow (`raise(SIGSTOP)`) を撤回し、attach を
/// 覗き窓として開いたままにすると定める。DR-0030 の attach 側 resume を test 固有
/// config で止め、子が実際に stopped の間も attach が running であることと停止表示を
/// 確認する。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0029 §1)"]
#[test]
fn stopped_child_keeps_attach_running_and_draws_notice() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui_with_config(
        "jobcontrol-stopped-notice",
        &["run", "--", "/bin/sleep", "30"],
        "[session]\non_child_suspend = \"show_child_action_menu\"\n",
    );

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    let attach_pid = h.pid().as_raw();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let child_pid = loop {
        if let Ok(descendants) = find_descendants(attach_pid)
            && let Some(child) = descendants
                .iter()
                .find(|process| process.comm.contains("sleep"))
        {
            break child.pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "/bin/sleep 子孫 process が見つからない"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), Signal::SIGSTOP)
        .expect("kill -STOP child");

    let out = h
        .wait_for("子プロセスが停止中", Duration::from_secs(5))
        .expect("stopped child の通知行が attach に描画されるはず");
    assert!(
        out.contains("子プロセスが停止中"),
        "停止通知が出力に含まれない: {out:?}"
    );

    let child_state = process_state_of(child_pid).expect("child process state");
    assert!(
        child_state.is_state('T'),
        "show_child_action_menu では child が stopped のままであるべき: {}",
        child_state.stat
    );
    let attach_state = process_state_of(attach_pid).expect("attach process state");
    assert!(
        !attach_state.is_state('T'),
        "DR-0029 §1: child が stopped でも attach は停止してはいけない: {}",
        attach_state.stat
    );

    let _ = h.kill();
}
