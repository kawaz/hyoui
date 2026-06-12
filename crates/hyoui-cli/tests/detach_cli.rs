//! DR-0020 §4 e2e: `hyoui detach` の target 別実動作。
//!
//! - `--target=all`: 全 attach client が drop され、各 client process が EOF で終了。
//!   daemon と子 PTY は生存継続する (= DR-0015 §2.3.1)。
//! - `--target=others`: 指定 client 以外が drop される。

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

/// `hyoui detach --socket=<sock> --target=<t>` を別 process で実行する。
fn run_detach(socket: &std::path::Path, target: &str) -> std::process::Output {
    Command::new(hyoui_bin())
        .args([
            "detach",
            &format!("--socket={}", socket.display()),
            &format!("--target={target}"),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
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

    // detach --target=all で全 client を引き剥がす。
    let out = run_detach(leader.socket(), "all");
    assert!(
        out.status.success(),
        "detach all は成功すべき。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[test]
fn attach_detach_others_steals_leadership() {
    let runner = HyouiTestRunner::new();
    let session = "detach-others-steal";

    // run = 元 leader client + daemon + 子。
    let mut leader = runner.spawn_hyoui(session, &["run", "--", "sh", "-c", "sleep 60"]);
    assert!(
        leader.wait_for_leader_ready(Duration::from_secs(10)),
        "leader attach ready"
    );

    // attach --detach-others = 接続成立時に元 leader を奪取 (= drop)。
    let mut stealer = runner.spawn_hyoui(session, &["attach", "--detach-others"]);

    // 元 leader process が EOF で終了する (= daemon が drop して socket close)。
    let leader_code = leader.wait_exit_code(Duration::from_secs(10));
    assert!(
        leader_code.is_some(),
        "--detach-others 後、元 leader client は奪取されて終了すべき"
    );

    // 後始末: stealer を pump して終了させ、daemon を kill。
    stealer.pump_pty();
    let _ = Command::new(hyoui_bin())
        .args([
            "kill",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = stealer.wait_exit_code(Duration::from_secs(5));
}

#[test]
fn attach_exclusive_denied_when_rw_client_present() {
    let runner = HyouiTestRunner::new();
    // Fable review M3: 旧 session 名 "exclusive-denied" は stderr の socket path に
    // 必ず含まれるため `stderr.contains("exclusive")` がトートロジーだった。
    // 検証対象の文言と重ならない名前にして実検証にする。
    let session = "excl-deny";

    // run = rw leader client が居る状態。
    let mut leader = runner.spawn_hyoui(session, &["run", "--", "sh", "-c", "sleep 60"]);
    assert!(
        leader.wait_for_leader_ready(Duration::from_secs(10)),
        "leader attach ready"
    );

    // attach --exclusive = 他 rw client が居るので handshake 段で拒否される。
    let out = Command::new(hyoui_bin())
        .args([
            "attach",
            "--exclusive",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("attach --exclusive");

    assert!(
        !out.status.success(),
        "他 rw client が居るとき --exclusive は拒否されるべき"
    );
    // Fable review M3: daemon の ErrorMessage.message が client の stderr まで
    // 中継されること (= 旧実装は汎用「hyoui status で確認」文言に握り潰されていた)。
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--exclusive denied") || stderr.contains("rw client が attach 中"),
        "daemon の exclusive 拒否理由が中継されるべき。stderr={stderr:?}"
    );

    // 後始末。
    let _ = Command::new(hyoui_bin())
        .args([
            "kill",
            &format!("--socket={}", runner.socket_path(session).display()),
        ])
        .env_remove("HYOUI_LOCK_TOKEN")
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = leader.wait_exit_code(Duration::from_secs(5));
}
