//! Unix-domain socket helpers used for the agent RPC channel.
//!
//! Security (W15 in the bootstrap design):
//!
//! * Parent directory of the socket must be mode `0o700` and owned by the
//!   current effective uid.
//! * `umask(0o077)` is set around `bind(2)` (via [`UmaskGuard`]) so the
//!   socket file is created mode `0o600`. This is single-thread-safe; if
//!   hyoui ever becomes multithreaded the umask trick should be replaced
//!   with `fchmod` (which doesn't exist for sockets) or `bind` to a
//!   pre-mkstemp'd path.
//! * `FD_CLOEXEC` is set on every socket fd (listener / connect / accept)
//!   via `set_cloexec` for fd-leak defense-in-depth. SOCK_CLOEXEC is darwin-
//!   incompatible so we use the portable `fcntl` path uniformly (= L6).
//!
//! [`UnixSock`] is an RAII wrapper: Drop closes the listening fd and
//! `unlink(2)`s the socket file (best-effort).

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::{FcntlArg, FdFlag, Flock, FlockArg, OFlag, fcntl};
use nix::sys::socket::{self, AddressFamily, Backlog, SockFlag, SockType, UnixAddr};

use super::error::{Error, Result};

/// Set `FD_CLOEXEC` on the given fd via `fcntl` (= portable defense-in-depth
/// for L6; darwin lacks SOCK_CLOEXEC so we cannot rely on socket-time flags).
fn set_cloexec<F: AsFd>(fd: &F) -> Result<()> {
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(Error::from)?;
    Ok(())
}

/// Serialize socket bind and removal in the same directory.
pub(crate) fn lock_socket_dir(path: &Path) -> Result<Flock<std::fs::File>> {
    let dir = path
        .parent()
        .ok_or(Error::Invalid("socket path has no parent directory"))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join(".dir.lock"))
        .map_err(Error::from)?;
    let held = Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, e)| Error::from(e))?;
    set_cloexec(&*held)?;
    Ok(held)
}

/// daemon の name lock (`<name>.lock`) を open/create して `LOCK_EX|LOCK_NB` で取る。
/// caller は [`lock_socket_dir`] を保持した状態で呼ぶ (= prune / Drop との直列化)。
/// 他プロセスが保持中なら `EWOULDBLOCK` を返す。
fn acquire_name_lock(lock_path: &Path) -> Result<Flock<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .map_err(Error::from)?;
    let lock =
        Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, e)| Error::from(e))?;
    set_cloexec(&*lock)?;
    Ok(lock)
}

/// `bind(2)` / `connect(2)` に渡せる `sun_path` 引数の最大バイト長 (= NUL 終端を除く)。
///
/// `libc::sockaddr_un` 全体サイズから `sun_path` field の offset を引いて
/// `sun_path` 配列のバイト数 (= macOS 104 / Linux 108) を求め、NUL 終端 1 byte を
/// 引く。上限が効くのは `sun_path` に渡す引数の長さで、ファイルシステム上のフルパスの
/// 長さではない (DR-0041 決定 5)。
pub const fn sun_path_max() -> usize {
    let cap = std::mem::size_of::<libc::sockaddr_un>()
        - std::mem::offset_of!(libc::sockaddr_un, sun_path);
    cap - 1
}

/// `path` をそのまま `sun_path` に渡せるか。
fn fits_sun_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() <= sun_path_max()
}

/// `fd` を `path` に bind / connect する (DR-0041 決定 5)。
///
/// `path` が `sun_path` に収まれば直接渡す。収まらない時だけ、`path` の dir を開いた
/// fd を基準に相対名 (= ファイル名) で行う (`raw::sockaddr_op_in_dir`、fork した子
/// だけが `fchdir` する)。ファイル名自体が `sun_path` に収まらなければ
/// `ENAMETOOLONG`。
fn sockaddr_op(op: super::raw::SockAddrOp, fd: BorrowedFd<'_>, path: &Path) -> Result<()> {
    use super::raw::SockAddrOp;
    if fits_sun_path(path) {
        let addr = UnixAddr::new(path).map_err(Error::from)?;
        return match op {
            SockAddrOp::Bind => socket::bind(fd.as_raw_fd(), &addr),
            SockAddrOp::Connect => socket::connect(fd.as_raw_fd(), &addr),
        }
        .map_err(Error::from);
    }
    let name = path
        .file_name()
        .ok_or(Error::Invalid("socket path has no file name"))?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let addr = UnixAddr::new(Path::new(name)).map_err(Error::from)?;
    let dir_fd = nix::fcntl::open(
        dir,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(Error::from)?;
    super::raw::sockaddr_op_in_dir(op, fd, dir_fd.as_fd(), &addr)
}

/// RAII wrapper around `umask(2)`. On Drop the previous mask is restored.
#[derive(Debug)]
pub struct UmaskGuard {
    prev: nix::sys::stat::Mode,
}

impl UmaskGuard {
    /// Set the umask to `mode`. The previous value is restored when the
    /// returned guard is dropped.
    pub fn set(mode: nix::sys::stat::Mode) -> Self {
        let prev = nix::sys::stat::umask(mode);
        Self { prev }
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // umask itself is async-signal-safe and cannot fail.
        let _ = nix::sys::stat::umask(self.prev);
    }
}

/// Owned listening Unix-domain socket. Drop unlinks the path.
///
/// DR-0028 Phase 3: fields are `Option<>` so [`into_parts_for_exec`] can `take()`
/// them without needing a raw-pointer destructure (= keeps low-level `unsafe`
/// blocks confined to `sys/raw.rs` + `sys/signal.rs` + `sys/env.rs`, satisfying
/// the `lint-unsafe` recipe).
/// Normal lifetime: both fields are always `Some` between `listen`/`from_listener_fd`
/// and the terminating `into_parts_for_exec` or `Drop`. Accessors use `expect()`.
#[derive(Debug)]
pub struct UnixSock {
    fd: Option<OwnedFd>,
    path: Option<PathBuf>,
    lock: Option<Flock<std::fs::File>>,
}

impl UnixSock {
    /// Verify `parent_of(path)` is mode 0700 and owned by current euid.
    ///
    /// R5-FB5: 不親切な error 文言 (= 旧版「socket parent directory must be
    /// mode 0700」だけ) を hint 付きに改善。`--socket /tmp/x.sock` のような
    /// 安易な指定で kawaz が混乱した issue に対応。文言だけ更新で `ErrorCode`
    /// 系の構造は触らない。
    fn check_parent_dir(path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or(Error::Invalid("socket path has no parent directory"))?;
        if parent.as_os_str().is_empty() {
            return Err(Error::Invalid("socket path parent is empty"));
        }
        let meta = std::fs::metadata(parent).map_err(Error::from)?;
        let mode = meta.mode() & 0o777;
        if mode != 0o700 {
            return Err(Error::Precondition(
                "socket parent directory must be mode 0700 \
                 (hyoui's own socket dir is <state root>/sessions, see HYOUI_STATE_DIR; or run `chmod 700 <parent>`)",
            ));
        }
        let euid = nix::unistd::geteuid();
        if nix::unistd::Uid::from_raw(meta.uid()) != euid {
            return Err(Error::Precondition(
                "socket parent directory must be owned by current euid \
                 (= 別 user 所有の dir を --socket で指定した可能性。\
                 hyoui の自動 path (<状態の root>/sessions、HYOUI_STATE_DIR 参照) を使う)",
            ));
        }
        Ok(())
    }

    /// Bind and listen on `path`. Backlog = 5. Sets `umask(0o077)` around
    /// `bind(2)` so the socket file is created mode `0600`.
    ///
    /// 同じ path の重複は bind / name lock の時点で原子的に失敗させる (DR-0041 決定 3)。
    /// name lock (`<name>.lock`) を別のプロセスが持っている、または bind が既存の
    /// socket file に当たる (= 死んだ daemon の socket を含む) と
    /// [`Error::SocketExists`]。bind の前に既存の socket file を消さない
    /// (= 確認してから作る 2 段にしない、生死を判定しない)。
    ///
    /// `path` が `sun_path` に収まらなければ dir の fd 基準の相対名で bind する
    /// (DR-0041 決定 5)。
    pub fn listen<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::check_parent_dir(&path)?;

        let _dir_lock = lock_socket_dir(&path)?;
        let lock_path = path.with_extension("lock");
        let lock_existed = lock_path.symlink_metadata().is_ok();
        let lock = match acquire_name_lock(&lock_path) {
            Ok(lock) => lock,
            Err(Error::Errno(nix::errno::Errno::EWOULDBLOCK)) => {
                return Err(Error::SocketExists(path));
            }
            Err(e) => return Err(e),
        };

        let mut bound = false;
        let result = (|| -> Result<OwnedFd> {
            let fd = socket::socket(
                AddressFamily::Unix,
                SockType::Stream,
                SockFlag::empty(),
                None,
            )
            .map_err(Error::from)?;
            set_cloexec(&fd)?;
            let _umask = UmaskGuard::set(nix::sys::stat::Mode::from_bits_truncate(0o077));
            sockaddr_op(super::raw::SockAddrOp::Bind, fd.as_fd(), &path)?;
            bound = true;
            drop(_umask);
            socket::listen(&fd, Backlog::new(5).map_err(Error::from)?).map_err(Error::from)?;
            Ok(fd)
        })();
        let fd = match result {
            Ok(fd) => fd,
            Err(e) => {
                if bound {
                    let _ = nix::unistd::unlink(&path);
                }
                // 自分が作った lock file は必ず消す (flock と dir lock を持ったまま)。
                // 片付けの経路 (discovery の prune) は「lock が在って誰も持っていない」
                // ことを socket の持ち主が死んだ根拠にするので、socket を作っていない者が
                // 作った lock を残すと、lock を持たない生きた daemon の socket (= 接続が
                // 拒否される瞬間がある) を死んだと誤って消させてしまう。先にあった lock
                // file (= その socket を作った daemon のもの) は残す。
                if !lock_existed {
                    let _ = nix::unistd::unlink(&lock_path);
                }
                drop(lock);
                return Err(match e {
                    Error::Errno(nix::errno::Errno::EADDRINUSE) => Error::SocketExists(path),
                    other => other,
                });
            }
        };

        Ok(Self {
            fd: Some(fd),
            path: Some(path),
            lock: Some(lock),
        })
    }

    /// Internal helper: unwrap `fd` assuming normal (non-consumed) lifetime.
    fn fd_ref(&self) -> &OwnedFd {
        self.fd
            .as_ref()
            .expect("UnixSock fd accessed after into_parts_for_exec (bug)")
    }

    /// Borrow the listening fd.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd_ref().as_fd()
    }

    /// Bound path.
    pub fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("UnixSock path accessed after into_parts_for_exec (bug)")
    }

    /// DR-0028 Phase 1: 既存 listener fd (= self-exec 前の親から継承した bind 済 fd)
    /// と socket path から `UnixSock` を組み立てる。bind / listen は既に済んでいる
    /// 前提。Drop で socket file を unlink するのは通常挙動と同じ。
    pub fn from_listener_fd(
        fd: OwnedFd,
        path: PathBuf,
        lock: Option<Flock<std::fs::File>>,
    ) -> Self {
        Self {
            fd: Some(fd),
            path: Some(path),
            lock,
        }
    }

    /// DR-0028 upgrade-resume: self-exec で継承した listener fd と name lock から
    /// `UnixSock` を組み立てる。
    ///
    /// - 継承 fd は exec 前に CLOEXEC を解除されているので、通常起動 ([`Self::listen`])
    ///   と同じ CLOEXEC 状態に戻す (= 以後 spawn する process に listener / lock を
    ///   漏らさない。lock が漏れると daemon 死亡後も lock が残り prune が効かなくなる)。
    /// - `lock` が `None` (= name lock を持たない旧 daemon からの upgrade) なら dir lock
    ///   下で `<name>.lock` を取り直す。lock を持たないまま serve すると discovery の
    ///   生存判定・同名 `run` の衝突検出・Drop の socket unlink がいずれも効かない。
    ///   取れなければ (= 別プロセスが同名 lock を保持) warning を出して lock 無しで
    ///   続行する (= upgrade は子を抱えたまま進めるので、ここで失敗させない)。
    pub fn resume_inherited(
        fd: OwnedFd,
        path: PathBuf,
        lock: Option<Flock<std::fs::File>>,
    ) -> Self {
        if let Err(e) = set_cloexec(&fd) {
            crate::daemon_log!("hyoui: warning: CLOEXEC restore on inherited listener failed: {e}");
        }
        let lock = match lock {
            Some(held) => {
                if let Err(e) = set_cloexec(&*held) {
                    crate::daemon_log!(
                        "hyoui: warning: CLOEXEC restore on inherited daemon lock failed: {e}"
                    );
                }
                Some(held)
            }
            None => {
                let lock_path = path.with_extension("lock");
                match lock_socket_dir(&path).and_then(|_dir_lock| acquire_name_lock(&lock_path)) {
                    Ok(held) => Some(held),
                    Err(e) => {
                        crate::daemon_log!(
                            "hyoui: warning: daemon lock {} を取得できない ({e}); lock 無しで続行",
                            lock_path.display()
                        );
                        None
                    }
                }
            }
        };
        Self::from_listener_fd(fd, path, lock)
    }

    /// DR-0028 Phase 1/3: self-exec 直前に fd + path を取り出す。**socket file の
    /// unlink を行わない** (= exec 後の新プロセスが同じ path で listener を継続使用
    /// するため)。Phase 3 で `Option::take` 方式に変更し `unsafe` を排除
    /// (`just lint-unsafe` 遵守)。
    ///
    /// 取り出し後は `self` の Drop が走っても both fields が `None` なので unlink
    /// は発生しない。exec 経路以外で使うと socket file が新プロセス側でも継承されず
    /// leak するので、DR-0028 upgrade path 専用。
    pub fn into_parts_for_exec(mut self) -> (OwnedFd, PathBuf, Option<Flock<std::fs::File>>) {
        let fd = self
            .fd
            .take()
            .expect("UnixSock::into_parts_for_exec called twice (bug)");
        let path = self
            .path
            .take()
            .expect("UnixSock::into_parts_for_exec called twice (bug)");
        let lock = self.lock.take();
        (fd, path, lock)
    }

    /// `accept(2)` + `FD_CLOEXEC` set via fcntl. Returns the client fd.
    pub fn accept(&self) -> Result<OwnedFd> {
        let raw_fd = socket::accept(self.fd_ref().as_raw_fd()).map_err(Error::from)?;
        // L6: brief window between accept and fcntl. hyoui is single-threaded
        // so no realistic race.
        let owned = crate::sys::raw::own_raw_fd(raw_fd);
        set_cloexec(&owned)?;
        Ok(owned)
    }
}

impl Drop for UnixSock {
    fn drop(&mut self) {
        if let Some(p) = self.path.as_ref()
            && let Ok(_dir_lock) = lock_socket_dir(p)
        {
            let same_lock = self.lock.as_ref().is_some_and(|lock| {
                std::fs::metadata(p.with_extension("lock"))
                    .and_then(|meta| lock.metadata().map(|held| meta.ino() == held.ino()))
                    .unwrap_or(false)
            });
            if same_lock {
                let _ = nix::unistd::unlink(p);
                let _ = nix::unistd::unlink(&p.with_extension("lock"));
            }
        }
    }
}

/// Return the process ID of the peer connected to a Unix-domain stream.
///
/// # Errors
///
/// Returns an I/O error when the operating system cannot provide peer credentials.
pub fn peer_pid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<u32> {
    #[cfg(target_os = "macos")]
    let pid = socket::getsockopt(stream, socket::sockopt::LocalPeerPid)?;
    #[cfg(target_os = "linux")]
    let pid = socket::getsockopt(stream, socket::sockopt::PeerCredentials)?.pid();
    u32::try_from(pid)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid peer PID"))
}

/// Connect a fresh Unix-domain socket to `path`. Returns the connected fd.
///
/// `path` が `sun_path` に収まらなければ dir の fd 基準の相対名で connect する
/// (DR-0041 決定 5)。
pub fn connect<P: AsRef<Path>>(path: P) -> Result<OwnedFd> {
    let fd = socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .map_err(Error::from)?;
    // L6: set FD_CLOEXEC on client fd (portable).
    set_cloexec(&fd)?;
    sockaddr_op(super::raw::SockAddrOp::Connect, fd.as_fd(), path.as_ref())?;
    Ok(fd)
}

/// Connect like [`connect`], but fail instead of waiting when the listener's
/// backlog is full. Returns a blocking fd on success.
///
/// A full backlog is reported as `ECONNREFUSED` on macOS and `EAGAIN` on Linux
/// (Linux blocks a blocking AF_UNIX connect until the peer accepts, with no timeout).
pub fn connect_no_wait<P: AsRef<Path>>(path: P) -> Result<OwnedFd> {
    let fd = socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .map_err(Error::from)?;
    set_cloexec(&fd)?;
    let flags = OFlag::from_bits_retain(fcntl(&fd, FcntlArg::F_GETFL).map_err(Error::from)?);
    // O_NONBLOCK は open file description の flag なので、fork した子が connect する
    // 経路 (`sockaddr_op`) でも効く。
    fcntl(&fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(Error::from)?;
    sockaddr_op(super::raw::SockAddrOp::Connect, fd.as_fd(), path.as_ref())?;
    fcntl(&fd, FcntlArg::F_SETFL(flags)).map_err(Error::from)?;
    Ok(fd)
}

/// [`connect`] して `std::os::unix::net::UnixStream` にする (= 長いパスでも届く口)。
///
/// # Errors
///
/// [`connect`] と同じ。
pub fn connect_stream<P: AsRef<Path>>(path: P) -> Result<std::os::unix::net::UnixStream> {
    connect(path).map(std::os::unix::net::UnixStream::from)
}

/// `path` に bind して listen する `std::os::unix::net::UnixListener` を作る
/// (= 長いパスでも bind できる口、DR-0041 決定 5)。
///
/// [`UnixSock::listen`] と違い、親 dir の検査・name lock・`umask` は持たない
/// (= 呼び出し側が置き場と排他を持つ socket 用)。既にファイルがあれば `EADDRINUSE`。
///
/// # Errors
///
/// socket の作成・bind・listen が失敗した時。
pub fn bind_listener<P: AsRef<Path>>(path: P) -> Result<std::os::unix::net::UnixListener> {
    let fd = socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .map_err(Error::from)?;
    set_cloexec(&fd)?;
    sockaddr_op(super::raw::SockAddrOp::Bind, fd.as_fd(), path.as_ref())?;
    socket::listen(&fd, Backlog::new(128).map_err(Error::from)?).map_err(Error::from)?;
    Ok(std::os::unix::net::UnixListener::from(fd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_0700_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir.path(), perms).expect("chmod 0700");
        dir
    }

    #[test]
    fn listen_creates_socket_and_unlinks_on_drop() {
        let dir = make_0700_dir();
        let path = dir.path().join("test.sock");
        let sock = UnixSock::listen(&path).expect("listen");
        assert!(path.exists());
        assert!(path.with_extension("lock").exists());
        assert!(dir.path().join(".dir.lock").exists());
        drop(sock);
        assert!(!path.exists(), "Drop should unlink the socket file");
        assert!(!path.with_extension("lock").exists());
        assert!(dir.path().join(".dir.lock").exists());
    }

    /// 別 open file description から `LOCK_EX|LOCK_NB` を試す (= discovery の prune 判定と同じ)。
    fn name_lock_is_held_elsewhere(lock_path: &Path) -> bool {
        let file = std::fs::File::open(lock_path).expect("open lock");
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(_) => false,
            Err((_, nix::errno::Errno::EWOULDBLOCK)) => true,
            Err((_, e)) => panic!("unexpected flock error: {e}"),
        }
    }

    fn has_cloexec<F: AsFd>(fd: &F) -> bool {
        let flags = fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD");
        FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC)
    }

    /// DR-0028 upgrade: exec 前に CLOEXEC を外して継承した listener / name lock を
    /// `resume_inherited` が同じ lock (inode) のまま保持し、CLOEXEC を戻す。
    #[test]
    fn resume_inherited_keeps_lock_and_restores_cloexec() {
        let dir = make_0700_dir();
        let path = dir.path().join("up.sock");
        let lock_path = path.with_extension("lock");
        let (fd, sock_path, lock) = UnixSock::listen(&path)
            .expect("listen")
            .into_parts_for_exec();
        let lock = lock.expect("listen holds name lock");
        let ino = lock.metadata().expect("lock metadata").ino();
        crate::sys::clear_cloexec(&fd).expect("clear listener cloexec");
        crate::sys::clear_cloexec(&*lock).expect("clear lock cloexec");

        let sock = UnixSock::resume_inherited(fd, sock_path, Some(lock));
        assert!(name_lock_is_held_elsewhere(&lock_path));
        assert_eq!(std::fs::metadata(&lock_path).expect("lock file").ino(), ino);
        assert!(
            has_cloexec(&sock.as_fd()),
            "listener CLOEXEC must be restored"
        );
        assert!(
            has_cloexec(&**sock.lock.as_ref().expect("lock kept")),
            "lock CLOEXEC must be restored"
        );

        drop(sock);
        assert!(!path.exists());
        assert!(!lock_path.exists());
    }

    /// name lock を持たない旧 daemon からの upgrade (= lock fd 未継承) では
    /// `resume_inherited` が `<name>.lock` を取り直し、生存判定と Drop の unlink を効かせる。
    #[test]
    fn resume_inherited_reacquires_missing_name_lock() {
        let dir = make_0700_dir();
        let path = dir.path().join("old.sock");
        let lock_path = path.with_extension("lock");
        let (fd, sock_path, lock) = UnixSock::listen(&path)
            .expect("listen")
            .into_parts_for_exec();
        drop(lock);
        std::fs::remove_file(&lock_path).expect("simulate lock-less old daemon");

        let sock = UnixSock::resume_inherited(fd, sock_path, None);
        assert!(lock_path.exists(), "name lock file must be recreated");
        assert!(name_lock_is_held_elsewhere(&lock_path));
        assert!(has_cloexec(&**sock.lock.as_ref().expect("lock reacquired")));

        drop(sock);
        assert!(!path.exists(), "Drop must unlink socket once lock is held");
        assert!(!lock_path.exists());
    }

    #[test]
    fn listen_rejects_world_writable_parent() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod 0755");
        let path = dir.path().join("nope.sock");
        let err = UnixSock::listen(&path).expect_err("expected Precondition");
        assert!(matches!(err, Error::Precondition(_)));
    }

    /// R5-FB5: parent mode 0700 違反時のエラー文言が next-action hint を
    /// 含むこと (= 旧版は「must be mode 0700」だけで、初見ユーザがどこを
    /// 直せばいいかわからなかった)。文言の正本は `check_parent_dir` の
    /// `Error::Precondition` リテラル。
    #[test]
    fn socket_parent_mode_error_includes_hint() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod 0755");
        let path = dir.path().join("nohint.sock");
        let err = UnixSock::listen(&path).expect_err("expected Precondition");
        let msg = format!("{err}");
        // 基本メッセージ
        assert!(
            msg.contains("mode 0700"),
            "error must mention mode 0700; got: {msg}"
        );
        // hint: 推奨 dir + 直し方
        assert!(
            msg.contains("<state root>/sessions") && msg.contains("HYOUI_STATE_DIR"),
            "error must hint at hyoui's own socket dir; got: {msg}"
        );
        assert!(
            msg.contains("chmod 700"),
            "error must hint at `chmod 700`; got: {msg}"
        );
    }

    /// 同じ path で 2 つ目の listen は name lock の時点で失敗し、1 つ目の socket と
    /// lock には触らない (DR-0041 決定 3)。
    #[test]
    fn second_listen_on_the_same_path_fails_and_leaves_the_first_alone() {
        let dir = make_0700_dir();
        let path = dir.path().join("dup.sock");
        let first = UnixSock::listen(&path).expect("first listen");
        let err = UnixSock::listen(&path).expect_err("second listen must fail");
        assert!(
            matches!(&err, Error::SocketExists(p) if p == &path),
            "err: {err:?}"
        );
        assert!(path.exists(), "the first socket must stay");
        assert!(name_lock_is_held_elsewhere(&path.with_extension("lock")));
        let _client = connect(&path).expect("the first listener still accepts connections");
        drop(first);
    }

    /// daemon が死んで socket file だけ残っている (= lock は誰も持っていない) 場合も、
    /// listen は既存の socket file を消さずに失敗する (DR-0041 決定 3)。片付けの経路が
    /// 死んだと判断できるよう、lock file は残す。
    #[test]
    fn listen_on_a_dead_socket_fails_without_removing_it() {
        let dir = make_0700_dir();
        let path = dir.path().join("dead.sock");
        let lock_path = path.with_extension("lock");
        drop(std::os::unix::net::UnixListener::bind(&path).expect("leave a dead socket"));
        std::fs::File::create(&lock_path).expect("dead daemon's lock file");
        let ino = std::fs::metadata(&path).unwrap().ino();

        let err = UnixSock::listen(&path).expect_err("must fail on the dead socket");
        assert!(matches!(err, Error::SocketExists(_)), "err: {err:?}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            ino,
            "the dead socket must not be replaced"
        );
        assert!(
            lock_path.exists(),
            "the lock file must stay for the cleanup path"
        );
        assert!(!name_lock_is_held_elsewhere(&lock_path));
    }

    /// lock file の無い socket に当たった時も消さずに失敗し、自分が作った lock file は
    /// 残さない (= lock を持たない生きた daemon を、片付けの経路に死んだと読ませない)。
    #[test]
    fn listen_on_a_lockless_socket_fails_without_leaving_a_lock_file() {
        let dir = make_0700_dir();
        let path = dir.path().join("orphan.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).expect("leave an orphan socket"));
        let err = UnixSock::listen(&path).expect_err("must fail on the orphan socket");
        assert!(matches!(err, Error::SocketExists(_)), "err: {err:?}");
        assert!(path.exists());
        assert!(!path.with_extension("lock").exists());
    }

    /// bind が重複以外の理由で失敗した時、自分が作った lock file は残さない。
    #[test]
    fn a_failed_listen_removes_only_its_own_lock_file() {
        let dir = make_0700_dir();
        // ファイル名だけで sun_path を超える = bind が ENAMETOOLONG で失敗する。
        let name = format!("{}.sock", "n".repeat(sun_path_max()));
        let path = dir.path().join(name);
        let err = UnixSock::listen(&path).expect_err("too long a file name");
        assert!(
            matches!(err, Error::Errno(nix::errno::Errno::ENAMETOOLONG)),
            "err: {err:?}"
        );
        assert!(!path.exists());
        assert!(!path.with_extension("lock").exists());
    }

    /// `sun_path` に収まらない深い dir を作る (= フルパスが上限を超える)。
    fn deep_0700_dir() -> (TempDir, PathBuf) {
        let base = make_0700_dir();
        let mut deep = base.path().to_path_buf();
        while deep.as_os_str().len() <= sun_path_max() + 8 {
            deep.push("deep-directory-segment");
        }
        std::fs::create_dir_all(&deep).expect("mkdir deep");
        std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o700)).unwrap();
        (base, deep)
    }

    /// フルパスが `sun_path` の上限を超えても、dir の fd 基準の相対名で bind / connect
    /// できる (DR-0041 決定 5)。親の cwd は変わらない。
    #[test]
    fn a_path_longer_than_sun_path_binds_and_connects() {
        let (_base, deep) = deep_0700_dir();
        let path = deep.join("long.sock");
        assert!(!fits_sun_path(&path), "the test path must exceed sun_path");
        let cwd_before = std::env::current_dir().unwrap();

        let server = UnixSock::listen(&path).expect("listen on a long path");
        assert!(path.exists(), "the socket file is created at the full path");
        let mode = std::fs::symlink_metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "umask 077 applies in the forked child: {mode:o}"
        );
        let client = connect(&path).expect("connect to a long path");
        let accepted = server.accept().expect("accept the long-path client");
        assert_eq!(std::env::current_dir().unwrap(), cwd_before);

        // 繋がった 2 つの fd の間で bytes が通る。
        nix::unistd::write(&client, b"x").expect("write");
        let mut buf = [0u8; 1];
        assert_eq!(nix::unistd::read(&accepted, &mut buf).expect("read"), 1);
        assert_eq!(&buf, b"x");

        // 重複判定も長いパスで効く。
        let err = UnixSock::listen(&path).expect_err("duplicate on a long path");
        assert!(matches!(err, Error::SocketExists(_)), "err: {err:?}");

        let _ = connect_no_wait(&path).expect("connect_no_wait to a long path");
        drop(server);
        assert!(!path.exists());
        let err = connect(&path).expect_err("nothing listens any more");
        assert!(
            matches!(err, Error::Errno(nix::errno::Errno::ENOENT)),
            "err: {err:?}"
        );
    }

    /// 長いパスの `bind_listener` / `connect_stream` も届く (= 監督者の制御 socket 用)。
    #[test]
    fn std_wrappers_reach_a_long_path() {
        use std::io::{Read, Write};
        let (_base, deep) = deep_0700_dir();
        let path = deep.join("std.sock");
        let listener = bind_listener(&path).expect("bind_listener");
        let mut client = connect_stream(&path).expect("connect_stream");
        let (mut accepted, _) = listener.accept().expect("accept");
        client.write_all(b"ok").unwrap();
        let mut buf = [0u8; 2];
        accepted.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ok");
        let err = bind_listener(&path).expect_err("the file already exists");
        assert!(
            matches!(err, Error::Errno(nix::errno::Errno::EADDRINUSE)),
            "err: {err:?}"
        );
    }

    /// 長いパスの bind を複数スレッドから同時に行っても、各スレッドが自分の socket を
    /// bind する (= fork した子だけが cwd を変え、親のスレッドの相対パス解決を巻き込まない)。
    #[test]
    fn long_path_binds_from_many_threads_do_not_interfere() {
        let (_base, deep) = deep_0700_dir();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = deep.join(format!("t{i}.sock"));
                std::thread::spawn(move || {
                    let sock = UnixSock::listen(&path).expect("listen");
                    let _c = connect(&path).expect("connect");
                    sock.accept().expect("accept");
                    drop(sock);
                    path
                })
            })
            .collect();
        for h in handles {
            let path = h.join().expect("thread");
            assert!(!path.exists());
        }
    }

    #[test]
    fn connect_accept_roundtrip() {
        let dir = make_0700_dir();
        let path = dir.path().join("rt.sock");
        let server = UnixSock::listen(&path).expect("listen");
        use crate::sys::fd::FdExt;
        server.as_fd().set_nonblocking(true).expect("nonblock");
        let _client = connect(&path).expect("connect");
        let mut accepted = None;
        for _ in 0..20 {
            match server.accept() {
                Ok(fd) => {
                    accepted = Some(fd);
                    break;
                }
                Err(Error::Errno(nix::errno::Errno::EAGAIN)) => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("accept error: {e:?}"),
            }
        }
        assert!(accepted.is_some(), "accept never produced a fd");
    }

    #[test]
    fn accepted_fd_has_cloexec_set() {
        let dir = make_0700_dir();
        let path = dir.path().join("cloexec.sock");
        let server = UnixSock::listen(&path).expect("listen");
        use crate::sys::fd::FdExt;
        server.as_fd().set_nonblocking(true).expect("nonblock");
        let _client = connect(&path).expect("connect");
        for _ in 0..20 {
            match server.accept() {
                Ok(fd) => {
                    let flags = fcntl(&fd, FcntlArg::F_GETFD).expect("F_GETFD");
                    let fdflag = FdFlag::from_bits_truncate(flags);
                    assert!(
                        fdflag.contains(FdFlag::FD_CLOEXEC),
                        "accepted fd should have FD_CLOEXEC"
                    );
                    return;
                }
                Err(Error::Errno(nix::errno::Errno::EAGAIN)) => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("accept error: {e:?}"),
            }
        }
        panic!("accept never produced a fd");
    }
}
