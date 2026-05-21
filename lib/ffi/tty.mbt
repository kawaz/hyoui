///|
pub struct TtyHandle(Int)

///|
extern "C" fn hyoui_tty_is_tty(fd : Int) -> Int = "hyoui_tty_is_tty"

///|
extern "C" fn hyoui_tty_size(fd : Int) -> Int64 = "hyoui_tty_size"

///|
extern "C" fn hyoui_tty_save(fd : Int) -> Int = "hyoui_tty_save"

///|
extern "C" fn hyoui_tty_set_raw(fd : Int) -> Int = "hyoui_tty_set_raw"

///|
extern "C" fn hyoui_tty_restore(handle : Int) -> Int = "hyoui_tty_restore"

///|
pub fn tty_is_tty(fd : Int) -> Bool {
  hyoui_tty_is_tty(fd) == 1
}

///|
pub fn tty_size(fd : Int) -> (Int, Int)? {
  let result = hyoui_tty_size(fd)
  if result < 0L { return None }
  let rows = ((result >> 16) & 0xFFFF_L).to_int()
  let cols = (result & 0xFFFF_L).to_int()
  Some((rows, cols))
}

///|
pub fn tty_save(fd : Int) -> TtyHandle? {
  let h = hyoui_tty_save(fd)
  if h < 0 { None } else { Some(TtyHandle(h)) }
}

///|
pub fn tty_set_raw(fd : Int) -> Bool {
  hyoui_tty_set_raw(fd) == 0
}

///|
pub fn tty_restore(handle : TtyHandle) -> Bool {
  let TtyHandle(h) = handle
  hyoui_tty_restore(h) == 0
}
