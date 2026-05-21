use crate::{bytes_length, get_errno};

// W8: pollfd layout static assertions
const _: () = assert!(std::mem::size_of::<libc::pollfd>() == 8);
const _: () = assert!(std::mem::align_of::<libc::pollfd>() <= 4);

/// Read up to max_len bytes from fd into buf.
/// Retries on EINTR (W3).
/// Returns the number of bytes read, 0 on EOF, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_io_read(fd: i32, buf: *mut u8, max_len: i32) -> i32 {
    // C1: null check and negative max_len check
    if buf.is_null() || max_len <= 0 {
        return -1;
    }
    loop {
        let n = unsafe {
            libc::read(fd, buf as *mut libc::c_void, max_len as usize)
        };
        if n < 0 {
            let errno = get_errno();
            if errno == libc::EINTR {
                continue; // W3: retry on EINTR
            }
            return -1;
        }
        return n as i32;
    }
}

/// Write all bytes of a MoonBit Bytes to fd.
/// Uses bytes_length() to determine the length from the MoonBit header.
/// Retries on partial writes to guarantee all bytes are written.
/// Returns the number of bytes written on success (W2), -1 on EAGAIN/error.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_io_write(fd: i32, data: *const u8) -> i32 {
    let len = bytes_length(data) as usize;
    if len == 0 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts(data, len) };
    let mut written = 0usize;
    while written < len {
        let n = unsafe {
            libc::write(
                fd,
                buf[written..].as_ptr() as *const libc::c_void,
                len - written,
            )
        };
        if n < 0 {
            let errno = get_errno();
            if errno == libc::EINTR {
                continue;
            }
            // EAGAIN or other error
            return -1;
        }
        if n == 0 {
            // Should not happen for blocking fd, but guard against it
            return -1;
        }
        written += n as usize;
    }
    len as i32 // W2: return bytes written instead of 0
}

/// Close a file descriptor.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_io_close(fd: i32) -> i32 {
    let ret = unsafe { libc::close(fd) };
    if ret == -1 { -1 } else { 0 }
}

/// Set a file descriptor to non-blocking mode.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_io_set_nonblocking(fd: i32) -> i32 {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return -1;
    }
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret == -1 { -1 } else { 0 }
}

/// Sentinel returned by hyoui_io_poll when poll() is interrupted by a signal
/// (EINTR). The caller (event loop) must treat this as non-fatal and re-poll;
/// when the self-pipe trick is in use, the pending signal byte will then be
/// observed as a readable fd.
pub const HYOUI_POLL_EINTR: i32 = -2;

/// Poll multiple file descriptors for I/O readiness.
///
/// `fds` is a packed pollfd buffer: 8 bytes per entry (fd:i32 + events:i16 + revents:i16).
/// This matches the layout of `struct pollfd` on all supported platforms.
/// `nfds` is the number of entries.
/// `timeout_ms` is the timeout in milliseconds (-1 for infinite).
///
/// Design rationale: poll() is NOT retried internally on EINTR. Instead EINTR
/// is reported distinctly as HYOUI_POLL_EINTR (-2) so the event loop can
/// re-poll while still processing self-pipe signal bytes. Returns the number
/// of ready fds, 0 on timeout, -2 on EINTR, -1 on other errors.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_io_poll(fds: *mut u8, nfds: i32, timeout_ms: i32) -> i32 {
    if nfds < 0 {
        return -1;
    }
    if nfds > 0 && fds.is_null() {
        return -1;
    }
    let pollfds = fds as *mut libc::pollfd;
    let ret = unsafe { libc::poll(pollfds, nfds as libc::nfds_t, timeout_ms) };
    if ret < 0 {
        let errno = get_errno();
        if errno == libc::EINTR {
            return HYOUI_POLL_EINTR;
        }
        return -1;
    }
    ret
}
