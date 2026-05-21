/// Convert a MoonBit String to UTF-8 encoded Bytes.
/// Design rationale: String.to_bytes() produces UTF-16 LE, but the Rust FFI
/// functions expect UTF-8. Using @utf8.encode() produces correct UTF-8 Bytes.
pub fn to_utf8(s : String) -> Bytes {
  @utf8.encode(s)
}

///|
#borrow(name)
extern "C" fn hyoui_getenv(name : Bytes) -> Bytes = "hyoui_getenv"

///|
extern "C" fn hyoui_getpid() -> Int = "hyoui_getpid"

///|
extern "C" fn hyoui_getuid() -> UInt = "hyoui_getuid"

///|
#borrow(path)
extern "C" fn hyoui_ensure_dir(path : Bytes) -> Int = "hyoui_ensure_dir"

///|
pub fn getenv(name : String) -> String {
  let result = hyoui_getenv(to_utf8(name))
  if result.length() == 0 {
    ""
  } else {
    @utf8.decode_lossy(result[:])
  }
}

///|
pub fn getpid() -> Int {
  hyoui_getpid()
}

///|
pub fn getuid() -> UInt {
  hyoui_getuid()
}

///|
pub fn ensure_dir(path : String) -> Bool {
  hyoui_ensure_dir(to_utf8(path)) == 0
}
