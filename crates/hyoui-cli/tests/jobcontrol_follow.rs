//! DR-0015 §2.2 軸 1 follow policy のマトリクス検証 (新方針版)。
//!
//! 旧 `matrix_jobcontrol_axis1.rs` (= 削除済) は 1 プロセス 2 thread モデル
//! 前提だった。DR-0015 で `hyoui run` は fork+exec attach pattern に置換され、
//! 軸 1 follow の経路が:
//!
//! ```
//! 子 PTY 内 self-stop (= kill -STOP <子 pid>)
//!   → daemon process が waitpid(WUNTRACED) で観測
//!   → SessionChildStoppedNotify を leader (= attach client) に送信
//!   → attach client が `on-child-suspend=follow` policy 発動
//!   → attach client が raise(SIGSTOP) → process が STOPPED に
//! ```
//!
//! 本 test は新経路の **regression check** として 1 cell (= 最小 round-trip)
//! を検証する。
//!
//! 完全マトリクス (multi-app × child_suspend policy) は別途拡張する余地あり。

mod common;

use std::time::Duration;

use common::pty::{HyouiTestRunner, find_descendants, process_state_of};
use nix::sys::signal::Signal;

/// 観測対象の state 遷移を待つ短い sleep。
fn settle() {
    std::thread::sleep(Duration::from_millis(200));
}

/// 短時間内に process state を待ち合わせる helper。
/// `expected_stat_char` (= 例: 'T' for STOPPED) が `stat` field の先頭文字に
/// 現れるまで poll、timeout で `None` を返す。
fn wait_for_stat(pid: i32, expected: char, timeout: Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(state) = process_state_of(pid) {
            if state.stat.starts_with(expected) {
                return Some(state.stat);
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// DR-0015 §2.2 軸 1 follow: 子 self-stop → SessionChildStoppedNotify →
/// attach client が follow policy で `raise(SIGSTOP)` → attach process が
/// STOPPED 状態になる、を確認する。
///
/// 1. `hyoui run -- /bin/sleep 30` で long-running 子を起動
///    (= 親 process は exec で `hyoui attach <session>` に置換される、その後の
///    pid は同じ)
/// 2. attach process の子孫から `sleep` 子 pid を見つけて `kill -STOP` で
///    self-stop を模擬
/// 3. attach process (= 親) が STOPPED (`T*`) になることを wait_for_stat で確認
///
/// `#[ignore]`: linger pattern / handshake race の CI 安定性確認待ち
/// (Task 25 完了で外す予定)。ローカル run は `cargo test -- --ignored`。
#[ignore = "DR-0015 Task 25 安定後に有効化、Task 23 の最小 cell"]
#[test]
fn follow_child_self_stop_makes_attach_stopped() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("jobcontrol-follow", &["run", "--", "/bin/sleep", "30"]);

    // attach client が起動して socket connect 完了するまで wait
    // (= sleep が起動して PTY 経由で何か出力する保証は無いので、wait_for ではなく
    // 子孫 process が現れるまで poll)
    let attach_pid = h.pid().as_raw();
    let mut child_pid: Option<i32> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Ok(descendants) = find_descendants(attach_pid) {
            if let Some(sleep_proc) = descendants.iter().find(|p| p.comm.contains("sleep")) {
                child_pid = Some(sleep_proc.pid);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let child_pid = child_pid.expect("/bin/sleep 子孫 process が見つからない");

    // 子に SIGSTOP を送って self-stop を模擬 (= line discipline 経由の Ctrl-Z
    // 相当、kernel が子 pgrp に SIGTSTP を配送した時と等価な状態に持っていく)
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), Signal::SIGSTOP)
        .expect("kill -STOP child");

    // daemon が waitpid(WUNTRACED) で観測 → notify を leader (= attach) に送信
    // → attach が raise(SIGSTOP) するまで wait (= follow policy 完了)
    let stat = wait_for_stat(attach_pid, 'T', Duration::from_secs(5))
        .expect("attach process が follow policy で STOPPED にならない (= 軸 1 follow 破綻)");
    assert!(
        stat.starts_with('T'),
        "attach stat should start with T (STOPPED), got: {stat:?}"
    );

    // cleanup: 親 + 子に SIGCONT を送って復帰 → SIGKILL で確実に終了
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(attach_pid), Signal::SIGCONT);
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), Signal::SIGCONT);
    settle();
    let _ = h.kill();
}
