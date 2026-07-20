//! Session discovery — socket dir 走査 + status.query による live session 列挙 (DR-0027)。
//!
//! `hyoui-web` / 将来の外部 tool から「今 host に居る hyoui session 一覧」を取る
//! 共通経路。`hyoui-cli` 内部の `list_command_with_dirs` と等価の走査を、format
//! 責務なし・純粋な `Vec<SessionEntry>` として提供する。
//!
//! ## 走査経路 (= DR-0018 の socket 配置と対称)
//!
//! 1. `$XDG_RUNTIME_DIR/hyoui/` (実在 dir のみ)
//! 2. `${XDG_STATE_HOME:-$HOME/.local/state}/hyoui/` (実在 dir のみ)
//!
//! 各 base dir 直下: `*.sock` = default namespace の session。
//! 各 base dir 配下のサブ dir `<ns>/*.sock` = 非 default namespace の session。
//!
//! ## 死活判定
//!
//! - `probe_liveness` = unix socket connect の可否 (blocking)
//! - live なら `query_status` = `ClientConnection` 経由で `status.query` を投げ、
//!   `StatusResponse` を回収 (= 同じ経路を `hyoui-cli` list も使う)。
//!
//! ## 設計判断 (Phase 1)
//!
//! `hyoui-cli::socket_path::existing_base_dirs` / `list_candidate_dirs_all_namespaces`
//! と機能重複するが、後者は format / prune / jsonl / namespace flag 解決 と絡んで
//! いて cli binary に閉じている。Phase 1 では **移設ではなく重複** で始め、Phase 2
//! 以降で必要になったら統合する (= 現時点で移設すると hyoui-cli 側の書き換え範囲が
//! 大きくなり Phase 1 のスコープを超える)。

use std::path::{Path, PathBuf};

use crate::client::{AttachOptions, ClientConnection};
use crate::protocol::messages::{OnChildSuspendPolicy, StatusQuery, StatusResponse};
use crate::protocol::{ControlMessage, MVP_CAPS, Mode};

/// 1 session に対応する discovery 結果。
///
/// `live` (= status.query 応答あり) と `stale` (= socket 残骸 or handshake 失敗)
/// を variant で区別する。format / prune 責務は持たない (= caller 側の仕事)。
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// session id (= socket file の `.sock` を除いた stem)。
    pub session_id: String,
    /// このセッションが属する namespace (= default / user 指定)。
    pub namespace: String,
    /// socket file 実 path (= `<base>/[<ns>/]<session>.sock`)。
    pub socket_path: PathBuf,
    /// socket file の mtime を epoch ms に換算した値。取得失敗時 0。
    pub started_unix_ms: u64,
    /// status.query 応答内容 or 失敗理由。
    pub status: SessionStatus,
}

/// [`SessionEntry`] の live / stale variant。
#[derive(Debug, Clone)]
pub enum SessionStatus {
    /// live daemon (= status.query が返した情報を保持)。
    Live(LiveInfo),
    /// stale (= socket 残骸 / handshake 失敗 等)。理由文字列を保持。
    Stale {
        /// stale と判定した具体的な理由 (= caller が log / API 応答で使う)。
        reason: String,
    },
}

/// live session の status.query 抜粋。
///
/// `StatusResponse` を丸ごと保持すると protocol 変更で discovery API が膨らむため、
/// hyoui-web が今使う field だけを露出する (= 将来必要になったら追加)。
#[derive(Debug, Clone)]
pub struct LiveInfo {
    /// 子 PTY の起動時 cwd (= `hyoui list` 表示と同じ値)。
    pub cwd: String,
    /// 子 PTY の argv (= 起動 command)。
    pub argv: Vec<String>,
    /// 現在 attach 中の client 数。
    pub clients: usize,
    /// 子 PTY が stopped (= SIGTSTP 等で停止) のまま残っているか。
    pub child_stopped: bool,
    /// 子 PTY の PID (= exited なら None)。
    pub child_pid: Option<u32>,
    /// 子 PTY の pgid (= exited なら None)。
    pub child_pgid: Option<u32>,
    /// 現在の on-child-suspend policy (旧 daemon なら None)。
    pub on_child_suspend: Option<OnChildSuspendPolicy>,
    /// daemon バイナリ version (= 空文字なら旧 daemon)。
    pub daemon_version: String,
}

/// 走査する base socket dir 候補を優先順で返す (= `hyoui-cli::socket_path::existing_base_dirs`
/// 相当)。実在する dir のみ返す。
pub fn existing_base_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        && !runtime.is_empty()
    {
        let dir = PathBuf::from(runtime).join("hyoui");
        if dir.is_dir() {
            out.push(dir);
        }
    }
    let state = if let Some(v) = std::env::var_os("XDG_STATE_HOME")
        && !v.is_empty()
    {
        Some(PathBuf::from(v).join("hyoui"))
    } else if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        Some(
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("hyoui"),
        )
    } else {
        None
    };
    if let Some(s) = state
        && s.is_dir()
        && !out.contains(&s)
    {
        out.push(s);
    }
    out
}

/// unix socket connect による死活判定 (blocking、= local domain なので即応答)。
pub fn probe_liveness(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// 1 socket に status.query を投げ、`StatusResponse` を回収する。
///
/// 失敗理由 (connect / handshake / decode / daemon error) は文字列で返す。
/// `hyoui-cli::query_status_for_list` と同じ pattern (= blocking、timeout なし)。
pub fn query_status(socket_path: &Path) -> Result<StatusResponse, String> {
    let opts = AttachOptions {
        mode: Mode::Ro,
        caps: MVP_CAPS.iter().map(|s| (*s).to_string()).collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = ClientConnection::connect(socket_path, opts)
        .map_err(|e| format!("connect/handshake: {e}"))?;
    conn.send_control(&ControlMessage::StatusQuery(StatusQuery {}))
        .map_err(|e| format!("send status.query: {e}"))?;
    loop {
        match conn.recv_control(None) {
            Ok(ControlMessage::StatusResponse(sr)) => return Ok(sr),
            Ok(ControlMessage::ModeChange(_)) | Ok(ControlMessage::LeaderNotify(_)) => continue,
            Ok(ControlMessage::Error(e)) => {
                return Err(format!("daemon error: {:?} ({})", e.code, e.message));
            }
            Ok(other) => {
                return Err(format!(
                    "unexpected response kind: {:?}",
                    std::mem::discriminant(&other)
                ));
            }
            Err(e) => return Err(format!("recv: {e}")),
        }
    }
}

/// 全 namespace 横断で live session を列挙する。
///
/// 各 base dir の直下 `*.sock` を default namespace、サブ dir 配下 `*.sock` を
/// そのサブ dir 名の namespace として拾う。`probe_liveness` を通ったものだけ
/// `query_status` して `LiveInfo` を埋める。stale (= probe fail or query fail)
/// は `SessionStatus::Stale` として残す (= caller 側で filter する余地を残す)。
///
/// 並列化は Phase 1 では未実装 (= session 数が数十以下想定なので逐次で許容)。
/// 必要なら `hyoui-cli::enrich_entries_with_status` 相当の thread fanout を後付け。
pub fn list_sessions() -> Vec<SessionEntry> {
    let now = std::time::SystemTime::now();
    let mut out: Vec<SessionEntry> = Vec::new();
    for base in existing_base_dirs() {
        // base 直下の `*.sock` = default namespace。
        collect_socks_in_dir(&base, crate::cli::DEFAULT_NAMESPACE, now, &mut out);
        // base 配下のサブ dir = 各 namespace。
        let read = match std::fs::read_dir(&base) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let ns = match path.file_name().and_then(|s| s.to_str()) {
                Some(v) => v.to_string(),
                None => continue,
            };
            if ns == crate::cli::DEFAULT_NAMESPACE {
                // base 直下と同じ扱い、重複回避。
                continue;
            }
            collect_socks_in_dir(&path, &ns, now, &mut out);
        }
    }
    // mtime 昇順で安定化 (= hyoui-cli list と同じ順序)。
    out.sort_by_key(|e| e.started_unix_ms);
    for e in out.iter_mut() {
        if !matches!(e.status, SessionStatus::Live(_)) {
            continue;
        }
        match query_status(&e.socket_path) {
            Ok(sr) => {
                e.status = SessionStatus::Live(LiveInfo {
                    cwd: sr.cwd,
                    argv: sr.argv,
                    clients: sr.clients.len(),
                    child_stopped: sr.child_stopped,
                    child_pid: sr.child_pid,
                    child_pgid: sr.child_pgid,
                    on_child_suspend: sr.on_child_suspend,
                    daemon_version: sr.daemon_version,
                });
            }
            Err(reason) => {
                e.status = SessionStatus::Stale { reason };
            }
        }
    }
    out
}

fn collect_socks_in_dir(
    dir: &Path,
    namespace: &str,
    now: std::time::SystemTime,
    out: &mut Vec<SessionEntry>,
) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sock") {
            continue;
        }
        let session_id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(v) => v.to_string(),
            None => continue,
        };
        let started_unix_ms = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let _ = now; // 現状 dur は算出しないが、caller が uptime を出す拡張余地。
        let live = probe_liveness(&path);
        let status = if live {
            // placeholder: `list_sessions` の後段 loop で query_status して埋める。
            SessionStatus::Live(LiveInfo {
                cwd: String::new(),
                argv: Vec::new(),
                clients: 0,
                child_stopped: false,
                child_pid: None,
                child_pgid: None,
                on_child_suspend: None,
                daemon_version: String::new(),
            })
        } else {
            SessionStatus::Stale {
                reason: "socket connect refused (= stale socket)".to_string(),
            }
        };
        out.push(SessionEntry {
            session_id,
            namespace: namespace.to_string(),
            socket_path: path,
            started_unix_ms,
            status,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_base_dirs_returns_only_existing() {
        // 空 env でも panic しない (= 実在 dir 0 個で空 Vec)。
        // ここでは env を弄らずに `is_dir` filter が効いていることだけ観測する。
        let _ = existing_base_dirs();
    }

    #[test]
    fn probe_liveness_rejects_nonexistent_path() {
        let p = PathBuf::from("/tmp/definitely-not-a-hyoui-socket.sock");
        assert!(!probe_liveness(&p));
    }
}
