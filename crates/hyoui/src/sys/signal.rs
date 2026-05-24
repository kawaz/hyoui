//! `unsafe` boundary #2 — `sigaction(2)` installation and async-signal-safe
//! handlers (SIGWINCH forwarder + generic self-pipe handler).
//!
//! Two pieces of mutable global state live here, both `AtomicI32`:
//!
//! * [`WINCH_MASTER_FD`] — destination fd that the SIGWINCH handler should
//!   apply the local terminal size to. Set/cleared via [`install_winch`].
//! * [`SELFPIPE_WRITE_FD`] — write end of the self-pipe. The signal handler
//!   writes the signal number to it; the agent's poll loop drains the
//!   read end.
//!
//! These are required because signal handlers must be async-signal-safe; they
//! cannot take a lock, allocate, or call most of libc. An atomic load + a
//! `write(2)` / `ioctl(2)` is the safe minimum.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicI32, Ordering};

use nix::errno::Errno;
use nix::sys::signal::{SigAction, SigHandler, SigSet, SigmaskHow, Signal};
use nix::unistd;

use super::error::{Error, Result};

/// Master fd that the `SIGWINCH` handler should propagate window resizes to.
/// `-1` when no PTY is registered.
pub(crate) static WINCH_MASTER_FD: AtomicI32 = AtomicI32::new(-1);

/// Write end of the self-pipe. `-1` until [`install_self_pipe`] is called.
static SELFPIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

// ---------------------------------------------------------------------------
// generic install helpers (SIG_IGN, SIG_DFL)
// ---------------------------------------------------------------------------

/// Set `signum`'s disposition to `SIG_IGN`. Empty mask, no flags.
pub fn install_ignore(signum: Signal) -> Result<()> {
    let action = SigAction::new(
        SigHandler::SigIgn,
        nix::sys::signal::SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: SigHandler::SigIgn is a synchronous disposition with no closure
    // captured. `sigaction` for SIG_IGN is documented async-signal-safe and
    // does not violate any Rust invariants.
    unsafe { nix::sys::signal::sigaction(signum, &action) }.map_err(Error::from)?;
    Ok(())
}

/// Restore `signum` to `SIG_DFL`. Used before re-raising SIGTSTP/SIGCONT so the
/// kernel does the stop/continue instead of re-entering our handler.
pub fn install_default(signum: Signal) -> Result<()> {
    let action = SigAction::new(
        SigHandler::SigDfl,
        nix::sys::signal::SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: SigHandler::SigDfl carries no closure and is async-signal-safe.
    unsafe { nix::sys::signal::sigaction(signum, &action) }.map_err(Error::from)?;
    Ok(())
}

/// Synchronously `raise(3)` `signum` on this process.
pub fn raise(signum: Signal) -> Result<()> {
    nix::sys::signal::raise(signum).map_err(Error::from)
}

// ---------------------------------------------------------------------------
// SIGWINCH forwarder
// ---------------------------------------------------------------------------

/// SIGWINCH handler: read this terminal's size from STDIN and forward it to
/// the PTY master stored in `WINCH_MASTER_FD`.
///
/// async-signal-safe: only `ioctl(2)` and an atomic load.
extern "C" fn sigwinch_handler(_sig: libc::c_int) {
    let master_fd = WINCH_MASTER_FD.load(Ordering::Relaxed);
    if master_fd < 0 {
        return;
    }
    // SAFETY: ws is fully overwritten by the first ioctl on success; only
    // forwarded on success. async-signal-safe per POSIX.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 {
        unsafe {
            libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws);
        }
    }
}

/// Install the SIGWINCH forwarder, targeting `master_fd`.
///
/// Replaces the previous target if one was installed. `SA_RESTART` is set so
/// blocking syscalls (other than the agent's `poll`, which uses the self-pipe
/// path) are not interrupted by terminal resizes.
pub fn install_winch(master_fd: BorrowedFd<'_>) -> Result<()> {
    WINCH_MASTER_FD.store(master_fd.as_raw_fd(), Ordering::Relaxed);

    let action = SigAction::new(
        SigHandler::Handler(sigwinch_handler),
        nix::sys::signal::SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: `sigwinch_handler` is async-signal-safe (atomic load + ioctl
    // only). Storing into `WINCH_MASTER_FD` before installation ensures any
    // race-of-first-delivery sees a valid fd.
    let result = unsafe { nix::sys::signal::sigaction(Signal::SIGWINCH, &action) };
    if let Err(errno) = result {
        WINCH_MASTER_FD.store(-1, Ordering::Relaxed);
        return Err(Error::from(errno));
    }
    Ok(())
}

/// Clear the SIGWINCH target if it matches `fd`. Called from `Pty::Drop` to
/// keep the global from pointing at a closed descriptor.
pub(crate) fn clear_winch_if(fd: i32) {
    let _ = WINCH_MASTER_FD.compare_exchange(fd, -1, Ordering::Relaxed, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// self-pipe
// ---------------------------------------------------------------------------

/// Holder of the self-pipe read end. The write end is registered globally and
/// has no Rust owner (the signal handler accesses it via the atomic).
#[derive(Debug)]
pub struct SelfPipe {
    /// Read end. Owned; closed on drop.
    pub read: OwnedFd,
    /// Stored write end; we keep it owned so that `Drop` closes it after the
    /// global has been cleared. While this `SelfPipe` is alive, this fd is
    /// also the value of `SELFPIPE_WRITE_FD`.
    write: OwnedFd,
}

impl SelfPipe {
    /// Number of pending signal bytes drained per call to [`SelfPipe::drain`].
    pub const DRAIN_BATCH: usize = 64;

    /// Drain up to [`Self::DRAIN_BATCH`] pending signal bytes. Returns the
    /// list of signal numbers observed. An empty `Vec` means no signals were
    /// pending (the pipe was non-blocking-empty).
    pub fn drain(&self) -> Result<Vec<u8>> {
        let mut buf = [0u8; Self::DRAIN_BATCH];
        loop {
            match nix::unistd::read(&self.read, &mut buf) {
                Ok(n) => return Ok(buf[..n].to_vec()),
                Err(Errno::EINTR) => continue,
                Err(Errno::EAGAIN) => return Ok(Vec::new()),
                Err(e) => return Err(Error::from(e)),
            }
        }
    }
}

impl Drop for SelfPipe {
    fn drop(&mut self) {
        // Clear the global before letting `self.write` Drop close the fd, so
        // a late-delivered signal does not write to a stale (potentially
        // reused) descriptor.
        let _ = SELFPIPE_WRITE_FD.compare_exchange(
            self.write.as_raw_fd(),
            -1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// Generic self-pipe signal handler. Writes the signal number to the
/// registered write fd. async-signal-safe.
extern "C" fn selfpipe_handler(sig: libc::c_int) {
    let fd = SELFPIPE_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte: [u8; 1] = [sig as u8];
    // SAFETY: write(2) is async-signal-safe; we ignore the result because
    // there is nothing safe to do on EAGAIN/EBADF inside a signal handler.
    unsafe {
        libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
    }
}

/// Create the self-pipe. Both ends are set non-blocking and `FD_CLOEXEC`. The
/// write end is stashed in a module-private atomic so [`selfpipe_handler`] can
/// reach it.
pub fn install_self_pipe() -> Result<SelfPipe> {
    use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};

    let (read_fd, write_fd) = nix::unistd::pipe().map_err(Error::from)?;
    for fd in [&read_fd, &write_fd] {
        let borrow = fd.as_fd();
        let cur = fcntl(borrow, FcntlArg::F_GETFL).map_err(Error::from)?;
        let cur = OFlag::from_bits_truncate(cur);
        fcntl(borrow, FcntlArg::F_SETFL(cur | OFlag::O_NONBLOCK)).map_err(Error::from)?;
        let cur = fcntl(borrow, FcntlArg::F_GETFD).map_err(Error::from)?;
        let cur = FdFlag::from_bits_truncate(cur);
        fcntl(borrow, FcntlArg::F_SETFD(cur | FdFlag::FD_CLOEXEC)).map_err(Error::from)?;
    }
    SELFPIPE_WRITE_FD.store(write_fd.as_raw_fd(), Ordering::Relaxed);
    Ok(SelfPipe {
        read: read_fd,
        write: write_fd,
    })
}

/// Register `signum` to deliver into the self-pipe. **`install_self_pipe`
/// must have been called first**; otherwise the handler observes `fd == -1`
/// and silently drops signals.
///
/// `SA_RESTART` is intentionally NOT set: a blocked `poll(2)` returns `EINTR`
/// and the agent loop responds by re-polling, which then picks up the
/// signal byte on the next iteration.
pub fn register_self_pipe(signum: Signal) -> Result<()> {
    let action = SigAction::new(
        SigHandler::Handler(selfpipe_handler),
        nix::sys::signal::SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: `selfpipe_handler` is async-signal-safe (atomic load + write).
    unsafe { nix::sys::signal::sigaction(signum, &action) }.map_err(Error::from)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// sigprocmask RAII
// ---------------------------------------------------------------------------

/// RAII wrapper around `sigprocmask`. On drop the previous mask is restored.
#[derive(Debug)]
pub struct SigMaskGuard {
    prev: SigSet,
}

impl SigMaskGuard {
    /// Block `signums` in addition to whatever is currently blocked.
    pub fn block(signums: &[Signal]) -> Result<Self> {
        let mut set = SigSet::empty();
        for s in signums {
            set.add(*s);
        }
        let mut prev = SigSet::empty();
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_BLOCK, Some(&set), Some(&mut prev))
            .map_err(Error::from)?;
        Ok(Self { prev })
    }
}

impl Drop for SigMaskGuard {
    fn drop(&mut self) {
        // Best-effort: a failure here means something has corrupted the
        // signal mask. We cannot do anything useful at drop time.
        let _ = nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, Some(&self.prev), None);
    }
}

#[allow(unused_imports)]
use unistd as _unistd;

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;

    #[test]
    fn sig_ignore_sigpipe() {
        install_ignore(Signal::SIGPIPE).expect("install ignore SIGPIPE");
    }

    #[test]
    fn selfpipe_roundtrip_via_raise() {
        // mirrors ffi_wbtest.mbt: "sig_selfpipe_init and sig_drain: roundtrip via raise"
        let pipe = install_self_pipe().expect("init");
        register_self_pipe(Signal::SIGUSR1).expect("register");
        raise(Signal::SIGUSR1).expect("raise");
        // give the kernel a tiny window to deliver
        for _ in 0..10 {
            let drained = pipe.drain().expect("drain");
            if !drained.is_empty() {
                assert_eq!(drained[0], Signal::SIGUSR1 as i32 as u8);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("self-pipe never observed SIGUSR1");
    }

    #[test]
    fn install_winch_does_not_crash() {
        // mirrors ffi_wbtest.mbt: "sig_setup_winch: succeeds on pty master fd"
        let pty = crate::sys::pty::Pty::open(80, 24).expect("open pty");
        install_winch(pty.master_fd()).expect("install winch");
    }
}
