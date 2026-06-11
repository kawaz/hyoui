//! `hyoui record start` → `record stop` の e2e 回帰テスト (DR-0016 §7)。
//!
//! 核心は **hang しないこと**。先の bug (commit a6b83a4c で修正) では daemon が
//! stop 成功時に無音 return し、client が recv で永久 hang していた。修正後は
//! daemon が `RecordStopResponse` を返すので client は即 exit 0 する。
//!
//! 各 test は CLI を timeout 付きで実行し、timeout したら panic (= 修正前の無音
//! daemon なら hang → timeout で test fail、修正後は pass)。`.output()` は無 timeout
//! で hang をそのまま CI hang に変えるため使わない (= `run_with_timeout` を使う)。
//!
//! daemon は lock_cli.rs と同じ in-process 構成 (= `Session::start` + thread `serve`)。
//! socket は tempdir 経由で一意なので、kawaz が別 session 名で並行操作しても衝突
//! しない。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use hyoui::daemon::{DaemonConfig, Session};

/// 同一 process 内で daemon を **1 つずつ** 立ち上げるための process-global lock。
///
/// `serve()` は SIGCHLD self-pipe slot を process 全体で 1 つだけ所有する設計
/// (= 実運用は 1 process 1 daemon が前提)。test binary は複数 test を thread pool で
/// 並列実行するため、複数 daemon が同時に live になると非所有 daemon の子 reap が
/// 競合し `daemon.join()` が hang しうる (= test artifact、product bug ではない)。
/// 各 test が daemon の生存期間をこの lock で直列化し、常に 1 daemon だけ live に保つ。
fn daemon_serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        // panic した test が lock を poison しても後続を巻き込まない。
        .unwrap_or_else(|e| e.into_inner())
}

/// hyoui binary の path (= cargo が test bin に注入する env var)。
fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 1 つの daemon session を test 用に in-process で立ち上げる。
///
/// 子は `/bin/cat` (= stdin を待って blocking、record 対象として十分)。socket は
/// tempdir 経由で一意。`serve` は別 thread で回す。
fn spawn_test_daemon() -> (
    PathBuf,
    tempfile::TempDir,
    std::thread::JoinHandle<Result<i32, hyoui::sys::Error>>,
) {
    let dir = tempfile::Builder::new()
        .prefix("hyoui-record-e2e-")
        .tempdir()
        .expect("tempdir");
    // UnixSock::listen の前提で parent dir を 0700 に。
    use std::os::unix::fs::PermissionsExt;
    let perm = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(dir.path(), perm).expect("chmod 0700");
    let sock_path = dir.path().join("daemon.sock");
    let cfg = DaemonConfig::new("cli-record-e2e", sock_path.clone(), vec!["/bin/cat".into()]);
    let session = Session::start(cfg).expect("daemon Session::start");
    let handle = std::thread::spawn(move || session.serve());
    (sock_path, dir, handle)
}

/// daemon を Kill control message で終わらせ、daemon が socket を閉じる (= serve()
/// 終了) まで **同期的に** 待つ。
///
/// 2 つの落とし穴を両方塞ぐ:
/// 1. **Kill の mode**: Kill は `Mode::Rw` (= leader-eligible) でないと daemon 側
///    `handle_kill` で reject される (`ensure_rw_mode`)。record client は既に切断済
///    なので Rw 接続すると leader になり Kill が通る。Ro だと Kill が無視され
///    `/bin/cat` 子が stdin を待ち続け `serve()` が終わらず `daemon.join()` が hang。
/// 2. **write-then-close race**: Kill 送信直後に conn を drop して socket を閉じると、
///    daemon の poll loop が buffered Kill frame を drain する前に socket HUP の
///    disconnect 分岐を取り、Kill が捨てられて serve() が終わらない (= 負荷時に再現)。
///    → 閉じずに **recv で読み続ける** ことで HUP を起こさない。daemon は Kill を
///    処理 → child SIGTERM → serve() 終了 → socket close、ここで recv が Err(EOF)
///    を返してループ脱出する。read timeout を張って万一 daemon が死なない場合でも
///    CI hang にせず Err で抜ける (= loud failure へ倒す)。
fn kill_daemon(sock_path: &Path) {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::ControlMessage;
    use hyoui::protocol::messages::Kill;

    // AttachOptions::default() は mode=Rw。
    let Ok(mut conn) = ClientConnection::connect(sock_path, AttachOptions::default()) else {
        return;
    };
    if conn
        .send_control(&ControlMessage::Kill(Kill {
            signal: None,
            wait: true,
        }))
        .is_err()
    {
        return;
    }
    // recv が無限 block しないよう reader fd に read timeout を張る (= daemon が
    // 死ななくても 5s で Err、最終的に daemon.join() 側が落ちて test failure になる)。
    // reader_fd() は BorrowedFd を返すので unsafe 不要。
    let _ = nix::sys::socket::setsockopt(
        &conn.reader_fd(),
        nix::sys::socket::sockopt::ReceiveTimeout,
        &nix::sys::time::TimeVal::new(5, 0),
    );
    // daemon が socket を閉じる (= serve() 終了) まで読み続ける。EOF / timeout で Err。
    while conn.recv_control(None).is_ok() {}
}

/// CLI を spawn し、`timeout` 内に終わらなければ kill して panic する。
///
/// `wait_with_output` を別 thread で回し、main thread は `recv_timeout` で時間制御。
/// timeout = hang とみなして test fail させるのがこの helper の役割 (= 無音 daemon
/// 回帰の検知点)。返り値は (exit_code, stdout, stderr)。
fn run_with_timeout(args: &[&str], timeout: Duration) -> (i32, String, String) {
    let child = Command::new(hyoui_bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hyoui");
    let id = child.id();
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::process::Output>(1);
    std::thread::spawn(move || {
        if let Ok(out) = child.wait_with_output() {
            let _ = tx.send(out);
        }
    });
    match rx.recv_timeout(timeout) {
        Ok(out) => (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(_) => {
            // hang した = 修正前の無音 daemon の症状。子を kill してから fail。
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(id as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            panic!(
                "`hyoui {}` hung (no exit within {timeout:?})",
                args.join(" ")
            );
        }
    }
}

/// record を 1 個 start する helper。stdout に "Started record #N" が出ることも確認。
fn start_one_record(sock: &Path, output: &Path) {
    let sock_s = sock.to_string_lossy().into_owned();
    let out_s = output.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run_with_timeout(
        &["record", "start", "--socket", &sock_s, "--output", &out_s],
        Duration::from_secs(10),
    );
    assert_eq!(code, 0, "record start should exit 0; stderr={stderr}");
    assert!(
        stdout.contains("Started record #"),
        "record start stdout should confirm start: {stdout}"
    );
}

// ===== Test: record stop --all が hang せず完了する (= 直接の hang 回帰) =====

/// `record start` → `record stop --all` が timeout 内に exit 0 で完了し、
/// "stopped 1 record(s)" を出すこと。修正前の無音 daemon なら hang → timeout で fail。
#[test]
fn record_start_then_stop_all_completes_without_hang() {
    let _serial = daemon_serial();
    let (sock, dir, daemon) = spawn_test_daemon();
    let output = dir.path().join("rec-stopall.jsonl");
    start_one_record(&sock, &output);

    let sock_s = sock.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run_with_timeout(
        &["record", "stop", "--socket", &sock_s, "--all"],
        Duration::from_secs(10),
    );
    assert_eq!(code, 0, "record stop --all should exit 0; stderr={stderr}");
    assert!(
        stdout.contains("stopped 1 record(s)"),
        "stop --all should report 1 stopped: stdout={stdout}"
    );

    kill_daemon(&sock);
    let _ = daemon.join();
}

// ===== Test: record stop --id が hang せず完了する =====

/// `--id <N>` 経路でも hang せず exit 0 で完了すること。record_id は start が #1 から
/// 採番する前提 (= daemon 起動直後の最初の record は id=1)。
#[test]
fn record_start_then_stop_by_id_completes_without_hang() {
    let _serial = daemon_serial();
    let (sock, dir, daemon) = spawn_test_daemon();
    let output = dir.path().join("rec-byid.jsonl");
    start_one_record(&sock, &output);

    let sock_s = sock.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run_with_timeout(
        &["record", "stop", "--socket", &sock_s, "--id", "1"],
        Duration::from_secs(10),
    );
    assert_eq!(code, 0, "record stop --id 1 should exit 0; stderr={stderr}");
    assert!(
        stdout.contains("stopped 1 record(s)"),
        "stop --id should report 1 stopped: stdout={stdout}"
    );

    kill_daemon(&sock);
    let _ = daemon.join();
}

// ===== Test: 存在しない id への stop は hang せず exit 1 (error) =====

/// 存在しない id への `record stop --id` は timeout 内に exit 1 (= RecordNotFound)
/// で完了し、hang しないこと (= 失敗経路も無音でないことの e2e 固定)。
#[test]
fn record_stop_nonexistent_id_errors_without_hang() {
    let _serial = daemon_serial();
    let (sock, _dir, daemon) = spawn_test_daemon();

    let sock_s = sock.to_string_lossy().into_owned();
    let (code, _stdout, stderr) = run_with_timeout(
        &["record", "stop", "--socket", &sock_s, "--id", "999"],
        Duration::from_secs(10),
    );
    assert_eq!(
        code, 1,
        "record stop --id 999 should exit 1 (RecordNotFound); stderr={stderr}"
    );
    assert!(
        stderr.contains("RecordNotFound") || stderr.contains("record not found"),
        "stderr should mention record-not-found: {stderr}"
    );

    kill_daemon(&sock);
    let _ = daemon.join();
}
