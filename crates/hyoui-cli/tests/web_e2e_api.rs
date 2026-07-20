//! DR-0027 Phase 1 e2e: `hyoui web` subcommand を実 subprocess として起動し、
//! 実 daemon (`hyoui run --detached`) と組み合わせて API 3 endpoint を検証する。
//!
//! ## test 内容
//!
//! 1. tempdir を `XDG_RUNTIME_DIR` として指定
//! 2. `hyoui run --detached --session=<sid> -- sh -c "while read...; echo"` で daemon 起動
//! 3. 同 env で `hyoui web --listen=127.0.0.1:0` を起動
//!    - port 0 は kernel 割り振り、bind した実 port を stderr 経由で拾う
//! 4. TCP 直叩きで HTTP/1.1 request を組み立て、3 endpoint を叩く
//! 5. input POST 後、screen dump に送信文字列が現れるまで待つ
//!
//! ## HTTP client を素朴に書く理由
//!
//! reqwest / hyper client を dev-dep に加えると依存が肥大する (= Phase 1 の
//! 実質メリット < コスト)。テスト目的では固定 4 行の HTTP/1.1 request を
//! `TcpStream::write_all` するのがサイズ最小。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

fn runtime_dir() -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("hyoui-web-e2e-")
        .tempdir()
        .expect("tempdir");
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    d
}

fn spawn_detached(runtime: &Path, sid: &str) {
    let status = Command::new(hyoui_bin())
        .args([
            "run",
            "--detached",
            &format!("--session={sid}"),
            "--",
            "sh",
            "-c",
            // stdin を line 単位で echo back。POST /input の text が visible に反映される。
            "while IFS= read -r line; do echo \"$line\"; done",
        ])
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("XDG_STATE_HOME")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn detached daemon");
    assert!(status.success(), "run --detached が成功すること");

    let sock = runtime.join("hyoui").join(format!("{sid}.sock"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if sock.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("socket が出現しない: {}", sock.display());
}

fn cleanup(runtime: &Path, sid: &str) {
    let _ = Command::new(hyoui_bin())
        .args(["kill", sid])
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `hyoui web --listen=127.0.0.1:0` を spawn し、bind した実 port を返す。
///
/// child は panic path でも `ChildGuard` 経由で kill/wait される (= zombie 防止)。
#[allow(clippy::zombie_processes)]
///
/// `hyoui_web::serve` は起動時に `hyoui web: listening on http://127.0.0.1:<port>`
/// の 1 行を stderr に書く (= lib.rs)。stderr を pipe で読み、port を parse する。
fn spawn_web(runtime: &Path) -> (Child, u16) {
    let mut child = Command::new(hyoui_bin())
        .args(["web", "--listen=127.0.0.1:0"])
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("XDG_STATE_HOME")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hyoui web");
    let stderr = child.stderr.take().expect("stderr pipe");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        line.clear();
        let n = reader.read_line(&mut line).expect("read stderr");
        if n == 0 {
            panic!("hyoui web が port を出力する前に stderr を閉じました (= 起動失敗)");
        }
        // 期待 line: "hyoui web: listening on http://127.0.0.1:<port>"
        if let Some(host_port) = line.trim().strip_prefix("hyoui web: listening on http://")
            && let Some(port_str) = host_port.rsplit(':').next()
            && let Ok(port) = port_str.parse::<u16>()
        {
            // 残りの stderr は捨てる (= 別 thread で drain して child が blocked に
            // ならないようにする)。
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = reader.into_inner().read_to_end(&mut buf);
            });
            return (child, port);
        }
    }
    let _ = child.kill();
    panic!("hyoui web listening 行が deadline 内に来ない");
}

/// 素朴な HTTP/1.1 request 送信。response body / status / content-type を返す。
struct HttpResponse {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

fn http_request(port: u16, method: &str, path: &str, body: Option<(&str, &[u8])>) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some((ctype, b)) = body {
        req.push_str(&format!("Content-Type: {ctype}\r\n"));
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    if let Some((_, b)) = body {
        stream.write_all(b).unwrap();
    }
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read_to_end");

    // parse: status line + headers + \r\n\r\n + body
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header/body separator");
    let head_bytes = &buf[..split];
    let body = buf[split + 4..].to_vec();
    let head = std::str::from_utf8(head_bytes).expect("utf-8 headers");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status code");
    let mut content_type = String::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-type")
        {
            content_type = v.trim().to_string();
        }
    }
    HttpResponse {
        status,
        content_type,
        body,
    }
}

#[test]
fn e2e_sessions_screen_input() {
    let runtime = runtime_dir();
    let sid = "web-e2e-1";

    spawn_detached(runtime.path(), sid);
    let (mut web, port) = spawn_web(runtime.path());

    let panic_guard = ChildGuard(&mut web);

    // 1. GET /api/sessions
    let r = http_request(port, "GET", "/api/sessions", None);
    assert_eq!(r.status, 200);
    let json: serde_json::Value = serde_json::from_slice(&r.body).expect("json parse");
    let arr = json.as_array().expect("array");
    let found = arr
        .iter()
        .find(|e| e["session_id"].as_str() == Some(sid))
        .unwrap_or_else(|| panic!("session {sid} が list に出ない: {json}"));
    assert_eq!(found["status"].as_str(), Some("live"));
    assert!(found["argv"].is_array());

    // 2. GET /api/sessions/:id/screen
    let r = http_request(port, "GET", &format!("/api/sessions/{sid}/screen"), None);
    assert_eq!(r.status, 200);
    assert!(
        r.content_type.starts_with("text/plain"),
        "content-type = {:?}",
        r.content_type
    );
    assert!(!r.body.is_empty(), "screen dump payload must not be empty");

    // 3. POST /api/sessions/:id/input で HELLO\n 相当 (text:HELLO + key:Enter)
    let body_json = serde_json::json!({"specs": ["text:HELLO", "key:Enter"]});
    let body = serde_json::to_vec(&body_json).unwrap();
    let r = http_request(
        port,
        "POST",
        &format!("/api/sessions/{sid}/input"),
        Some(("application/json", &body)),
    );
    assert_eq!(
        r.status,
        200,
        "input POST body={:?}",
        String::from_utf8_lossy(&r.body)
    );
    let input_resp: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(input_resp["specs"].as_u64(), Some(2));

    // 画面反映を待つ (echo back → visible 領域に "HELLO")。
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_hello = false;
    while Instant::now() < deadline {
        let r = http_request(port, "GET", &format!("/api/sessions/{sid}/screen"), None);
        if r.status == 200 && r.body.windows(5).any(|w| w == b"HELLO") {
            got_hello = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(got_hello, "input 後の screen dump に 'HELLO' が現れない");

    // 4. 未知 session_id → 404
    let r = http_request(port, "GET", "/api/sessions/no-such-xyz/screen", None);
    assert_eq!(r.status, 404);

    // 5. 不正 spec → 400
    let bad = serde_json::to_vec(&serde_json::json!({"specs": ["notaknownprefix:xx"]})).unwrap();
    let r = http_request(
        port,
        "POST",
        &format!("/api/sessions/{sid}/input"),
        Some(("application/json", &bad)),
    );
    assert_eq!(r.status, 400);

    drop(panic_guard);
    cleanup(runtime.path(), sid);
}

/// DR-0022 auto-lock の web 側統合を検証する e2e。
///
/// 前提: `hyoui input` invocation の auto-lock を web `POST /input` にも入れたので、
/// **外部 CLI が lock を保持している間** に web から input を投げると 409 Conflict で
/// 失敗すること (= web は default 5s で timeout する)。
///
/// この振る舞いは DR-0022 の意味論 (= 他 client 入力中は wait する) と、web の HTTP
/// レスポンス性の要求 (= 応答待ちを長引かせない) の両立点。
#[test]
fn e2e_input_returns_409_while_external_client_holds_lock() {
    let runtime = runtime_dir();
    let sid = "web-e2e-lock-2";

    spawn_detached(runtime.path(), sid);
    let (mut web, port) = spawn_web(runtime.path());
    let panic_guard = ChildGuard(&mut web);

    // 外部 CLI で lock acquire → stdout に token が 1 行 print される。
    // acquire は blocking で socket が生きている限り保持する (= release まで)。
    let mut acquire_child = Command::new(hyoui_bin())
        .args(["lock", "acquire", sid])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lock acquire");
    let mut stdout = BufReader::new(acquire_child.stdout.take().expect("stdout pipe"));
    let mut token_line = String::new();
    let n = stdout.read_line(&mut token_line).expect("read token line");
    assert!(
        n > 0,
        "lock acquire が token を出力する前に stdout を閉じました"
    );
    let acquire_guard = ChildGuard(&mut acquire_child);

    // 別 client が lock を保持している状態で web から input を投げる → 409 が返る。
    let body_json = serde_json::json!({"specs": ["text:BLOCKED", "key:Enter"]});
    let body = serde_json::to_vec(&body_json).unwrap();
    let t0 = Instant::now();
    let r = http_request(
        port,
        "POST",
        &format!("/api/sessions/{sid}/input"),
        Some(("application/json", &body)),
    );
    let elapsed = t0.elapsed();
    assert_eq!(
        r.status,
        409,
        "外部 lock 保持中の input は 409 になるべき: status={}, body={}",
        r.status,
        String::from_utf8_lossy(&r.body)
    );
    // web の default timeout は 5s。少なくとも半分は待つはず (= すぐに 409 で返らない)。
    assert!(
        elapsed >= Duration::from_secs(1),
        "409 が早すぎます (= 実際に retry しているか怪しい): {elapsed:?}"
    );
    // 画面には送っていないはず (= BLOCKED 文字列は入らない)。
    let r = http_request(port, "GET", &format!("/api/sessions/{sid}/screen"), None);
    assert_eq!(r.status, 200);
    assert!(
        !r.body.windows(7).any(|w| w == b"BLOCKED"),
        "409 なのに画面に BLOCKED が出ています: 応答が実は成功した?"
    );

    drop(acquire_guard); // lock を解放 (= release、CLI が exit する)。

    // release 後は input が通ること。
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut succeeded = false;
    let body_json = serde_json::json!({"specs": ["text:AFTER", "key:Enter"]});
    let body = serde_json::to_vec(&body_json).unwrap();
    while Instant::now() < deadline {
        let r = http_request(
            port,
            "POST",
            &format!("/api/sessions/{sid}/input"),
            Some(("application/json", &body)),
        );
        if r.status == 200 {
            succeeded = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(succeeded, "外部 lock 解放後の input が 200 にならない");

    drop(panic_guard);
    cleanup(runtime.path(), sid);
}

/// panic 時に web subprocess を確実に kill する RAII guard。
struct ChildGuard<'a>(&'a mut Child);

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
