//! Pseudo-terminal pair + child spawn.
//!
//! Two flows:
//!
//! 1. [`Pty::open`] — `openpty(3)` for tests / scenarios that need a
//!    master+slave pair without forking (e.g. winsize tests).
//!
//! 2. [`Pty::spawn`] — `forkpty(3)` + `execvp(3)`. `forkpty` internally calls
//!    `login_tty(3)` in the child, so the new process gets a controlling
//!    terminal *deterministically*. This is the central reason DR-0001's
//!    two-axis job control works (without a separate `TIOCSCTTY` step that
//!    races against `setsid`).
//!
//! Note: `forkpty` opens its own PTY pair. We deliberately do NOT chain
//! `Pty::open` into `Pty::spawn` because that would leak a second master fd.
//!
//! Why `forkpty` rather than `posix_spawn + SETSID + TIOCSCTTY` (the bootstrap
//! design): `posix_spawn` cannot do `login_tty`, so the child has to acquire
//! its ctty manually, which is racy on macOS. See
//! `docs/journal/2026-05-22-rust-rewrite.md`.

use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

use nix::pty::{OpenptyResult, Winsize};

use super::error::{Error, Result};
use super::raw;
use super::signal as sig;

/// Owned PTY master, optionally with a slave kept by the parent.
///
/// * [`Pty::open`] returns one with `slave = Some(_)`.
/// * [`Pty::spawn`] returns one with `slave = None` (the child holds it via
///   `login_tty`).
///
/// Both fields are `Option` so [`Pty::into_master`] can move them out via
/// `Option::take` without needing any `unsafe`-based ManuallyDrop / ptr::read
/// trick (the `Drop` impl otherwise forbids destructuring `self`).
#[derive(Debug)]
pub struct Pty {
    master: Option<OwnedFd>,
    slave: Option<OwnedFd>,
}

/// Result of [`Pty::spawn`].
#[derive(Debug)]
pub struct Spawned {
    /// Master side of the PTY. Slave is owned by the child.
    pub pty: Pty,
    /// Child process ID.
    pub child: nix::unistd::Pid,
}

impl Pty {
    fn master_owned(&self) -> &OwnedFd {
        // Both `master` Options are `Some` for the lifetime of any Pty
        // produced by `open`/`spawn`; only `into_master` (which consumes
        // `self`) takes them out. Panicking here would indicate a bug.
        self.master.as_ref().expect("Pty master fd already taken")
    }

    /// `openpty(3)` — open a fresh master/slave pair sized `(cols, rows)`.
    /// No fork happens.
    pub fn open(cols: u16, rows: u16) -> Result<Self> {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let OpenptyResult { master, slave } = nix::pty::openpty(&ws, None).map_err(Error::from)?;
        Ok(Self {
            master: Some(master),
            slave: Some(slave),
        })
    }

    /// `forkpty(3) + execvp(3)`. Opens a new PTY, forks, and in the child
    /// calls `login_tty` (via `forkpty`) and then `execvp(argv[0], argv)`.
    ///
    /// Returns the parent-side [`Spawned`]. The child path **does not
    /// return** — it always execs or `_exit(127)`s.
    pub fn spawn(argv: &[&str], cols: u16, rows: u16) -> Result<Spawned> {
        if argv.is_empty() {
            return Err(Error::Invalid("argv must not be empty"));
        }
        let argv_c: Vec<CString> = argv
            .iter()
            .map(|s| CString::new(*s).map_err(|_| Error::Invalid("argv contained NUL")))
            .collect::<Result<_>>()?;
        let forked = raw::forkpty_then_exec(&argv_c, cols, rows)?;
        Ok(Spawned {
            pty: Self {
                master: Some(forked.master),
                slave: None,
            },
            child: forked.child,
        })
    }

    /// Borrow the master fd. Always valid while `self` lives.
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master_owned().as_fd()
    }

    /// Borrow the slave fd, if still held by the parent.
    pub fn slave_fd(&self) -> Option<BorrowedFd<'_>> {
        self.slave.as_ref().map(|fd| fd.as_fd())
    }

    /// Consume `self`, returning the owned master fd.
    pub fn into_master(mut self) -> OwnedFd {
        // Run the SIGWINCH-clear side effect before Drop strips it, so
        // a late SIGWINCH cannot target a fd that is now owned by the
        // caller (and might be closed independently).
        sig::clear_winch_if(self.master_owned().as_raw_fd());
        // Take master out; slave (if any) Drops normally via Pty::drop.
        self.master.take().expect("Pty master fd already taken")
    }

    /// Apply a new window size via `TIOCSWINSZ` on the master.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        raw::ioctl_set_winsize(self.master_fd(), cols, rows)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(m) = self.master.as_ref() {
            sig::clear_winch_if(m.as_raw_fd());
        }
        // master and slave (Options of OwnedFd) close via their normal Drop.
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_returns_valid_handle() {
        // mirrors ffi_wbtest.mbt: "pty_open: returns valid handle"
        let pty = Pty::open(80, 24).expect("open");
        assert!(pty.master_fd().as_raw_fd() >= 0);
    }

    #[test]
    fn master_fd_is_valid() {
        // mirrors ffi_wbtest.mbt: "pty_master_fd: returns valid fd"
        let pty = Pty::open(80, 24).expect("open");
        assert!(pty.master_fd().as_raw_fd() >= 0);
    }

    #[test]
    fn drop_succeeds() {
        // mirrors ffi_wbtest.mbt: "pty_close: succeeds"
        let pty = Pty::open(80, 24).expect("open");
        drop(pty);
    }

    #[test]
    fn spawn_with_empty_argv_errors() {
        // mirrors ffi_wbtest.mbt: "pty_spawnv: empty argv returns None"
        let err = Pty::spawn(&[], 80, 24).expect_err("expected Invalid");
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn resize_succeeds() {
        // mirrors ffi_wbtest.mbt: "pty_resize: succeeds on valid handle"
        let pty = Pty::open(80, 24).expect("open");
        pty.resize(120, 40).expect("resize");
    }
}
