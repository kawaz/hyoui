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
use nix::sys::signal::{SigAction, SigHandler, SigSet, Signal};
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

/// fork した子が **親から継承した [`selfpipe_handler`]** を走らせないよう、子側で
/// write fd の登録を外す。`fork(2)` 直後・`execve(2)` 前の子で呼ぶ。
///
/// 子は handler の disposition と self-pipe の write fd を両方継承する。fd は
/// `FD_CLOEXEC` なので exec 時に閉じるが、**fork〜exec の窓では生きている**。この窓で
/// 子に signal が届くと handler が走り、**親の self-pipe に signal byte を書き込む**。
/// 親はそれを自分宛ての signal として解釈するため、送っていない SIGTERM を受けたと
/// 誤認する (= 実測。1 プロセスで複数 daemon を動かすテストでは、無関係な daemon が
/// 落とされて `UnexpectedEof` になっていた)。
///
/// 単体では `fork` から本関数の実行までの窓が残るため、[`block_handled_signals`] と
/// 併用する。async-signal-safe (= atomic store のみ)。
pub fn disarm_self_pipe_in_child() {
    SELFPIPE_WRITE_FD.store(-1, Ordering::Relaxed);
}

/// 自前 handler を張りうる signal 群 (= [`block_handled_signals`] の対象)。
const HANDLED_SIGNALS: [libc::c_int; 8] = [
    libc::SIGTERM,
    libc::SIGINT,
    libc::SIGHUP,
    libc::SIGQUIT,
    libc::SIGCHLD,
    libc::SIGWINCH,
    libc::SIGUSR1,
    libc::SIGTSTP,
];

/// Signal mask saved by [`block_handled_signals`], restored via [`restore_mask`].
pub struct SavedMask(libc::sigset_t);

/// `fork(2)` の **直前** に呼び、自前 handler を張る signal を block する。
///
/// これで子は [`disarm_self_pipe_in_child`] を終えるまで signal を受け取らず、
/// 「fork 済みだが disarm 前」の窓が閉じる。親・子とも用が済んだら
/// [`restore_mask`] で戻すこと。**`execve` は signal mask を引き継ぐ**ため、
/// 子側での復元は必須 (= 忘れると exec 後の子が SIGTERM を受け付けなくなる)。
pub fn block_handled_signals() -> SavedMask {
    // SAFETY: sigset_t の初期化・操作を libc の API のみで行う。
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in HANDLED_SIGNALS {
            libc::sigaddset(&mut set, sig);
        }
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
        SavedMask(old)
    }
}

/// Restore a mask saved by [`block_handled_signals`]. async-signal-safe。
pub fn restore_mask(saved: &SavedMask) {
    // SAFETY: `saved` は block 時に kernel が埋めた有効な sigset_t。
    unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, &saved.0, std::ptr::null_mut());
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

#[allow(unused_imports)]
use unistd as _unistd;

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// Serializes the tests in this module.
    ///
    /// Design rationale: every test here mutates **process-global** state
    /// (`sigaction` dispositions, [`WINCH_MASTER_FD`], [`SELFPIPE_WRITE_FD`]).
    /// `cargo test` runs `#[test]` fns on multiple threads by default, so two
    /// of these tests racing in the same process can see each other's signal
    /// handlers and globals (e.g. one test installing a handler while another
    /// still expects the previous disposition, or two tests overwriting
    /// `SELFPIPE_WRITE_FD` with each other's pipe write ends). We serialize
    /// with a module-private mutex instead of pulling in `serial_test` for
    /// three tests.
    ///
    /// `PoisonError::into_inner` is used so that a panic in one test does not
    /// poison the mutex and block the remaining tests (the panic itself is
    /// already surfaced as a test failure).
    static SIGNAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn signal_test_guard() -> MutexGuard<'static, ()> {
        SIGNAL_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn sig_ignore_sigpipe() {
        let _guard = signal_test_guard();
        install_ignore(Signal::SIGPIPE).expect("install ignore SIGPIPE");
    }

    #[test]
    fn selfpipe_roundtrip_via_raise() {
        let _guard = signal_test_guard();
        // mirrors ffi_wbtest.mbt: "sig_selfpipe_init and sig_drain: roundtrip via raise"
        let pipe = install_self_pipe().expect("init");
        register_self_pipe(Signal::SIGUSR1).expect("register");
        raise(Signal::SIGUSR1).expect("raise");
        // give the kernel a tiny window to deliver
        for _ in 0..10 {
            let drained = pipe.drain().expect("drain");
            if !drained.is_empty() {
                assert_eq!(drained[0], Signal::SIGUSR1 as i32 as u8);
                // Restore SIGUSR1 to default before the SelfPipe drops so a
                // late delivery doesn't hit a handler whose write fd was
                // just closed. `SelfPipe::drop` also clears
                // `SELFPIPE_WRITE_FD`, but resetting the disposition removes
                // the dangling handler entirely.
                let _ = install_default(Signal::SIGUSR1);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = install_default(Signal::SIGUSR1);
        panic!("self-pipe never observed SIGUSR1");
    }

    /// R5-H6: SIGCHLD self-pipe wakes immediately when a child changes state
    /// (= SIGSTOP / SIGCONT / exit). Verifies the self-pipe receives the
    /// SIGCHLD signal number byte without depending on the 500ms polling
    /// fallback in serve_loop.
    #[test]
    fn sigchld_received_when_child_state_changes() {
        let _guard = signal_test_guard();

        use crate::sys::pty::Pty;
        use nix::sys::wait::{WaitPidFlag, waitpid};

        let pipe = install_self_pipe().expect("init self-pipe");
        register_self_pipe(Signal::SIGCHLD).expect("register SIGCHLD");

        // forkpty で子を生成。setsid 済の独立 process group に入る。
        let spawned = Pty::spawn(&["/bin/sleep", "30"], 80, 24, None).expect("spawn sleep");
        let child = spawned.child;

        // 子の生成自体では SIGCHLD は配信されない (exit/stop/cont のみ)。
        // 念のため pre-existing な SIGCHLD バイトを drain しておく。
        let _ = pipe.drain();

        // SIGSTOP を送る → 子が stop → SIGCHLD が親に配信 → self-pipe に書き込まれる
        nix::sys::signal::kill(child, Signal::SIGSTOP).expect("SIGSTOP");

        // kernel が SIGCHLD 配信 + handler 実行 + write(2) 完了するまで
        // 短いリトライで待つ。500ms 上限。
        let mut saw_sigchld = false;
        for _ in 0..50 {
            let drained = pipe.drain().expect("drain");
            if drained.contains(&(Signal::SIGCHLD as i32 as u8)) {
                saw_sigchld = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // cleanup: 必ず子を回収してから assert (= test 失敗時も zombie 残さない)
        let _ = nix::sys::signal::kill(child, Signal::SIGCONT);
        let _ = nix::sys::signal::kill(child, Signal::SIGKILL);
        let _ = waitpid(child, Some(WaitPidFlag::empty()));
        // SIGCHLD disposition を default に戻して、SelfPipe drop と pipe 閉鎖の
        // 後にも遅延配信された SIGCHLD が dangling handler を呼ばないようにする。
        let _ = install_default(Signal::SIGCHLD);

        assert!(
            saw_sigchld,
            "self-pipe should observe SIGCHLD within 500ms of SIGSTOP"
        );
    }

    #[test]
    fn install_winch_does_not_crash() {
        let _guard = signal_test_guard();
        // mirrors ffi_wbtest.mbt: "sig_setup_winch: succeeds on pty master fd"
        let pty = crate::sys::pty::Pty::open(80, 24).expect("open pty");
        let master_raw = pty.master_fd().as_raw_fd();
        install_winch(pty.master_fd()).expect("install winch");
        // Restore SIGWINCH to default and clear the global so the next test
        // (or production code in the same process) does not observe a
        // handler pointing at an fd that is closed when `pty` drops.
        let _ = install_default(Signal::SIGWINCH);
        clear_winch_if(master_raw);
    }
}
