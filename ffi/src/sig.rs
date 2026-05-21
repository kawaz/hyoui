use std::sync::atomic::{AtomicI32, Ordering};

/// Global storage for the master fd used by SIGWINCH handler.
/// Set by hyoui_sig_setup_winch, cleared by pty_close.
pub(crate) static WINCH_MASTER_FD: AtomicI32 = AtomicI32::new(-1);

/// Ignore the specified signal by setting its disposition to SIG_IGN.
/// Uses sigaction() instead of signal() for portable behavior (W12).
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_ignore(signum: i32) -> i32 {
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = libc::SIG_IGN;
    sa.sa_flags = 0;
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    let ret = unsafe { libc::sigaction(signum, &sa, std::ptr::null_mut()) };
    if ret != 0 { -1 } else { 0 }
}

/// SIGWINCH handler: read terminal size from stdin and apply to master_fd.
///
/// This function is async-signal-safe: it uses only ioctl (async-signal-safe)
/// and atomic load (lock-free).
extern "C" fn sigwinch_handler(_sig: libc::c_int) {
    let master_fd = WINCH_MASTER_FD.load(Ordering::Relaxed);
    if master_fd < 0 {
        return;
    }
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 {
        unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws) };
    }
}

/// Set up a SIGWINCH handler that forwards terminal size changes to `master_fd`.
///
/// Stores `master_fd` in WINCH_MASTER_FD for use by the signal handler.
/// The handler reads the current terminal size from stdin (TIOCGWINSZ)
/// and applies it to master_fd (TIOCSWINSZ).
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_setup_winch(master_fd: i32) -> i32 {
    WINCH_MASTER_FD.store(master_fd, Ordering::Relaxed);

    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = sigwinch_handler as *const () as usize;
    sa.sa_flags = libc::SA_RESTART;
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };

    let ret = unsafe { libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut()) };
    if ret != 0 {
        WINCH_MASTER_FD.store(-1, Ordering::Relaxed);
        -1
    } else {
        0
    }
}
