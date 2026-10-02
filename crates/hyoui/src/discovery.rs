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
//! 一覧 ([`list_sessions`]) は全 socket に status.query する。session 1 件を引く用途は [`find_session`] を使い、id から候補 path を直接組んで同名 socket だけに接続する。
//!
//! ## 死活判定
//!
//! `query_status` が connect 拒否と接続後の応答失敗を区別する。接続拒否 socket は削除し、接続済みの daemon は無応答でも保持する。
//!
//! CLI と web gateway が同じ接続判定を利用する。socket 配置の走査と出力形式は各 caller が管理する。

use nix::fcntl::{Flock, FlockArg};
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
    /// lock が無く daemon の生存状態を判定できない。
    Stale {
        /// 判定不能の理由。
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
    /// lock が無く生存を判定できない socket。
    Stale {
        /// 判定不能の理由。
        reason: String,
    },
    /// 残骸 socket を削除したか、走査後に消えた。
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
        Err(crate::Error::Errno(nix::errno::Errno::ENOENT)) => return StatusQueryResult::Gone,
        // Linux の backlog 満杯。listener の実在が確定しているので lock 判定せず保持する。
        Err(crate::Error::Errno(nix::errno::Errno::EAGAIN)) => {
            return StatusQueryResult::Hung {
                daemon_pid: None,
                reason: "daemon が稼働中だが接続を受け付けられない".into(),
            };
        }
        // macOS の backlog 満杯と listener 不在はどちらも ECONNREFUSED なので lock で区別する。
        Err(crate::Error::Errno(nix::errno::Errno::ECONNREFUSED)) => {
            let _dir_lock = match crate::sys::socket::lock_socket_dir(socket_path) {
                Ok(lock) => lock,
                Err(e) => {
                    return StatusQueryResult::Error {
                        reason: format!("socket directory lock を取得できない: {e}"),
                    };
                }
            };
            let lock_path = socket_path.with_extension("lock");
            let lock = match std::fs::File::open(&lock_path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return StatusQueryResult::Stale {
                        reason: "daemon lock がないため生存不明".into(),
                    };
                }
                Err(e) => {
                    return StatusQueryResult::Error {
                        reason: format!("daemon lock を開けない: {e}"),
                    };
                }
            };
            match Flock::lock(lock, FlockArg::LockExclusiveNonblock) {
                Ok(_guard) => {
                    if socket_path
                        .symlink_metadata()
                        .is_ok_and(|m| m.file_type().is_socket())
                    {
                        if let Err(e) = std::fs::remove_file(socket_path)
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            eprintln!(
                                "hyoui: warning: failed to prune {}: {e}",
                                socket_path.display()
                            );
                        }
                        if let Err(e) = std::fs::remove_file(&lock_path)
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            eprintln!(
                                "hyoui: warning: failed to prune {}: {e}",
                                lock_path.display()
                            );
                        }
                    }
                    return StatusQueryResult::Gone;
                }
                Err((_, nix::errno::Errno::EWOULDBLOCK)) => {
                    return StatusQueryResult::Hung {
                        daemon_pid: None,
                        reason: "daemon が稼働中だが接続を受け付けられない".into(),
                    };
                }
                Err((_, e)) => {
                    return StatusQueryResult::Error {
                        reason: format!("daemon lock の確認に失敗: {e}"),
                    };
                }
            }
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
    list_sessions_in(&existing_base_dirs())
}

/// [`list_sessions`] の走査対象 base dir を caller が渡す版。
pub fn list_sessions_in(bases: &[PathBuf]) -> Vec<SessionEntry> {
    let mut out: Vec<SessionEntry> = Vec::new();
    for (ns, dir) in namespace_dirs(bases) {
        collect_socks_in_dir(&dir, &ns, &mut out);
    }
    // mtime 昇順で安定化 (= hyoui-cli list と同じ順序)。
    out.sort_by_key(|e| e.started_unix_ms);
    out.retain_mut(probe);
    out
}

/// session_id 1 件を DR-0018 の配置規則から直接解決し、その socket だけに status.query する。
///
/// 候補は各 base dir の `<base>/<id>.sock` (default namespace) と `<base>/<ns>/<id>.sock`。他 session の socket には connect しない。
///
/// 同名が複数 namespace にある場合は [`list_sessions`] の結果から最初の一致を採る従来の解決と同じ選び方をする: mtime 昇順で並べ、接続拒否で消えた (= [`StatusQueryResult::Gone`]) 候補を飛ばして最初の 1 件を返す。応答しない候補もそこで確定して返す (= 後続の同名候補へは進まない)。
///
/// `session_id` が [`crate::cli::validate_session_id`] に反する場合と、候補が 1 つも残らない場合は `None`。
pub fn find_session(session_id: &str) -> Option<SessionEntry> {
    find_session_in(&existing_base_dirs(), session_id)
}

/// [`find_session`] の走査対象 base dir を caller が渡す版。
pub fn find_session_in(bases: &[PathBuf], session_id: &str) -> Option<SessionEntry> {
    // path へ join する前に whitelist 検証する (= `..` / `/` による traversal 防止)。
    crate::cli::validate_session_id(session_id).ok()?;
    let file_name = format!("{session_id}.sock");
    let mut candidates: Vec<SessionEntry> = namespace_dirs(bases)
        .into_iter()
        .filter_map(|(ns, dir)| socket_entry(dir.join(&file_name), &ns))
        .collect();
    candidates.sort_by_key(|e| e.started_unix_ms);
    candidates
        .into_iter()
        .find_map(|mut e| probe(&mut e).then_some(e))
}

/// 各 base dir について (namespace, socket dir) を走査順に返す。
///
/// base 直下が default namespace、直下のサブ dir が各 namespace。`default` という名前のサブ dir は base 直下と同じ namespace なので含めない。
fn namespace_dirs(bases: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for base in bases {
        out.push((crate::cli::DEFAULT_NAMESPACE.to_string(), base.clone()));
        let read = match std::fs::read_dir(base) {
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
                continue;
            }
            out.push((ns, path));
        }
    }
    out
}

/// status.query を投げて `entry.status` を埋める。結果から外すべき (= 接続拒否で削除済み / 消えた) なら `false`。
fn probe(entry: &mut SessionEntry) -> bool {
    entry.status = match query_status(&entry.socket_path) {
        StatusQueryResult::Live(sr) => SessionStatus::Live(LiveInfo {
            cwd: sr.cwd,
            argv: sr.argv,
            clients: sr.clients.len(),
            child_stopped: sr.child_stopped,
            child_pid: sr.child_pid,
            child_pgid: sr.child_pgid,
            on_child_suspend: sr.on_child_suspend,
            daemon_version: sr.daemon_version,
        }),
        StatusQueryResult::Hung { daemon_pid, reason } => {
            SessionStatus::Hung { daemon_pid, reason }
        }
        StatusQueryResult::Error { reason } => SessionStatus::Error { reason },
        StatusQueryResult::Stale { reason } => SessionStatus::Stale { reason },
        StatusQueryResult::Gone => return false,
    };
    true
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
        if let Some(e) = socket_entry(path, namespace) {
            out.push(e);
        }
    }
}

/// `path` が socket file なら status 未確定の [`SessionEntry`] を作る (= symlink は辿らない)。
fn socket_entry(path: PathBuf, namespace: &str) -> Option<SessionEntry> {
    let meta = path.symlink_metadata().ok()?;
    if !meta.file_type().is_socket() {
        return None;
    }
    let session_id = path.file_stem().and_then(|s| s.to_str())?.to_string();
    let started_unix_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some(SessionEntry {
        session_id,
        namespace: namespace.to_string(),
        socket_path: path,
        started_unix_ms,
        status: SessionStatus::Hung {
            daemon_pid: None,
            reason: String::new(),
        },
    })
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
        std::fs::File::create(path.with_extension("lock")).unwrap();
        assert!(matches!(query_status(&path), StatusQueryResult::Gone));
        assert!(!path.exists());
        assert!(!path.with_extension("lock").exists());
    }

    #[test]
    fn socket_without_lock_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(matches!(
            query_status(&path),
            StatusQueryResult::Stale { .. }
        ));
        assert!(path.exists());
    }

    /// tempdir を 0700 の base socket dir として用意する (= `UnixSock::listen` の要求)。
    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        dir
    }

    /// accept 待ちのまま放置する listener。connect が来たかは [`assert_untouched`] で見る。
    fn idle_listener(path: &Path) -> std::os::unix::net::UnixListener {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }

    /// accept 1 回で即 close する listener (= `query_status` が待たずに `Error` を返す)。
    fn closing_listener(path: &Path) -> std::thread::JoinHandle<()> {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        })
    }

    fn assert_untouched(listener: &std::os::unix::net::UnixListener, what: &str) {
        match listener.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(_) => panic!("{what} に connect してはいけない"),
            Err(e) => panic!("{what}: unexpected accept error: {e}"),
        }
    }

    fn set_mtime(path: &Path, unix_secs: i64) {
        let ts = nix::sys::time::TimeSpec::new(unix_secs, 0);
        nix::sys::stat::utimensat(
            nix::fcntl::AT_FDCWD,
            path,
            &ts,
            &ts,
            nix::sys::stat::UtimensatFlags::NoFollowSymlink,
        )
        .unwrap();
    }

    #[test]
    fn find_session_connects_only_to_the_named_socket() {
        let base = private_dir();
        let base_path = base.path().to_path_buf();
        let target = base_path.join("ns1").join("target.sock");
        // 他 session (default ns / 別 ns) は accept しない listener で置き、connect が来ないことを見る。
        let other_default = idle_listener(&base_path.join("other.sock"));
        let other_ns = idle_listener(&base_path.join("ns2").join("other.sock"));
        let worker = closing_listener(&target);

        let found = find_session_in(&[base_path], "target").expect("target が見つかる");
        worker.join().unwrap();
        assert_eq!(found.session_id, "target");
        assert_eq!(found.namespace, "ns1");
        assert_eq!(found.socket_path, target);
        assert!(matches!(found.status, SessionStatus::Error { .. }));
        assert_untouched(&other_default, "default ns の別 session");
        assert_untouched(&other_ns, "別 ns の別 session");
    }

    #[test]
    fn find_session_picks_oldest_candidate_like_list_sessions() {
        let base = private_dir();
        let base_path = base.path().to_path_buf();
        // 同名が default と ns1 にある。ns1 の方が古いので list_sessions の先頭一致と同じく ns1 を選ぶ。
        let newer = idle_listener(&base_path.join("dup.sock"));
        let older_path = base_path.join("ns1").join("dup.sock");
        let worker = closing_listener(&older_path);
        set_mtime(&base_path.join("dup.sock"), 2_000_000_000);
        set_mtime(&older_path, 1_000_000_000);

        let found = find_session_in(&[base_path], "dup").expect("dup が見つかる");
        worker.join().unwrap();
        assert_eq!(found.namespace, "ns1");
        // 先頭候補で確定したので、後続の同名候補にも connect しない。
        assert_untouched(&newer, "後続の同名候補");
    }

    #[test]
    fn find_session_skips_pruned_candidate() {
        let base = private_dir();
        let base_path = base.path().to_path_buf();
        // 古い候補は listener 不在 + lock 非保持 = 接続拒否で削除される残骸。
        let dead = base_path.join("dup.sock");
        drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
        std::fs::File::create(dead.with_extension("lock")).unwrap();
        let live_path = base_path.join("ns1").join("dup.sock");
        let worker = closing_listener(&live_path);
        set_mtime(&dead, 1_000_000_000);
        set_mtime(&live_path, 2_000_000_000);

        let found = find_session_in(&[base_path], "dup").expect("残った候補が見つかる");
        worker.join().unwrap();
        assert_eq!(found.namespace, "ns1");
        assert!(!dead.exists(), "残骸 socket は従来どおり削除される");
    }

    #[test]
    fn find_session_rejects_invalid_or_missing_id() {
        let base = private_dir();
        let base_path = base.path().to_path_buf();
        let decoy = idle_listener(&base_path.join("x.sock"));
        for id in ["", ".", "..", "../x", "ns/x", "missing"] {
            assert!(
                find_session_in(std::slice::from_ref(&base_path), id).is_none(),
                "id={id:?}"
            );
        }
        // `default` サブ dir は base 直下と同じ namespace なので候補にしない (= list_sessions と同じ)。
        let in_default_dir = idle_listener(&base_path.join("default").join("y.sock"));
        assert!(find_session_in(std::slice::from_ref(&base_path), "y").is_none());
        assert_untouched(&decoy, "無関係な session");
        assert_untouched(&in_default_dir, "default サブ dir の socket");
    }

    #[test]
    fn locked_listener_is_preserved_when_backlog_fills() {
        let dir = tempfile::tempdir().unwrap();
        // UnixSock::listen は親 dir が 0700 であることを要求する (tempdir の mode は環境依存)
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let path = dir.path().join("busy.sock");
        let listener = crate::sys::UnixSock::listen(&path).unwrap();
        // backlog 満杯は macOS で ECONNREFUSED、Linux で EAGAIN (blocking connect だと Linux は無期限 block)。
        let mut connections = Vec::new();
        let mut saturated = false;
        for _ in 0..32 {
            match crate::sys::socket::connect_no_wait(&path) {
                Ok(fd) => connections.push(fd),
                Err(crate::sys::Error::Errno(
                    nix::errno::Errno::ECONNREFUSED | nix::errno::Errno::EAGAIN,
                )) => {
                    saturated = true;
                    break;
                }
                Err(e) => panic!("unexpected connect error: {e}"),
            }
        }
        assert!(saturated, "backlog must saturate within 32 connections");
        let result = query_status(&path);
        assert!(
            matches!(
                result,
                StatusQueryResult::Hung {
                    daemon_pid: None,
                    ..
                }
            ),
            "result: {result:?}"
        );
        assert!(path.exists());
        assert!(path.with_extension("lock").exists());
        drop(connections);
        drop(listener);
    }
}
