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
//! default (= notify) では daemon は起こさないが、`hyoui run` は DR-0015 で
//! 「fork daemon + attach client」の合成なので rw attach client が同居しており、
//! DR-0030 によりその client が resume を要求する。したがって `run` 経由では
//! default でも子は復帰する (= 起動直後 / attach 中のどちらで止まっても同じ)。
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
    // DR-0030 §4 では daemon policy と rw attach resume が補完関係にあるが、この test
    // が名指しする退行は daemon の `auto-resume` 経路。attach 側でも同じ marker が出る
    // 状態では daemon policy の破壊を検出できないため、runner 固有 config で client
    // resume を止め、daemon が単独で子を起こすことを検証する。
    let mut h = runner.spawn_hyoui_with_config(
        "jobcontrol-auto-resume",
        &[
            "run",
            "--on-child-suspend=auto-resume",
            "--",
            SELF_STOP_THEN_ECHO[0],
            SELF_STOP_THEN_ECHO[1],
            SELF_STOP_THEN_ECHO[2],
        ],
        "[attach]\nresume_stopped_child = false\n",
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

/// 起動直後に self-stop する子。attach 成立時点で既に stopped なので、handshake
/// snapshot 経由の resume 経路 (DR-0030 の発火点その 1) が効く。
///
/// **仕様変更の記録**: 本 test は 2026-07-29 の kawaz 裁定 (👺RS-Q1) で期待値が
/// 反転した。旧 test `notify_default_does_not_resume_self_stopped_child` は
/// 「default では起こされない」を期待していたが、それは DR-0019 §3 (daemon は
/// 勝手に起こさない) だけを見て `run` に同居する attach client を見落とした
/// 期待値だった。DR-0030 が「rw attach 中は子を停止させたままにしない」を原則と
/// して確定したので、現在は起こされるのが正。**失敗を消すための test 改変ではなく、
/// 裁定による仕様変更**である (経緯は DR-0030 §Context)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn run_resumes_child_that_is_already_stopped_at_attach() {
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

    let out = h
        .wait_for("RESUMED_MARKER", Duration::from_secs(8))
        .expect("DR-0030: rw attach 中なので default policy でも子は復帰するはず");
    assert!(
        out.contains("RESUMED_MARKER"),
        "resume 後の出力に marker が含まれない: {out:?}"
    );

    let _ = h.kill();
}

/// attach **成立後**に self-stop する子。handshake snapshot は running なので、
/// DR-0030 の発火点その 2 (= attach 中の `SessionChildStoppedNotify` 受信) だけが
/// 復帰経路になる。
///
/// 裁定の主眼はこちら: 「TUI アプリを hyoui 直で起動した時、attach しているのに
/// 一切の操作が効かなくなる」害の再発防止 (= 起動直後 self-stop は稀で、実運用で
/// 効くのは走行中の self-stop)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn run_resumes_child_that_stops_while_attached() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "jobcontrol-stop-while-attached",
        &[
            "run",
            "--",
            "sh",
            "-c",
            // attach 成立を STARTED で同期してから止まる (= handshake 時は running)。
            "echo STARTED; sleep 1; kill -STOP $$; echo RESUMED_MARKER; sleep 5",
        ],
    );

    h.wait_for("STARTED", Duration::from_secs(8))
        .expect("子が起動して STARTED を出すはず");
    let out = h
        .wait_for("RESUMED_MARKER", Duration::from_secs(8))
        .expect("DR-0030: attach 中に止まった子も rw client の要求で復帰するはず");
    assert!(
        out.contains("RESUMED_MARKER"),
        "resume 後の出力に marker が含まれない: {out:?}"
    );

    let _ = h.kill();
}

/// resume 後に `hyoui status` の `child-state` が running へ戻ることを検証する。
///
/// 上の 3 test は「子が復帰したか」を子の出力 (= RESUMED_MARKER) だけで見ており、
/// daemon が保持する `child_stopped` latch が下りたかを一切見ていなかった。
/// そのため「子は動いているのに status は stopped に貼り付く」状態
/// (docs/issue/2026-06-12-bug-child-stopped-flag-not-cleared.md) を素通しし、
/// DR-0030 が実機で動いていないという誤診の原因になった。観測可能な state も
/// 復帰することをここで押さえる。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn status_child_state_returns_to_running_after_resume() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "jobcontrol-status-after-resume",
        &[
            "run",
            "--",
            "sh",
            "-c",
            "echo STARTED; sleep 1; kill -STOP $$; echo RESUMED_MARKER; sleep 10",
        ],
    );

    h.wait_for("STARTED", Duration::from_secs(8))
        .expect("子が起動して STARTED を出すはず");
    h.wait_for("RESUMED_MARKER", Duration::from_secs(8))
        .expect("DR-0030: attach 中に止まった子は復帰するはず");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    let mut running = false;
    while std::time::Instant::now() < deadline {
        h.pump_pty();
        last = h.status_text().unwrap_or_default();
        if last.contains("child-state: running") {
            running = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        running,
        "resume 後は child-state が running に戻るはず: {last}"
    );

    let _ = h.kill();
}
