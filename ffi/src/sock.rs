use crate::{bytes_length, bytes_to_cstring};

/// Build a sockaddr_un from a path byte slice (W11: extracted as helper).
/// Returns None if the path is too long for sun_path.
fn build_sockaddr_un(path_bytes: &[u8]) -> Option<libc::sockaddr_un> {
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

    let sun_path_len = addr.sun_path.len(); // 104 on macOS, 108 on Linux
    if path_bytes.len() >= sun_path_len {
        return None; // path too long (must leave room for null terminator)
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr(),
            addr.sun_path.as_mut_ptr() as *mut u8,
            path_bytes.len(),
        );
    }
    // sun_path is already zeroed, so null terminator is in place
    Some(addr)
}

/// Create a Unix domain socket, bind to `path`, and listen.
///
/// - `path` is MoonBit Bytes (UTF-8, not null-terminated).
/// - Sets umask(0o077) before bind, restores after.
/// - Verifies parent directory has permissions 0700 and is owned by current euid (W15).
/// - Unlinks existing path before bind.
/// - listen backlog = 5.
///
/// Returns the listening fd on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sock_listen(path: *const u8) -> i32 {
    let len = bytes_length(path);
    if len <= 0 {
        return -1;
    }
    let path_bytes = unsafe { std::slice::from_raw_parts(path, len as usize) };
    let path_str = match std::str::from_utf8(path_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    // Build null-terminated C string for path (W10: use bytes_to_cstring)
    let path_cstr = match bytes_to_cstring(path) {
        Some(c) => c,
        None => return -1,
    };

    // Check parent directory permissions (must be 0700) and owner (must be euid) (W15)
    if let Some(parent_end) = path_str.rfind('/') {
        let parent = &path_str[..parent_end];
        if !parent.is_empty() {
            let mut parent_cstr = Vec::with_capacity(parent.len() + 1);
            parent_cstr.extend_from_slice(parent.as_bytes());
            parent_cstr.push(0);

            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::stat(parent_cstr.as_ptr() as *const libc::c_char, &mut st) } != 0 {
                return -1;
            }
            if (st.st_mode & 0o777) != 0o700 {
                return -1;
            }
            // W15: verify parent directory is owned by current effective user
            if st.st_uid != unsafe { libc::geteuid() } {
                return -1;
            }
        }
    }

    // Build sockaddr_un (W11: use helper)
    let addr = match build_sockaddr_un(path_bytes) {
        Some(a) => a,
        None => return -1,
    };

    // Unlink existing path (ignore errors -- may not exist)
    unsafe { libc::unlink(path_cstr.as_ptr() as *const libc::c_char) };

    // Create socket
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return -1;
    }

    // Design rationale (W13): umask(0o077) before bind ensures the socket file is created
    // with permissions 0600 (owner-only). This is safe in hyoui because hyoui-agent is
    // single-threaded; no other thread can create files with the modified umask.
    // If hyoui becomes multi-threaded, this should be replaced with fchmod() after bind.
    let old_umask = unsafe { libc::umask(0o077) };

    let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let bind_ret = unsafe {
        libc::bind(fd, &addr as *const libc::sockaddr_un as *const libc::sockaddr, addr_len)
    };

    // Restore umask
    unsafe { libc::umask(old_umask) };

    if bind_ret != 0 {
        unsafe { libc::close(fd) };
        return -1;
    }

    if unsafe { libc::listen(fd, 5) } != 0 {
        unsafe { libc::close(fd) };
        return -1;
    }

    fd
}

/// Accept an incoming connection on the listening socket.
///
/// Returns the client fd on success, -1 on failure (including EAGAIN).
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sock_accept(fd: i32) -> i32 {
    let client_fd = unsafe {
        libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut())
    };
    if client_fd < 0 {
        return -1;
    }
    client_fd
}

/// Connect to a Unix domain socket at `path`.
///
/// - `path` is MoonBit Bytes (UTF-8, not null-terminated).
///
/// Returns the connected fd on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sock_connect(path: *const u8) -> i32 {
    let len = bytes_length(path);
    if len <= 0 {
        return -1;
    }
    let path_bytes = unsafe { std::slice::from_raw_parts(path, len as usize) };

    // Build sockaddr_un (W11: use helper)
    let addr = match build_sockaddr_un(path_bytes) {
        Some(a) => a,
        None => return -1,
    };

    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return -1;
    }

    let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let ret = unsafe {
        libc::connect(fd, &addr as *const libc::sockaddr_un as *const libc::sockaddr, addr_len)
    };
    if ret != 0 {
        unsafe { libc::close(fd) };
        return -1;
    }

    fd
}

/// Unlink (remove) a socket file at `path`.
///
/// - `path` is MoonBit Bytes (UTF-8, not null-terminated).
///
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_sock_unlink(path: *const u8) -> i32 {
    // W10: use bytes_to_cstring helper
    let path_cstr = match bytes_to_cstring(path) {
        Some(c) => c,
        None => return -1,
    };
    let ret = unsafe { libc::unlink(path_cstr.as_ptr() as *const libc::c_char) };
    if ret != 0 { -1 } else { 0 }
}
