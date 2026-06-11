//! issue 2026-06-11 優先3: daemon SIGTERM graceful shutdown の e2e。
//!
//! 経路:
//! ```
//! daemon process に kill -TERM
//!   → SIGTERM handler (self-pipe) → handle_suspend_signals が
//!     killpg(child, SIGTERM) + RelayOutcome::ClientDetachedOrKilled を返す
//!   → finalize escalation (CONT+TERM → grace → KILL)
//!   → SessionExitNotify broadcast → attach client が exit → PTY EOF
//!   → drop(listener) で socket unlink
//! ```
//!
//! 旧実装は handler 未登録で daemon 即死 → child SIGHUP 巻き添え死 + socket 残骸
//! だった。本変更で「意図的停止時は秩序立てて畳む」ようにした。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// daemon に SIGTERM → child (sleep 60) が escalation で終了 → client が
/// SessionExitNotify を受けて PTY EOF → socket file が unlink される。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行"]
#[test]
fn daemon_sigterm_terminates_child_and_unlinks_socket() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("daemon-sigterm", &["run", "--", "/bin/sleep", "60"]);

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );
    // socket file が存在することを確認 (= daemon が listen 中)。
    assert!(
        h.socket().exists(),
        "daemon 稼働中は socket file が存在するはず"
    );

    // daemon process に SIGTERM。
    let daemon = h.daemon_pid().expect("daemon process が見つからない");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("kill -TERM daemon");

    // graceful shutdown 経路: child が終了 → SessionExitNotify → client exit →
    // PTY EOF。sleep 60 が満了する前に EOF になるはず (= 数秒以内)。
    let result = h.wait_for("NEVER_PRINTED_SENTINEL", Duration::from_secs(8));
    assert!(
        matches!(
            result.as_ref().map_err(std::io::Error::kind),
            Err(std::io::ErrorKind::UnexpectedEof)
        ),
        "daemon SIGTERM で session が畳まれ PTY EOF になるはず: {result:?}"
    );

    // socket file が unlink されている (= graceful cleanup が走った証拠)。
    // linger (2s) + finalize escalation 完了後に unlink されるので少し待つ。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut unlinked = false;
    while std::time::Instant::now() < deadline {
        if !h.socket().exists() {
            unlinked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        unlinked,
        "graceful shutdown 後は socket file が unlink されるはず (残骸を残さない)"
    );

    let _ = h.kill();
}
