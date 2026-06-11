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

/// M1 (issue 2026-06-11 優先3 後退修正): graceful shutdown シーケンス中に届く
/// **2 発目の SIGTERM** で daemon が即死せず、escalation → socket unlink が完走する。
///
/// OS shutdown は「TERM → 猶予 → 再 TERM」が普通なので現実的なシナリオ。
/// 子は `trap '' TERM INT` で SIGTERM/SIGINT を無視するため、finalize_child の
/// killpg(SIGTERM) では死なず ~5s grace 後の SIGKILL escalation で死ぬ。その grace
/// 区間に daemon へ 2 発目 SIGTERM を送る。
///
/// 旧実装は `release_suspend_signal_handlers` を finalize_child より **前** に
/// 呼んで handler を SIG_DFL に戻していたため、2 発目 SIGTERM で daemon が即死し
/// escalation も socket unlink もすっ飛んだ。修正で release を socket unlink 後に
/// 遅延させ、shutdown 中の handler を据え置く (= 2 発目以降は no-op で握り潰す)。
#[ignore = "PTY child + signal を使う、grace 5s を跨ぐためローカルで --ignored 実行"]
#[test]
fn daemon_second_sigterm_during_shutdown_completes_unlink() {
    let runner = HyouiTestRunner::new();
    // SIGTERM/SIGINT を無視し、起こされても寝続ける子 (= escalation grace を強制)。
    let mut h = runner.spawn_hyoui(
        "daemon-sigterm-2nd",
        &[
            "run",
            "--",
            "sh",
            "-c",
            "trap '' TERM INT; while :; do sleep 1; done",
        ],
    );

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );
    assert!(
        h.socket().exists(),
        "daemon 稼働中は socket file が存在するはず"
    );

    let daemon = h.daemon_pid().expect("daemon process が見つからない");
    let daemon_pid = nix::unistd::Pid::from_raw(daemon);

    // 1 発目 SIGTERM → graceful shutdown 開始 (= finalize escalation grace に入る)。
    nix::sys::signal::kill(daemon_pid, nix::sys::signal::Signal::SIGTERM)
        .expect("kill -TERM daemon (1st)");

    // escalation grace (~5s) の最中に 2 発目 SIGTERM を送る。
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        nix::sys::signal::kill(daemon_pid, None).is_ok(),
        "1 発目直後の daemon は生存しているはず (= 即死していない)"
    );
    nix::sys::signal::kill(daemon_pid, nix::sys::signal::Signal::SIGTERM)
        .expect("kill -TERM daemon (2nd, during shutdown)");

    // 2 発目で即死していたら socket は残骸として残る。修正後は escalation 完走 →
    // SIGKILL で子を倒し socket unlink まで到達する。grace 5s + 余裕を見て待つ。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
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
        "2 発目 SIGTERM で daemon が即死せず escalation → socket unlink が完走するはず"
    );

    let _ = h.kill();
}
