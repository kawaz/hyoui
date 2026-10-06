//! Session discovery — socket dir 走査 + status.query による live session 列挙 (DR-0027)。
//!
//! `hyoui-web` / 将来の外部 tool から「今 host に居る hyoui session 一覧」を取る
//! 共通経路。`hyoui-cli` 内部の `list_command` と等価の走査を、format
//! 責務なし・純粋な `Vec<SessionEntry>` として提供する。
//!
//! ## 走査経路 (DR-0041 決定 4)
//!
//! `<状態の root>/sessions/*.sock` だけを見る ([`crate::paths::Env::sessions_dir`])。
//! root 直下や他の dir (古い置き場) は読まない。古い置き場に socket が残っているかは
//! [`legacy_session_places`] が別に見る (= 警告のためだけで、session としては拾わない)。
//!
//! 一覧 ([`list_sessions`]) は全 socket に status.query する。session 1 件を引く用途は [`find_session`] を使い、`sessions/<id>.sock` を直接組んでその socket だけに接続する。
//!
//! ## 死活判定
//!
//! `query_status` が connect 拒否と接続後の応答失敗を区別する。接続拒否 socket は削除し、接続済みの daemon は無応答でも保持する。
//!
//! CLI と web gateway が同じ接続判定を利用する。出力形式は各 caller が管理する。

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
    /// socket file 実 path (= `<状態の root>/sessions/<session>.sock`)。
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

/// 今の env の session の置き場 (`<状態の root>/sessions`)。root を決められなければ
/// `None` (= 列挙する場所が無い)。
#[must_use]
pub fn sessions_dir() -> Option<PathBuf> {
    crate::paths::Env::current().sessions_dir().ok()
}

/// 古い置き場 (DR-0041 決定 7) に残っている session の socket の dir。
///
/// 新しいバイナリは `sessions/` だけを読む。ここで見るのは警告のためだけで、中の
/// socket は session として拾わない (= 古い daemon とは id の形も置き場も違う)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySessionPlace {
    /// socket が残っている dir (= 状態の root 直下か、その下の古い namespace の dir)。
    pub dir: PathBuf,
    /// その dir に残っている socket の数。
    pub sockets: usize,
    /// dir 自体が symlink か (= 古いバイナリのために残した symlink)。
    pub symlink: bool,
}

/// 古い置き場に socket が残っていないかを見る (DR-0041 決定 7)。
///
/// 見るのは状態の root 直下の `*.sock` と、root 直下の dir (機能別の `sessions/` /
/// `web/` を除く) の直下の `*.sock`。どちらも新しいバイナリが session を置かない
/// 場所なので、在れば古いバイナリの session か、移行で残した symlink である。
#[must_use]
pub fn legacy_session_places(env: &crate::paths::Env) -> Vec<LegacySessionPlace> {
    let Ok(root) = env.state_root() else {
        return Vec::new();
    };
    let mut places = Vec::new();
    let count = |dir: &Path| {
        std::fs::read_dir(dir)
            .map(|read| {
                read.flatten()
                    .filter(|e| {
                        e.path().extension().and_then(|x| x.to_str()) == Some("sock")
                            && e.file_type().is_ok_and(|t| t.is_socket() || t.is_symlink())
                    })
                    .count()
            })
            .unwrap_or(0)
    };
    let in_root = count(&root);
    if in_root > 0 {
        places.push(LegacySessionPlace {
            dir: root.clone(),
            sockets: in_root,
            symlink: false,
        });
    }
    let Ok(read) = std::fs::read_dir(&root) else {
        return places;
    };
    let mut dirs: Vec<PathBuf> = read
        .flatten()
        .filter(|e| !matches!(e.file_name().to_str(), Some("sessions" | "web")))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let sockets = count(&dir);
        if sockets > 0 {
            let symlink = dir
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());
            places.push(LegacySessionPlace {
                dir,
                sockets,
                symlink,
            });
        }
    }
    places
}

/// [`legacy_session_places`] の警告文 (stderr に出す 1 行ずつ)。
#[must_use]
pub fn legacy_session_warnings(env: &crate::paths::Env) -> Vec<String> {
    legacy_session_places(env)
        .into_iter()
        .map(|place| {
            if place.symlink {
                format!(
                    "{} is a symlink left for older hyoui binaries; remove it once none of them run (DR-0041)",
                    place.dir.display()
                )
            } else {
                format!(
                    "{} has {} session socket(s) in the old layout; this hyoui does not read them (sessions now live in <state root>/sessions). Operate them with the older hyoui, or let them finish (DR-0041)",
                    place.dir.display(),
                    place.sockets
                )
            }
        })
        .collect()
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

/// 接続可能な session を列挙する。
///
/// 接続拒否 socket は削除して結果から除外し、接続後の応答失敗は PID を添えて保持する。なお各 socket の応答待ちは最大 5 秒。
pub fn list_sessions() -> Vec<SessionEntry> {
    match sessions_dir() {
        Some(dir) => list_sessions_in(&dir),
        None => Vec::new(),
    }
}

/// [`list_sessions`] の走査対象 dir (= `sessions/`) を caller が渡す版。
pub fn list_sessions_in(dir: &Path) -> Vec<SessionEntry> {
    let mut out: Vec<SessionEntry> = Vec::new();
    collect_socks_in_dir(dir, &mut out);
    // 起動時刻 (= socket の mtime) の昇順 (= hyoui-cli list と同じ順序)。id の版に
    // 依存しない (DR-0041 決定 2)。
    out.sort_by_key(|e| e.started_unix_ms);
    out.retain_mut(probe);
    out
}

/// session_id 1 件を `sessions/<id>.sock` から直接解決し、その socket だけに status.query する。
///
/// 他 session の socket には connect しない。`session_id` が [`crate::cli::validate_session_id`] に反する場合と、socket が無い・接続拒否で消えた場合は `None`。
pub fn find_session(session_id: &str) -> Option<SessionEntry> {
    find_session_in(&sessions_dir()?, session_id)
}

/// [`find_session`] の走査対象 dir (= `sessions/`) を caller が渡す版。
pub fn find_session_in(dir: &Path, session_id: &str) -> Option<SessionEntry> {
    // path へ join する前に検証する (= UUID 標準形以外を path にしない)。
    crate::cli::validate_session_id(session_id).ok()?;
    let mut entry = socket_entry(dir.join(format!("{session_id}.sock")))?;
    probe(&mut entry).then_some(entry)
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

fn collect_socks_in_dir(dir: &Path, out: &mut Vec<SessionEntry>) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sock") {
            continue;
        }
        if let Some(e) = socket_entry(path) {
            out.push(e);
        }
    }
}

/// `path` が socket file なら status 未確定の [`SessionEntry`] を作る (= symlink は辿らない)。
fn socket_entry(path: PathBuf) -> Option<SessionEntry> {
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
        let listener = crate::sys::socket::bind_listener(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }

    /// accept 1 回で即 close する listener (= `query_status` が待たずに `Error` を返す)。
    fn closing_listener(path: &Path) -> std::thread::JoinHandle<()> {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = crate::sys::socket::bind_listener(path).unwrap();
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

    const TARGET: &str = "0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f";
    const OTHER: &str = "5c9d0e1f-2a3b-4c4d-8e5f-6a7b8c9d0e1f";

    fn env_at(root: &Path) -> crate::paths::Env {
        let root = root.as_os_str().to_os_string();
        crate::paths::Env::from_lookup(|name| (name == "HYOUI_STATE_DIR").then(|| root.clone()))
    }

    #[test]
    fn find_session_connects_only_to_the_named_socket() {
        let root = private_dir();
        let sessions = root.path().join("sessions");
        let target = sessions.join(format!("{TARGET}.sock"));
        // 他 session は accept しない listener で置き、connect が来ないことを見る。
        let other = idle_listener(&sessions.join(format!("{OTHER}.sock")));
        let worker = closing_listener(&target);

        let found = find_session_in(&sessions, TARGET).expect("target が見つかる");
        worker.join().unwrap();
        assert_eq!(found.session_id, TARGET);
        assert_eq!(found.socket_path, target);
        assert!(matches!(found.status, SessionStatus::Error { .. }));
        assert_untouched(&other, "別 session");
    }

    /// discovery は `sessions/` の中だけを見る (DR-0041 決定 4)。root 直下や他の dir に
    /// 同じ id の socket があっても、一覧にも 1 件の解決にも出さず、connect もしない。
    #[test]
    fn discovery_reads_only_the_sessions_dir() {
        let root = private_dir();
        let sessions = root.path().join("sessions");
        let in_root = idle_listener(&root.path().join(format!("{TARGET}.sock")));
        let in_old_ns = idle_listener(&root.path().join("workers").join(format!("{TARGET}.sock")));
        let in_web = idle_listener(&root.path().join("web").join(format!("{OTHER}.sock")));
        std::fs::create_dir_all(&sessions).unwrap();

        assert!(find_session_in(&sessions, TARGET).is_none());
        assert!(list_sessions_in(&sessions).is_empty());
        assert_untouched(&in_root, "root 直下の socket");
        assert_untouched(&in_old_ns, "古い namespace の dir の socket");
        assert_untouched(&in_web, "web/ の socket");

        let live = sessions.join(format!("{OTHER}.sock"));
        let worker = closing_listener(&live);
        let listed = list_sessions_in(&sessions);
        worker.join().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|e| e.session_id.as_str())
                .collect::<Vec<_>>(),
            [OTHER]
        );
        assert_untouched(&in_root, "root 直下の socket");
    }

    /// 一覧は socket の mtime (= 起動時刻) の昇順で、id の並びに依らない。
    #[test]
    fn list_sessions_orders_by_start_time_not_by_id() {
        let root = private_dir();
        let sessions = root.path().join("sessions");
        // id の辞書順は TARGET < OTHER。起動時刻は OTHER の方が古い。
        let newer = sessions.join(format!("{TARGET}.sock"));
        let older = sessions.join(format!("{OTHER}.sock"));
        let w1 = closing_listener(&newer);
        let w2 = closing_listener(&older);
        set_mtime(&newer, 2_000_000_000);
        set_mtime(&older, 1_000_000_000);
        let listed = list_sessions_in(&sessions);
        w1.join().unwrap();
        w2.join().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|e| e.session_id.as_str())
                .collect::<Vec<_>>(),
            [OTHER, TARGET]
        );
    }

    #[test]
    fn find_session_skips_a_pruned_socket() {
        let root = private_dir();
        let sessions = root.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // listener 不在 + lock 非保持 = 接続拒否で削除される残骸。
        let dead = sessions.join(format!("{TARGET}.sock"));
        drop(crate::sys::socket::bind_listener(&dead).unwrap());
        std::fs::File::create(dead.with_extension("lock")).unwrap();

        assert!(find_session_in(&sessions, TARGET).is_none());
        assert!(!dead.exists(), "残骸 socket は削除される");
    }

    #[test]
    fn find_session_rejects_invalid_or_missing_id() {
        let root = private_dir();
        let sessions = root.path().join("sessions");
        let decoy = idle_listener(&sessions.join(format!("{TARGET}.sock")));
        let upper = TARGET.to_ascii_uppercase();
        let bare = TARGET.replace('-', "");
        for id in [
            "",
            ".",
            "..",
            "../x",
            "ns/x",
            &upper,
            &bare,
            &TARGET[..8],
            OTHER,
        ] {
            assert!(find_session_in(&sessions, id).is_none(), "id={id:?}");
        }
        assert_untouched(&decoy, "表記違いで同じ UUID の socket");
    }

    /// 古い置き場 (root 直下と古い namespace の dir) の socket は警告の対象になり、
    /// `sessions/` と `web/` は対象にならない (DR-0041 決定 7)。
    #[test]
    fn legacy_places_are_the_root_and_old_namespace_dirs() {
        let root = private_dir();
        let env = env_at(root.path());
        assert!(legacy_session_places(&env).is_empty());

        let _s = idle_listener(&root.path().join("sessions").join(format!("{TARGET}.sock")));
        let _w = idle_listener(&root.path().join("web").join("run").join("supervisor.sock"));
        let _w2 = idle_listener(&root.path().join("web").join("x.sock"));
        assert!(
            legacy_session_places(&env).is_empty(),
            "sessions/ と web/ は古い置き場ではない"
        );

        let _a = idle_listener(&root.path().join("run-1-abcd.sock"));
        let _b = idle_listener(&root.path().join("workers").join("w1.sock"));
        let _c = idle_listener(&root.path().join("workers").join("w2.sock"));
        let real = root.path().join("real-ns");
        let _d = idle_listener(&real.join("x.sock"));
        std::os::unix::fs::symlink(&real, root.path().join("linked")).unwrap();

        let places = legacy_session_places(&env);
        let got: Vec<(String, usize, bool)> = places
            .iter()
            .map(|p| {
                (
                    p.dir
                        .strip_prefix(root.path())
                        .unwrap()
                        .display()
                        .to_string(),
                    p.sockets,
                    p.symlink,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (String::new(), 1, false),
                ("linked".to_string(), 1, true),
                ("real-ns".to_string(), 1, false),
                ("workers".to_string(), 2, false),
            ]
        );
        let warnings = legacy_session_warnings(&env);
        assert_eq!(warnings.len(), 4);
        assert!(
            warnings[0].contains("does not read them"),
            "{}",
            warnings[0]
        );
        assert!(warnings[1].contains("symlink"), "{}", warnings[1]);
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
