//! DR-0020 §1 e2e: 子プロセスへ `HYOUI_SESSION_ID` が常時注入される。
//!
//! `hyoui run --session-id=<uuid> -- sh -c 'echo MARK=$HYOUI_SESSION_ID'` を実機 PTY で
//! 起動し、子 shell が展開した env の値が確定済 session id と一致することを確認する。
//! daemon の child spawn 経路で注入される。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

#[test]
fn child_inherits_hyoui_session_id_env() {
    let runner = HyouiTestRunner::new();
    let session = "selfid-e2e";
    let sid = hyoui::cli::new_session_id();
    let sid_arg = format!("--session-id={sid}");

    // 子 shell に env を echo させてから keep-alive (= daemon が exit して socket が
    // 消える前に出力を観測する)。`MARK=` prefix で他出力と区別する。
    let mut h = runner.spawn_hyoui(
        session,
        &[
            "run",
            &sid_arg,
            "--",
            "sh",
            "-c",
            "echo MARK=$HYOUI_SESSION_ID; sleep 30",
        ],
    );

    let out = h
        .wait_for("MARK=", Duration::from_secs(10))
        .expect("MARK= が出力されること");

    // 注入値は確定済 session id (= --session-id で指定した値) と一致する。
    assert!(
        out.contains(&format!("MARK={sid}")),
        "HYOUI_SESSION_ID は確定 session id を指すべき。出力: {out:?}"
    );

    // 後始末: 子を kill して daemon を畳む。
    let _ = h.signal(nix::sys::signal::Signal::SIGKILL);
}
