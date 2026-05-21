mod pty;
mod tty;
mod io;
mod sock;
mod sig;
mod proc;

// MoonBit runtime functions
unsafe extern "C" {
    fn moonbit_make_bytes_raw(size: i32) -> *mut u8;
}

/// Get the length of a MoonBit Bytes object from its header.
/// meta field is at offset -4 from data pointer. Lower 28 bits = byte length.
pub(crate) fn bytes_length(ptr: *const u8) -> i32 {
    if ptr.is_null() {
        return 0;
    }
    let meta = unsafe { *((ptr as *const u32).offset(-1)) };
    (meta & 0x0FFF_FFFF) as i32
}

/// Convert a MoonBit Bytes pointer to a null-terminated C string (Vec<u8>).
/// Returns None if ptr is null or length is 0.
pub(crate) fn bytes_to_cstring(ptr: *const u8) -> Option<Vec<u8>> {
    if ptr.is_null() {
        return None;
    }
    let len = bytes_length(ptr) as usize;
    if len == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut cstr = Vec::with_capacity(len + 1);
    cstr.extend_from_slice(bytes);
    cstr.push(0);
    Some(cstr)
}

/// Get errno in a platform-independent way.
pub(crate) fn get_errno() -> i32 {
    #[cfg(target_os = "macos")]
    {
        unsafe { *libc::__error() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        unsafe { *libc::__errno_location() }
    }
}

/// Get an environment variable. Returns a MoonBit Bytes (UTF-8).
/// Returns empty Bytes (length 0) if not set or empty.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_getenv(name: *const u8) -> *mut u8 {
    let cstr = match bytes_to_cstring(name) {
        Some(c) => c,
        None => {
            let ptr = unsafe { moonbit_make_bytes_raw(0) };
            return ptr;
        }
    };
    let key = match std::str::from_utf8(&cstr[..cstr.len()-1]) {
        Ok(s) => s,
        Err(_) => {
            return unsafe { moonbit_make_bytes_raw(0) };
        }
    };
    match std::env::var(key) {
        Ok(val) => {
            let bytes = val.as_bytes();
            let len = bytes.len();
            let ptr = unsafe { moonbit_make_bytes_raw(len as i32) };
            if !ptr.is_null() && len > 0 {
                unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len) };
            }
            ptr
        }
        Err(_) => unsafe { moonbit_make_bytes_raw(0) },
    }
}

/// Get the current process ID.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_getpid() -> i32 {
    std::process::id() as i32
}

/// Get the current user ID.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_getuid() -> u32 {
    unsafe { libc::getuid() }
}

/// Ensure a directory exists with mode 0700. Creates parent directories as needed.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_ensure_dir(path: *const u8) -> i32 {
    let cstr = match bytes_to_cstring(path) {
        Some(c) => c,
        None => return -1,
    };
    let path_str = match std::str::from_utf8(&cstr[..cstr.len()-1]) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let path = std::path::Path::new(path_str);
    match std::fs::create_dir_all(path) {
        Ok(_) => {
            // Set permissions to 0700
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            match std::fs::set_permissions(path, perms) {
                Ok(_) => 0,
                Err(_) => -1,
            }
        }
        Err(_) => -1,
    }
}

// Test helpers (for MoonBit tests): mkdir with 0700 and rmdir for socket tests.
// These are exported for use by MoonBit whitebox tests and cannot be #[cfg(test)].

/// Create a directory with permissions 0700. (Test helper)
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_test_mkdir(path: *const u8) -> i32 {
    let cstr = match bytes_to_cstring(path) {
        Some(c) => c,
        None => return -1,
    };
    let ret = unsafe { libc::mkdir(cstr.as_ptr() as *const libc::c_char, 0o700) };
    if ret != 0 { -1 } else { 0 }
}

/// Remove an empty directory. (Test helper)
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_test_rmdir(path: *const u8) -> i32 {
    let cstr = match bytes_to_cstring(path) {
        Some(c) => c,
        None => return -1,
    };
    let ret = unsafe { libc::rmdir(cstr.as_ptr() as *const libc::c_char) };
    if ret != 0 { -1 } else { 0 }
}
