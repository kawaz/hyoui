//! `hyoui lock acquire` / `hyoui lock release` / `hyoui unlock` の integration test
//! (DR-0006 §7、task #20)。
//!
//! 実 binary を subprocess として spawn し、daemon は test process 内で
//! `Session::start` で立ち上げる構成。lock の取得・保持・解放・fail mode を
//! end-to-end で検証する。

mod common;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::pty::join_with_deadline;
use hyoui::daemon::{DaemonConfig, Session};

/// 1 つの daemon session を test 用に spawn する。
///
/// daemon は thread 内で `serve` まで進める。socket path は tempfile 経由で
/// 一意に。session_id は `cli-lock-test`。
fn spawn_test_daemon() -> (
    PathBuf,
    tempfile::TempDir,
    std::thread::JoinHandle<Result<i32, hyoui::sys::Error>>,
) {
    let dir = tempfile::Builder::new()
        .prefix("hyoui-cli-lock-test-")
        .tempdir()
        .expect("tempdir");
    // parent dir を mode 0700 にする (UnixSock::listen の前提)
    use std::os::unix::fs::PermissionsExt;
    let perm = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(dir.path(), perm).expect("chmod 0700");
    let sock_path = dir.path().join("daemon.sock");
    let cfg = DaemonConfig::new(
        "cli-lock-test",
        sock_path.clone(),
        vec!["/bin/sleep".into(), "30".into()],
    );
    let session = Session::start(cfg).expect("daemon Session::start");
    let handle = std::thread::spawn(move || session.serve());
    (sock_path, dir, handle)
}

/// hyoui binary の path (= cargo が test bin に注入する env var)。
fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// daemon を Kill control message で終わらせる helper。
///
/// **Rw で接続すること** (= issue 2026-06-11): Ro client の Kill は daemon が
/// `mode.not-allowed` で拒否する (= `serve_ro_client_kill_rejected` が仕様の正本)。
/// 旧実装は Ro で送っていたため kill が no-op になり、各テストの `daemon.join()` は
/// 子 `/bin/sleep 30` の自然 exit (~28 秒) を黙って待っていた。
fn kill_daemon(sock_path: &std::path::Path) {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::ControlMessage;
    use hyoui::protocol::messages::Kill;
    let opts = AttachOptions {
        mode: hyoui::protocol::Mode::Rw,
        ..AttachOptions::default()
    };
    if let Ok(mut conn) = ClientConnection::connect(sock_path, opts) {
        let _ = conn.send_control(&ControlMessage::Kill(Kill {
            signal: None,
            wait: true,
        }));
    }
}

/// stdout から 1 行 (= token) を timeout 付きで読む。
///
/// `hyoui lock acquire` は token を 1 行 print してから block する。
/// stdout が pipe の場合、binary 側の `println!` + `flush` で即時届くはず。
fn read_token_line(child: &mut Child, timeout: Duration) -> String {
    // `child.stdout` を take して別 thread で blocking `read_line`、main thread は
    // sync_channel の `recv_timeout` で時間制御。token 受信後、stdout owner は
    // thread の中で drop され、子の stdout pipe は **read 側だけ閉じる**
    // (= 子は SIGPIPE を受けず、後続の release hint stderr write も継続する)。
    let mut stdout_owned = child.stdout.take().expect("stdout take");
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let mut r = BufReader::new(&mut stdout_owned);
        let _ = r.read_line(&mut line);
        let _ = tx.send(line);
    });
    let start = Instant::now();
    let remaining = timeout
        .checked_sub(start.elapsed())
        .unwrap_or(Duration::from_millis(0));
    match rx.recv_timeout(remaining) {
        Ok(s) => s.trim_end_matches('\n').to_string(),
        Err(_) => panic!("token line not received within {timeout:?}"),
    }
}

/// 子 process が timeout 内に終了するのを待つ。
fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("child did not exit within {timeout:?}");
}

// ===== Test: lock acquire success path =====

/// `hyoui lock acquire --socket=<path>` が token を stdout に出して block すること、
/// SIGTERM で正常終了 (exit 0) すること。
#[test]
#[ignore = "flaky: SIGTERM タイミング依存、ローカル単独実行で確認"]
fn lock_acquire_prints_token_and_blocks_until_sigterm() {
    let (sock, _dir, daemon) = spawn_test_daemon();
    let mut child = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        // stdin は pipe、EOF を起こさないために close せず開いたまま保持
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hyoui");
    let token = read_token_line(&mut child, Duration::from_secs(5));
    assert_eq!(token.len(), 32, "token must be 32 hex chars, got {token:?}");
    assert!(
        token.chars().all(|c| c.is_ascii_hexdigit()),
        "token must be hex, got {token:?}"
    );
    // SIGTERM を送って release を促す
    let pid = child.id() as i32;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("send SIGTERM");
    let status = wait_for_exit(&mut child, Duration::from_secs(5));
    assert!(
        status.success(),
        "hyoui lock acquire should exit 0 on SIGTERM, got {status:?}"
    );
    kill_daemon(&sock);
    // 無限ハング防止 (= issue 2026-06-11): daemon serve が wedge しても deadline fail。
    let _ = join_with_deadline(daemon, Duration::from_secs(30), "lock_cli daemon serve");
}

// ===== Test: lock acquire stdin EOF path =====

/// stdin を close すると `hyoui lock acquire` が release して exit 0 する。
#[test]
fn lock_acquire_releases_on_stdin_eof() {
    let (sock, _dir, daemon) = spawn_test_daemon();
    let mut child = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hyoui");
    let _ = read_token_line(&mut child, Duration::from_secs(5));
    // stdin を drop して EOF を起こす
    drop(child.stdin.take());
    let status = wait_for_exit(&mut child, Duration::from_secs(5));
    assert!(
        status.success(),
        "hyoui lock acquire should exit 0 on stdin EOF, got {status:?}"
    );
    kill_daemon(&sock);
    let _ = daemon.join();
}

// ===== Test: lock acquire --mode=fail path =====

/// 他者が lock 保持中なら `--mode=fail` で即時 exit 1 する。
///
/// 構成: 1 番目の hyoui lock acquire (block 中) → 2 番目に --mode=fail で acquire → exit 1
#[test]
fn lock_acquire_mode_fail_returns_error_when_locked() {
    let (sock, _dir, daemon) = spawn_test_daemon();
    let mut holder = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn holder");
    let _ = read_token_line(&mut holder, Duration::from_secs(5));

    // 2 番目で --mode=fail
    let second = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        .arg("--mode=fail")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn second");
    assert!(
        !second.status.success(),
        "second acquire with --mode=fail should fail when locked, status={:?}",
        second.status
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("denied") || stderr.contains("denied"),
        "stderr should mention denied: {stderr}"
    );

    // cleanup: holder を SIGTERM で終わらせる
    let pid = holder.id() as i32;
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    );
    let _ = wait_for_exit(&mut holder, Duration::from_secs(5));
    kill_daemon(&sock);
    let _ = daemon.join();
}

// ===== Test: lock release from another process gets LockNotHeld =====

/// 別 process からの `hyoui lock release --token=<T>` は daemon の holder 照合で
/// 必ず reject される (= MVP 制約)。stderr に hint が出て exit 1 する。
#[test]
fn lock_release_from_different_process_gets_not_held_with_hint() {
    let (sock, _dir, daemon) = spawn_test_daemon();
    // まず lock を取る (holder process)
    let mut holder = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn holder");
    let token = read_token_line(&mut holder, Duration::from_secs(5));

    // 別 process から release を試みる (= 必ず失敗するはず)
    let release = Command::new(hyoui_bin())
        .arg("lock")
        .arg("release")
        .arg("--socket")
        .arg(&sock)
        .arg(format!("--token={token}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn release");
    assert!(
        !release.status.success(),
        "release from another process should fail (holder mismatch); status={:?}",
        release.status
    );
    let stderr = String::from_utf8_lossy(&release.stderr);
    assert!(
        stderr.contains("hint") || stderr.contains("LockNotHeld") || stderr.contains("not-held"),
        "stderr should mention hint or LockNotHeld: {stderr}"
    );

    // cleanup
    let pid = holder.id() as i32;
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    );
    let _ = wait_for_exit(&mut holder, Duration::from_secs(5));
    kill_daemon(&sock);
    let _ = daemon.join();
}

// ===== Test: unlock parser alias =====

/// `hyoui unlock <session>` も `hyoui lock release` と同じ behavior (= holder mismatch
/// で失敗するが parser / dispatcher が正しく配線されている)。
#[test]
fn unlock_alias_routes_to_release_dispatcher() {
    let (sock, _dir, daemon) = spawn_test_daemon();
    let mut holder = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn holder");
    let token = read_token_line(&mut holder, Duration::from_secs(5));

    let unlock = Command::new(hyoui_bin())
        .arg("unlock")
        .arg("--socket")
        .arg(&sock)
        .arg(format!("--token={token}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn unlock");
    assert!(
        !unlock.status.success(),
        "unlock from another process should fail (holder mismatch); status={:?}",
        unlock.status
    );
    // stderr が "unlock:" prefix で出ているか確認 (= alias 経由で cmd_label が unlock になる)
    let stderr = String::from_utf8_lossy(&unlock.stderr);
    assert!(
        stderr.contains("unlock"),
        "stderr should mention `unlock` (cmd_label): {stderr}"
    );

    // cleanup
    let pid = holder.id() as i32;
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    );
    let _ = wait_for_exit(&mut holder, Duration::from_secs(5));
    kill_daemon(&sock);
    let _ = daemon.join();
}

// ===== Test: lock release without token returns exit 2 =====

/// `hyoui lock release` で `--token` も `HYOUI_LOCK_TOKEN` env も無いと exit 2 で error。
#[test]
fn lock_release_without_token_exits_2() {
    // daemon は不要 (= token check は connect 前で起きる)
    let out = Command::new(hyoui_bin())
        .arg("lock")
        .arg("release")
        .arg("--socket")
        .arg("/nonexistent.sock") // socket は使わない (= token check が先)
        // 親 process の HYOUI_LOCK_TOKEN は子に継承されるので明示的に空に
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn");
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit code 2 (missing token), got {:?}, stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--token") || stderr.contains("HYOUI_LOCK_TOKEN"),
        "stderr should mention --token / HYOUI_LOCK_TOKEN: {stderr}"
    );
}
