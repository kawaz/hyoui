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
//!
//! [`UnixSock`] is an RAII wrapper: Drop closes the listening fd and
//! `unlink(2)`s the socket file (best-effort).

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nix::sys::socket::{self, AddressFamily, Backlog, SockFlag, SockType, UnixAddr};

use super::error::{Error, Result};

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
                "socket parent directory must be mode 0700",
            ));
        }
        let euid = nix::unistd::geteuid();
        if nix::unistd::Uid::from_raw(meta.uid()) != euid {
            return Err(Error::Precondition(
                "socket parent directory must be owned by current euid",
            ));
        }
        Ok(())
    }

    /// Bind and listen on `path`. Backlog = 5. Sets `umask(0o077)` around
    /// `bind(2)` so the socket file is created mode `0600`.
    pub fn listen<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::check_parent_dir(&path)?;

        // Unlink stale socket if any. Ignore ENOENT.
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

        let addr = UnixAddr::new(path.as_path()).map_err(Error::from)?;

        // Restrict mode around bind. UmaskGuard restores on drop.
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

    /// `accept(2)`. Returns the client fd.
    pub fn accept(&self) -> Result<OwnedFd> {
        let raw_fd = socket::accept(self.fd.as_raw_fd()).map_err(Error::from)?;
        // `accept` returned a fresh kernel fd; no other Rust owner exists.
        // We funnel the wrapping through `raw::own_raw_fd` so this file
        // remains free of direct `unsafe`.
        Ok(crate::sys::raw::own_raw_fd(raw_fd))
    }
}

impl Drop for UnixSock {
    fn drop(&mut self) {
        // Close fd happens via OwnedFd::drop after this body.
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
    socket::connect(fd.as_raw_fd(), &addr).map_err(Error::from)?;
    Ok(fd)
}

// Needed for socket::bind / socket::connect (they take raw fd).
use std::os::fd::AsRawFd;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_0700_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        // tempfile creates with 0700 on unix already, but be explicit.
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir.path(), perms).expect("chmod 0700");
        dir
    }

    #[test]
    fn listen_creates_socket_and_unlinks_on_drop() {
        // mirrors ffi_wbtest.mbt: "sock_listen: creates socket and cleanup"
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

    #[test]
    fn connect_accept_roundtrip() {
        // mirrors ffi_wbtest.mbt: "sock_connect and sock_accept: roundtrip"
        let dir = make_0700_dir();
        let path = dir.path().join("rt.sock");
        let server = UnixSock::listen(&path).expect("listen");
        // Make the server non-blocking so accept doesn't hang if connect races.
        use crate::sys::fd::FdExt;
        server.as_fd().set_nonblocking(true).expect("nonblock");
        let _client = connect(&path).expect("connect");
        // Loop a few times to catch the case where accept races.
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
}
