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
//! ## 認証と state dir の隔離 (DR-0036 決定 9)
//!
//! 認証には無認証 mode が無いので、`/api/*` と WS attach は登録 fixture で通す。
//! **`XDG_STATE_HOME` は必ず tempdir を指す** — `env_remove` にすると gateway が
//! 実利用の `~/.local/state/hyoui-web/auth.json` を読み、kawaz の本番 credential に
//! 対して test が走る。record を直に置き、access token を `Authorization: Bearer`
//! (WS は subprotocol `hyoui.token.<値>`) で提示する。
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

/// 隔離した `XDG_STATE_HOME` (= 認証登録簿の置き場、決定 9)。
fn state_home() -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("hyoui-web-e2e-state-")
        .tempdir()
        .expect("tempdir");
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    d
}

/// 認証済みの API client (= bind した port と、その endpoint の access token)。
struct Api {
    port: u16,
    token: String,
}

impl Api {
    /// `Authorization: Bearer` 付きで 1 要求投げる。
    fn request(&self, method: &str, path: &str, body: Option<(&str, &[u8])>) -> HttpResponse {
        http_request(self.port, method, path, body, Some(&self.token))
    }

    /// WS attach の subprotocol (= access token の運び方、決定 5)。
    fn ws_protocol(&self) -> String {
        format!("hyoui.token.{}", self.token)
    }
}

/// 隔離した state dir に登録 fixture を置き、access token を返す。
///
/// endpoint は gateway が bind した `http://127.0.0.1:<port>/` の正規形。**この値が
/// record の key と一致することが、正規形の扱い (決定 3) の test でもある。**
fn seed_credential(state: &Path, port: u16) -> String {
    use hyoui_web::auth::{AuthFile, StateDir, token};

    let endpoint = hyoui_web::contract::Endpoint::parse(&format!("http://127.0.0.1:{port}/"))
        .expect("bind 先の endpoint は正規化できる");
    let access = token::random_token();
    StateDir::under_state_home(state)
        .auth()
        .update::<AuthFile, _, _>(|file| {
            file.mint_family(
                &endpoint,
                "e2e-1",
                "fam-e2e".to_string(),
                access.clone(),
                token::random_token(),
                hyoui::time::now_unix_ms(),
            );
        })
        .expect("登録 fixture を置く");
    access
}

fn spawn_detached(runtime: &Path, state: &Path, sid: &str) {
    spawn_detached_command(
        runtime,
        state,
        sid,
        &[],
        // stdin を line 単位で echo back。POST /input の text が visible に反映される。
        "while IFS= read -r line; do echo \"$line\"; done",
    );
}

fn spawn_detached_command(
    runtime: &Path,
    state: &Path,
    sid: &str,
    run_options: &[&str],
    command: &str,
) {
    let mut args = vec![
        "run".to_string(),
        "--detached".to_string(),
        format!("--session={sid}"),
    ];
    args.extend(run_options.iter().map(|arg| (*arg).to_string()));
    args.extend(["--", "sh", "-c"].map(str::to_string));
    args.push(command.to_string());

    let status = Command::new(hyoui_bin())
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", state)
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
fn spawn_web(runtime: &Path, state: &Path) -> (Child, Api) {
    let mut child = Command::new(hyoui_bin())
        .args(["web", "--listen=127.0.0.1:0"])
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_STATE_HOME", state)
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
            // 認証は常に有効なので (決定 9)、port が決まった時点で fixture を置く。
            let token = seed_credential(state, port);
            return (child, Api { port, token });
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

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<(&str, &[u8])>,
    token: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some(token) = token {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
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
fn e2e_screen_both_preserves_alternate_screen_mode() {
    let runtime = runtime_dir();
    let state = state_home();
    let sid = "web-e2e-alt-screen";
    spawn_detached_command(
        runtime.path(),
        state.path(),
        sid,
        &["--size=80x24"],
        "printf '\\033[?1049h\\033[2J\\033[HWEB-ALT-PROBE'; exec sleep 60",
    );
    let (mut web, api) = spawn_web(runtime.path(), state.path());
    let panic_guard = ChildGuard(&mut web);

    let deadline = Instant::now() + Duration::from_secs(5);
    let response = loop {
        let response = api.request(
            "GET",
            &format!("/api/sessions/{sid}/screen?layer=both"),
            None,
        );
        if response.status == 200
            && response
                .body
                .windows(b"WEB-ALT-PROBE".len())
                .any(|window| window == b"WEB-ALT-PROBE")
        {
            break response;
        }
        assert!(
            Instant::now() < deadline,
            "alt screen marker did not arrive"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(
        response.body.starts_with(b"\x1b[?1049h"),
        "both-layer response must restore alternate screen mode, got prefix: {:?}",
        &response.body[..response.body.len().min(16)]
    );

    drop(panic_guard);
    cleanup(runtime.path(), sid);
}

#[test]
fn e2e_screen_layer_query_selects_visible_scrollback_or_both() {
    let runtime = runtime_dir();
    let state = state_home();
    let sid = "web-e2e-screen-layer";
    spawn_detached_command(
        runtime.path(),
        state.path(),
        sid,
        &["--size=80x10", "--scrollback-rows=100"],
        "i=1; while [ $i -le 40 ]; do printf 'WEB-HISTORY-%03d\\n' $i; i=$((i+1)); done; printf 'WEB-VISIBLE-END\\n'; exec sleep 60",
    );
    let (mut web, api) = spawn_web(runtime.path(), state.path());
    let panic_guard = ChildGuard(&mut web);

    // query 省略時は既存 API と同じ visible layer。web UI は full reset 時に
    // `layer=both` を明示し、他の API caller は必要な範囲を選べる。
    let deadline = Instant::now() + Duration::from_secs(5);
    let visible = loop {
        let response = api.request("GET", &format!("/api/sessions/{sid}/screen"), None);
        if response.status == 200 && response.body.windows(15).any(|w| w == b"WEB-VISIBLE-END") {
            break response;
        }
        assert!(Instant::now() < deadline, "visible marker did not arrive");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        !visible.body.windows(15).any(|w| w == b"WEB-HISTORY-001"),
        "default visible layer must not contain old scrollback"
    );

    // both は daemon の rows-based ring と現 viewport を連結する。web 初期表示は
    // この layer を選び、xterm.js が古い行を scrollback として復元できる。
    let both = api.request(
        "GET",
        &format!("/api/sessions/{sid}/screen?layer=both"),
        None,
    );
    assert_eq!(both.status, 200);
    assert!(both.body.windows(15).any(|w| w == b"WEB-HISTORY-001"));
    assert!(both.body.windows(15).any(|w| w == b"WEB-VISIBLE-END"));
    assert!(both.body.len() > visible.body.len());

    // scrollback layer は過去行だけを返し、現在の viewport は混ぜない。
    let scrollback = api.request(
        "GET",
        &format!("/api/sessions/{sid}/screen?layer=scrollback"),
        None,
    );
    assert_eq!(scrollback.status, 200);
    assert!(scrollback.body.windows(15).any(|w| w == b"WEB-HISTORY-001"));
    assert!(
        !scrollback.body.windows(15).any(|w| w == b"WEB-VISIBLE-END"),
        "scrollback-only response must exclude the visible viewport"
    );

    // 未知の layer は visible へ黙って fallback せず 400 にする。typo で初期履歴を
    // 欠落させても成功扱いになる状態を避けるため。
    let invalid = api.request(
        "GET",
        &format!("/api/sessions/{sid}/screen?layer=unknown"),
        None,
    );
    assert_eq!(invalid.status, 400);

    drop(panic_guard);
    cleanup(runtime.path(), sid);
}

#[test]
fn e2e_sessions_screen_input() {
    let runtime = runtime_dir();
    let state = state_home();
    let sid = "web-e2e-1";

    spawn_detached(runtime.path(), state.path(), sid);
    let (mut web, api) = spawn_web(runtime.path(), state.path());

    let panic_guard = ChildGuard(&mut web);

    // 1. GET /api/sessions
    let r = api.request("GET", "/api/sessions", None);
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
    let r = api.request("GET", &format!("/api/sessions/{sid}/screen"), None);
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
    let r = api.request(
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
        let r = api.request("GET", &format!("/api/sessions/{sid}/screen"), None);
        if r.status == 200 && r.body.windows(5).any(|w| w == b"HELLO") {
            got_hello = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(got_hello, "input 後の screen dump に 'HELLO' が現れない");

    // 4. 未知 session_id → 404
    let r = api.request("GET", "/api/sessions/no-such-xyz/screen", None);
    assert_eq!(r.status, 404);

    // 5. 不正 spec → 400
    let bad = serde_json::to_vec(&serde_json::json!({"specs": ["notaknownprefix:xx"]})).unwrap();
    let r = api.request(
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
    let state = state_home();
    let sid = "web-e2e-lock-2";

    spawn_detached(runtime.path(), state.path(), sid);
    let (mut web, api) = spawn_web(runtime.path(), state.path());
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
    let r = api.request(
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
    let r = api.request("GET", &format!("/api/sessions/{sid}/screen"), None);
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
        let r = api.request(
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
    let state = state_home();
    let sid = "web-e2e-resize";

    spawn_detached(runtime.path(), state.path(), sid);
    let (mut web, api) = spawn_web(runtime.path(), state.path());
    let panic_guard = ChildGuard(&mut web);

    // 未知 session → 404
    let body = serde_json::to_vec(&serde_json::json!({"cols": 100u16, "rows": 30u16})).unwrap();
    let r = api.request(
        "POST",
        "/api/sessions/no-such-xyz/resize",
        Some(("application/json", &body)),
    );
    assert_eq!(r.status, 404, "unknown session must 404");

    // cols=0 → 400
    let bad = serde_json::to_vec(&serde_json::json!({"cols": 0u16, "rows": 30u16})).unwrap();
    let r = api.request(
        "POST",
        &format!("/api/sessions/{sid}/resize"),
        Some(("application/json", &bad)),
    );
    assert_eq!(r.status, 400, "cols=0 must 400");

    // valid → 204
    let ok = serde_json::to_vec(&serde_json::json!({"cols": 123u16, "rows": 37u16})).unwrap();
    let r = api.request(
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
    let state = state_home();
    let sid = "web-e2e-wsattach";

    spawn_detached(runtime.path(), state.path(), sid);
    let (mut web, api) = spawn_web(runtime.path(), state.path());
    let port = api.port;
    let panic_guard = ChildGuard(&mut web);

    // upgrade 要求の組み立て。token は subprotocol で運ぶ (DR-0036 決定 5)。
    let upgrade_request = |path: &str, protocol: Option<String>| {
        let builder = Request::builder()
            .uri(format!("ws://127.0.0.1:{port}{path}"))
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
        let builder = match protocol {
            Some(protocol) => builder.header("Sec-WebSocket-Protocol", protocol),
            None => builder,
        };
        builder.body(()).unwrap()
    };
    // 失敗は文字列に畳む (= handshake の Err 型は大きく、Result に載せると
    // 呼び出し側の戻り値が膨らむ)。
    let connect = |request, timeout| {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
        stream.set_read_timeout(Some(timeout)).unwrap();
        client(request, stream).map_err(|e| e.to_string())
    };

    // 未知 session の WS upgrade は handshake 前に落ちる (= tungstenite が Err)。
    assert!(
        connect(
            upgrade_request(
                "/api/sessions/no-such-xyz-ws/attach",
                Some(api.ws_protocol())
            ),
            Duration::from_secs(5),
        )
        .is_err(),
        "unknown session の WS handshake は失敗すべき"
    );

    // token を提示しない WS attach は 401 で弾かれる (決定 1)。
    assert!(
        connect(
            upgrade_request(&format!("/api/sessions/{sid}/attach"), None),
            Duration::from_secs(5),
        )
        .is_err(),
        "認証なしの WS attach は 401 で落ちるべき (決定 1)"
    );

    // 正常 session の WS attach。**選んだ subprotocol はそのまま echo される** (決定 5)。
    let (mut ws, response) = connect(
        upgrade_request(
            &format!("/api/sessions/{sid}/attach"),
            Some(api.ws_protocol()),
        ),
        Duration::from_secs(10),
    )
    .expect("ws handshake");
    assert_eq!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok()),
        Some(api.ws_protocol().as_str()),
        "server は選んだ subprotocol を echo する (決定 5)"
    );

    // DR-0035 決定 3: 最初の text frame は hello で、attach.info より前に来る。
    // browser は再接続のたびにこれを見て世代を比べるので、順序が契約である。
    // caps は daemon と intersect 済みの集合 (決定 4)。
    let first_text = loop {
        match ws.read().expect("WS first frame") {
            Message::Text(text) => {
                break serde_json::from_str::<serde_json::Value>(text.as_str())
                    .expect("WS control frame JSON");
            }
            // binary (= 接続時点の redraw) は順序の対象外。
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => panic!("WS closed before hello"),
        }
    };
    assert_eq!(
        first_text["kind"], "hello",
        "最初の text frame は hello: {first_text}"
    );
    assert_eq!(
        first_text["protocol"],
        serde_json::json!(hyoui_web::contract::WEB_PROTOCOL_VERSION),
        "hello の protocol: {first_text}"
    );
    assert!(
        first_text["caps"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|cap| cap == "data")),
        "hello の caps は intersect 済みの集合: {first_text}"
    );
    // access の期限が載る (DR-0036 決定 5)。browser は残り寿命の 90% 時点で
    // `/auth/refresh` を打ち、得た access を同一接続の `auth.extend` で提示する。
    // **認証は常に有効なので null にならない** (決定 9)。
    assert!(
        first_text["auth_expires_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z')),
        "hello の auth_expires_at: {first_text}"
    );

    // 同一接続で認証の期限を延ばす (決定 5)。**失効を確立済み接続に反映する
    // 唯一の点でもある** (決定 4)。
    ws.send(Message::Text(
        serde_json::json!({
            "kind": "auth.extend",
            "requestId": 7u64,
            "accessToken": api.token,
        })
        .to_string()
        .into(),
    ))
    .expect("auth.extend send");
    let deadline = Instant::now() + Duration::from_secs(5);
    let extend_ack = loop {
        assert!(Instant::now() < deadline, "auth.extend.result timeout");
        match ws.read() {
            Ok(Message::Text(text)) => {
                let value: serde_json::Value =
                    serde_json::from_str(text.as_str()).expect("WS control response JSON");
                if value["kind"] == "auth.extend.result" {
                    break value;
                }
            }
            Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => panic!("WS closed before auth.extend.result"),
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("auth.extend read error: {e}"),
        }
    };
    assert_eq!(extend_ack["ok"], true, "auth.extend ack={extend_ack}");
    assert_eq!(extend_ack["requestId"], 7u64);
    assert!(
        extend_ack["expires_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z')),
        "延長後の期限が返る: {extend_ack}"
    );

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
    let response = api.request(
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
    // error は {code, message} の 1 型 (DR-0035 決定 2)。
    assert_eq!(
        zero_ack["error"]["code"], "invalid-request",
        "zero resize error={zero_ack}"
    );
    assert!(
        zero_ack["error"]["message"]
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

    // 失効させると、**次の `auth.extend` でこの接続が切れる** (決定 4 の
    // 「失効はいつ効くか」)。CLI は gateway に通知しないので、確立済み接続に
    // 失効が反映される点はここだけである。
    hyoui_web::auth::StateDir::under_state_home(state.path())
        .auth()
        .update::<hyoui_web::auth::AuthFile, _, _>(|file| {
            file.tombstone_sub("e2e-1", hyoui::time::now_unix_ms());
        })
        .expect("family を tombstone にする");
    ws.send(Message::Text(
        serde_json::json!({
            "kind": "auth.extend",
            "requestId": 8u64,
            "accessToken": api.token,
        })
        .to_string()
        .into(),
    ))
    .expect("auth.extend send after revocation");
    let deadline = Instant::now() + Duration::from_secs(5);
    let revoked_ack = loop {
        assert!(
            Instant::now() < deadline,
            "失効後の auth.extend.result timeout"
        );
        match ws.read() {
            Ok(Message::Text(text)) => {
                let value: serde_json::Value =
                    serde_json::from_str(text.as_str()).expect("WS control response JSON");
                if value["kind"] == "auth.extend.result" && value["requestId"] == 8u64 {
                    break value;
                }
            }
            Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => panic!("応答より先に閉じた (= 理由が読めない)"),
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("失効後の auth.extend read error: {e}"),
        }
    };
    assert_eq!(revoked_ack["ok"], false, "失効: {revoked_ack}");
    assert_eq!(revoked_ack["error"]["code"], "auth-failed");
    // 応答の後に閉じる (= 理由を送ってから切る)。
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut closed = false;
    while Instant::now() < deadline {
        match ws.read() {
            Ok(Message::Close(_)) => {
                closed = true;
                break;
            }
            Ok(_) => {}
            // 相手が閉じた後の read は protocol error / io error になりうる。
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => {
                closed = true;
                break;
            }
        }
    }
    assert!(closed, "失効を返した接続は閉じる (決定 4)");

    // client → daemon の明示 Close はここでは不要 (= gateway が既に閉じた)。
    // daemon 側 attach の cleanup は socket 切断に任せる。

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
