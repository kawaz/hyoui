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

/// Global write end of the self-pipe used to deliver signals to the poll loop.
/// -1 until hyoui_sig_selfpipe_init is called.
static SELFPIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Self-pipe signal handler.
///
/// async-signal-safe: writes a single byte (the signal number) to the
/// self-pipe write end using write(2), which is async-signal-safe. errno is
/// not touched in a way that escapes (we ignore the result). The atomic load
/// is lock-free.
extern "C" fn selfpipe_handler(sig: libc::c_int) {
    let fd = SELFPIPE_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte: [u8; 1] = [sig as u8];
    // Ignore the result: if the pipe is full or closed there is nothing safe
    // to do in a signal handler. Saving/restoring errno is unnecessary here
    // because the handler runs in poll-driven code that re-checks errno.
    unsafe {
        libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
    }
}

/// Create the self-pipe used for the self-pipe trick.
///
/// Returns a packed i64: lower 32 bits = read fd, upper 32 bits = write fd.
/// Returns -1 (as i64) on failure. The read fd should be added to the poll set
/// by the caller; the write fd is stored internally for the signal handler.
/// Both ends are set non-blocking and close-on-exec.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_selfpipe_init() -> i64 {
    let mut fds: [libc::c_int; 2] = [-1, -1];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return -1;
    }
    for &fd in fds.iter() {
        let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if fl == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) } == -1 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return -1;
        }
        let fd_fl = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if fd_fl == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, fd_fl | libc::FD_CLOEXEC) } == -1
        {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return -1;
        }
    }
    SELFPIPE_WRITE_FD.store(fds[1], Ordering::Relaxed);
    ((fds[1] as i64) << 32) | (fds[0] as u32 as i64)
}

/// Register `signum` to be delivered to the self-pipe.
///
/// The handler writes the signal number to the self-pipe write end.
/// SA_RESTART is intentionally NOT set so that a blocking poll() is
/// interrupted with EINTR (the loop treats EINTR as non-fatal and re-polls,
/// which then observes the self-pipe byte).
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_register_selfpipe(signum: i32) -> i32 {
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = selfpipe_handler as *const () as usize;
    sa.sa_flags = 0; // no SA_RESTART: poll() returns EINTR
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    let ret = unsafe { libc::sigaction(signum, &sa, std::ptr::null_mut()) };
    if ret != 0 { -1 } else { 0 }
}

/// Set a signal's disposition to SIG_DFL (default).
///
/// Used to restore SIGTSTP/SIGCONT to default before re-raising them, so that
/// the kernel actually stops/continues the process instead of re-entering the
/// handler. Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_default(signum: i32) -> i32 {
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = libc::SIG_DFL;
    sa.sa_flags = 0;
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    let ret = unsafe { libc::sigaction(signum, &sa, std::ptr::null_mut()) };
    if ret != 0 { -1 } else { 0 }
}

/// Raise a signal on the current process (raise(3)).
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_raise(signum: i32) -> i32 {
    let ret = unsafe { libc::raise(signum) };
    if ret != 0 { -1 } else { 0 }
}

/// Drain pending signal bytes from the self-pipe read fd.
///
/// Reads up to `max` bytes (each byte is a signal number) into `buf`.
/// Returns the number of signal bytes read, 0 if none pending, -1 on error.
/// EAGAIN/EWOULDBLOCK is reported as 0 (no signals pending).
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sig_drain(read_fd: i32, buf: *mut u8, max: i32) -> i32 {
    if buf.is_null() || max <= 0 {
        return -1;
    }
    loop {
        let n = unsafe { libc::read(read_fd, buf as *mut libc::c_void, max as usize) };
        if n < 0 {
            let errno = crate::get_errno();
            if errno == libc::EINTR {
                continue;
            }
            if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
                return 0;
            }
            return -1;
        }
        return n as i32;
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
