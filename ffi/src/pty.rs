use std::sync::Mutex;
use std::sync::atomic::Ordering;

use crate::bytes_to_cstring;
use crate::sig::WINCH_MASTER_FD;

struct PtyState {
    master_fd: i32,
    slave_fd: i32, // -1 after spawn
}

static PTY_TABLE: Mutex<Vec<Option<PtyState>>> = Mutex::new(Vec::new());

/// Create a PTY pair with the given window size.
/// Returns a handle (index into PTY_TABLE) on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_pty_open(cols: i32, rows: i32) -> i32 {
    let mut master_fd: libc::c_int = -1;
    let mut slave_fd: libc::c_int = -1;

    let ret = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ret != 0 {
        return -1;
    }

    // Set window size (W1: check ioctl return value)
    let ws = libc::winsize {
        ws_row: rows as u16,
        ws_col: cols as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws) } == -1 {
        unsafe {
            libc::close(master_fd);
            libc::close(slave_fd);
        }
        return -1;
    }

    let state = PtyState { master_fd, slave_fd };

    let mut table = match PTY_TABLE.lock() {
        Ok(t) => t,
        Err(_) => {
            unsafe {
                libc::close(master_fd);
                libc::close(slave_fd);
            }
            return -1;
        }
    };

    let handle = table.len() as i32;
    table.push(Some(state));
    handle
}

/// Helper macro-like function to clean up posix_spawn resources on error (W1).
fn cleanup_spawn_resources(
    file_actions: &mut libc::posix_spawn_file_actions_t,
    attr: Option<&mut libc::posix_spawnattr_t>,
) {
    unsafe {
        libc::posix_spawn_file_actions_destroy(file_actions);
    }
    if let Some(a) = attr {
        unsafe {
            libc::posix_spawnattr_destroy(a);
        }
    }
}

/// Spawn a child process on the PTY using posix_spawn.
/// `cmd` is a MoonBit Bytes (UTF-8, not null-terminated).
/// Returns the child PID on success, -1 on failure.
///
/// Design rationale: Uses openpty + posix_spawn instead of forkpty + exec.
/// MoonBit's runtime (GC, etc.) makes fork unsafe because the child inherits
/// a copy of the runtime state and crashes during GC. posix_spawn avoids
/// this by creating the child process without running any code in the forked
/// address space.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_pty_spawn(handle: i32, cmd: *const u8) -> i32 {
    // W10: use bytes_to_cstring helper
    let cmd_cstr = match bytes_to_cstring(cmd) {
        Some(c) => c,
        None => return -1,
    };

    let (master_fd, slave_fd) = {
        let mut table = match PTY_TABLE.lock() {
            Ok(t) => t,
            Err(_) => return -1,
        };
        let entry = match table.get_mut(handle as usize) {
            Some(Some(s)) => s,
            _ => return -1,
        };
        if entry.slave_fd < 0 {
            return -1; // Already spawned
        }
        (entry.master_fd, entry.slave_fd)
    };

    // Reset SIGCHLD to default so waitpid works (W12: use sigaction instead of signal).
    // Design rationale: The MoonBit runtime may install a SIGCHLD handler
    // that automatically reaps child processes, which breaks waitpid.
    {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = libc::SIG_DFL;
        sa.sa_flags = 0;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        unsafe { libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut()) };
    }

    // C2: Platform-specific POSIX_SPAWN_SETSID
    #[cfg(target_os = "macos")]
    const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
    #[cfg(target_os = "linux")]
    const POSIX_SPAWN_SETSID: libc::c_short = 0x02; // POSIX_SPAWN_SETSID_NP in glibc 2.26+

    // macOS POSIX_SPAWN_CLOEXEC_DEFAULT: close all fds not explicitly handled.
    #[cfg(target_os = "macos")]
    const POSIX_SPAWN_CLOEXEC_DEFAULT: libc::c_short = 0x1000;

    // Set up posix_spawn file actions
    let mut file_actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
    if unsafe { libc::posix_spawn_file_actions_init(&mut file_actions) } != 0 {
        return -1;
    }

    // W1: Check return values of all posix_spawn_file_actions_* and posix_spawnattr_* calls.
    // Close master_fd in child
    if unsafe { libc::posix_spawn_file_actions_addclose(&mut file_actions, master_fd) } != 0 {
        cleanup_spawn_resources(&mut file_actions, None);
        return -1;
    }
    // Dup slave_fd to stdin/stdout/stderr
    if unsafe { libc::posix_spawn_file_actions_adddup2(&mut file_actions, slave_fd, 0) } != 0 {
        cleanup_spawn_resources(&mut file_actions, None);
        return -1;
    }
    if unsafe { libc::posix_spawn_file_actions_adddup2(&mut file_actions, slave_fd, 1) } != 0 {
        cleanup_spawn_resources(&mut file_actions, None);
        return -1;
    }
    if unsafe { libc::posix_spawn_file_actions_adddup2(&mut file_actions, slave_fd, 2) } != 0 {
        cleanup_spawn_resources(&mut file_actions, None);
        return -1;
    }
    // Close original slave_fd after dup (it's now 0/1/2)
    if slave_fd > 2 {
        if unsafe { libc::posix_spawn_file_actions_addclose(&mut file_actions, slave_fd) } != 0 {
            cleanup_spawn_resources(&mut file_actions, None);
            return -1;
        }
    }

    // C2: On Linux, explicitly close master_fd in child since CLOEXEC_DEFAULT is not available.
    // The master_fd close is already handled above. For a full close_range approach:
    // TODO: Future improvement: use close_range(3, UINT_MAX, CLOSE_RANGE_CLOEXEC) on Linux 5.9+
    // to close all fds >= 3 that aren't explicitly handled.

    // Set up posix_spawn attributes
    let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
    if unsafe { libc::posix_spawnattr_init(&mut attr) } != 0 {
        cleanup_spawn_resources(&mut file_actions, None);
        return -1;
    }

    // Set flags: SETSID + CLOEXEC_DEFAULT (macOS only)
    #[cfg(target_os = "macos")]
    let flags = POSIX_SPAWN_SETSID | POSIX_SPAWN_CLOEXEC_DEFAULT;
    #[cfg(not(target_os = "macos"))]
    let flags = POSIX_SPAWN_SETSID;

    // W1: check posix_spawnattr_setflags return value
    if unsafe { libc::posix_spawnattr_setflags(&mut attr, flags) } != 0 {
        cleanup_spawn_resources(&mut file_actions, Some(&mut attr));
        return -1;
    }

    // Build argv: ["/bin/sh", "-c", cmd, NULL]
    let sh_path = b"/bin/sh\0";
    let dash_c = b"-c\0";
    let argv: [*mut libc::c_char; 4] = [
        sh_path.as_ptr() as *mut libc::c_char,
        dash_c.as_ptr() as *mut libc::c_char,
        cmd_cstr.as_ptr() as *mut libc::c_char,
        std::ptr::null_mut(),
    ];

    // Get current environment
    unsafe extern "C" {
        static environ: *const *const libc::c_char;
    }

    let mut child_pid: libc::pid_t = 0;
    let spawn_ret = unsafe {
        libc::posix_spawn(
            &mut child_pid,
            sh_path.as_ptr() as *const libc::c_char,
            &file_actions,
            &attr,
            argv.as_ptr(),
            environ as *const *mut libc::c_char,
        )
    };

    // Clean up spawn resources
    cleanup_spawn_resources(&mut file_actions, Some(&mut attr));

    if spawn_ret != 0 {
        return -1;
    }

    // Parent closes slave_fd (child has its own copy)
    unsafe { libc::close(slave_fd) };

    // Mark slave_fd as closed in the table
    if let Ok(mut table) = PTY_TABLE.lock() {
        if let Some(Some(entry)) = table.get_mut(handle as usize) {
            entry.slave_fd = -1;
        }
    }

    child_pid
}

/// Get the master fd for a PTY handle.
/// Returns the fd on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_pty_master_fd(handle: i32) -> i32 {
    let table = match PTY_TABLE.lock() {
        Ok(t) => t,
        Err(_) => return -1,
    };
    match table.get(handle as usize) {
        Some(Some(state)) => state.master_fd,
        _ => -1,
    }
}

/// Resize the PTY window.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_pty_resize(handle: i32, cols: i32, rows: i32) -> i32 {
    let table = match PTY_TABLE.lock() {
        Ok(t) => t,
        Err(_) => return -1,
    };
    let master_fd = match table.get(handle as usize) {
        Some(Some(state)) => state.master_fd,
        _ => return -1,
    };

    let ws = libc::winsize {
        ws_row: rows as u16,
        ws_col: cols as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws) };
    if ret == -1 { -1 } else { 0 }
}

/// Close the PTY and release resources.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_pty_close(handle: i32) -> i32 {
    let mut table = match PTY_TABLE.lock() {
        Ok(t) => t,
        Err(_) => return -1,
    };
    let entry = match table.get_mut(handle as usize) {
        Some(slot @ Some(_)) => slot.take().unwrap(),
        _ => return -1,
    };

    let ret = unsafe { libc::close(entry.master_fd) };

    // Reset WINCH_MASTER_FD if it was pointing to this master_fd
    WINCH_MASTER_FD.compare_exchange(
        entry.master_fd,
        -1,
        Ordering::Relaxed,
        Ordering::Relaxed,
    ).ok();

    if ret == -1 { -1 } else { 0 }
}
