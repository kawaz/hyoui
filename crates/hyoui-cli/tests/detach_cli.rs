//! DR-0020 §4 e2e: `hyoui detach` の実動作。
//!
//! `hyoui detach <session>` (= 常に all) で全 attach client が drop され、各 client
//! process が EOF で終了する。daemon と子 PTY は生存継続する (= DR-0015 §2.3.1)。

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::pty::HyouiTestRunner;

fn hyoui_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// status を引いて attach client 数 (= `id=` 出現回数) が `n` になるまで待つ。
fn wait_for_client_count(socket: &std::path::Path, n: usize, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let out = Command::new(hyoui_bin())
            .args(["status", &format!("--socket={}", socket.display())])
            .env_remove("HYOUI_LOCK_TOKEN")
            .env_remove("HYOUI_SESSION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        if let Ok(o) = out
            && o.status.success()
        {
            let text = String::from_utf8_lossy(&o.stdout);
            // status 自身も一時 client を張るが、その間 attach client も並ぶ。
            // `id=` の数が安定して n 以上になったら成立とみなす。
            let count = text.matches("id=").count();
            if count >= n {
                return true;
            }
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `hyoui detach --socket=<sock>` (= 常に all) を別 process で実行する。
fn run_detach(socket: &std::path::Path) -> std::process::Output {
    Command::new(hyoui_bin())
        .args(["detach", &format!("--socket={}", socket.display())])
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("detach")
}

#[test]
fn detach_all_drops_every_client_but_daemon_survives() {
    let runner = HyouiTestRunner::new();
    let session = "detach-all";

    // run = leader client + daemon + 子 (sleep 60 で生存)。
    let mut leader = runner.spawn_hyoui(session, &["run", "--", "sh", "-c", "sleep 60"]);
    // leader が daemon に接続して leader 登録を終えるまで待つ。
    assert!(
        leader.wait_for_leader_ready(Duration::from_secs(10)),
        "leader attach ready"
    );

    // 2nd client を attach。leader + 2nd + status 一時接続 = 3 client になるまで待つ
    // (= 2nd の接続確定。leader のみなら leader + status = 2 で止まる)。
    let mut second = runner.attach(session);
    assert!(
        wait_for_client_count(leader.socket(), 3, Duration::from_secs(10)),
        "2nd client attach ready (clients >= 3 including status probe)"
    );
    // pump して 2nd の PTY buffer を詰まらせない。
    second.pump_pty();

    // detach (= 常に all) で全 client を引き剥がす。
    let out = run_detach(leader.socket());
    assert!(
        out.status.success(),
        "detach all は成功すべき。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Fable Minor3: 成功報告に drop 件数が出る (= leader + 2nd + detach CLI 自身 = 3)。
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("3 client(s) detached"),
        "DetachAck の件数報告が出るべき。stdout={stdout:?}"
    );

    // 両 client process が EOF で終了する (= daemon が drop して socket close)。
    let leader_code = leader.wait_exit_code(Duration::from_secs(10));
    let second_code = second.wait_exit_code(Duration::from_secs(10));
    assert!(
        leader_code.is_some(),
        "detach all 後、leader client は終了すべき"
    );
    assert!(
        second_code.is_some(),
        "detach all 後、2nd client は終了すべき"
    );

    // daemon は生存継続 (= 子 PTY 接続維持、DR-0015 §2.3.1)。socket がまだ live で
    // status が取れることで確認する。
    let status = Command::new(hyoui_bin())
        .args(["status", &format!("--socket={}", leader.socket().display())])
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("status");
    assert!(
        status.status.success(),
        "detach all 後も daemon は生存し status が取れるべき。stderr={:?}",
        String::from_utf8_lossy(&status.stderr)
    );

    // 後始末: 子 + daemon を kill。
    let _ = Command::new(hyoui_bin())
        .args(["kill", &format!("--socket={}", leader.socket().display())])
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// DR-0020 §5 / Fable M4: attach 成立時の発見性ヒントが stderr (= PTY) に出る。
/// 出力は raw mode 前 (= 外側端末の scrollback に残る位置)。文言は Ctrl+Z ガードの
/// 有効時 default (DR-0029 §2) を反映する。
#[test]
fn attach_emits_discovery_hint_for_ctrl_z_guard() {
    let runner = HyouiTestRunner::new();
    let session = "hint-emit";

    let mut leader = runner.spawn_hyoui(session, &["run", "--", "sh", "-c", "sleep 30"]);
    let out = leader
        .wait_for("[hyoui] detach:", Duration::from_secs(10))
        .expect("ヒントが PTY (stderr) に出るべき");
    assert!(
        out.contains("[hyoui] detach: Ctrl+Z"),
        "ヒントは Ctrl+Z ガードの操作を案内すべき。out={out:?}"
    );

    // 後始末。
    let _ = Command::new(hyoui_bin())
        .args([
            "kill",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = leader.wait_exit_code(Duration::from_secs(5));
}

#[test]
fn status_lists_clients_with_mode_leader_and_connected_time() {
    // DR-0020 §5: status の client 一覧に mode / leader / 接続時刻 (connected=) が出る。
    let runner = HyouiTestRunner::new();
    let session = "status-clients";

    let mut leader = runner.spawn_hyoui(session, &["run", "--", "sh", "-c", "sleep 30"]);
    assert!(
        leader.wait_for_leader_ready(Duration::from_secs(10)),
        "leader attach ready"
    );

    let out = Command::new(hyoui_bin())
        .args([
            "status",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("status");
    assert!(out.status.success(), "status 成功");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(stdout.contains("clients:"), "clients セクションがある");
    assert!(
        stdout.contains("leader"),
        "leader client が示される。stdout={stdout:?}"
    );
    assert!(
        stdout.contains("connected="),
        "接続時刻 (connected=) が示される。stdout={stdout:?}"
    );

    // 後始末。
    let _ = Command::new(hyoui_bin())
        .args([
            "kill",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = leader.wait_exit_code(Duration::from_secs(5));
}
