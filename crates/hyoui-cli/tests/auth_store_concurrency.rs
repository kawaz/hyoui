//! 認証 state を **本物の複数プロセス** で共有する (DR-0036 決定 4 / W2-4 の gate)。
//!
//! `crates/hyoui-web/src/auth/store.rs` の unit test は同一プロセス内の thread で
//! lock が効くことを見る。ここで見るのはその外側 — 2 つの unit と CLI が別プロセス
//! として同じ file を書いた時に、
//!
//! - **read-modify-write が落ちない** (= `passkey add` を並行に打っても登録が消えない)
//! - **6 桁コードの試行回数が 2 プロセス合計で数えられる** (= 総当たりの回数上限が
//!   unit の数だけ緩まない)
//! - **片方の unit で登録した credential で、もう片方の unit の認証が通る**
//!   (= HA endpoint の fallback で再認証を求められない)
//!
//! の 3 つ。どれも `$XDG_STATE_HOME` を tempdir に隔離して回す (決定 9)。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hyoui_web::auth::token;
use hyoui_web::auth::{AuthFile, PendingFile, PendingRegistration, REGISTRATION_TTL_MS, StateDir};
use hyoui_web::contract::Endpoint;

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

fn state_home() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("hyoui-auth-concurrency-")
        .tempdir()
        .expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

/// `hyoui web` を 1 台起こし、bind した port を返す。
///
/// 2 台に同じ `XDG_STATE_HOME` を渡すのがこの test の主眼である (= 2 unit が
/// 同一ホストで同じ file を読む、決定 4)。
///
/// 返した `Child` は呼び出し側が `UnitGuard` に包んで kill + wait する
/// (= panic path でも zombie を残さない)。
#[allow(clippy::zombie_processes)]
fn spawn_unit(state: &Path) -> (Child, u16) {
    let mut child = Command::new(hyoui_bin())
        .args(["web", "--listen=127.0.0.1:0"])
        .env("XDG_STATE_HOME", state)
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
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
        let read = reader.read_line(&mut line).expect("read stderr");
        assert!(read > 0, "hyoui web が port を出す前に stderr を閉じた");
        if let Some(host_port) = line.trim().strip_prefix("hyoui web: listening on http://")
            && let Some(port) = host_port.rsplit(':').next().and_then(|v| v.parse().ok())
        {
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = reader.into_inner().read_to_end(&mut sink);
            });
            return (child, port);
        }
    }
    let _ = child.kill();
    panic!("hyoui web の listening 行が来ない");
}

/// 素朴な HTTP/1.1 request (= 依存を増やさない、web_e2e_api.rs と同じ理由)。
fn post_json(
    port: u16,
    path: &str,
    body: &serde_json::Value,
    bearer: Option<&str>,
) -> (u16, Vec<u8>) {
    let payload = serde_json::to_vec(body).unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        payload.len()
    );
    if let Some(bearer) = bearer {
        request.push_str(&format!("Authorization: Bearer {bearer}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(&payload).unwrap();
    read_response(stream)
}

fn get(port: u16, path: &str, bearer: Option<&str>) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some(bearer) = bearer {
        request.push_str(&format!("Authorization: Bearer {bearer}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    read_response(stream)
}

fn read_response(mut stream: TcpStream) -> (u16, Vec<u8>) {
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).expect("read_to_end");
    let split = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header/body separator");
    let head = std::str::from_utf8(&buffer[..split]).expect("utf-8 headers");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .expect("status code");
    (status, buffer[split + 4..].to_vec())
}

/// panic path でも unit を確実に落とす。
struct UnitGuard(Child);

impl Drop for UnitGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 別プロセスの `passkey add` を並行に打っても登録が落ちない (決定 4)。
#[test]
fn concurrent_passkey_add_processes_do_not_lose_registrations() {
    let state = state_home();
    const WRITERS: usize = 6;

    let children: Vec<Child> = (0..WRITERS)
        .map(|index| {
            Command::new(hyoui_bin())
                .args([
                    "web",
                    "passkey",
                    "add",
                    // endpoint を散らす: key の 1 段が endpoint なので (決定 4)、
                    // 同じ file の別 bucket への同時書き込みになる。
                    &format!("--endpoint=https://hyoui-{index}.example.jp/"),
                ])
                .env("XDG_STATE_HOME", state.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn passkey add")
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().expect("wait passkey add");
        assert!(
            output.status.success(),
            "passkey add が失敗した: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let pending: PendingFile = StateDir::under_state_home(state.path())
        .pending()
        .read()
        .expect("pending を読む");
    let registrations: usize = pending
        .registrations
        .values()
        .map(|by_jti| by_jti.len())
        .sum();
    assert_eq!(
        registrations,
        WRITERS,
        "lock 下の read-modify-write なので登録は落ちない (決定 4): {:?}",
        pending.registrations.keys().collect::<Vec<_>>()
    );
}

/// 6 桁コードの試行回数が 2 unit **合計** で数えられる (決定 4)。
///
/// lock の外で数えると、2 unit に来た要求で数え落とし、総当たりの回数上限が
/// unit の数だけ緩む。
#[test]
fn wrong_code_attempts_are_counted_across_two_units() {
    let state = state_home();
    let dir = StateDir::under_state_home(state.path());
    let endpoint = Endpoint::parse("https://hyoui.example.jp/").unwrap();
    let now_ms = hyoui::time::now_unix_ms();
    let secret = token::random_secret();
    let jwt = token::encode_registration(
        &token::RegistrationClaims {
            sub: "hyoui.example.jp-1".to_string(),
            endpoint: endpoint.clone(),
            rp_id: endpoint.rp_id().to_string(),
            user_id: token::base64url(&[7u8; 16]),
            access: Default::default(),
            exp: now_ms / 1000 + 600,
            jti: "j1".to_string(),
        },
        &secret,
    );
    dir.pending()
        .update::<PendingFile, _, _>(|pending| {
            pending
                .registrations
                .entry(endpoint.clone())
                .or_default()
                .insert(
                    "j1".to_string(),
                    PendingRegistration {
                        jti: "j1".to_string(),
                        sub: "hyoui.example.jp-1".to_string(),
                        user_id: vec![7; 16],
                        access: Default::default(),
                        hmac_secret: secret,
                        code_hash: token::code_hash("123456"),
                        code_attempts: 0,
                        issued_label: None,
                        expires_at_ms: now_ms + REGISTRATION_TTL_MS,
                    },
                );
        })
        .expect("登録を置く");

    let (first, first_port) = spawn_unit(state.path());
    let (second, second_port) = spawn_unit(state.path());
    let _first = UnitGuard(first);
    let _second = UnitGuard(second);

    let body = serde_json::json!({
        "endpoint": endpoint.as_str(),
        "challenge_id": "c-missing",
        "jwt": jwt,
        "code": "000000",
        "credential": {},
    });
    // 別 unit に 1 回ずつ誤ったコードを投げる。
    for port in [first_port, second_port] {
        let (status, _) = post_json(port, "/auth/register", &body, None);
        assert_eq!(status, 401, "誤ったコードは 401 (port={port})");
    }

    let pending: PendingFile = dir.pending().read().expect("pending を読む");
    let registration = pending
        .registrations
        .get(&endpoint)
        .and_then(|by_jti| by_jti.get("j1"))
        .expect("まだ焼けていない");
    assert_eq!(
        registration.code_attempts, 2,
        "試行回数は 2 プロセス合計で数える (決定 4)"
    );
}

/// 片方の unit で置いた認証セッションで、もう片方の unit の認証が通る (決定 4)。
///
/// **HA endpoint の fallback で再認証を求められない**ことがこの共有の目的である。
/// record は unit を跨いで同じ file から引け、検証に使うのは record の値だけで、
/// どの unit が受けたかは関係しない (決定 3)。
#[test]
fn a_session_minted_once_is_accepted_by_either_unit() {
    let state = state_home();
    let dir = StateDir::under_state_home(state.path());
    // HA endpoint 宛の登録 1 本。
    let endpoint = Endpoint::parse("https://hyoui.example.jp/").unwrap();
    let access = token::random_token();
    dir.auth()
        .update::<AuthFile, _, _>(|file| {
            file.mint_family(
                &endpoint,
                "hyoui.example.jp-1",
                "fam-ha".to_string(),
                access.clone(),
                token::random_token(),
                hyoui::time::now_unix_ms(),
            );
        })
        .expect("family を置く");

    let (first, first_port) = spawn_unit(state.path());
    let (second, second_port) = spawn_unit(state.path());
    let _first = UnitGuard(first);
    let _second = UnitGuard(second);

    for port in [first_port, second_port] {
        let (status, _) = get(port, "/api/sessions", Some(&access));
        assert_eq!(
            status, 200,
            "同じ file を読む unit なのでどちらでも通る (port={port})"
        );
    }

    // `passkey remove` 相当で畳むと、**両方の unit** で落ちる (= 失効も共有される)。
    dir.auth()
        .update::<AuthFile, _, _>(|file| {
            file.tombstone_sub("hyoui.example.jp-1", hyoui::time::now_unix_ms());
        })
        .expect("tombstone");
    for port in [first_port, second_port] {
        let (status, _) = get(port, "/api/sessions", Some(&access));
        assert_eq!(
            status, 401,
            "family の検証は cache を使わず毎回 file を読む (決定 4、port={port})"
        );
    }
}
