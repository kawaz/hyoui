//! Terminal control: raw-mode entry + size query + RAII restore.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use nix::sys::termios::{self, SetArg, Termios};

use super::error::{Error, Result};
use super::raw;

/// Terminal `(rows, cols)` returned by [`tty_size`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    /// Number of rows (lines) the terminal can display.
    pub rows: u16,
    /// Number of columns (characters per line) the terminal can display.
    pub cols: u16,
}

/// `isatty(3)`.
pub fn is_tty(fd: BorrowedFd<'_>) -> bool {
    nix::unistd::isatty(fd).unwrap_or(false)
}

/// `ioctl(fd, TIOCGWINSZ)`. Returns `None` if `fd` is not a terminal.
pub fn tty_size(fd: BorrowedFd<'_>) -> Result<Option<WinSize>> {
    if !is_tty(fd) {
        return Ok(None);
    }
    let ws = raw::ioctl_get_winsize(fd)?;
    Ok(Some(WinSize {
        rows: ws.ws_row,
        cols: ws.ws_col,
    }))
}

/// RAII guard for terminal raw-mode. On Drop the saved termios is restored
/// with `TCSAFLUSH`.
///
/// `fd` is held in an `Option` so [`TtyGuard::into_inner_keep_raw`] can move
/// it out via `Option::take` without needing `unsafe` ManuallyDrop tricks.
#[derive(Debug)]
pub struct TtyGuard {
    fd: Option<OwnedFd>,
    saved: Termios,
}

impl TtyGuard {
    /// Borrow the underlying fd.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_ref().expect("TtyGuard fd already taken").as_fd()
    }

    /// Recover the wrapped owned fd, *without* restoring termios. Useful
    /// when the caller explicitly wants to keep raw mode (the caller takes
    /// over responsibility for restoration).
    pub fn into_inner_keep_raw(mut self) -> OwnedFd {
        // Take the fd out so Drop's tcsetattr below becomes a no-op.
        self.fd.take().expect("TtyGuard fd already taken")
    }
}

impl Drop for TtyGuard {
    fn drop(&mut self) {
        if let Some(fd) = self.fd.as_ref() {
            // Best-effort restore; the fd is closed right after by OwnedFd::drop.
            let _ = termios::tcsetattr(fd.as_fd(), SetArg::TCSAFLUSH, &self.saved);
        }
    }
}

/// Put the terminal at `fd` into raw mode (`cfmakeraw`), returning a guard
/// that restores the previous termios on Drop.
///
/// `fd` is consumed (owned) so the guard can deterministically restore on
/// Drop even if the caller forgets to keep a reference.
pub fn enter_raw(fd: OwnedFd) -> Result<TtyGuard> {
    let saved = termios::tcgetattr(fd.as_fd()).map_err(Error::from)?;
    let mut raw_t = saved.clone();
    termios::cfmakeraw(&mut raw_t);
    // R5-M9: `cfmakeraw` clears `IUTF8`. Re-enable it on platforms that have
    // it (Linux/Android, Apple) so multi-byte UTF-8 input (Japanese etc.)
    // is erased a full code point at a time on Backspace, instead of one
    // byte at a time which corrupts the display.
    //
    // Design rationale: ICANON is off in raw mode so the kernel does not
    // run BS itself, but `IUTF8` still influences in-kernel line discipline
    // helpers used by some terminals/screen readers; keeping it set matches
    // the cooked-mode default and is a cheap defense-in-depth.
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        raw_t.input_flags |= termios::InputFlags::IUTF8;
    }
    termios::tcsetattr(fd.as_fd(), SetArg::TCSAFLUSH, &raw_t).map_err(Error::from)?;
    Ok(TtyGuard {
        fd: Some(fd),
        saved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Produce a `BorrowedFd` pointing at a known-closed fd so a syscall
    /// against it returns `EBADF`. We cannot use `BorrowedFd::borrow_raw(-1)`
    /// because that panics in modern std; the helper in [`raw`] is the
    /// crate's single `unsafe` entrypoint for this.
    fn closed_fd_borrow() -> BorrowedFd<'static> {
        use std::os::fd::AsRawFd;
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        let raw_fd = rd.as_fd().as_raw_fd();
        drop(rd);
        raw::borrow_raw_fd(raw_fd)
    }

    #[test]
    fn is_tty_on_closed_fd_is_false() {
        // mirrors ffi_wbtest.mbt: "tty_is_tty: invalid fd returns false"
        assert!(!is_tty(closed_fd_borrow()));
    }

    #[test]
    fn is_tty_on_stdin_does_not_crash() {
        // mirrors ffi_wbtest.mbt: "tty_is_tty: does not crash on stdin"
        let stdin = std::io::stdin();
        let _ = is_tty(stdin.as_fd());
    }

    #[test]
    fn enter_raw_and_restore_on_pty_master() {
        // mirrors ffi_wbtest.mbt:
        // "tty_set_raw and tty_restore: works on pty master fd"
        let pty = crate::sys::pty::Pty::open(80, 24).expect("open pty");
        let master = pty.into_master();
        let guard = enter_raw(master).expect("enter raw");
        drop(guard); // restore on drop
    }

    #[test]
    fn tty_size_on_closed_fd_returns_none() {
        // mirrors ffi_wbtest.mbt: "tty_size: invalid fd returns None"
        let s = tty_size(closed_fd_borrow()).expect("ok");
        assert!(s.is_none(), "expected None for closed fd, got {s:?}");
    }

    #[test]
    fn tty_size_on_stdin_does_not_panic() {
        // mirrors ffi_wbtest.mbt: "tty_size: returns None or valid size"
        let stdin = std::io::stdin();
        match tty_size(stdin.as_fd()).expect("ok") {
            None => {}
            Some(ws) => {
                assert!(ws.rows > 0);
                assert!(ws.cols > 0);
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn enter_raw_sets_iutf8_on_supported_platforms() {
        // R5-M9: cfmakeraw drops IUTF8; enter_raw must re-set it so multi-byte
        // input is erased a whole code point at a time. We verify by reading
        // back termios from the master while the guard is active.
        let pty = crate::sys::pty::Pty::open(80, 24).expect("open pty");
        let master = pty.into_master();
        let guard = enter_raw(master).expect("enter raw");
        let cur = termios::tcgetattr(guard.fd()).expect("tcgetattr");
        assert!(
            cur.input_flags.contains(termios::InputFlags::IUTF8),
            "IUTF8 must be set after enter_raw on UTF-8-aware platforms",
        );
    }

    #[test]
    fn enter_raw_invalid_fd_errors() {
        // mirrors ffi_wbtest.mbt: "tty_save: invalid fd returns None"
        // We model it as: tcgetattr on a non-tty fd fails with ENOTTY.
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        let err = enter_raw(rd).expect_err("expected ENOTTY");
        if let Error::Errno(e) = err {
            assert_eq!(e, nix::errno::Errno::ENOTTY);
        } else {
            panic!("unexpected error variant");
        }
    }
}
