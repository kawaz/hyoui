//! hyoui web gateway (DR-0027 Phase 1)。
//!
//! `crates/hyoui` の blocking `ClientConnection` / `discovery` / `input_bytes` /
//! `cli::parse_input_spec` を axum ハンドラから `spawn_blocking` 経由で呼び、
//! HTTP API 3 endpoint を提供する。
//!
//! ## Endpoints
//!
//! - `GET  /api/sessions` — 全 namespace 横断で live/stale session を JSON list で返す
//! - `GET  /api/sessions/:id/screen` — `screen.dump.request` の ANSI payload を
//!   `text/plain; charset=utf-8` で返す
//! - `POST /api/sessions/:id/input` — body `{"specs": ["text:...", "key:Enter"]}`
//!   を hyoui input と同じ流儀で送信 (= `parse_input_spec` → `input_bytes` → raw_data frame)
//!
//! ## namespace の扱い (Phase 1)
//!
//! path param `:id` は **session_id 名のみ**を受け付ける。全 namespace を横断走査して
//! 同名 session を探す (= `discovery::list_sessions` が返した中から最初の match を採用)。
//! 名前衝突が起きる運用は現状想定していない (= namespace は運用グループ分離目的、
//! 同名 session を複数 namespace で並走させる事故は list JSON で発覚する)。将来
//! `?namespace=X` query の受理は必要になった段階で追加する。

#![warn(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use include_dir::{Dir, include_dir};

pub use axum;

/// リリースビルドに埋め込む静的アセット (= `crates/hyoui-web/assets/`)。
///
/// 開発モードで `--web-assets-dir <path>` (or config `[web].assets_dir`) を
/// 指定すると、ここではなくローカルディレクトリを都度読む (= DR-0027 §4)。
static EMBEDDED_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

/// axum の shared state。
///
/// `Config` は `hyoui::config::Config` の owned copy を持ち回る (= Arc は不要な size、
/// でも handler 内で clone 増やしすぎない用に Arc に包む)。
#[derive(Clone)]
struct AppState {
    #[allow(dead_code)]
    // 将来 `[web]` セクションから rate limit / auth を読む余地。現時点では未使用。
    config: Arc<hyoui::config::Config>,
    /// 開発モードで指定されたローカル assets ディレクトリ (= `--web-assets-dir` /
    /// config `[web].assets_dir`)。`Some` の時は都度ファイル読み込み、`None` なら
    /// `EMBEDDED_ASSETS` から返す。
    assets_dir: Option<Arc<PathBuf>>,
}

/// axum Router を返す (= test / bin 側で `axum::serve` に渡すか
/// `tower::ServiceExt::oneshot` で直接叩ける)。
///
/// `assets_dir` が `Some` なら開発モード: `/assets/*` と HTML page 群を
/// そのディレクトリから都度読む。`None` なら埋め込みアセットを返す。
pub fn router(config: hyoui::config::Config, assets_dir: Option<PathBuf>) -> Router {
    let state = AppState {
        config: Arc::new(config),
        assets_dir: assets_dir.map(Arc::new),
    };
    Router::new()
        .route("/", get(get_index_page))
        .route("/sessions/{id}", get(get_session_page))
        .route("/assets/{*path}", get(get_asset))
        .route("/api/sessions", get(get_sessions))
        .route("/api/sessions/{id}/screen", get(get_screen))
        .route("/api/sessions/{id}/input", post(post_input))
        .route("/api/sessions/{id}/resume", post(post_resume))
        .with_state(state)
}

/// listen アドレスに bind して axum server を回す。
///
/// `hyoui web` subcommand から呼ばれる本体。Ctrl+C で graceful shutdown。
///
/// **iframe 埋め込み方針** (= ccmsg webui の Terminal タブ等が `?embed=1` で iframe に
/// 貼るユースケース): `X-Frame-Options` / `Content-Security-Policy: frame-ancestors`
/// を意図的に付けない。tailnet 内利用を前提とした DR-0027 の scope で、外部からの
/// clickjacking / CSRF リスクは network 側 (= tailnet ACL) で担保している。もし
/// public network で serve する運用が出てきたら、その時点で reverse proxy 側で
/// header を付けるか、`[web]` config に flag を追加する (= 現時点では yagni)。
pub async fn serve(
    listen: &str,
    config: hyoui::config::Config,
    assets_dir: Option<PathBuf>,
) -> std::io::Result<()> {
    let app = router(config, assets_dir);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("hyoui web: listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

// -----------------------------------------------------------------------------
// GET /api/sessions
// -----------------------------------------------------------------------------

/// live/stale session 一覧を JSON で返す (= `hyoui list --format=jsonl` の JSON 配列版)。
async fn get_sessions() -> Response {
    let entries = match tokio::task::spawn_blocking(hyoui::discovery::list_sessions).await {
        Ok(v) => v,
        Err(e) => return internal_error(format!("session enumeration join error: {e}")),
    };
    let json: Vec<serde_json::Value> = entries.iter().map(session_entry_to_json).collect();
    axum::Json(json).into_response()
}

fn session_entry_to_json(e: &hyoui::discovery::SessionEntry) -> serde_json::Value {
    use hyoui::discovery::SessionStatus;
    match &e.status {
        SessionStatus::Live(info) => serde_json::json!({
            "session_id": e.session_id,
            "namespace": e.namespace,
            "socket_path": e.socket_path.display().to_string(),
            "started_unix_ms": e.started_unix_ms,
            "status": if info.child_stopped { "stopped" } else { "live" },
            "cwd": info.cwd,
            "argv": info.argv,
            "clients": info.clients,
            "child_pid": info.child_pid,
            "child_pgid": info.child_pgid,
            "child_stopped": info.child_stopped,
            "on_child_suspend": info.on_child_suspend.map(|p| p.as_str()),
            "daemon_version": if info.daemon_version.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(info.daemon_version.clone())
            },
        }),
        SessionStatus::Stale { reason } => serde_json::json!({
            "session_id": e.session_id,
            "namespace": e.namespace,
            "socket_path": e.socket_path.display().to_string(),
            "started_unix_ms": e.started_unix_ms,
            "status": "stale",
            "reason": reason,
        }),
    }
}

// -----------------------------------------------------------------------------
// GET /api/sessions/:id/screen
// -----------------------------------------------------------------------------

/// `screen.dump.request` の ANSI payload を `text/plain; charset=utf-8` で返す。
async fn get_screen(Path(id): Path<String>, State(_state): State<AppState>) -> Response {
    let sock = match resolve_socket(&id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let bytes = match tokio::task::spawn_blocking(move || dump_screen_blocking(&sock)).await {
        Ok(Ok(b)) => b,
        Ok(Err(msg)) => return internal_error(format!("screen dump failed: {msg}")),
        Err(e) => return internal_error(format!("screen dump join error: {e}")),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        bytes,
    )
        .into_response()
}

fn dump_screen_blocking(socket_path: &std::path::Path) -> Result<Vec<u8>, String> {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::messages::{ScreenDumpFormat, ScreenDumpLayer, ScreenDumpRequest};
    use hyoui::protocol::{ControlMessage, MVP_CAPS, Mode};

    let opts = AttachOptions {
        mode: Mode::Ro,
        caps: MVP_CAPS.iter().map(|s| (*s).to_string()).collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = ClientConnection::connect(socket_path, opts)
        .map_err(|e| format!("connect/handshake: {e}"))?;
    let req = ScreenDumpRequest {
        format: ScreenDumpFormat::Ansi,
        layer: ScreenDumpLayer::Visible,
        rect: None,
        serial: Some(1),
    };
    conn.send_control(&ControlMessage::ScreenDumpRequest(req))
        .map_err(|e| format!("send screen.dump.request: {e}"))?;
    loop {
        match conn.recv_control(None).map_err(|e| format!("recv: {e}"))? {
            ControlMessage::ScreenDumpResponse(resp) => return Ok(resp.payload),
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            ControlMessage::Error(e) => {
                return Err(format!("daemon error: {:?} ({})", e.code, e.message));
            }
            other => {
                return Err(format!(
                    "unexpected response kind: {:?}",
                    std::mem::discriminant(&other)
                ));
            }
        }
    }
}

// -----------------------------------------------------------------------------
// POST /api/sessions/:id/input
// -----------------------------------------------------------------------------

/// input request body: `{"specs": ["text:hello", "key:Enter"]}`。
#[derive(Debug, serde::Deserialize)]
struct InputRequest {
    specs: Vec<String>,
}

/// input 送信結果 (= 成功時: 送った bytes 総数、失敗時: どの spec が失敗したか)。
#[derive(Debug, serde::Serialize)]
struct InputResponse {
    sent_bytes: usize,
    specs: usize,
}

/// `send_input_blocking` の失敗分類。HTTP status code へのマップに使う。
///
/// DR-0021 (raw_ack) / DR-0022 (auto-lock) に沿って原因種別を保持する。
#[derive(Debug)]
enum InputError {
    /// daemon socket 接続 / handshake 失敗。500。
    Connect(String),
    /// DR-0022 auto-lock 取得失敗 (= 他 client が保持中で timeout)。409 Conflict。
    LockContention {
        /// message: どの timeout で失敗したかを含む。
        message: String,
    },
    /// DR-0022 auto-lock 取得失敗 (= daemon 応答異常 / protocol / I/O)。503。
    LockUnavailable {
        /// daemon 状態を含む message。
        message: String,
    },
    /// DR-0021 raw-ack 経路の失敗、または送信 I/O 失敗。500。
    /// どの spec (0-index) で起きたかを保持する。
    SpecSend {
        /// specs[] の 0-index。
        spec_index: usize,
        /// エラー文言。raw-ack timeout / daemon error / I/O 混在。
        message: String,
    },
}

impl InputError {
    fn into_response(self) -> Response {
        match self {
            InputError::Connect(msg) => internal_error(format!("input send failed: {msg}")),
            InputError::LockContention { message } => (
                StatusCode::CONFLICT,
                format!("他 client が入力中のため lock を取得できません: {message}"),
            )
                .into_response(),
            InputError::LockUnavailable { message } => (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("auto-lock 取得に失敗しました: {message}"),
            )
                .into_response(),
            InputError::SpecSend {
                spec_index,
                message,
            } => internal_error(format!(
                "input send failed at specs[{spec_index}]: {message}"
            )),
        }
    }
}

async fn post_input(
    Path(id): Path<String>,
    State(_state): State<AppState>,
    axum::Json(body): axum::Json<InputRequest>,
) -> Response {
    // Phase 1: file: / wait: / wait-idle: は非サポート (network 経由で server 側の file を
    // 読む経路 = セキュリティ懸念、wait 系は long-poll になり client の HTTP timeout と
    // 相性が悪い)。text: / hex: / key: / paste: のみ受理。
    let parsed = match parse_specs(&body.specs) {
        Ok(v) => v,
        Err(msg) => return bad_request(msg),
    };
    let sock = match resolve_socket(&id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let result = tokio::task::spawn_blocking(move || send_input_blocking(&sock, &parsed)).await;
    match result {
        Ok(Ok(sent_bytes)) => axum::Json(InputResponse {
            sent_bytes,
            specs: body.specs.len(),
        })
        .into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(e) => internal_error(format!("input join error: {e}")),
    }
}

/// spec 文字列列を bytes ペイロード列に変換する (= parse + handler の合成)。
///
/// 失敗した spec の index を error message に含める (= どこで止まったかが分かる)。
fn parse_specs(specs: &[String]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::with_capacity(specs.len());
    for (idx, s) in specs.iter().enumerate() {
        let parsed = hyoui::cli::parse_input_spec(s)
            .map_err(|e| format!("specs[{idx}] parse error: {e}"))?;
        let bytes = spec_to_bytes(&parsed).map_err(|e| format!("specs[{idx}]: {e}"))?;
        out.push(bytes);
    }
    Ok(out)
}

/// `InputSpec` を bytes に落とす (= file: / wait: / wait-idle: は reject)。
fn spec_to_bytes(spec: &hyoui::cli::InputSpec) -> Result<Vec<u8>, String> {
    use hyoui::cli::InputSpec;
    use hyoui::input_bytes;
    match spec {
        InputSpec::Text(s) => Ok(input_bytes::handle_text(s)),
        InputSpec::Hex(b) => Ok(input_bytes::handle_hex(b)),
        InputSpec::Paste(s) => input_bytes::handle_paste(s),
        InputSpec::Key(name) => input_bytes::handle_key(name),
        InputSpec::File(_) => Err(
            "file: spec は web gateway では未サポート (= server 側 file 読み込み経路は Phase 1 では受け付けない)".to_string(),
        ),
        InputSpec::Wait(_) | InputSpec::WaitIdle(_) => Err(
            "wait / wait-idle spec は web gateway では未サポート (= long-poll 相当、Phase 1 対象外)".to_string(),
        ),
        _ => Err("unsupported InputSpec variant".to_string()),
    }
}

/// web gateway の auto-lock 取得 timeout (DR-0022)。CLI の 30s より短く 5s に
/// 設定して、HTTP client の応答待ちを長引かせない (= 「他 client が入力中」の
/// 状態を早く 409 で返すほうが web UI として都合が良い)。
const WEB_INPUT_AUTO_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn send_input_blocking(
    socket_path: &std::path::Path,
    payloads: &[Vec<u8>],
) -> Result<usize, InputError> {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::Mode;

    // DR-0022: `HYOUI_LOCK_TOKEN` env で外側 token を継承している場合は auto-acquire を
    // skip する (= 外側 wrapper が既に holder、内側で acquire/release すると外側の
    // lock を壊す)。CLI `hyoui input` と同じ挙動。
    let inherited_token = std::env::var("HYOUI_LOCK_TOKEN").ok();

    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: inherited_token.clone(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = ClientConnection::connect(socket_path, opts)
        .map_err(|e| InputError::Connect(format!("connect/handshake: {e}")))?;

    // DR-0022: invocation 全体で auto-lock を 1 本 acquire (= web POST 1 回 = 1
    // invocation 相当)。CLI `input_command` と同じ意味論。外側 token を継承していれば
    // skip。取得失敗は原因種別で 409 / 503 にマップする (= `InputError` variant で
    // 上位へ)。
    let owned_lock_token: Option<String> = if inherited_token.is_none() {
        match hyoui::client::acquire_auto_lock(&mut conn, WEB_INPUT_AUTO_LOCK_TIMEOUT) {
            Ok(tok) => Some(tok),
            Err(hyoui::client::AutoLockError::Timeout { .. }) => {
                return Err(InputError::LockContention {
                    message: format!("timeout ({} ms)", WEB_INPUT_AUTO_LOCK_TIMEOUT.as_millis()),
                });
            }
            Err(e) => {
                return Err(InputError::LockUnavailable {
                    message: e.to_string(),
                });
            }
        }
    } else {
        None
    };

    // 全 spec を順に送信。各 spec の送信は DR-0021 raw_ack で PTY drain 完了まで
    // 同期 block する (= `send_raw_bytes` 内部の `recv_raw_ack_inner` が待つ)。失敗は
    // どの spec で起きたかを保持して上位へ返す (= caller が 500 + spec index を
    // レスポンスに反映)。
    let send_result: Result<usize, InputError> = (|| {
        let mut sent = 0usize;
        for (idx, payload) in payloads.iter().enumerate() {
            conn.send_raw_bytes(payload)
                .map_err(|e| InputError::SpecSend {
                    spec_index: idx,
                    message: format!("send_raw_bytes: {e}"),
                })?;
            sent += payload.len();
        }
        Ok(sent)
    })();

    // DR-0022: 明示 release (= dispatch 成否に関わらず取った lock は必ず返す)。
    // 失敗しても daemon の process-bound GC が接続切断時に回収する。
    if let Some(tok) = owned_lock_token.as_ref()
        && let Err(e) = hyoui::client::release_auto_lock(&mut conn, tok)
    {
        eprintln!("hyoui-web: auto-lock release 失敗 (best-effort、daemon GC で回収): {e}");
    }

    send_result
}

// -----------------------------------------------------------------------------
// POST /api/sessions/:id/resume
// -----------------------------------------------------------------------------

/// stopped child に SIGCONT を送るための resume request を daemon に投げる
/// (= 既存 `SessionChildResumeRequest`、新 protocol message は追加しない)。
///
/// rw attach で connect し `send_child_resume()` を呼ぶ (= CLI `hyoui attach` の
/// reattach auto-resume 経路と同じ)。子が既に running でも daemon 側で SIGCONT が
/// 冪等に飛ぶので害はない (= running プロセスへの SIGCONT は no-op)。
async fn post_resume(Path(id): Path<String>, State(_state): State<AppState>) -> Response {
    let sock = match resolve_socket(&id).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let result = tokio::task::spawn_blocking(move || resume_child_blocking(&sock)).await;
    match result {
        Ok(Ok(())) => (StatusCode::NO_CONTENT, "").into_response(),
        Ok(Err(msg)) => internal_error(format!("resume failed: {msg}")),
        Err(e) => internal_error(format!("resume join error: {e}")),
    }
}

fn resume_child_blocking(socket_path: &std::path::Path) -> Result<(), String> {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::{MVP_CAPS, Mode};

    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: MVP_CAPS.iter().map(|s| (*s).to_string()).collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = ClientConnection::connect(socket_path, opts)
        .map_err(|e| format!("connect/handshake: {e}"))?;
    conn.send_child_resume()
        .map_err(|e| format!("send resume.request: {e}"))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// session_id から live socket path を解決する。live entry のみ受理 (= stale は 404)。
async fn resolve_socket(id: &str) -> Result<PathBuf, Response> {
    let id = id.to_string();
    let entries = match tokio::task::spawn_blocking(hyoui::discovery::list_sessions).await {
        Ok(v) => v,
        Err(e) => {
            return Err(internal_error(format!(
                "session enumeration join error: {e}"
            )));
        }
    };
    for e in entries {
        if e.session_id != id {
            continue;
        }
        match e.status {
            hyoui::discovery::SessionStatus::Live(_) => return Ok(e.socket_path),
            hyoui::discovery::SessionStatus::Stale { reason } => {
                return Err(not_found(format!("session {id:?} is stale: {reason}")));
            }
        }
    }
    Err(not_found(format!("no session named {id:?}")))
}

// -----------------------------------------------------------------------------
// HTML pages + static assets (DR-0027 Phase 2)
// -----------------------------------------------------------------------------

/// `GET /` — セッション一覧ページ (静的 HTML)。`/api/sessions` は client 側 JS が叩く。
async fn get_index_page(State(state): State<AppState>) -> Response {
    serve_asset(&state, "index.html").await
}

/// `GET /sessions/:id` — セッションページ。`:id` は client 側 JS が URL から抽出する。
/// server 側では session の存在確認をせず HTML を返す (= 存在チェックは JS が
/// `/api/sessions/:id/screen` の 404 で扱う。ページ自体は 200 で返す方が
/// bookmarkable で扱いやすい)。
async fn get_session_page(Path(_id): Path<String>, State(state): State<AppState>) -> Response {
    serve_asset(&state, "session.html").await
}

/// `GET /assets/*path` — 静的アセット配信。
///
/// `path` は `..` を含めない (= axum の path parser が normalize するが、
/// 念のため component 単位で reject する)。埋め込み or ローカル dir から読む。
async fn get_asset(Path(path): Path<String>, State(state): State<AppState>) -> Response {
    // Reject any traversal component. `axum::extract::Path` decodes %2E etc.
    if path.split('/').any(|c| c == ".." || c.is_empty()) {
        return not_found(format!("invalid asset path: {path:?}"));
    }
    serve_asset(&state, &path).await
}

/// アセットを 1 個返す。開発モード (`assets_dir` set) では tokio::fs、
/// リリースでは `EMBEDDED_ASSETS` から。存在しない場合は 404。
async fn serve_asset(state: &AppState, rel: &str) -> Response {
    let ct = content_type_for(rel);
    if let Some(dir) = state.assets_dir.as_ref() {
        let full = dir.join(rel);
        match tokio::fs::read(&full).await {
            Ok(bytes) => (StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                not_found(format!("asset not found: {rel}"))
            }
            Err(e) => internal_error(format!("asset read error: {e}")),
        }
    } else {
        match EMBEDDED_ASSETS.get_file(rel) {
            Some(f) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, ct)],
                f.contents().to_vec(),
            )
                .into_response(),
            None => not_found(format!("asset not found: {rel}")),
        }
    }
}

/// 最小限の content-type 判定。UI で使う拡張子のみ扱う。
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "webmanifest" => "application/manifest+json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, msg.into()).into_response()
}

fn not_found(msg: impl Into<String>) -> Response {
    (StatusCode::NOT_FOUND, msg.into()).into_response()
}

fn internal_error(msg: impl Into<String>) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.into()).into_response()
}

#[cfg(test)]
mod tests {
    //! router 単体の smoke test (= daemon 不要)。
    //!
    //! 本物の daemon を巻き込む e2e は `crates/hyoui-cli/tests/web_e2e_api.rs` 側 —
    //! `CARGO_BIN_EXE_hyoui` は自 crate の bin target を指す env で cargo が
    //! 用意するため、bin target を持つ hyoui-cli test でしか取れない。

    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn parse_specs_rejects_file_and_wait() {
        // web gateway は file: / wait: を **spec_to_bytes** で reject する。
        // (= parse_input_spec 自体は成功する)。
        let text = hyoui::cli::parse_input_spec("text:x").unwrap();
        assert!(spec_to_bytes(&text).is_ok());

        let file = hyoui::cli::parse_input_spec("file:/etc/hosts").unwrap();
        let err = spec_to_bytes(&file).unwrap_err();
        assert!(err.contains("file:"), "err={err}");

        let wait = hyoui::cli::parse_input_spec("wait:foo").unwrap();
        let err = spec_to_bytes(&wait).unwrap_err();
        assert!(err.contains("wait"), "err={err}");
    }

    #[tokio::test]
    async fn missing_session_returns_404() {
        // daemon が居ない状態でも router は動く。unknown session_id は 404。
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions/no-such-session-xyz/screen")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sessions_endpoint_returns_array() {
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // env に何が居ようと Ok な JSON array であること (要素数は環境依存で assert しない)。
        assert!(json.is_array(), "expected array, got {json}");
    }

    #[tokio::test]
    async fn index_page_serves_html_with_xterm_ref() {
        // 埋め込みモードで / が index.html を返す (= session.html は xterm.js を参照)。
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/html"), "content-type={ct}");
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains("<title>"), "body missing title: {s}");
        assert!(s.contains("hyoui sessions"), "body missing h1: {s}");
    }

    #[tokio::test]
    async fn session_page_returns_html_and_references_xterm() {
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(
            s.contains("/assets/vendor/xterm.js"),
            "body missing xterm.js reference: {s}"
        );
    }

    #[tokio::test]
    async fn embedded_asset_xterm_js_served() {
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/assets/vendor/xterm.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("application/javascript"), "ct={ct}");
        let body = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
        assert!(body.len() > 1000, "xterm.js too small: {} B", body.len());
    }

    #[tokio::test]
    async fn asset_traversal_is_rejected() {
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/assets/../Cargo.toml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // axum の path parser が `..` を含む path を正規化する場合もあるが、
        // 少なくとも 200 で Cargo.toml が返ってはいけない。404 か 400 のいずれか。
        assert!(
            resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST,
            "unexpected status: {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn assets_dir_mode_reads_local_file() {
        // 開発モード (--web-assets-dir) の動作: 一時 dir に index.html を置いて、
        // router が埋め込みではなくその dir から読むことを確認。
        let tmp = tempfile::tempdir().unwrap();
        let custom_body = "<!doctype html><title>custom-index</title>DEVMODE";
        std::fs::write(tmp.path().join("index.html"), custom_body).unwrap();
        let app = router(
            hyoui::config::Config::default(),
            Some(tmp.path().to_path_buf()),
        );
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), custom_body);
    }

    #[tokio::test]
    async fn session_page_has_no_iframe_blocking_headers() {
        // iframe 埋め込み (?embed=1) を tailnet 前提で許容する方針の regression guard。
        // 将来 middleware で X-Frame-Options / frame-ancestors を default で付けたく
        // なった場合、この test が失敗して意思決定を強制する (= 気付かず制限が入るのを防ぐ)。
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/anything?embed=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("x-frame-options").is_none(),
            "X-Frame-Options should not be set (tailnet 前提)"
        );
        let csp = resp
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            !csp.to_ascii_lowercase().contains("frame-ancestors"),
            "CSP frame-ancestors should not be set: {csp}"
        );
    }

    #[tokio::test]
    async fn manifest_and_icon_served() {
        // PWA 用の manifest / icon が embedded 経路で正しい content-type で配信されること。
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/manifest.webmanifest")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.starts_with("application/manifest+json"),
            "manifest ct={ct}"
        );
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).expect("manifest is JSON");
        assert_eq!(json["name"], "hyoui web");
        assert_eq!(json["display"], "standalone");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/assets/icon.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(ct, "image/svg+xml", "icon ct={ct}");
    }

    #[tokio::test]
    async fn hackgen_font_served_as_woff2() {
        // vendored HackGen Console NF が embedded 経路で配信されて font/woff2 で返ること。
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/assets/vendor/fonts/HackGenConsoleNF-Regular.woff2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(ct, "font/woff2", "ct={ct}");
        let body = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.unwrap();
        // woff2 magic: "wOF2".
        assert!(
            body.starts_with(b"wOF2"),
            "not a woff2 (first 4 bytes = {:?})",
            &body[..4.min(body.len())]
        );
        assert!(body.len() > 100_000, "woff2 too small: {} B", body.len());
    }

    #[tokio::test]
    async fn resume_unknown_session_returns_404() {
        let app = router(hyoui::config::Config::default(), None);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/no-such-session-xyz/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn input_bad_json_returns_400() {
        let app = router(hyoui::config::Config::default(), None);
        // 空 body / 不正 JSON は axum::Json extractor が 400 に落とす。
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/anything/input")
                    .header("content-type", "application/json")
                    .body(Body::from(b"not a json".as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // axum の JSON extractor は 400 を返す。
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
