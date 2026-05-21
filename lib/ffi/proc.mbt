///|
extern "C" fn hyoui_proc_waitpid(pid : Int, nohang : Int) -> Int64 = "hyoui_proc_waitpid"

///|
extern "C" fn hyoui_proc_waitpid_stopped(pid : Int) -> Int64 = "hyoui_proc_waitpid_stopped"

///|
extern "C" fn hyoui_proc_exit(status : Int) = "hyoui_proc_exit"

// proc_waitpid_stopped の kind 値
///|
pub const WAIT_NONE : Int = 0

///|
pub const WAIT_EXITED : Int = 1

///|
pub const WAIT_SIGNALED : Int = 2

///|
pub const WAIT_STOPPED : Int = 3

///|
extern "C" fn hyoui_proc_kill(pid : Int, signum : Int) -> Int = "hyoui_proc_kill"

///|
extern "C" fn hyoui_proc_killpg(pgid : Int, signum : Int) -> Int = "hyoui_proc_killpg"

///|
extern "C" fn hyoui_clock_monotonic_ms() -> Int64 = "hyoui_clock_monotonic_ms"

///|
pub fn proc_waitpid(pid : Int, nohang : Bool) -> (Int, Int)? {
  let result = hyoui_proc_waitpid(pid, if nohang { 1 } else { 0 })
  let result_pid = (result & 0xFFFFFFFF_L).to_int()
  if result_pid == -1 { return None }
  let status = (result >> 32).to_int()
  Some((result_pid, status))
}

///|
/// Wait (non-blocking, WUNTRACED) for a child state change.
/// Returns (pid, raw_status, kind) where kind is one of WAIT_EXITED /
/// WAIT_SIGNALED / WAIT_STOPPED. Returns None if there is no state change.
pub fn proc_waitpid_stopped(pid : Int) -> (Int, Int, Int)? {
  let result = hyoui_proc_waitpid_stopped(pid)
  let result_pid = (result & 0xFFFFFFFF_L).to_int()
  if result_pid == -1 {
    return None
  }
  let status = ((result >> 32) & 0xFFFFFF_L).to_int()
  let kind = ((result >> 56) & 0xFF_L).to_int()
  if kind == WAIT_NONE {
    return None
  }
  Some((result_pid, status, kind))
}

///|
pub fn proc_exit(status : Int) -> Unit {
  hyoui_proc_exit(status)
}

///|
pub fn proc_kill(pid : Int, signum : Int) -> Bool {
  hyoui_proc_kill(pid, signum) == 0
}

///|
/// Send a signal to a process group (killpg).
pub fn proc_killpg(pgid : Int, signum : Int) -> Bool {
  hyoui_proc_killpg(pgid, signum) == 0
}

///|
/// Read the monotonic clock in milliseconds. Returns -1 on failure.
pub fn clock_monotonic_ms() -> Int64 {
  hyoui_clock_monotonic_ms()
}
