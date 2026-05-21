// シグナル番号（macOS / Linux 共通）

///|
pub const SIGHUP : Int = 1

///|
pub const SIGINT : Int = 2

///|
pub const SIGQUIT : Int = 3

///|
pub const SIGKILL : Int = 9

///|
pub const SIGPIPE : Int = 13

///|
pub const SIGTERM : Int = 15

///|
pub const SIGCHLD : Int = 20

///|
pub const SIGCONT : Int = 19

///|
pub const SIGSTOP : Int = 17

///|
pub const SIGTSTP : Int = 18

///|
pub const SIGWINCH : Int = 28

///|
extern "C" fn hyoui_sig_ignore(signum : Int) -> Int = "hyoui_sig_ignore"

///|
extern "C" fn hyoui_sig_setup_winch(master_fd : Int) -> Int = "hyoui_sig_setup_winch"

///|
extern "C" fn hyoui_sig_selfpipe_init() -> Int64 = "hyoui_sig_selfpipe_init"

///|
extern "C" fn hyoui_sig_register_selfpipe(signum : Int) -> Int = "hyoui_sig_register_selfpipe"

///|
extern "C" fn hyoui_sig_default(signum : Int) -> Int = "hyoui_sig_default"

///|
extern "C" fn hyoui_sig_raise(signum : Int) -> Int = "hyoui_sig_raise"

///|
#borrow(buf)
extern "C" fn hyoui_sig_drain(read_fd : Int, buf : Bytes, max : Int) -> Int = "hyoui_sig_drain"

///|
pub fn sig_ignore(signum : Int) -> Bool {
  hyoui_sig_ignore(signum) == 0
}

///|
pub fn sig_setup_winch(master_fd : Int) -> Bool {
  hyoui_sig_setup_winch(master_fd) == 0
}

///|
/// Create the self-pipe for the self-pipe trick.
/// Returns (read_fd, write_fd) on success, None on failure.
/// The write_fd is also stored internally for the signal handler; the caller
/// should add read_fd to its poll set.
pub fn sig_selfpipe_init() -> (Int, Int)? {
  let packed = hyoui_sig_selfpipe_init()
  if packed < 0L {
    return None
  }
  let read_fd = (packed & 0xFFFFFFFF_L).to_int()
  let write_fd = (packed >> 32).to_int()
  Some((read_fd, write_fd))
}

///|
/// Register `signum` so it is delivered to the self-pipe.
pub fn sig_register_selfpipe(signum : Int) -> Bool {
  hyoui_sig_register_selfpipe(signum) == 0
}

///|
/// Restore `signum` to its default disposition (SIG_DFL).
pub fn sig_default(signum : Int) -> Bool {
  hyoui_sig_default(signum) == 0
}

///|
/// Raise `signum` on the current process.
pub fn sig_raise(signum : Int) -> Bool {
  hyoui_sig_raise(signum) == 0
}

///|
/// Drain pending signal bytes from the self-pipe read fd.
/// Returns an array of signal numbers that arrived (possibly empty).
pub fn sig_drain(read_fd : Int) -> Array[Int] {
  let buf = Bytes::make(64, 0)
  let result : Array[Int] = []
  let n = hyoui_sig_drain(read_fd, buf, 64)
  if n > 0 {
    for i = 0; i < n; i = i + 1 {
      result.push(buf[i].to_int())
    }
  }
  result
}
