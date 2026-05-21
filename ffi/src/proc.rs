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

    let result = unsafe { libc::waitpid(pid, &mut status, flags) };

    if result < 0 {
        // Failure: lower 32 bits = 0xFFFFFFFF
        0xFFFF_FFFF_i64
    } else {
        // Pack: (status << 32) | (result_pid & 0xFFFFFFFF)
        ((status as i64) << 32) | (result as u32 as i64)
    }
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
