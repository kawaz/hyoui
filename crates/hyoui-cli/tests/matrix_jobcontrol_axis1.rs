//! DR-0001 軸 1 マトリクス検証 — 子側 suspend / 親側 follow 挙動の現実態固定。
//!
//! 検証対象 (DR-0001 §軸 1):
//! - **interactive (= default `follow`)**: 子が自分を suspend したら親 hyoui も
//!   `SIGSTOP` を raise して両者停止。外側 shell に制御を返し、`fg` で揃って復帰。
//! - **headless (= default `auto-resume`)**: 子が自分を suspend したら親が
//!   即 `SIGCONT` を子 pgrp に送って続行。
//!
//! ## 本 file の test の位置付け
//!
//! DR-0001 軸 1 は **task #34 で実装済 (2026-05-27)**。本 file の assert は
//! 期待動作 (= DR-0001 §軸 1 の仕様) を確認する regression 防止 test として
//! 機能する。`OnChildSuspend::Follow` で親が STOPPED に follow、`AutoResume` で
//! 子が即復帰することを検証する。
//!
//! ## マトリクス cell の選定 (= DR-0014 §最低 3 種類の category)
//!
//! `claude` のような TUI alt screen 系は harness で扱うのが難しいため本 task では
//! 別 category 3 種で代替:
//!
//! | Category | App | 性質 |
//! |---|---|---|
//! | Simple long-running | `/bin/sleep 30` | signal 受信のみ、I/O なし |
//! | Line-oriented stdin reader | `/bin/cat` | stdin → stdout echo |
//! | Interactive REPL | `bash --norc -i` | shell prompt、job control 持つ |
//!
//! ## probe で確認した OS 挙動 (= 重要)
//!
//! - **`POSIX_SPAWN_SETSID` 子は session leader & process group leader** で、
//!   その session には子の process group しかいない (= orphaned process group)。
//!   POSIX §3.107 により orphan group メンバーが SIGTSTP / SIGTTIN / SIGTTOU を
//!   受け取ると **kernel が discard** する。SIGSTOP は catch 不可で常に届く。
//! - 結果: 「子が `kill -TSTP $$` で自分を suspend する」シナリオは sh のような
//!   非対話 shell では成立しない (= signal discard される)。実機検証は
//!   **外部から子に SIGSTOP を直接送る** 形で代替し、本来の `Ctrl-Z` 経路 (=
//!   PTY line discipline → 内側 cooked が SIGTSTP を発火) は本 task の scope 外
//!   (= bash interactive の test で代替検証)

mod common;

use std::time::Duration;

use common::pty::{HyouiTestRunner, process_state_of};
use nix::sys::signal::Signal;

/// 短い時間待ってから state を観測する helper (= 子起動 / signal 配送の同期)。
///
/// signal 配送 / state 遷移用の短 sleep。
fn settle() {
    std::thread::sleep(Duration::from_millis(300));
}

/// 子プロセスが現れるまで polling して待つ (= spawn → posix_spawn 完了の同期)。
/// 最大 `max_ms` 待ち、見つからなければ `Err`。
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

/// matrix cell A1: `/bin/sleep 30` を interactive mode で起動し、
/// 子 (sleep) に **SIGSTOP** を送って STOP。親 hyoui が follow して STOPPED に
/// なることを検証する (= DR-0001 軸 1 `follow` の期待動作)。
#[test]
#[ignore = "matrix test: PTY + signal + procfs/sysctl 観測のため CI 環境で不安定 (macos panic / ubuntu hang 実績あり)。ローカルは `cargo test -- --ignored`。詳細 DR-0014"]
fn axis1_sleep_interactive_default_external_sigstop() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis1-sleep-interactive",
        &["run", "--mode=interactive", "--", "/bin/sleep", "30"],
    );
    // 子 (sleep) が posix_spawn されるまで待つ
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");
    assert!(
        child.comm.contains("sleep"),
        "expected sleep child, got: {child:?}"
    );
    // DR-0001 §実装ノート「子は独立セッションリーダーなので pgid == pid」
    assert_eq!(
        child.pgid, child.pid,
        "child should be session leader (pgid == pid): {child:?}"
    );

    // 子に SIGSTOP (= orphan group でも届く)
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    settle();

    let child_after = process_state_of(child.pid).expect("child still observable");
    assert!(
        child_after.is_state('T'),
        "child should be Stopped after SIGSTOP, got: {child_after:?}"
    );

    let parent_after = h.process_state().expect("parent observable");
    // DR-0001 軸 1 `follow` 実装後: 子 STOPPED 観測時に親が `raise(SIGSTOP)` で
    // 自身も停止する。`stat` は 'T' で始まる (= Stopped)。
    assert!(
        parent_after.is_state('T'),
        "DR-0001 軸 1 follow: 子 STOPPED 後は親も STOPPED ('T') になるはず。\
         got: {parent_after:?}"
    );

    // cleanup: 親と子の両方に SIGCONT を送ってから kill。
    // 親が STOPPED のままだと kill (= SIGTERM) を受け取って処理できない。
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A2: 明示 `--on-child-suspend=follow` を指定したときも親が
/// follow して STOPPED になることを検証 (= A1 と同じ挙動だが flag override 経路の
/// 配線が機能していることを確認)。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_jobcontrol_axis1 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn axis1_sleep_interactive_explicit_follow() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis1-sleep-explicit-follow",
        &[
            "run",
            "--mode=interactive",
            "--on-child-suspend=follow",
            "--",
            "/bin/sleep",
            "30",
        ],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    settle();

    let parent_after = h.process_state().expect("parent observable");
    // DR-0001 軸 1 `follow` 実装後: 子 STOPPED 観測時に親も STOPPED へ follow。
    assert!(
        parent_after.is_state('T'),
        "DR-0001 軸 1 explicit follow: 親 STOPPED ('T') になるはず。\
         got: {parent_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A3: 明示 `--on-child-suspend=auto-resume` で子を強制復帰。
///
/// **DR-0001 軸 1 `auto-resume`**: 子 STOPPED → 親が即 SIGCONT を送り
/// 子が即 Running に戻る (= 子の STOP 滞留は観測されない)。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_jobcontrol_axis1 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn axis1_sleep_interactive_explicit_auto_resume() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis1-sleep-explicit-autoresume",
        &[
            "run",
            "--mode=interactive",
            "--on-child-suspend=auto-resume",
            "--",
            "/bin/sleep",
            "30",
        ],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    settle();

    let child_after = process_state_of(child.pid).expect("child still observable");
    // DR-0001 軸 1 `auto-resume` 実装後: 子は SIGSTOP を受けて一瞬 STOPPED に
    // 遷移するが、親が SIGCHLD self-pipe で transition を観測した直後に
    // `killpg(child, SIGCONT)` を投げて復帰させる。観測時点では既に Running
    // (= 'R') または Sleeping (= 'S') に戻っている。
    assert!(
        !child_after.is_state('T'),
        "DR-0001 軸 1 auto-resume: 子は SIGSTOP 後も走り続ける (= R/S)。\
         got: {child_after:?}"
    );

    // cleanup
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A4: `--mode=headless` の default は `auto-resume`。
///
/// 子 SIGSTOP → DR-0001 軸 1 `auto-resume` で即復帰する (= preset 経由の配線確認)。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_jobcontrol_axis1 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn axis1_sleep_headless_default_auto_resume() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis1-sleep-headless",
        &["run", "--mode=headless", "--", "/bin/sleep", "30"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("sleep child should appear");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    settle();

    let child_after = process_state_of(child.pid).expect("child observable");
    // DR-0001 軸 1 `auto-resume` (headless preset 経由): 子は即復帰。
    assert!(
        !child_after.is_state('T'),
        "DR-0001 軸 1 auto-resume (headless preset): 子は STOP しない (= R/S)。\
         got: {child_after:?}"
    );

    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A5: `/bin/cat` (= line-oriented stdin reader) を interactive で
/// 起動し、子に SIGSTOP。category 違い (TUI ではなく line-oriented) でも軸 1
/// `follow` が機能することを確認 (= サンプル多様性、DR-0014 §最低 3 種類)。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_jobcontrol_axis1 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn axis1_cat_interactive_default_external_sigstop() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis1-cat-interactive",
        &["run", "--mode=interactive", "--", "/bin/cat"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("cat child should appear");
    assert!(
        child.comm.contains("cat"),
        "expected cat child, got: {child:?}"
    );

    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    settle();

    let parent_after = h.process_state().expect("parent observable");
    assert!(
        parent_after.is_state('T'),
        "DR-0001 軸 1 follow (cat category): 親も STOPPED ('T') に follow するはず。\
         got: {parent_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A6: `bash --norc -i` (= interactive REPL category) を起動し、
/// 子 bash に SIGSTOP。3 種目の category で軸 1 `follow` を確認。
///
/// bash interactive は **job control を持つ** ため、本来は内側で job control が
/// 走る (= bash が前景子の signal を受けて分散する) はず。ただし bash 自身が
/// 子側 STOPPED になった時点で「親 hyoui が follow するか」は DR-0001 軸 1 の
/// 主題なので、bash の内側 job control 有無は本 cell では不問。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_jobcontrol_axis1 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn axis1_bash_interactive_default_external_sigstop() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "axis1-bash-interactive",
        &["run", "--mode=interactive", "--", "bash", "--norc", "-i"],
    );
    let child = wait_for_child(h.pid().as_raw(), 2000).expect("bash child should appear");
    assert!(
        child.comm.contains("bash"),
        "expected bash child, got: {child:?}"
    );

    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    settle();

    let child_after = process_state_of(child.pid).expect("bash observable");
    assert!(
        child_after.is_state('T'),
        "bash should be STOP after SIGSTOP, got: {child_after:?}"
    );

    let parent_after = h.process_state().expect("parent observable");
    assert!(
        parent_after.is_state('T'),
        "DR-0001 軸 1 follow (bash REPL category): 親も STOPPED ('T') に follow するはず。\
         got: {parent_after:?}"
    );

    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(h.pid().as_raw()),
        Signal::SIGCONT,
    );
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}
