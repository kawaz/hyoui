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
    // 10s: spawn → daemonize → fork+exec の完了待ち。3s だと並行 cargo build 等の
    // 高負荷下で間に合わないことがある (= 2026-06-11 実測)。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(descendants) = find_descendants(attach_pid)
            && let Some(sleep_proc) = descendants.iter().find(|p| p.comm.contains("sleep"))
        {
            child_pid = Some(sleep_proc.pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let child_pid = child_pid.expect("/bin/sleep 子孫 process が見つからない");

    // readiness race 対策 (= issue 2026-06-11): 子 process が現れても、attach client が
    // daemon に leader 登録を終えるまで follow policy (notify → raise(SIGSTOP)) は配線
    // されない。子の出現だけで SIGSTOP すると leader 不在の窓で notify が空振りし、
    // attach が STOPPED にならず偽 fail する。leader 接続完了を待ってから stop を送る。
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(5)),
        "attach client が leader として daemon に接続しない (= follow 配線前)"
    );

    // 子に SIGSTOP を送って self-stop を模擬 (= line discipline 経由の Ctrl-Z
    // 相当、kernel が子 pgrp に SIGTSTP を配送した時と等価な状態に持っていく)
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), Signal::SIGSTOP)
        .expect("kill -STOP child");

    // daemon が waitpid(WUNTRACED) で観測 → notify を leader (= attach) に送信
    // → attach が raise(SIGSTOP) するまで wait (= follow policy 完了)。
    //
    // **PTY pump 必須 (= issue 2026-06-11)**: attach は follow 発動時、SIGSTOP の前に
    // 外側端末へ reset escape を書き `tcsetattr(TCSAFLUSH)` で drain を待つ。test が
    // master を読まないと drain が完了せず attach が ioctl で永久ブロックする
    // (= 実端末では端末エミュレータが常時読むので起きない)。よって stat poll の
    // 各周回で `pump_pty` で master を吸い続ける。
    let stat = {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut found: Option<String> = None;
        loop {
            let _ = h.pump_pty();
            if let Ok(state) = process_state_of(attach_pid)
                && state.stat.starts_with('T')
            {
                found = Some(state.stat);
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        found
    };
    let stat = stat.unwrap_or_else(|| {
        // 診断: daemon が stop を観測したか (= status の child-state) と各 process の
        // 実状態を出してから fail する (= どの段で follow が切れたか判別可能にする)。
        let status = h
            .status_text()
            .unwrap_or_else(|e| format!("(status query failed: {e})"));
        let attach_stat = process_state_of(attach_pid)
            .map(|s| s.stat)
            .unwrap_or_else(|e| format!("(gone: {e})"));
        let child_stat = process_state_of(child_pid)
            .map(|s| s.stat)
            .unwrap_or_else(|e| format!("(gone: {e})"));
        panic!(
            "attach process が follow policy で STOPPED にならない (= 軸 1 follow 破綻)\n\
             attach pid={attach_pid} stat={attach_stat}\n\
             child pid={child_pid} stat={child_stat}\n\
             daemon status:\n{status}"
        );
    });
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
