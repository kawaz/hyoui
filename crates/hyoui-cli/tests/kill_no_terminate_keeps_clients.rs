//! `hyoui kill --no-terminate` が attach client を蹴らないことの e2e
//! (docs/issue/2026-07-21-sigcont-alive-child-session-vanish.md)。
//!
//! `--no-terminate` は「signal を 1 発送るだけで session を畳まない」操作なので、
//! 他 client を detach する必然性がない。旧実装は terminate 経路と同じ
//! `AttachOptions { detach_others: true }` で接続していたため、
//! `hyoui kill <s> --signal=CONT --no-terminate` (= DR-0029 §1 の画面通知が案内する
//! 「停止中の子を起こす」正規手段) が attach client を全員切断していた。
//!
//! 対比として terminate 経路 (= `--no-terminate` なし) は従来どおり奪取して効く。

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::pty::HyouiTestRunner;

fn hyoui_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// `--signal=CONT --no-terminate` を送っても attach client が繋がったまま。
#[test]
fn no_terminate_signal_keeps_attach_client_connected() {
    let runner = HyouiTestRunner::new();
    let session = "kill-no-terminate-keeps";
    let mut h = runner.spawn_hyoui(session, &["run", "--", "/bin/cat"]);
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    let out = Command::new(hyoui_bin())
        .args([
            "kill",
            &format!("--socket={}", runner.socket_path(session).display()),
            "--signal=CONT",
            "--no-terminate",
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("kill --no-terminate");
    assert!(
        out.status.success(),
        "kill --no-terminate は成功するはず: {out:?}"
    );

    // client は切断されない (= exit しない)。切断されていれば short timeout で
    // exit code が拾える。
    assert_eq!(
        h.wait_exit_code(Duration::from_secs(3)),
        None,
        "--no-terminate で attach client が切断されてはいけない"
    );
    // status からも rw leader が居ることを確認する (= プロセス生存だけでなく接続維持)。
    let status = h.status_text().expect("status");
    assert!(
        status.contains("mode=Rw") && status.contains("leader"),
        "rw leader が client 一覧に残るはず: {status}"
    );

    let _ = h.kill();
}

/// 対比: terminate 経路 (= `--no-terminate` なし) は従来どおり session を畳み、
/// attach client も終了する。
#[test]
fn terminate_kill_still_ends_attach_client() {
    let runner = HyouiTestRunner::new();
    let session = "kill-terminate-ends";
    let mut h = runner.spawn_hyoui(session, &["run", "--", "/bin/cat"]);
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    let out = Command::new(hyoui_bin())
        .args([
            "kill",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("kill");
    assert!(out.status.success(), "kill は成功するはず: {out:?}");

    assert!(
        h.wait_exit_code(Duration::from_secs(10)).is_some(),
        "terminate 経路では attach client も終了するはず"
    );
}
