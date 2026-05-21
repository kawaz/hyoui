///|
#borrow(buf)
extern "C" fn hyoui_io_read(fd : Int, buf : Bytes, max_len : Int) -> Int = "hyoui_io_read"

///|
#borrow(data)
extern "C" fn hyoui_io_write(fd : Int, data : Bytes) -> Int = "hyoui_io_write"

///|
extern "C" fn hyoui_io_close(fd : Int) -> Int = "hyoui_io_close"

///|
extern "C" fn hyoui_io_set_nonblocking(fd : Int) -> Int = "hyoui_io_set_nonblocking"

///|
#borrow(fds)
extern "C" fn hyoui_io_poll(fds : Bytes, nfds : Int, timeout_ms : Int) -> Int = "hyoui_io_poll"

///|
pub fn io_read(fd : Int, buf : Bytes, max_len : Int) -> Int {
  hyoui_io_read(fd, buf, max_len)
}

///|
pub fn io_write(fd : Int, data : Bytes) -> Int {
  hyoui_io_write(fd, data)
}

///|
pub fn io_close(fd : Int) -> Bool {
  hyoui_io_close(fd) == 0
}

///|
pub fn io_set_nonblocking(fd : Int) -> Bool {
  hyoui_io_set_nonblocking(fd) == 0
}

///|
pub fn io_poll(fds : Bytes, nfds : Int, timeout_ms : Int) -> Int {
  hyoui_io_poll(fds, nfds, timeout_ms)
}

// poll イベント定数
///|
pub const POLLIN : Int = 0x0001

///|
pub const POLLHUP : Int = 0x0010

///|
pub const POLLERR : Int = 0x0008

// Pollfd ヘルパー: packed pollfd = [fd:i32(4B), events:i16(2B), revents:i16(2B)] = 8B/entry
// ネイティブエンディアン (LE on x86-64/aarch64)

///|
pub fn pollfd_pack(entries : Array[(Int, Int)]) -> Bytes {
  let n = entries.length()
  let buf : Array[Byte] = Array::make(n * 8, b'\x00')
  for i = 0; i < n; i = i + 1 {
    let (fd, events) = entries[i]
    // fd: i32 LE at offset i*8
    buf[i * 8 + 0] = (fd & 0xFF).to_byte()
    buf[i * 8 + 1] = ((fd >> 8) & 0xFF).to_byte()
    buf[i * 8 + 2] = ((fd >> 16) & 0xFF).to_byte()
    buf[i * 8 + 3] = ((fd >> 24) & 0xFF).to_byte()
    // events: i16 LE at offset i*8+4
    buf[i * 8 + 4] = (events & 0xFF).to_byte()
    buf[i * 8 + 5] = ((events >> 8) & 0xFF).to_byte()
    // revents: i16 LE at offset i*8+6 (initialized to 0)
    buf[i * 8 + 6] = b'\x00'
    buf[i * 8 + 7] = b'\x00'
  }
  Bytes::from_array(buf[:])
}

///|
pub fn pollfd_revents(buf : Bytes, index : Int) -> Int {
  let offset = index * 8 + 6
  buf[offset].to_int() | (buf[offset + 1].to_int() << 8)
}
