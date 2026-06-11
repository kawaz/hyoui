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

/// detach key (`Ctrl-A d`) で client が自分から離脱したら exit 0 (= Detached)。
/// daemon と子は生き続ける (= ConnectionLost ではない)。
#[test]
fn detach_key_makes_client_exit_zero() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("daemon-death-detach", &["run", "--", "/bin/sleep", "60"]);

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );
    settle();

    // Ctrl-A (0x01) + 'd' を PTY 経由で送る → 自発 detach。
    h.send_bytes(&[0x01, b'd']).expect("send detach key");

    // attach client は Detached で exit 0 する (= 子・daemon は残る)。
    let code = h.wait_exit_code(Duration::from_secs(10));
    assert_eq!(
        code,
        Some(0),
        "detach key で client は exit 0 (Detached) するはず"
    );

    // 後始末: daemon と子 (/bin/sleep) はまだ生きている。client (= h.pid) は exec で
    // attach に化けた後 detach で exit 済なので、Drop の subtree 掃除は daemon を
    // 拾えない (= daemon は ppid=1 に re-parent 済)。正規経路で daemon を畳む。
    h.kill_daemon_via_cli();
    let _ = h.kill();
}
