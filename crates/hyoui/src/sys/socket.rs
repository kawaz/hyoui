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
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::sys::socket::{self, AddressFamily, Backlog, SockFlag, SockType, UnixAddr};

use super::error::{Error, Result};

/// Set `FD_CLOEXEC` on the given fd via `fcntl` (= portable defense-in-depth
/// for L6; darwin lacks SOCK_CLOEXEC so we cannot rely on socket-time flags).
fn set_cloexec<F: AsFd>(fd: &F) -> Result<()> {
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(Error::from)?;
    Ok(())
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
#[derive(Debug)]
pub struct UnixSock {
    fd: OwnedFd,
    path: PathBuf,
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
                 (use $XDG_RUNTIME_DIR or $TMPDIR, or run `chmod 700 <parent>`)",
            ));
        }
        let euid = nix::unistd::geteuid();
        if nix::unistd::Uid::from_raw(meta.uid()) != euid {
            return Err(Error::Precondition(
                "socket parent directory must be owned by current euid \
                 (= 別 user 所有の dir を --socket で指定した可能性。\
                 hyoui の自動 path ($XDG_RUNTIME_DIR/hyoui or $TMPDIR/hyoui-<uid>) を使う)",
            ));
        }
        Ok(())
    }

    /// Bind and listen on `path`. Backlog = 5. Sets `umask(0o077)` around
    /// `bind(2)` so the socket file is created mode `0600`.
    pub fn listen<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::check_parent_dir(&path)?;

        match nix::unistd::unlink(&path) {
            Ok(()) => {}
            Err(nix::errno::Errno::ENOENT) => {}
            Err(e) => return Err(Error::from(e)),
        }

        let fd = socket::socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .map_err(Error::from)?;
        // L6: set FD_CLOEXEC on listener fd (portable).
        set_cloexec(&fd)?;

        let addr = UnixAddr::new(path.as_path()).map_err(Error::from)?;

        let _umask = UmaskGuard::set(nix::sys::stat::Mode::from_bits_truncate(0o077));
        socket::bind(fd.as_raw_fd(), &addr).map_err(Error::from)?;
        drop(_umask);

        socket::listen(&fd, Backlog::new(5).map_err(Error::from)?).map_err(Error::from)?;

        Ok(Self { fd, path })
    }

    /// Borrow the listening fd.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Bound path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `accept(2)` + `FD_CLOEXEC` set via fcntl. Returns the client fd.
    pub fn accept(&self) -> Result<OwnedFd> {
        let raw_fd = socket::accept(self.fd.as_raw_fd()).map_err(Error::from)?;
        // L6: brief window between accept and fcntl. hyoui is single-threaded
        // so no realistic race.
        let owned = crate::sys::raw::own_raw_fd(raw_fd);
        set_cloexec(&owned)?;
        Ok(owned)
    }
}

impl Drop for UnixSock {
    fn drop(&mut self) {
        let _ = nix::unistd::unlink(&self.path);
    }
}

/// Connect a fresh Unix-domain socket to `path`. Returns the connected fd.
pub fn connect<P: AsRef<Path>>(path: P) -> Result<OwnedFd> {
    let addr = UnixAddr::new(path.as_ref()).map_err(Error::from)?;
    let fd = socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .map_err(Error::from)?;
    // L6: set FD_CLOEXEC on client fd (portable).
    set_cloexec(&fd)?;
    socket::connect(fd.as_raw_fd(), &addr).map_err(Error::from)?;
    Ok(fd)
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
        drop(sock);
        assert!(!path.exists(), "Drop should unlink the socket file");
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
            msg.contains("XDG_RUNTIME_DIR") || msg.contains("TMPDIR"),
            "error must hint at $XDG_RUNTIME_DIR / $TMPDIR; got: {msg}"
        );
        assert!(
            msg.contains("chmod 700"),
            "error must hint at `chmod 700`; got: {msg}"
        );
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
