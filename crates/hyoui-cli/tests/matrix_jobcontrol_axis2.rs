//! DR-0001 軸 2 マトリクス検証 — 親側 suspend / 子側 transparent / decouple
//! 挙動の現実態固定。
//!
//! 検証対象 (DR-0001 §軸 2):
//! - **interactive (= default `transparent`)**: 親 hyoui が外部から SIGTSTP され
//!   たら、子 pgrp にも SIGSTOP を送ってから親も SIGSTOP を raise。両者ペアで
//!   停止 → 再開も SIGCONT pair。
//! - **headless (= default `decouple`)**: 親のみ停止、子は走り続ける。
//!
//! ## 本 file の test の位置付け
//!
//! DR-0001 軸 2 は **task #34 で実装済 (2026-05-27)**。本 file は期待動作を確認する
//! regression 防止 test。`OnParentSuspend::Transparent` で子も連動 STOPPED に、
//! `Decouple` で子はそのまま走り続けることを検証する。
//!
//! ## 検証手段
//!
//! `kill -TSTP <hyoui_pid>` で親 hyoui に外部 SIGTSTP を送り、その後の
//! 親 / 子の state を観測する。軸 1 と違い「親が SIGTSTP を catch する」のは
//! 自然な経路 (= orphan group ではない、shell から子として起動された通常 process)。

mod common;

use std::time::Duration;

use common::pty::{HyouiTestRunner, process_state_of};
use nix::sys::signal::Signal;

/// 観測対象の state 遷移を待つ短い sleep。
fn settle() {
    std::thread::sleep(Duration::from_millis(300));
}

/// 子が現れるまで polling。`matrix_jobcontrol_axis1` と同型。
fn wait_for_child(parent_pid: i32, max_ms: u64) -> std::io::Result<common::pty::ProcessState> {
    let deadline = std::time::Instant::now() + Duration::from_millis(max_ms);
    let mut last_err: Option<std::io::Error> = None;
    while std::time::Instant::now() < deadline {
        match common::pty::find_child_of(parent_pid) {
            Ok(c) => return Ok(c),
            Err(e) => last_err = Some(e),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "wait_for_child timeout")
    }))
}

/// matrix cell B1: `/bin/sleep 30` + 親 hyoui に外部 SIGTSTP (interactive default)。
///
/// **DR-0001 軸 2 期待動作 (`transparent`)**: 親 STOPPED → 子も STOPPED。
/// **現実態 (= 軸 2 未実装)**: 親は SIGTSTP の default 動作で STOPPED になる
/// (= shell から起動された通常 process なので)、子は走り続ける。
#[test]
fn axis2_sleep_interactive_default_external_tstp() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis2-sleep-interactive",
        &["run", "--mode=interactive", "--", "/bin/sleep", "30"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");
    // DR-0001 §実装ノート: 子は独立 session leader (= pgid == pid)
    assert_eq!(
        child.pgid, child.pid,
        "child must be session leader: {child:?}"
    );

    // 親 hyoui に外部 SIGTSTP
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGTSTP,
    )
    .expect("kill -TSTP parent");
    settle();

    // 観察: 親 hyoui が SIGTSTP を受けて停止しているかは現実態次第。
    // 現実装では SIGTSTP に独自 handler を入れていないため、default 動作 (= STOP)
    // で停止する可能性が高い。ただし PTY raw mode の line discipline 等の影響で
    // 配送が変わる可能性もあるので、まず親と子の状態を観測して **両方の現実態**
    // を記録する。
    let parent_after = h.process_state().expect("parent observable");
    let child_after = process_state_of(child.pid).expect("child observable");

    // DR-0001 軸 2 `transparent` 実装後: 親 SIGTSTP 受信 → 子 pgrp にも SIGSTOP
    // → 子も STOPPED ('T') に follow。
    assert!(
        child_after.is_state('T'),
        "DR-0001 軸 2 transparent: 親 SIGTSTP 後に子も STOPPED ('T') に follow するはず。\
         got: {child_after:?}"
    );

    // 親も SIGTSTP → raise(SIGSTOP) で STOPPED になる想定。informational として
    // log は残すが、test の assert はしない (= SIGTSTP の default 動作は環境差で
    // 揺れる可能性、本軸の主題は子の追従挙動)。
    eprintln!(
        "[axis2_sleep_interactive_default_external_tstp] parent_after.stat = {:?} (informational)",
        parent_after.stat
    );

    // cleanup
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell B2: `/bin/sleep 30` + 親に外部 SIGTSTP (headless default)。
///
/// **DR-0001 軸 2 期待動作 (`decouple`)**: 親 STOPPED でも子は走り続ける。
/// **現実態**: decouple は「子に何もしない」なので、軸 2 未実装の現実態と一致
/// する可能性が高い。test は実態を assert で固定する (= 修正時に注意喚起)。
#[test]
fn axis2_sleep_headless_default_external_tstp_decouple() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis2-sleep-headless",
        &["run", "--mode=headless", "--", "/bin/sleep", "30"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGTSTP,
    )
    .expect("kill -TSTP parent");
    settle();

    let child_after = process_state_of(child.pid).expect("child observable");
    // headless default = decouple → 子は走り続ける。
    // 現実態 (= 軸 2 未実装) でも結果として同じになるが、両者の根拠は別物:
    //   - decouple: 「親が子の停止を引き起こさない」明示判断
    //   - 軸 2 未実装: 「親が子に signal を伝播する仕組み自体が無い」
    // 実装後にもこの test は pass し続ける想定 (= decouple 設計が現実態に近い)。
    assert!(
        !child_after.is_state('T'),
        "headless decouple: 子は親 SIGTSTP に無反応で走り続ける。got: {child_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    h.kill().ok();
}

/// matrix cell B3: 明示 `--on-parent-suspend=decouple` (interactive で override)。
///
/// interactive default `transparent` を decouple に override しても、現実態は
/// 同じ (= 子は走り続ける)。実装後は decouple が明示的に「子を止めない」を保証
/// するため、本 test は pass し続ける想定。
#[test]
fn axis2_sleep_interactive_explicit_decouple() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis2-sleep-explicit-decouple",
        &[
            "run",
            "--mode=interactive",
            "--on-parent-suspend=decouple",
            "--",
            "/bin/sleep",
            "30",
        ],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGTSTP,
    )
    .expect("kill -TSTP parent");
    settle();

    let child_after = process_state_of(child.pid).expect("child observable");
    // decouple 指定: 子は走り続ける。
    assert!(
        !child_after.is_state('T'),
        "explicit decouple: 子は親 SIGTSTP に無反応。got: {child_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    h.kill().ok();
}

/// matrix cell B4: `bash --norc -i` を起動して親 hyoui に SIGTSTP。
///
/// REPL category での現実態確認。bash 自体が job control を持つ独立した子だが、
/// 軸 2 の主題は「親 hyoui が止まった時に子の bash が連動するか」なので、
/// 子の job control 内部挙動は本 cell では不問。
#[test]
fn axis2_bash_interactive_default_external_tstp() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis2-bash-interactive",
        &["run", "--mode=interactive", "--", "bash", "--norc", "-i"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("bash child should appear");
    assert!(
        child.comm.contains("bash"),
        "expected bash child, got: {child:?}"
    );

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGTSTP,
    )
    .expect("kill -TSTP parent");
    settle();

    let child_after = process_state_of(child.pid).expect("bash observable");
    // DR-0001 軸 2 transparent (bash REPL category): 子 bash も STOPPED に follow。
    assert!(
        child_after.is_state('T'),
        "DR-0001 軸 2 transparent (bash REPL category): 子 bash も STOPPED ('T') に \
         follow するはず。got: {child_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell B5: `/bin/cat` を起動して親 hyoui に SIGTSTP (= line-oriented
/// category)。軸 2 transparent で子 cat も連動 STOPPED することを検証。
#[test]
fn axis2_cat_interactive_default_external_tstp() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis2-cat-interactive",
        &["run", "--mode=interactive", "--", "/bin/cat"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("cat child should appear");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGTSTP,
    )
    .expect("kill -TSTP parent");
    settle();

    let child_after = process_state_of(child.pid).expect("cat observable");
    // DR-0001 軸 2 transparent (cat line-oriented category): 子も STOPPED に follow。
    assert!(
        child_after.is_state('T'),
        "DR-0001 軸 2 transparent (cat category): 子も STOPPED ('T') に follow するはず。\
         got: {child_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}
