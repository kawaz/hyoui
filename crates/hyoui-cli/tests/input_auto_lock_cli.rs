//! `hyoui input` invocation auto-lock の e2e テスト (DR-0022)。
//!
//! 並列 `hyoui input` の bytes 混線 race が auto-lock で消えること、外側 `lock acquire`
//! の token 継承で auto-acquire skip されることを確認する。
//!
//! 構成: test process 内で daemon を立ち上げ、子は `/bin/cat` (= line-oriented、stdout
//! に echo するので screen dump で bytes 順序が検証できる)。subprocess として
//! `hyoui input` を spawn する。

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::pty::join_with_deadline;
use hyoui::daemon::{DaemonConfig, Session};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// `/bin/cat` を子にした daemon を立ち上げる。screen 上に書き込んだ bytes が
/// echo されるので、bytes 順序を screen dump で検証できる。
fn spawn_cat_daemon(
    label: &str,
) -> (
    PathBuf,
    tempfile::TempDir,
    std::thread::JoinHandle<Result<i32, hyoui::sys::Error>>,
) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("hyoui-auto-lock-{label}-"))
        .tempdir()
        .expect("tempdir");
    use std::os::unix::fs::PermissionsExt;
    let perm = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(dir.path(), perm).expect("chmod 0700");
    let sock_path = dir.path().join("daemon.sock");
    let cfg = DaemonConfig::new(label, sock_path.clone(), vec!["/bin/cat".into()]);
    let session = Session::start(cfg).expect("daemon Session::start");
    let handle = std::thread::spawn(move || session.serve());
    (sock_path, dir, handle)
}

fn kill_daemon(sock_path: &std::path::Path) {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::ControlMessage;
    use hyoui::protocol::messages::Kill;
    let opts = AttachOptions {
        mode: hyoui::protocol::Mode::Rw,
        ..AttachOptions::default()
    };
    if let Ok(mut conn) = ClientConnection::connect(sock_path, opts) {
        // SIGKILL を選ぶ理由: /bin/cat は raw mode で stdin が EOF にならない限り
        // SIGTERM をハンドルして自然 exit しないことがある (= 子 stdin pipe は
        // daemon が PTY master fd を保持しているため通常の EOF にならない)。
        // SIGKILL なら確実に process が死に、daemon serve は finalize_child 経路で
        // exit する。wait: true で daemon serve の TerminateSession ack を強制。
        let _ = conn.send_control(&ControlMessage::Kill(Kill {
            signal: Some("SIGKILL".into()),
            wait: true,
        }));
        // 接続を drop して daemon の client_disconnect 経路を発火させる
        // (= auto-lock の process-bound GC で安全側に倒す)。
        drop(conn);
    }
}

/// 並列 input × 2 が直列化されることを確認する。
///
/// `/bin/cat` に対し 2 つの input が同時に "A...A\n" / "B...B\n" を送ると、
/// auto-lock 前は bytes が交錯したが、本 DR 後は **先着の "A" block 全 echo →
/// 後着の "B" block 全 echo** の順で screen に現れる。
///
/// 確認: 両 process が exit 0、screen dump 内で A block と B block が
/// 連続して並ぶ (= "AAAA...AAAA" の途中に "B" が混ざらない)。
#[test]
fn parallel_input_serialized_by_auto_lock() {
    let (sock, _dir, daemon) = spawn_cat_daemon("parallel-serial");
    // 子 cat が start するまで少し待つ
    std::thread::sleep(Duration::from_millis(200));

    // 約 100 文字の "A" (改行込み) と "B" の連続入力を 2 プロセスで同時 spawn。
    let a_payload = "A".repeat(100) + "\n";
    let b_payload = "B".repeat(100) + "\n";

    let mut a_child = Command::new(hyoui_bin())
        .arg("input")
        .arg("--socket")
        .arg(&sock)
        .arg(format!("text:{a_payload}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn A");
    let mut b_child = Command::new(hyoui_bin())
        .arg("input")
        .arg("--socket")
        .arg(&sock)
        .arg(format!("text:{b_payload}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn B");

    // 両方完了を待つ
    let a_status = wait_for_exit(&mut a_child, Duration::from_secs(15));
    let b_status = wait_for_exit(&mut b_child, Duration::from_secs(15));
    assert!(a_status.success(), "A exit: {a_status:?}");
    assert!(b_status.success(), "B exit: {b_status:?}");

    // bytes 順序の検証は screen dump で。`/bin/cat` は input を stdout に echo するので、
    // screen 内で A の 100 文字 block と B の 100 文字 block が **連続** (= 混ざらず)
    // 並ぶことを確認する。
    let dump = run_screen_dump_text(&sock);
    let stripped: String = dump.chars().filter(|c| !c.is_whitespace()).collect();

    // 「A が 100 連続後に B が 100 連続」 OR 「B が 100 連続後に A が 100 連続」のどちらか。
    // 文字種で A → B / B → A のいずれかの transition が **ちょうど 1 回**で済むことを
    // 確認 (= 混線していれば transition 数 ≥ 2)。
    let mut transitions = 0usize;
    let mut prev: Option<char> = None;
    for c in stripped.chars() {
        if c != 'A' && c != 'B' {
            continue;
        }
        if let Some(p) = prev
            && p != c
        {
            transitions += 1;
        }
        prev = Some(c);
    }
    assert!(
        transitions <= 1,
        "auto-lock 後は A/B の transition は 1 回以下のはず (= 直列化)。\
         got transitions={transitions}, stripped={stripped:?}, full_dump={dump:?}"
    );

    kill_daemon(&sock);
    let _ = join_with_deadline(daemon, Duration::from_secs(30), "auto-lock parallel daemon");
}

/// 外側 lock acquire で取った token を `--lock-token` で渡すと、`hyoui input` は
/// auto-acquire を skip する (= 外側の lock を壊さない)。
///
/// 確認方法: `hyoui lock acquire` を起動して token を取り、その token を inner input
/// に渡す。input 完了後、外側の lock がまだ生きていることを「別 client が input を
/// 試みると lock-not-held で reject される」ことで確認。
#[test]
fn outer_token_inheritance_skips_auto_acquire() {
    let (sock, _dir, daemon) = spawn_cat_daemon("outer-token");
    std::thread::sleep(Duration::from_millis(200));

    // 1. 外側 lock acquire を spawn (= 別 process で token を保持)
    let mut outer = Command::new(hyoui_bin())
        .arg("lock")
        .arg("acquire")
        .arg("--socket")
        .arg(&sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn outer lock");
    let token = read_first_stdout_line(&mut outer, Duration::from_secs(5));
    assert_eq!(token.len(), 32, "token must be 32 hex chars: {token:?}");

    // 2. inner input を --lock-token 付きで呼ぶ。token 継承により auto-acquire skip。
    //    外側が holder なので daemon は inner 接続を holder と見なさず、raw_data は
    //    `client.lock-not-held` で reject される (= 既存実装の挙動)。
    //    本 test では「auto-acquire を skip して reject される」ことを確認することで、
    //    inner が外側 lock を release してしまっていない (= skip した) ことを示す。
    let inner = Command::new(hyoui_bin())
        .arg("input")
        .arg("--socket")
        .arg(&sock)
        .arg(format!("--lock-token={token}"))
        .arg("text:hello\n")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run inner input");

    // inner は raw_data が lock-not-held で reject されて exit 1 になる想定
    // (= 外側 holder が別接続なので)。auto-acquire を skip した証拠でもある
    // (= もし auto-acquire していれば 30s wait → timeout exit 1 = 違うエラーメッセージ)。
    let stderr = String::from_utf8_lossy(&inner.stderr);
    assert!(
        !inner.status.success(),
        "inner input should fail (= 別接続が holder)、stderr={stderr}"
    );
    // skip の証拠: auto-lock acquire timeout のメッセージが **出ていない** こと
    assert!(
        !stderr.contains("auto-lock acquire 失敗"),
        "auto-acquire を skip しているはず、stderr={stderr}"
    );

    // 3. 外側 lock を release (= stdin EOF で graceful release)
    drop(outer.stdin.take());
    let _ = wait_for_exit(&mut outer, Duration::from_secs(5));

    kill_daemon(&sock);
    let _ = join_with_deadline(daemon, Duration::from_secs(30), "outer-token daemon");
}

/// 単一 `hyoui input` (= 他に競合する client なし) でも auto-lock が成功し、
/// 通常通り spec 送信できることを確認する。回帰テスト。
#[test]
fn single_input_succeeds_with_auto_lock() {
    let (sock, _dir, daemon) = spawn_cat_daemon("single-input");
    std::thread::sleep(Duration::from_millis(200));

    let out = Command::new(hyoui_bin())
        .arg("input")
        .arg("--socket")
        .arg(&sock)
        .arg("text:hello\n")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run input");
    assert!(
        out.status.success(),
        "single input should succeed, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    kill_daemon(&sock);
    let _ = join_with_deadline(daemon, Duration::from_secs(30), "single-input daemon");
}

// ===== helpers =====

fn wait_for_exit(child: &mut std::process::Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("child did not exit within {timeout:?}");
}

fn read_first_stdout_line(child: &mut std::process::Child, timeout: Duration) -> String {
    use std::io::{BufRead, BufReader};
    let mut stdout_owned = child.stdout.take().expect("stdout take");
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let mut r = BufReader::new(&mut stdout_owned);
        let _ = r.read_line(&mut line);
        let _ = tx.send(line);
    });
    match rx.recv_timeout(timeout) {
        Ok(s) => s.trim_end_matches('\n').to_string(),
        Err(_) => panic!("first stdout line not received within {timeout:?}"),
    }
}

fn run_screen_dump_text(sock: &std::path::Path) -> String {
    let out = Command::new(hyoui_bin())
        .arg("screen")
        .arg("dump")
        .arg("--socket")
        .arg(sock)
        .arg("--format=text")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run screen dump");
    assert!(
        out.status.success(),
        "screen dump failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
