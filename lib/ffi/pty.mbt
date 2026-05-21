///|
pub struct PtyHandle(Int)

///|
extern "C" fn hyoui_pty_open(cols : Int, rows : Int) -> Int = "hyoui_pty_open"

///|
#borrow(argv_blob)
extern "C" fn hyoui_pty_spawnv(
  handle : Int,
  argv_blob : Bytes,
  argc : Int,
) -> Int = "hyoui_pty_spawnv"

///|
extern "C" fn hyoui_pty_master_fd(handle : Int) -> Int = "hyoui_pty_master_fd"

///|
extern "C" fn hyoui_pty_resize(handle : Int, cols : Int, rows : Int) -> Int = "hyoui_pty_resize"

///|
extern "C" fn hyoui_pty_close(handle : Int) -> Int = "hyoui_pty_close"

///|
pub fn pty_open(cols : Int, rows : Int) -> PtyHandle? {
  let h = hyoui_pty_open(cols, rows)
  if h < 0 { None } else { Some(PtyHandle(h)) }
}

///|
/// Spawn a child process on the PTY using execvp-style PATH resolution.
///
/// `argv` is the full argument vector; argv[0] is the program name and is
/// resolved against $PATH. The argv is marshalled to Rust as a single
/// NUL-separated UTF-8 blob plus the argument count.
/// Returns the child PID, or None on failure.
pub fn pty_spawnv(handle : PtyHandle, argv : Array[String]) -> Int? {
  if argv.is_empty() {
    return None
  }
  let PtyHandle(h) = handle
  // Build the NUL-separated blob: arg0\0arg1\0...\0
  let blob : Array[Byte] = []
  for arg in argv {
    let utf8 = @utf8.encode(arg)
    for i = 0; i < utf8.length(); i = i + 1 {
      blob.push(utf8[i])
    }
    blob.push(b'\x00')
  }
  let pid = hyoui_pty_spawnv(h, Bytes::from_array(blob[:]), argv.length())
  if pid < 0 {
    None
  } else {
    Some(pid)
  }
}

///|
pub fn pty_master_fd(handle : PtyHandle) -> Int {
  let PtyHandle(h) = handle
  hyoui_pty_master_fd(h)
}

///|
pub fn pty_resize(handle : PtyHandle, cols : Int, rows : Int) -> Bool {
  let PtyHandle(h) = handle
  hyoui_pty_resize(h, cols, rows) == 0
}

///|
pub fn pty_close(handle : PtyHandle) -> Bool {
  let PtyHandle(h) = handle
  hyoui_pty_close(h) == 0
}
