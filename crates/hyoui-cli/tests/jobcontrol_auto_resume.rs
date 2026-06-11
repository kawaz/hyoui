//! DR-0019: `hyoui run --on-child-suspend=auto-resume` の daemon 配線 e2e。
//!
//! 経路:
//!
//! ```
//! 子 PTY 内 self-stop (= sh -c 'kill -STOP $$; echo MARKER')
//!   → daemon process が waitpid(WUNTRACED) で観測
//!   → on_child_suspend=auto-resume なら daemon が即 killpg(child, SIGCONT)
//!     (= SessionChildStoppedNotify は送らない)
//!   → 子が復帰して `MARKER` を出力
//! ```
//!
//! 対比として default (= notify) では子が起こされず、短時間 `MARKER` が出ない
//! ことを確認する (= auto-resume の効果が daemon 配線由来であることの裏取り)。
//!
//! `#[ignore]`: PTY child + signal を使うため CI 安定性の都合でローカル実行
//! (= `cargo test -- --ignored`)。jobcontrol_follow.rs と同じ運用。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// self-stop してから marker を出力する子。AutoResume なら daemon が復帰させる。
const SELF_STOP_THEN_ECHO: &[&str] = &["sh", "-c", "kill -STOP $$; echo RESUMED_MARKER"];

/// auto-resume: 子が self-stop しても daemon が即 SIGCONT で起こすので、
/// 復帰後の `echo` 出力 (= RESUMED_MARKER) が PTY 経由で観測できる。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn auto_resume_resumes_self_stopped_child() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "jobcontrol-auto-resume",
        &[
            "run",
            "--on-child-suspend=auto-resume",
            "--",
            SELF_STOP_THEN_ECHO[0],
            SELF_STOP_THEN_ECHO[1],
            SELF_STOP_THEN_ECHO[2],
        ],
    );

    // daemon が auto-resume で子を起こせば、復帰後の echo が PTY に出る。
    let out = h
        .wait_for("RESUMED_MARKER", Duration::from_secs(8))
        .expect("auto-resume で子が復帰して RESUMED_MARKER を出力するはず");
    assert!(
        out.contains("RESUMED_MARKER"),
        "auto-resume 出力に marker が含まれない: {out:?}"
    );

    let _ = h.kill();
}

/// notify (default): 子が self-stop すると daemon は起こさない。leader (= attach)
/// が follow して止まり、子も stopped のまま → 短時間 `RESUMED_MARKER` は出ない。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn notify_default_does_not_resume_self_stopped_child() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "jobcontrol-notify-default",
        &[
            "run",
            "--",
            SELF_STOP_THEN_ECHO[0],
            SELF_STOP_THEN_ECHO[1],
            SELF_STOP_THEN_ECHO[2],
        ],
    );

    // notify では子は stopped のまま。短い timeout で marker が出ない (= Err) ことを確認。
    let result = h.wait_for("RESUMED_MARKER", Duration::from_secs(2));
    assert!(
        result.is_err(),
        "notify default では子が起こされず marker は出ないはず: {result:?}"
    );

    let _ = h.kill();
}
