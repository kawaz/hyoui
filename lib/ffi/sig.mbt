// シグナル番号（macOS / Linux 共通）
///|
pub const SIGHUP : Int = 1

///|
pub const SIGPIPE : Int = 13

///|
pub const SIGTERM : Int = 15

///|
pub const SIGWINCH : Int = 28

///|
extern "C" fn hyoui_sig_ignore(signum : Int) -> Int = "hyoui_sig_ignore"

///|
extern "C" fn hyoui_sig_setup_winch(master_fd : Int) -> Int = "hyoui_sig_setup_winch"

///|
pub fn sig_ignore(signum : Int) -> Bool {
  hyoui_sig_ignore(signum) == 0
}

///|
pub fn sig_setup_winch(master_fd : Int) -> Bool {
  hyoui_sig_setup_winch(master_fd) == 0
}
