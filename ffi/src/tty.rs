use std::sync::Mutex;

struct TtyState {
    fd: i32,
    termios: libc::termios,
}

static TTY_TABLE: Mutex<Vec<Option<TtyState>>> = Mutex::new(Vec::new());

/// Check if the given fd is a terminal.
/// Returns 1 if true, 0 if false.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_tty_is_tty(fd: i32) -> i32 {
    let result = unsafe { libc::isatty(fd) };
    if result == 1 { 1 } else { 0 }
}

/// Get the terminal size for the given fd.
/// Returns a packed i64: lower 32 bits = (rows << 16) | cols, upper 32 bits = 0.
/// Returns -1 (as i64) on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_tty_size(fd: i32) -> i64 {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if ret == -1 {
        return -1i64;
    }
    let rows = ws.ws_row as i64;
    let cols = ws.ws_col as i64;
    (rows << 16) | cols
}

/// Save the current termios state for the given fd.
/// Returns a handle (index into TTY_TABLE) on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_tty_save(fd: i32) -> i32 {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } == -1 {
        return -1;
    }

    let state = TtyState { fd, termios };

    let mut table = match TTY_TABLE.lock() {
        Ok(t) => t,
        Err(_) => return -1,
    };

    let handle = table.len() as i32;
    table.push(Some(state));
    handle
}

/// Set the given fd to raw mode.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_tty_set_raw(fd: i32) -> i32 {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } == -1 {
        return -1;
    }
    unsafe { libc::cfmakeraw(&mut termios) };
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &termios) } == -1 {
        return -1;
    }
    0
}

/// Restore the saved termios state.
/// The handle is consumed (slot set to None).
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub extern "C" fn hyoui_tty_restore(handle: i32) -> i32 {
    let mut table = match TTY_TABLE.lock() {
        Ok(t) => t,
        Err(_) => return -1,
    };
    let entry = match table.get_mut(handle as usize) {
        Some(slot @ Some(_)) => slot.take().unwrap(),
        _ => return -1,
    };

    let ret = unsafe { libc::tcsetattr(entry.fd, libc::TCSAFLUSH, &entry.termios) };
    if ret == -1 { -1 } else { 0 }
}
