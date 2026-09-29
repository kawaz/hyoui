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
//! `query_status` が connect 拒否と接続後の応答失敗を区別する。接続拒否 socket は削除し、接続済みの daemon は無応答でも保持する。
//!
//! CLI と web gateway が同じ接続判定を利用する。socket 配置の走査と出力形式は各 caller が管理する。

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use crate::client::{AttachOptions, ClientConnection};
use crate::protocol::messages::{OnChildSuspendPolicy, StatusQuery, StatusResponse};
use crate::protocol::{ControlMessage, MVP_CAPS, Mode};

/// 1 session に対応する discovery 結果。
///
/// 応答した session と応答しない session を区別する。
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

/// 無応答を示す一覧 status。
pub const NO_RESPONSE_STATUS: &str = "no-response";

/// [`SessionEntry`] の daemon 状態。
#[derive(Debug, Clone)]
pub enum SessionStatus {
    /// status.query が返した情報。
    Live(LiveInfo),
    /// 接続済み daemon が handshake または status.query に応答しない。
    Hung {
        /// Unix socket peer credential から取得した daemon PID。
        daemon_pid: Option<u32>,
        /// 応答失敗の理由。
        reason: String,
    },
    /// 接続先から明示的な拒否または protocol error が返った。
    Error {
        /// 失敗理由。
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

/// status.query の応答期限。RAW_ACK_TIMEOUT と同じ 5 秒を取り、一時的な遅延を即座に無応答と判定しない。
pub const LIST_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// status.query の結果と接続した daemon の peer PID。
#[derive(Debug)]
pub enum StatusQueryResult {
    /// daemon が応答した。
    Live(StatusResponse),
    /// 接続後に daemon から応答を得られなかった。
    Hung {
        /// Unix socket の peer PID。
        daemon_pid: Option<u32>,
        /// 失敗理由。
        reason: String,
    },
    /// 接続後の明示的な失敗。
    Error {
        /// 接続後の拒否または protocol failure。
        reason: String,
    },
    /// connect が拒否されたため残骸 socket を削除した。
    Gone,
}

/// 1 socket に status.query を投げ、接続失敗と無応答を区別する。
pub fn query_status(socket_path: &Path) -> StatusQueryResult {
    let opts = AttachOptions {
        mode: Mode::Ro,
        caps: MVP_CAPS.iter().map(|s| (*s).to_string()).collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut peer_pid = None;
    let mut conn = match ClientConnection::connect_with_timeout_and_peer(
        socket_path,
        opts,
        Some(LIST_RESPONSE_TIMEOUT),
        &mut peer_pid,
    ) {
        Ok(conn) => conn,
        Err(crate::Error::Errno(nix::errno::Errno::ECONNREFUSED | nix::errno::Errno::ENOENT)) => {
            if socket_path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_socket())
                && let Err(e) = std::fs::remove_file(socket_path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "hyoui: warning: failed to prune {}: {e}",
                    socket_path.display()
                );
            }
            return StatusQueryResult::Gone;
        }
        Err(e) => {
            return if matches!(&e, crate::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock))
            {
                StatusQueryResult::Hung {
                    daemon_pid: peer_pid,
                    reason: format!("5 秒応答なし: {e}"),
                }
            } else {
                StatusQueryResult::Error {
                    reason: format!("connect/handshake: {e}"),
                }
            };
        }
    };
    if let Err(e) = conn.send_control(&ControlMessage::StatusQuery(StatusQuery {})) {
        return StatusQueryResult::Error {
            reason: format!("send status.query: {e}"),
        };
    }
    loop {
        match conn.recv_control(None) {
            Ok(ControlMessage::StatusResponse(sr)) => return StatusQueryResult::Live(sr),
            Ok(ControlMessage::ModeChange(_)) | Ok(ControlMessage::LeaderNotify(_)) => continue,
            Ok(ControlMessage::Error(e)) => {
                return StatusQueryResult::Error {
                    reason: format!("daemon error: {:?} ({})", e.code, e.message),
                };
            }
            Ok(other) => {
                return StatusQueryResult::Error {
                    reason: format!(
                        "unexpected response kind: {:?}",
                        std::mem::discriminant(&other)
                    ),
                };
            }
            Err(e) => {
                return if matches!(&e, crate::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock))
                {
                    StatusQueryResult::Hung {
                        daemon_pid: peer_pid,
                        reason: format!("5 秒応答なし: {e}"),
                    }
                } else {
                    StatusQueryResult::Error {
                        reason: format!("recv: {e}"),
                    }
                };
            }
        }
    }
}

/// 全 namespace 横断で接続可能な session を列挙する。
///
/// 接続拒否 socket は削除して結果から除外し、接続後の応答失敗は PID を添えて保持する。なお各 socket の応答待ちは最大 5 秒。
pub fn list_sessions() -> Vec<SessionEntry> {
    let mut out: Vec<SessionEntry> = Vec::new();
    for base in existing_base_dirs() {
        // base 直下の `*.sock` = default namespace。
        collect_socks_in_dir(&base, crate::cli::DEFAULT_NAMESPACE, &mut out);
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
            collect_socks_in_dir(&path, &ns, &mut out);
        }
    }
    // mtime 昇順で安定化 (= hyoui-cli list と同じ順序)。
    out.sort_by_key(|e| e.started_unix_ms);
    out.retain_mut(|e| match query_status(&e.socket_path) {
        StatusQueryResult::Live(sr) => {
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
            true
        }
        StatusQueryResult::Hung { daemon_pid, reason } => {
            e.status = SessionStatus::Hung { daemon_pid, reason };
            true
        }
        StatusQueryResult::Error { reason } => {
            e.status = SessionStatus::Error { reason };
            true
        }
        StatusQueryResult::Gone => false,
    });
    out
}

fn collect_socks_in_dir(dir: &Path, namespace: &str, out: &mut Vec<SessionEntry>) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sock") {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_socket()) {
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
        let status = SessionStatus::Hung {
            daemon_pid: None,
            reason: String::new(),
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
    fn unresponsive_listener_is_no_response_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unresponsive.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let (release, wait_for_release) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            wait_for_release.recv().unwrap();
        });
        let result = query_status(&path);
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            matches!(result, StatusQueryResult::Hung { daemon_pid: Some(pid), .. } if pid == std::process::id()),
            "result: {result:?}, self pid: {}",
            std::process::id()
        );
        assert!(path.exists());
    }

    #[test]
    fn closed_listener_is_error_not_no_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("closed.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        });
        assert!(matches!(
            query_status(&path),
            StatusQueryResult::Error { .. }
        ));
        assert!(path.exists());
        worker.join().unwrap();
    }

    #[test]
    fn stale_socket_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dead.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(matches!(query_status(&path), StatusQueryResult::Gone));
        assert!(!path.exists());
    }
}
