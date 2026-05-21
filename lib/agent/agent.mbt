///|
pub struct Agent {
  pty_handle : @ffi.PtyHandle
  master_fd : Int
  child_pid : Int
  socket_fd : @ffi.SockFd
  socket_path : Bytes
  tty_handle : @ffi.TtyHandle? // stdin が tty でなければ None
  observer : &@observer.Observer
  mut stdin_open : Bool
}

/// Generate socket path for this agent.
/// Uses $XDG_RUNTIME_DIR/hyoui/agent-{PID}.sock or $TMPDIR/hyoui-{UID}/agent-{PID}.sock
fn socket_path() -> String {
  let pid = @ffi.getpid()
  let xdg = @ffi.getenv("XDG_RUNTIME_DIR")
  if xdg != "" {
    let dir = xdg + "/hyoui"
    let _ = @ffi.ensure_dir(dir)
    return dir + "/agent-" + pid.to_string() + ".sock"
  }
  let tmpdir = @ffi.getenv("TMPDIR")
  let base = if tmpdir != "" { tmpdir } else { "/tmp" }
  let uid = @ffi.getuid()
  let dir = base + "/hyoui-" + uid.to_string()
  let _ = @ffi.ensure_dir(dir)
  dir + "/agent-" + pid.to_string() + ".sock"
}

/// Create a new Agent.
/// shell: command to spawn (default: $SHELL or "/bin/sh")
pub fn Agent::new(
  observer : &@observer.Observer,
  shell~ : String = ""
) -> Agent raise {
  // Determine shell
  let cmd = if shell != "" {
    shell
  } else {
    let env_shell = @ffi.getenv("SHELL")
    if env_shell != "" { env_shell } else { "/bin/sh" }
  }
  // Get terminal size
  let (rows, cols) = match @ffi.tty_size(0) {
    Some((r, c)) => (r, c)
    None => (24, 80)
  }
  // Open PTY
  let pty_handle = match @ffi.pty_open(cols, rows) {
    Some(h) => h
    None => raise Failure::Failure("pty_open failed")
  }
  // Spawn child process
  let child_pid = match @ffi.pty_spawn(pty_handle, @ffi.to_utf8(cmd)) {
    Some(pid) => pid
    None => {
      let _ = @ffi.pty_close(pty_handle)
      raise Failure::Failure("pty_spawn failed")
    }
  }
  let master_fd = @ffi.pty_master_fd(pty_handle)
  // Setup socket
  let sock_path_str = socket_path()
  let sock_path_bytes = @ffi.to_utf8(sock_path_str)
  let socket_fd = match @ffi.sock_listen(sock_path_bytes) {
    Some(fd) => fd
    None => {
      let _ = @ffi.pty_close(pty_handle)
      raise Failure::Failure("sock_listen failed")
    }
  }
  // Set socket to nonblocking
  if not(@ffi.sock_set_nonblocking(socket_fd)) {
    let _ = @ffi.sock_close(socket_fd)
    let _ = @ffi.sock_unlink(sock_path_bytes)
    let _ = @ffi.pty_close(pty_handle)
    raise Failure::Failure("sock_set_nonblocking failed")
  }
  // Setup signals
  let _ = @ffi.sig_ignore(@ffi.SIGPIPE)
  let _ = @ffi.sig_setup_winch(master_fd)
  // Setup TTY if stdin is a terminal
  let tty_handle = if @ffi.tty_is_tty(0) {
    let h = @ffi.tty_save(0)
    match h {
      Some(handle) => {
        let _ = @ffi.tty_set_raw(0)
        Some(handle)
      }
      None => None
    }
  } else {
    None
  }
  Agent::{
    pty_handle,
    master_fd,
    child_pid,
    socket_fd,
    socket_path: sock_path_bytes,
    tty_handle,
    observer,
    stdin_open: true,
  }
}

/// Cleanup agent resources.
pub fn Agent::cleanup(self : Agent) -> Unit {
  // Restore TTY
  match self.tty_handle {
    Some(handle) => {
      let _ = @ffi.tty_restore(handle)
    }
    None => ()
  }
  // Remove socket
  let _ = @ffi.sock_unlink(self.socket_path)
  let _ = @ffi.sock_close(self.socket_fd)
  // Close PTY
  let _ = @ffi.pty_close(self.pty_handle)
}

/// Create a sub-Bytes from buf[0..len).
/// Design rationale: MoonBit's Bytes has no sub_bytes/slice method.
/// FixedArray::makei + Bytes::from_array is the idiomatic workaround.
fn bytes_take(buf : Bytes, len : Int) -> Bytes {
  Bytes::from_array(FixedArray::makei(len, fn(i) { buf[i] })[:])
}

/// Poll once for events. Returns false when the loop should exit (child process ended).
pub fn Agent::poll_once(self : Agent) -> Bool {
  // Build pollfd array
  // Index 0: stdin (if still open)
  // Index 1: master_fd
  // Index 2: socket_fd
  let entries : Array[(Int, Int)] = []
  if self.stdin_open {
    entries.push((0, @ffi.POLLIN)) // stdin = fd 0
  } else {
    entries.push((-1, 0)) // skip stdin
  }
  entries.push((self.master_fd, @ffi.POLLIN))
  entries.push((@ffi.sock_fd_raw(self.socket_fd), @ffi.POLLIN))
  let poll_fds = @ffi.pollfd_pack(entries)
  let ready = @ffi.io_poll(poll_fds, 3, -1) // block indefinitely
  if ready < 0 {
    return false // poll error
  }
  // Check stdin
  if self.stdin_open {
    let stdin_revents = @ffi.pollfd_revents(poll_fds, 0)
    if (stdin_revents & @ffi.POLLIN) != 0 {
      let buf = Bytes::make(4096, 0)
      let n = @ffi.io_read(0, buf, 4096)
      if n > 0 {
        let data = self.observer.on_input(bytes_take(buf, n))
        let _ = @ffi.io_write(self.master_fd, data)

      } else {
        // stdin EOF
        self.stdin_open = false
      }
    }
    if (stdin_revents & (@ffi.POLLHUP | @ffi.POLLERR)) != 0 {
      self.stdin_open = false
    }
  }
  // Check master_fd
  let master_revents = @ffi.pollfd_revents(poll_fds, 1)
  if (master_revents & @ffi.POLLIN) != 0 {
    let buf = Bytes::make(4096, 0)
    let n = @ffi.io_read(self.master_fd, buf, 4096)
    if n > 0 {
      let data = self.observer.on_output(bytes_take(buf, n))
      let _ = @ffi.io_write(1, data) // stdout = fd 1
    }
  }
  if (master_revents & (@ffi.POLLHUP | @ffi.POLLERR)) != 0 {
    return false // child process ended
  }
  // Check socket
  let sock_revents = @ffi.pollfd_revents(poll_fds, 2)
  if (sock_revents & @ffi.POLLIN) != 0 {
    // Accept new connection
    match @ffi.sock_accept(self.socket_fd) {
      Some(client) => {
        let client_raw = @ffi.sock_fd_raw(client)
        // Set client to nonblocking
        let _ = @ffi.io_set_nonblocking(client_raw)
        // Read message with timeout
        match read_socket_message(client_raw, timeout_ms=100, max_len=16384) {
          Some(data) => {
            let processed = self.observer.on_input(data)
            let _ = @ffi.io_write(self.master_fd, processed)

          }
          None => ()
        }
        let _ = @ffi.io_close(client_raw)

      }
      None => ()
    }
  }
  true
}

/// Run the agent event loop. Returns the child process exit status.
pub fn Agent::run(self : Agent) -> Int {
  while self.poll_once() {

  }
  self.cleanup()
  // Wait for child process
  match @ffi.proc_waitpid(self.child_pid, false) {
    Some((_pid, status)) => status
    None => 1
  }
}

/// Read a length-prefixed message from a socket fd.
/// Protocol: [4B LE length][payload]
/// The client_fd should already be nonblocking.
/// Uses io_poll with timeout to wait for data.
/// Returns None on timeout, error, or if message exceeds max_len.
pub fn read_socket_message(
  client_fd : Int,
  timeout_ms? : Int = 100,
  max_len? : Int = 16384
) -> Bytes? {
  // Poll for readable data (length header)
  let poll_fds = @ffi.pollfd_pack([(client_fd, @ffi.POLLIN)])
  let ready = @ffi.io_poll(poll_fds, 1, timeout_ms)
  if ready <= 0 {
    return None
  }
  let revents = @ffi.pollfd_revents(poll_fds, 0)
  if (revents & (@ffi.POLLHUP | @ffi.POLLERR)) != 0 {
    return None
  }
  if (revents & @ffi.POLLIN) == 0 {
    return None
  }
  // Read 4-byte length header
  let header = Bytes::make(4, 0)
  let n = @ffi.io_read(client_fd, header, 4)
  if n != 4 {
    return None
  }
  // Decode LE u32 length
  let len = header[0].to_int()
    | (header[1].to_int() << 8)
    | (header[2].to_int() << 16)
    | (header[3].to_int() << 24)
  if len <= 0 || len > max_len {
    return None
  }
  // Read payload
  let payload = Bytes::make(len, 0)
  let n2 = @ffi.io_read(client_fd, payload, len)
  if n2 != len {
    return None
  }
  Some(payload)
}

/// Write a length-prefixed message to a socket fd.
/// Protocol: [4B LE length][payload]
pub fn write_socket_message(fd : Int, data : Bytes) -> Bool {
  let len = data.length()
  // Build 4-byte LE length header
  let header : Array[Byte] = [
    (len & 0xFF).to_byte(),
    ((len >> 8) & 0xFF).to_byte(),
    ((len >> 16) & 0xFF).to_byte(),
    ((len >> 24) & 0xFF).to_byte(),
  ]
  let header_bytes = Bytes::from_array(header[:])
  // Write header
  let n1 = @ffi.io_write(fd, header_bytes)
  if n1 < 0 {
    return false
  }
  // Write payload
  let n2 = @ffi.io_write(fd, data)
  if n2 < 0 {
    return false
  }
  true
}
