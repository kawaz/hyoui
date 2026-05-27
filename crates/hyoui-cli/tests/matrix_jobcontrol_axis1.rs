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
//! 現状 DR-0001 軸 1/2 は **未実装** (= task #34 で実装予定)。本 file の test は
//! DR-0014 §検証主義に従い、**「期待動作」ではなく「現実態」を assert で記録**
//! する形式。task #34 で実装が入ったら、本 file の assert が壊れることが
//! 「修正が効いている」trigger になる (= 修正時に必ず本 file を見直すよう強制
//! する仕組み)。
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
/// 子 (sleep) に **SIGSTOP** を送って STOP。親 hyoui の現実態を観測する。
///
/// **DR-0001 軸 1 期待動作 (interactive `follow`)**: 子 STOPPED → 親も STOPPED。
/// **現実態 (= 軸 1 未実装)**: 子 STOPPED、親は Running のまま。
#[test]
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
    // 現実態固定: 軸 1 未実装なので親は Running のまま (= follow しない)。
    // task #34 で follow が実装されたら、ここは `is_state('T')` に反転する。
    assert!(
        !parent_after.is_state('T'),
        "現実態: 軸 1 follow 未実装のため親は STOP に follow しない。\
         task #34 で実装されたら assert を反転する。got: {parent_after:?}"
    );

    // cleanup: 子 SIGCONT + 親 kill
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A2: 明示 `--on-child-suspend=follow` を指定しても挙動が変わらないこと。
///
/// flag は parse されるが配線されていない現実態を assert で記録 (= task #34 trigger)。
#[test]
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
    // 現実態固定: explicit follow 指定でも親は STOP しない (= flag は parse 済だが
    // 実装側で配線されていない)。task #34 で配線されたら反転。
    assert!(
        !parent_after.is_state('T'),
        "現実態: --on-child-suspend=follow 配線未実装。got: {parent_after:?}"
    );

    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A3: 明示 `--on-child-suspend=auto-resume` で子を強制復帰。
///
/// **期待動作 (DR-0001 軸 1 `auto-resume`)**: 子 STOPPED → 親が即 SIGCONT を送り
/// 子が即 Running に戻る (= 子の STOP は実質的に観測されない、または非常に短い)。
/// **現実態**: 配線未実装なので子は STOP したまま放置。
#[test]
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
    // 現実態固定: 軸 1 auto-resume 未実装のため、SIGSTOP 後は STOP のまま。
    // task #34 で実装されたら Running ('R') / Sleeping ('S') に戻るはず。
    assert!(
        child_after.is_state('T'),
        "現実態: auto-resume 未配線のため子は STOP したまま。\
         task #34 で実装されたら R/S に戻る。got: {child_after:?}"
    );

    // cleanup
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A4: `--mode=headless` の default は `auto-resume`。
///
/// 子 SIGSTOP → 期待は即復帰、現実態は STOP 放置。
#[test]
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
    // 現実態固定: headless preset の auto-resume が配線されていない。
    assert!(
        child_after.is_state('T'),
        "現実態: headless auto-resume 未配線。\
         task #34 で実装されたら R/S に戻る。got: {child_after:?}"
    );

    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A5: `/bin/cat` (= line-oriented stdin reader) を interactive で
/// 起動し、子に SIGSTOP。category 違い (TUI ではなく line-oriented) でも現実態が
/// 同じことを確認 (= サンプル多様性、DR-0014 §最低 3 種類)。
#[test]
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
        !parent_after.is_state('T'),
        "現実態 (cat category): 軸 1 follow 未配線、親は STOP に follow しない。\
         got: {parent_after:?}"
    );

    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}

/// matrix cell A6: `bash --norc -i` (= interactive REPL category) を起動し、
/// 子 bash に SIGSTOP。3 種目の category で現実態を確認。
///
/// bash interactive は **job control を持つ** ため、本来は内側で job control が
/// 走る (= bash が前景子の signal を受けて分散する) はず。ただし bash 自身が
/// 子側 STOPPED になった時点で「親 hyoui が follow するか」は DR-0001 軸 1 の
/// 主題なので、bash の内側 job control 有無は本 cell では不問。
#[test]
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
        !parent_after.is_state('T'),
        "現実態 (bash REPL category): 軸 1 follow 未配線、親は STOP に follow しない。\
         got: {parent_after:?}"
    );

    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.pid), Signal::SIGCONT);
    h.kill().ok();
}
