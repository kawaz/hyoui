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
        // stdin は `Stdio::null()` にしない: `hyoui lock acquire` は token 出力後の
        // block phase で **stdin EOF を release trigger にする** ため (= main.rs
        // `wait_until_release_signal` の POLLHUP / read=0 path)。/dev/null からの
        // 読み取りは即 EOF になるので、token を stdout に出した直後に CLI が exit → daemon が
        // process-bound GC で lock を release してしまい、web POST が来た時には lock が
        // 既に消えていて 409 でなく 200 が返る race を起こす (= CI Linux で観測、run
        // 29762474605)。piped で child が保持する ChildStdin を **drop せず生かし続ける**
        // ことで stdin を open のまま維持 = block phase から抜けない = lock 保持継続。
        // Child が drop (= ChildGuard::drop の kill + wait) された時点でまとめて閉じる。
        .stdin(Stdio::piped())
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

/// `POST /api/sessions/:id/resize` の e2e。
///
/// - valid body → 204、実際に daemon 側の window_size が反映される
///   (= `hyoui screen snapshot --include=WindowSize --format=json` で検証)
/// - cols=0 / rows=0 body → 400
/// - 不明 session_id → 404
#[test]
fn e2e_resize_endpoint() {
    let runtime = runtime_dir();
    let sid = "web-e2e-resize";

    spawn_detached(runtime.path(), sid);
    let (mut web, port) = spawn_web(runtime.path());
    let panic_guard = ChildGuard(&mut web);

    // 未知 session → 404
    let body = serde_json::to_vec(&serde_json::json!({"cols": 100u16, "rows": 30u16})).unwrap();
    let r = http_request(
        port,
        "POST",
        "/api/sessions/no-such-xyz/resize",
        Some(("application/json", &body)),
    );
    assert_eq!(r.status, 404, "unknown session must 404");

    // cols=0 → 400
    let bad = serde_json::to_vec(&serde_json::json!({"cols": 0u16, "rows": 30u16})).unwrap();
    let r = http_request(
        port,
        "POST",
        &format!("/api/sessions/{sid}/resize"),
        Some(("application/json", &bad)),
    );
    assert_eq!(r.status, 400, "cols=0 must 400");

    // valid → 204
    let ok = serde_json::to_vec(&serde_json::json!({"cols": 123u16, "rows": 37u16})).unwrap();
    let r = http_request(
        port,
        "POST",
        &format!("/api/sessions/{sid}/resize"),
        Some(("application/json", &ok)),
    );
    assert_eq!(
        r.status,
        204,
        "valid resize must 204 (body={})",
        String::from_utf8_lossy(&r.body)
    );

    // daemon 側で window_size が反映されるまで待って `screen snapshot` で検証。
    // snapshot は CBOR 出力なので --format=json + jq 相当の parse は入れず、
    // 「cols=123 & rows=37 を含む JSON テキスト」を最大 2 秒待って部分マッチする
    // 軽量検証にとどめる (= 依存 crate を増やしたくない)。
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut matched = false;
    let mut attempts = 0usize;
    let mut last = String::new();
    while Instant::now() < deadline {
        attempts += 1;
        let out = Command::new(hyoui_bin())
            .args([
                "screen",
                "snapshot",
                sid,
                "--include=WindowSize",
                "--format=json",
            ])
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env_remove("HYOUI_SESSION_ID")
            .env_remove("HYOUI_LOCK_TOKEN")
            .env_remove("HYOUI_NAMESPACE")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn screen snapshot");
        let text = String::from_utf8_lossy(&out.stdout);
        // snapshot --format=json は pretty JSON (`"cols": 123`) を出す。空白許容で
        // 部分文字列マッチ。
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("\"cols\":123") && compact.contains("\"rows\":37") {
            matched = true;
            break;
        }
        last = format!(
            "status={:?} stdout={:?} stderr={:?}",
            out.status.code(),
            text,
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        matched,
        "resize 後に snapshot の window_size が cols=123 rows=37 に反映されない \
         (attempts={attempts}, last={last})"
    );

    drop(panic_guard);
    cleanup(runtime.path(), sid);
}

/// DR-0027 Phase 3: `WS /api/sessions/:id/attach` の e2e。
///
/// - WS 接続 (upgrade) が確立できる
/// - client → daemon: WS binary で "HELLOWS\n" を送る → PTY に届き echo される
/// - daemon → client: echo bytes が WS binary message として返る (= 部分マッチ)
/// - WS close で bridge が正常終了する
///
/// 使う tungstenite は blocking client (= dev-dep のみ、prod は axum 内蔵の
/// tokio-tungstenite が bridge 実装)。std::net::TcpStream に対して handshake +
/// read_message / send_message する薄い client。
#[test]
fn e2e_ws_attach_bridge_roundtrip() {
    use std::net::TcpStream;
    use tungstenite::{Message, client, handshake::client::Request};

    let runtime = runtime_dir();
    let sid = "web-e2e-wsattach";

    spawn_detached(runtime.path(), sid);
    let (mut web, port) = spawn_web(runtime.path());
    let panic_guard = ChildGuard(&mut web);

    // 未知 session の WS upgrade は 404 が返るはず。upgrade 前の HTTP status で
    // handshake が失敗すること (= tungstenite が Err を返す) だけ確認。
    {
        let bad_url = format!("ws://127.0.0.1:{port}/api/sessions/no-such-xyz-ws/attach");
        let bad_req = Request::builder()
            .uri(&bad_url)
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let res = client(bad_req, stream);
        assert!(res.is_err(), "unknown session の WS handshake は失敗すべき");
    }

    // 正常 session の WS attach。
    let url = format!("ws://127.0.0.1:{port}/api/sessions/{sid}/attach");
    let req = Request::builder()
        .uri(&url)
        .header("Host", format!("127.0.0.1:{port}"))
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(())
        .unwrap();
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let (mut ws, _resp) = client(req, stream).expect("ws handshake");

    // WS → daemon: "HELLOWS\n" を送る (= line-echo shell が echo back する)。
    ws.send(Message::Binary(b"HELLOWS\n".to_vec().into()))
        .expect("ws send");

    // daemon → WS: echo bytes を含む binary message が届くまで最大 5s 待つ。
    // 中間で他の frame (= 過去 screen redraw 等) が挟まる可能性があるため、
    // 累積 buffer に「HELLOWS」が現れれば OK とする。
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut cum: Vec<u8> = Vec::new();
    let mut matched = false;
    while Instant::now() < deadline {
        match ws.read() {
            Ok(Message::Binary(b)) => {
                cum.extend_from_slice(&b);
                if cum.windows(7).any(|w| w == b"HELLOWS") {
                    matched = true;
                    break;
                }
            }
            Ok(Message::Text(s)) => cum.extend_from_slice(s.as_bytes()),
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => break,
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => panic!("ws read error: {e}"),
        }
    }
    assert!(
        matched,
        "WS 経由の echo bytes に 'HELLOWS' が現れない (cum={:?})",
        String::from_utf8_lossy(&cum)
    );

    // WS bridge が persistent leader を保持中、fallback POST は正直に 409 を返す。
    let post_body = serde_json::to_vec(&serde_json::json!({"cols": 77u16, "rows": 22u16})).unwrap();
    let response = http_request(
        port,
        "POST",
        &format!("/api/sessions/{sid}/resize"),
        Some(("application/json", &post_body)),
    );
    assert_eq!(
        response.status,
        409,
        "WS leader 保持中の POST resize は 409 になるべき: body={}",
        String::from_utf8_lossy(&response.body)
    );

    // zero size は daemon に転送せず、同じ WS 上で明示 error result を返す。
    ws.send(Message::Text(
        serde_json::json!({
            "kind": "resize",
            "requestId": 41u64,
            "cols": 0u16,
            "rows": 1u16,
        })
        .to_string()
        .into(),
    ))
    .expect("WS zero resize send");
    let deadline = Instant::now() + Duration::from_secs(5);
    let zero_ack = loop {
        assert!(Instant::now() < deadline, "WS zero resize.result timeout");
        match ws.read() {
            Ok(Message::Text(text)) => {
                let value: serde_json::Value =
                    serde_json::from_str(text.as_str()).expect("WS control response JSON");
                if value["kind"] == "resize.result" && value["requestId"] == 41u64 {
                    break value;
                }
            }
            Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => panic!("WS closed before zero resize result"),
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => panic!("WS zero resize read error: {e}"),
        }
    };
    assert_eq!(zero_ack["ok"], false, "zero resize ack={zero_ack}");
    assert!(
        zero_ack["error"]
            .as_str()
            .is_some_and(|message| message.contains("must be > 0")),
        "zero resize error={zero_ack}"
    );

    // 同じ WS leader connection の text control message から有効な resize を送る。
    ws.send(Message::Text(
        serde_json::json!({
            "kind": "resize",
            "requestId": 42u64,
            "cols": 91u16,
            "rows": 33u16,
        })
        .to_string()
        .into(),
    ))
    .expect("WS resize send");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut resize_ack = None;
    while Instant::now() < deadline {
        match ws.read() {
            Ok(Message::Text(text)) => {
                let value: serde_json::Value =
                    serde_json::from_str(text.as_str()).expect("WS control response JSON");
                if value["kind"] == "resize.result" && value["requestId"] == 42u64 {
                    resize_ack = Some(value);
                    break;
                }
            }
            Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => break,
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => panic!("WS resize read error: {e}"),
        }
    }
    let resize_ack = resize_ack.expect("WS resize.result が届くこと");
    assert_eq!(resize_ack["ok"], true, "resize ack={resize_ack}");

    // 成功応答は FIFO barrier 後なので、直後の snapshot で実サイズを観測できる。
    let out = Command::new(hyoui_bin())
        .args([
            "screen",
            "snapshot",
            sid,
            "--include=WindowSize",
            "--format=json",
        ])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_NAMESPACE")
        .stdin(Stdio::null())
        .output()
        .expect("screen snapshot after WS resize");
    assert!(out.status.success(), "snapshot stderr={:?}", out.stderr);
    let compact: String = String::from_utf8_lossy(&out.stdout)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        compact.contains("\"cols\":91") && compact.contains("\"rows\":33"),
        "WS resize が daemon window size に反映されていない: {compact}"
    );

    // client → daemon: 明示 Close。daemon 側 attach が cleanup されて daemon の
    // client 数が減ることは snapshot でも観測可能だが本 test では skip (= 別 test で
    // sessions API が client 数を出すのを確認済み)。
    ws.close(None).expect("ws close");
    // close frame の echo 待ちで少し drain。
    let _ = ws.read();

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
