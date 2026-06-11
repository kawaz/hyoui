//! DR-0019 §4: `hyoui run --timeout` / `--idle-timeout` の daemon 配線 e2e。
//!
//! 経路:
//!
//! ```
//! RunConfig.{timeout_ms,idle_timeout_ms}
//!   → daemonize (DaemonizeInit JSON) → DaemonConfig
//!   → serve_loop が eval_timeout で発火判定
//!   → 発火時 killpg(child, SIGTERM) → finalize escalation (= --until と同手順)
//!   → 子 exit → SessionExitNotify → attach client が exit → PTY EOF
//! ```
//!
//! 発火を「子が走り続けるはずの sleep が途中で殺され、PTY が EOF になる」ことで
//! 観測する。対比として timeout なしでは同条件で session が生き続ける
//! (= 発火が daemon 配線由来であることの裏取り)。
//!
//! `#[ignore]`: PTY child + signal + 実時間 sleep を使うため CI 安定性の都合で
//! ローカル実行 (= `cargo test -- --ignored`)。jobcontrol_*.rs と同じ運用。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// idle-timeout: 子が `READY` を出して 30s 黙るが、`--idle-timeout=1s` なので
/// daemon が出力途絶 1s で session を畳む。READY 観測後に sleep が殺され PTY EOF。
#[ignore = "PTY child + 実時間 sleep を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn idle_timeout_terminates_silent_child() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "timeout-idle",
        &[
            "run",
            "--idle-timeout=1s",
            "--",
            "sh",
            "-c",
            "echo READY; sleep 30",
        ],
    );

    // READY は即出る。
    let out = h
        .wait_for("READY", Duration::from_secs(5))
        .expect("子は起動直後に READY を出すはず");
    assert!(out.contains("READY"), "READY が出ていない: {out:?}");

    // 以降 30s 黙るが idle-timeout=1s で発火 → session 終了 → PTY EOF。
    // sleep 30 が満了する前に EOF になることを確認 (= 5s 以内に終わるはず)。
    let result = h.wait_for("NEVER_PRINTED_SENTINEL", Duration::from_secs(5));
    assert!(
        matches!(
            result.as_ref().map_err(std::io::Error::kind),
            Err(std::io::ErrorKind::UnexpectedEof)
        ),
        "idle-timeout で session が畳まれ PTY EOF になるはず: {result:?}"
    );

    let _ = h.kill();
}

/// overall timeout: 子は `READY` 後ずっと喋り続ける (= idle にはならない) が、
/// `--timeout=2s` で起動からの overall 上限で daemon が session を畳む。
#[ignore = "PTY child + 実時間 sleep を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn overall_timeout_terminates_busy_child() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "timeout-overall",
        &[
            "run",
            "--timeout=2s",
            "--",
            "sh",
            "-c",
            // 毎 200ms 喋り続ける (= idle-timeout では刈れない)。overall で刈る。
            "echo READY; while true; do echo tick; sleep 0.2; done",
        ],
    );

    let out = h
        .wait_for("READY", Duration::from_secs(5))
        .expect("子は起動直後に READY を出すはず");
    assert!(out.contains("READY"), "READY が出ていない: {out:?}");

    // overall=2s で発火 → session 終了 → PTY EOF。busy ループは自然終了しないので、
    // EOF が来れば overall timeout 由来と確定する。
    let result = h.wait_for("NEVER_PRINTED_SENTINEL", Duration::from_secs(6));
    assert!(
        matches!(
            result.as_ref().map_err(std::io::Error::kind),
            Err(std::io::ErrorKind::UnexpectedEof)
        ),
        "overall timeout で session が畳まれ PTY EOF になるはず: {result:?}"
    );

    let _ = h.kill();
}

/// 対比: timeout 指定なしでは、黙っている子の session は畳まれず生き続ける
/// (= 発火が timeout 配線由来であることの裏取り)。
#[ignore = "PTY child + 実時間 sleep を使う、ローカルで --ignored 実行 (DR-0019)"]
#[test]
fn no_timeout_keeps_silent_child_alive() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "timeout-none",
        &["run", "--", "sh", "-c", "echo READY; sleep 30"],
    );

    let out = h
        .wait_for("READY", Duration::from_secs(5))
        .expect("子は起動直後に READY を出すはず");
    assert!(out.contains("READY"), "READY が出ていない: {out:?}");

    // timeout 無しなので、3s 待っても session は生きたまま (= sentinel は出ず、
    // かつ EOF にもならず TimedOut で返る)。
    let result = h.wait_for("NEVER_PRINTED_SENTINEL", Duration::from_secs(3));
    assert!(
        matches!(
            result.as_ref().map_err(std::io::Error::kind),
            Err(std::io::ErrorKind::TimedOut)
        ),
        "timeout 無しでは session は生き続けるはず (TimedOut 期待): {result:?}"
    );

    let _ = h.kill();
}
