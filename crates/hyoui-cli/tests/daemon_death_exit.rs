//! issue 2026-06-11 優先1: daemon 死亡時の attach client exit code semantics の e2e。
//!
//! `RunOutcome` 分解により、attach client の終了原因が exit code に反映される:
//! - daemon が予期せず消滅 (= kill -9) → 非 0 (= 9) + stderr 警告 (ConnectionLost)
//! - detach key で自分から離脱 → exit 0 (Detached)
//! - 子 PTY が正常 exit → 子の exit code を伝搬 (ChildExited)
//!
//! 旧実装はこれらを区別できず全部 exit 0 だったため、スクリプトから「子が正常
//! 終了した」と「daemon が落ちた」を取り違える問題があった。

mod common;

use std::time::Duration;

use common::pty::{HyouiTestRunner, find_descendants};

/// 観測対象の遷移を待つ短い sleep。
fn settle() {
    std::thread::sleep(Duration::from_millis(200));
}

/// daemon を kill -9 で予期せず落とすと、attach client は exit 9 (= ConnectionLost)
/// で終わり、stderr に接続喪失メッセージを出す。
#[test]
fn daemon_kill9_makes_client_exit_connection_lost() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("daemon-death-c9", &["run", "--", "/bin/sleep", "60"]);

    // attach client が leader として daemon に接続するまで待つ (= socket 確立)。
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );
    settle();

    // daemon process を特定して SIGKILL。socket 引数で探す → 見つからなければ
    // 自プロセス子孫から hyoui session leader を探す fallback。
    let daemon = h.daemon_pid().or_else(|| {
        find_descendants(h.pid().as_raw()).ok().and_then(|ds| {
            ds.iter()
                .find(|p| p.comm.contains("hyoui") && p.pgid == p.pid)
                .map(|p| p.pid)
        })
    });
    let daemon = daemon.expect("daemon process が見つからない");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("kill -9 daemon");

    // attach client は socket EOF を ConnectionLost と判定して exit 9 する。
    let code = h.wait_exit_code(Duration::from_secs(10));
    assert_eq!(
        code,
        Some(9),
        "daemon kill -9 で client は exit 9 (ConnectionLost) するはず"
    );

    // stderr メッセージ (= 接続喪失警告) が出力に含まれる。PTY 経由で stdout/stderr
    // は混在するが、substring で確認できる。
    let out = String::from_utf8_lossy(&h.drain_output()).to_string();
    assert!(
        out.contains("接続が失われ"),
        "接続喪失メッセージが stderr に出るはず。出力: {out:?}"
    );
}

/// 子 PTY が正常 exit すると、その exit code が attach client にそのまま伝搬する
/// (= ChildExited)。daemon は落ちていないので ConnectionLost ではない。
#[test]
fn child_exit_propagates_code() {
    let runner = HyouiTestRunner::new();
    // 少し生きてから exit 7 する子。即 exit だと attach client の exec が daemon の
    // handshake に間に合わず connect 失敗 (= exit 1) になる race があるため、
    // leader 接続が成立する猶予を持たせてから exit させる。
    // SessionExitNotify → ChildExited{7} → client が exit 7。
    let mut h = runner.spawn_hyoui(
        "daemon-death-child7",
        &["run", "--", "/bin/sh", "-c", "sleep 3; exit 7"],
    );
    // leader 接続が成立する (= handshake 完了) のを待ってから exit を待つ。子の sleep を
    // 3s 取るのは、並列負荷で daemon 起動 / handshake が遅れても、子が exit する前に
    // leader 接続が確実に成立するようにするため (= 「即 exit で connect が間に合わず
    // exit 1」「leader_ready 待ちに時間を食って子の exit を取りこぼす」race を両方回避)。
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    let code = h.wait_exit_code(Duration::from_secs(15));
    assert_eq!(
        code,
        Some(7),
        "子の exit 7 が client に伝搬するはず (= ChildExited)"
    );
}

/// Ctrl+Z 単発 (= ガード発火、DR-0029 §2) は **detach ではなく client suspend** なので、
/// client process は終了せず attach も維持される (= 2026-07-30 kawaz 裁定で仕様変更。
/// 旧 test は exit 0 = Detached を期待していた)。
///
/// この test process が spawn した client の pgrp は orphan (= 親が別 session) なので、
/// POSIX により SIGTSTP は破棄され実際には停止しない。ここで確かめるのは
/// 「**client が終了しない**」ことだけで、停止 / `fg` 復帰の観測は job control を持つ
/// 親 shell が要るため e2e `ctrlz_suspend_client.rs` が担当する。
#[test]
fn single_ctrl_z_does_not_terminate_client() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("daemon-death-suspend", &["run", "--", "/bin/sleep", "60"]);

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );
    settle();

    // Ctrl+Z (0x1a) を 1 発だけ PTY 経由で送る → delay 満了で client suspend が起動。
    h.send_bytes(&[0x1a]).expect("send ctrl-z");

    // suspend は接続を畳まないので、client は exit しないまま生き続ける。
    let code = h.wait_exit_code(Duration::from_secs(3));
    assert_eq!(
        code, None,
        "Ctrl+Z 単発で client が終了してはいけない (= suspend であって detach ではない)"
    );

    // 後始末: 正規経路で daemon を畳んでから client を落とす。
    h.kill_daemon_via_cli();
    let _ = h.kill();
}
