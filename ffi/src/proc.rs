/// Wait for a child process.
///
/// - `pid`: the process ID to wait for.
/// - `nohang`: if 1, use WNOHANG (non-blocking); otherwise block.
///
/// Returns an i64 packing two values:
///   - Upper 32 bits: wait status
///   - Lower 32 bits: result pid (unsigned)
///
/// On failure (waitpid returns -1), the lower 32 bits are 0xFFFFFFFF.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_proc_waitpid(pid: i32, nohang: i32) -> i64 {
    let flags = if nohang == 1 { libc::WNOHANG } else { 0 };
    let mut status: libc::c_int = 0;

    // Retry on EINTR: a SIGCHLD (or any signal) delivered while blocking in
    // waitpid otherwise causes a spurious -1/ECHILD-looking failure.
    let result = loop {
        let r = unsafe { libc::waitpid(pid, &mut status, flags) };
        if r < 0 && crate::get_errno() == libc::EINTR {
            continue;
        }
        break r;
    };

    if result < 0 {
        // Failure: lower 32 bits = 0xFFFFFFFF
        0xFFFF_FFFF_i64
    } else {
        // Pack: (status << 32) | (result_pid & 0xFFFFFFFF)
        ((status as i64) << 32) | (result as u32 as i64)
    }
}

/// Wait for a child process state change, reporting stops as well as exits.
///
/// Uses WNOHANG | WUNTRACED so a stopped (SIGSTOP/SIGTSTP) child is reported
/// without blocking.
///
/// Returns an i64 packing three values:
///   - bits 0..31  : result pid (0xFFFFFFFF on waitpid failure / no change)
///   - bits 32..55 : raw wait status (24 bits is ample for status fields)
///   - bits 56..63 : kind: 0 = no change, 1 = exited, 2 = signalled, 3 = stopped
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_proc_waitpid_stopped(pid: i32) -> i64 {
    let mut status: libc::c_int = 0;
    let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
    if result <= 0 {
        // <= 0 : error (-1) or no state change (0). Report "no change".
        return 0xFFFF_FFFF_i64;
    }
    let kind: i64 = if libc::WIFEXITED(status) {
        1
    } else if libc::WIFSIGNALED(status) {
        2
    } else if libc::WIFSTOPPED(status) {
        3
    } else {
        0
    };
    (kind << 56) | (((status as i64) & 0x00FF_FFFF) << 32) | (result as u32 as i64)
}

/// Exit the process with the given status code.
///
/// Design rationale: MoonBit's generated C code may conflict with stdlib's
/// `exit` symbol, so we provide a namespaced wrapper.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_proc_exit(status: i32) {
    std::process::exit(status);
}

/// Send a signal to a process.
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_proc_kill(pid: i32, signum: i32) -> i32 {
    let ret = unsafe { libc::kill(pid, signum) };
    if ret != 0 { -1 } else { 0 }
}

/// Send a signal to a process group (killpg).
///
/// `pgid` is the process group ID. Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_proc_killpg(pgid: i32, signum: i32) -> i32 {
    let ret = unsafe { libc::killpg(pgid, signum) };
    if ret != 0 { -1 } else { 0 }
}

/// Read the monotonic clock in milliseconds.
///
/// Uses CLOCK_MONOTONIC, which is unaffected by wall-clock adjustments and is
/// suitable for measuring elapsed time. Returns -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_clock_monotonic_ms() -> i64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if ret != 0 {
        return -1;
    }
    (ts.tv_sec as i64) * 1000 + (ts.tv_nsec as i64) / 1_000_000
}
