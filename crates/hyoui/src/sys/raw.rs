//! `unsafe` boundary #1 — direct libc calls and `unsafe` `nix` calls that
//! cannot be expressed through `nix`'s safe API alone.
//!
//! Every `unsafe` block in the crate (outside [`super::signal`]) lives here.
//! Functions exported from this module are safe to call from elsewhere
//! because each `unsafe` body is paired with a `SAFETY:` justification.
//!
//! Contents:
//!
//! * [`ioctl_get_winsize`] / [`ioctl_set_winsize`] — `TIOCGWINSZ` /
//!   `TIOCSWINSZ` against any fd.
//!
//! * [`forkpty_then_exec`] — `forkpty(3)` + `execvp(3)`. The child path is
//!   async-signal-safe (no allocation, no locks; on `execvp` failure it
//!   calls `_exit(127)`).
//!
//! * [`borrow_raw_fd`] and [`own_raw_fd`] — thin wrappers around
//!   `BorrowedFd::borrow_raw` / `OwnedFd::from_raw_fd` so the rest of the
//!   crate never spells those operations directly.

use std::ffi::CString;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use nix::errno::Errno;
use nix::pty::{ForkptyResult, Winsize};
use nix::unistd::{self, Pid};

use super::error::{Error, Result};

// ---------------------------------------------------------------------------
// ioctl(TIOC{G,S}WINSZ)
// ---------------------------------------------------------------------------

/// Read the current window size for `fd`.
pub fn ioctl_get_winsize(fd: BorrowedFd<'_>) -> Result<libc::winsize> {
    // SAFETY: `ws` is fully overwritten by `ioctl` on success; on failure we
    // never read it. `fd` is borrowed valid by the type system.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
    if r == -1 {
        return Err(Error::Errno(Errno::last()));
    }
    Ok(ws)
}

/// Apply the given (`cols`, `rows`) to the terminal behind `fd`.
pub fn ioctl_set_winsize(fd: BorrowedFd<'_>, cols: u16, rows: u16) -> Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `&ws` outlives the call; `fd` is borrowed valid; the ioctl
    // number is the canonical `TIOCSWINSZ`.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    if r == -1 {
        return Err(Error::Errno(Errno::last()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// forkpty + execvp
// ---------------------------------------------------------------------------

/// Result of [`forkpty_then_exec`] in the parent process.
#[derive(Debug)]
pub struct ForkedChild {
    /// PID of the new child process.
    pub child: Pid,
    /// PTY master fd owned by the parent.
    pub master: OwnedFd,
}

/// `forkpty(3)` + (in the child) `execvp(3)` with the given argv.
///
/// `forkpty` internally calls `login_tty(3)`, so the child acquires a
/// controlling terminal deterministically without a separate `TIOCSCTTY`
/// step. This is the only place in the crate that calls
/// `nix::pty::forkpty` or `libc::_exit`. The child path is
/// async-signal-safe.
pub fn forkpty_then_exec(argv: &[CString], cols: u16, rows: u16) -> Result<ForkedChild> {
    if argv.is_empty() {
        return Err(Error::Invalid("argv must not be empty"));
    }
    let ws = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: `nix::pty::forkpty` is documented `unsafe` because in the
    // child path only async-signal-safe code may run. Between fork and exec
    // we only call `execvp` (async-signal-safe) and on its failure
    // `_exit(127)` (async-signal-safe). No allocation, no locks, no Rust
    // destructors.
    let result = unsafe { nix::pty::forkpty(&ws, None) }.map_err(Error::from)?;
    match result {
        ForkptyResult::Parent { child, master } => Ok(ForkedChild { child, master }),
        ForkptyResult::Child => {
            // execvp returns only on failure.
            let _ = unistd::execvp(&argv[0], argv);
            // SAFETY: `_exit` is async-signal-safe and never returns; we
            // must NOT run Rust destructors in the child.
            unsafe { libc::_exit(127) };
        }
    }
}

// ---------------------------------------------------------------------------
// fd-wrapping helpers
// ---------------------------------------------------------------------------

/// Take ownership of a raw fd. Caller must guarantee `raw` is a valid,
/// currently-open fd with no other Rust owner.
///
/// `unsafe` を呼び出し側に伝播させないための薄い wrapper。`hyoui-cli` の
/// `forbid(unsafe_code)` 下で daemon の parent → child fd 継承を扱う等に使う。
pub fn own_raw_fd(raw: RawFd) -> OwnedFd {
    // SAFETY: delegated to the caller's contract.
    unsafe { OwnedFd::from_raw_fd(raw) }
}

/// Borrow a fd by raw integer. Caller must guarantee the fd outlives the
/// returned `BorrowedFd`. Used by tests that exercise error paths on
/// closed fds.
#[allow(dead_code)] // only consumed by `#[cfg(test)]` callers
pub(crate) fn borrow_raw_fd<'a>(raw: RawFd) -> BorrowedFd<'a> {
    // SAFETY: delegated to the caller's contract.
    unsafe { BorrowedFd::borrow_raw(raw) }
}
