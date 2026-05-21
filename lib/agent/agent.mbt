///|
/// Reason the event loop terminated, used to compute the exit status.
enum ExitReason {
  /// Child exited or was signalled naturally; carry the wait status.
  ChildExited(Int)
  /// A timeout (--timeout or --idle-timeout) fired.
  TimedOut
  /// The --until pattern was observed in the output stream.
  UntilHit
}

///|
pub struct Agent {
  pty_handle : @ffi.PtyHandle
  master_fd : Int
  child_pid : Int
  socket_fd : @ffi.SockFd
  socket_path : Bytes
  tty_handle : @ffi.TtyHandle? // 実 tty を raw 化した場合のみ Some
  observer : &@observer.Observer
  config : Config
  /// self-pipe read fd, monitored by the poll loop.
  sig_read_fd : Int
  mut stdin_open : Bool
  /// monotonic ms timestamp at startup, for --timeout.
  start_ms : Int64
  /// monotonic ms timestamp of the last PTY output, for --idle-timeout.
  mut last_output_ms : Int64
  /// tail of the previous output read, kept for cross-boundary --until matching.
  mut until_tail : Bytes
  /// set once a stop condition fires, so we wait for the child to die.
  mut exit_reason : ExitReason?
}

///|
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

///|
/// Create a new Agent from a parsed CLI Config.
pub fn Agent::new(
  observer : &@observer.Observer,
  config : Config,
) -> Agent raise {
  // Determine the PTY size.
  // interactive: prefer the real tty size, fall back to the configured size.
  // headless: always use the configured (virtual) size; never touch the tty.
  let (rows, cols) = match config.mode {
    Interactive =>
      match @ffi.tty_size(0) {
        Some((r, c)) => (r, c)
        None => (config.rows, config.cols)
      }
    Headless => (config.rows, config.cols)
  }
  // Open PTY
  let pty_handle = match @ffi.pty_open(cols, rows) {
    Some(h) => h
    None => raise Failure::Failure("pty_open failed")
  }
  // Spawn child process with execvp-style PATH resolution.
  let child_pid = match @ffi.pty_spawnv(pty_handle, config.command) {
    Some(pid) => pid
    None => {
      let _ = @ffi.pty_close(pty_handle)
      raise Failure::Failure("pty_spawnv failed")
    }
  }
  let master_fd = @ffi.pty_master_fd(pty_handle)
  // Setup socket (explicit path or auto-generated).
  let sock_path_str = match config.socket {
    Some(p) => p
    None => socket_path()
  }
  let sock_path_bytes = @ffi.to_utf8(sock_path_str)
  let socket_fd = match @ffi.sock_listen(sock_path_bytes) {
    Some(fd) => fd
    None => {
      let _ = @ffi.pty_close(pty_handle)
      raise Failure::Failure("sock_listen failed")
    }
  }
  if not(@ffi.sock_set_nonblocking(socket_fd)) {
    let _ = @ffi.sock_close(socket_fd)
    let _ = @ffi.sock_unlink(sock_path_bytes)
    let _ = @ffi.pty_close(pty_handle)
    raise Failure::Failure("sock_set_nonblocking failed")
  }
  // Setup signals.
  let _ = @ffi.sig_ignore(@ffi.SIGPIPE)
  // interactive only: SIGWINCH forwards real terminal resizes to the PTY.
  if config.mode is Interactive {
    let _ = @ffi.sig_setup_winch(master_fd)
  }
  // Self-pipe trick: route job-control / termination signals through a pipe
  // so they are handled synchronously in the poll loop.
  let sig_read_fd = match @ffi.sig_selfpipe_init() {
    Some((r, _w)) => {
      for s in [
        @ffi.SIGCHLD, @ffi.SIGTSTP, @ffi.SIGCONT, @ffi.SIGINT, @ffi.SIGTERM,
        @ffi.SIGQUIT, @ffi.SIGHUP,
      ] {
        let _ = @ffi.sig_register_selfpipe(s)

      }
      r
    }
    None => {
      let _ = @ffi.sock_close(socket_fd)
      let _ = @ffi.sock_unlink(sock_path_bytes)
      let _ = @ffi.pty_close(pty_handle)
      raise Failure::Failure("sig_selfpipe_init failed")
    }
  }
  // Setup TTY: interactive mode raw-izes the real tty; headless never does.
  let tty_handle = match config.mode {
    Interactive =>
      if @ffi.tty_is_tty(0) {
        match @ffi.tty_save(0) {
          Some(handle) => {
            let _ = @ffi.tty_set_raw(0)
            Some(handle)
          }
          None => None
        }
      } else {
        None
      }
    Headless => None
  }
  Agent::{
    pty_handle,
    master_fd,
    child_pid,
    socket_fd,
    socket_path: sock_path_bytes,
    tty_handle,
    observer,
    config,
    sig_read_fd,
    stdin_open: true,
    start_ms: @ffi.clock_monotonic_ms(),
    last_output_ms: @ffi.clock_monotonic_ms(),
    until_tail: b"",
    exit_reason: None,
  }
}

///|
/// Cleanup agent resources.
pub fn Agent::cleanup(self : Agent) -> Unit {
  match self.tty_handle {
    Some(handle) => {
      let _ = @ffi.tty_restore(handle)

    }
    None => ()
  }
  let _ = @ffi.io_close(self.sig_read_fd)
  let _ = @ffi.sock_unlink(self.socket_path)
  let _ = @ffi.sock_close(self.socket_fd)
  let _ = @ffi.pty_close(self.pty_handle)

}

///|
/// Create a sub-Bytes from buf[0..len).
/// Design rationale: MoonBit's Bytes has no sub_bytes/slice method.
/// FixedArray::makei + Bytes::from_array is the idiomatic workaround.
fn bytes_take(buf : Bytes, len : Int) -> Bytes {
  Bytes::from_array(FixedArray::makei(len, fn(i) { buf[i] })[:])
}

///|
/// Concatenate two Bytes.
fn bytes_concat(a : Bytes, b : Bytes) -> Bytes {
  let out : Array[Byte] = []
  for i = 0; i < a.length(); i = i + 1 {
    out.push(a[i])
  }
  for i = 0; i < b.length(); i = i + 1 {
    out.push(b[i])
  }
  Bytes::from_array(out[:])
}

///|
/// Return whether `needle` occurs as a substring of `haystack`.
fn bytes_contains(haystack : Bytes, needle : Bytes) -> Bool {
  let nlen = needle.length()
  if nlen == 0 {
    return true
  }
  let hlen = haystack.length()
  if nlen > hlen {
    return false
  }
  for start = 0; start <= hlen - nlen; start = start + 1 {
    let mut matched = true
    for j = 0; j < nlen; j = j + 1 {
      if haystack[start + j] != needle[j] {
        matched = false
        break
      }
    }
    if matched {
      return true
    }
  }
  false
}

///|
/// Scan freshly read PTY output for the --until pattern.
/// Concatenates the retained tail with `data` so matches spanning two reads
/// are detected, then keeps a new tail of len(pattern)-1 bytes.
fn Agent::scan_until(self : Agent, data : Bytes) -> Unit {
  match self.config.until {
    None => ()
    Some(pattern) => {
      let needle = @ffi.to_utf8(pattern)
      let combined = bytes_concat(self.until_tail, data)
      if bytes_contains(combined, needle) {
        self.exit_reason = Some(UntilHit)
      }
      // Retain the last (len(pattern)-1) bytes for the next scan.
      let keep = needle.length() - 1
      if keep > 0 && combined.length() > keep {
        self.until_tail = bytes_take(
          Bytes::from_array(
            FixedArray::makei(keep, fn(i) {
              combined[combined.length() - keep + i]
            })[:],
          ),
          keep,
        )
      } else if keep <= 0 {
        self.until_tail = b""
      } else {
        self.until_tail = combined
      }
    }
  }
}

///|
/// Compute the poll timeout (ms) until the next deadline, or -1 for infinite.
/// Considers --timeout and --idle-timeout; returns the smallest remaining.
fn Agent::poll_timeout_ms(self : Agent) -> Int {
  let now = @ffi.clock_monotonic_ms()
  let mut best : Int64? = None
  match self.config.timeout_ms {
    Some(t) => {
      let remaining = self.start_ms + t.to_int64() - now
      best = Some(remaining)
    }
    None => ()
  }
  match self.config.idle_timeout_ms {
    Some(t) => {
      let remaining = self.last_output_ms + t.to_int64() - now
      best = match best {
        Some(b) => if remaining < b { Some(remaining) } else { Some(b) }
        None => Some(remaining)
      }
    }
    None => ()
  }
  match best {
    None => -1
    Some(ms) => if ms < 0L { 0 } else { ms.to_int() }
  }
}

///|
/// Check whether a deadline has been reached; if so set exit_reason.
fn Agent::check_deadlines(self : Agent) -> Unit {
  if self.exit_reason is Some(_) {
    return
  }
  let now = @ffi.clock_monotonic_ms()
  match self.config.timeout_ms {
    Some(t) => if now - self.start_ms >= t.to_int64() {
      self.exit_reason = Some(TimedOut)
      return
    }
    None => ()
  }
  match self.config.idle_timeout_ms {
    Some(t) => if now - self.last_output_ms >= t.to_int64() {
      self.exit_reason = Some(TimedOut)
    }
    None => ()
  }
}

///|
/// Terminate the child group: SIGTERM, brief grace period, then SIGKILL.
/// The child is its own session/group leader (POSIX_SPAWN_SETSID), so the
/// child's pgid equals its pid; killpg(child_pid) targets the whole group.
fn Agent::terminate_child(self : Agent) -> Unit {
  let _ = @ffi.proc_killpg(self.child_pid, @ffi.SIGTERM)
  // Grace period: poll waitpid for up to ~2s before escalating to SIGKILL.
  for _i = 0; _i < 20; _i = _i + 1 {
    match @ffi.proc_waitpid(self.child_pid, true) {
      Some((pid, _)) => if pid == self.child_pid { return }
      None => ()
    }
    // sleep 100ms via poll on no fds
    let _ = @ffi.io_poll(b"", 0, 100)

  }
  let _ = @ffi.proc_killpg(self.child_pid, @ffi.SIGKILL)

}

///|
/// Handle a single signal number delivered through the self-pipe.
/// Returns true if the loop should continue, false if the child has ended.
fn Agent::handle_signal(self : Agent, sig : Int) -> Bool {
  if sig == @ffi.SIGCHLD {
    // Reap any state change. WUNTRACED reports stopped children.
    match @ffi.proc_waitpid_stopped(self.child_pid) {
      Some((pid, status, kind)) =>
        if pid == self.child_pid {
          if kind == @ffi.WAIT_EXITED || kind == @ffi.WAIT_SIGNALED {
            self.exit_reason = Some(ChildExited(status))
            return false
          } else if kind == @ffi.WAIT_STOPPED {
            // invariant: if the parent is running, the child must run too.
            match self.config.on_child_suspend {
              AutoResume => {
                let _ = @ffi.proc_killpg(self.child_pid, @ffi.SIGCONT)

              }
              Follow => {
                // Parent stops too; both stopped keeps the invariant intact.
                let _ = @ffi.sig_raise(@ffi.SIGSTOP)

              }
            }
          }
        }
      None => ()
    }
  } else if sig == @ffi.SIGTSTP {
    match self.config.on_parent_suspend {
      Transparent => {
        // Stop the child group first, then stop the parent.
        let _ = @ffi.proc_killpg(self.child_pid, @ffi.SIGSTOP)
        let _ = @ffi.sig_raise(@ffi.SIGSTOP)

      }
      Decouple => {
        // Stop only the parent; the child keeps running.
        let _ = @ffi.sig_raise(@ffi.SIGSTOP)

      }
    }
  } else if sig == @ffi.SIGCONT {
    // Parent resumed: restore the invariant by continuing the child if stopped.
    match @ffi.proc_waitpid_stopped(self.child_pid) {
      Some((pid, _status, kind)) =>
        if pid == self.child_pid && kind == @ffi.WAIT_STOPPED {
          let _ = @ffi.proc_killpg(self.child_pid, @ffi.SIGCONT)

        }
      None => ()
    }
  } else if sig == @ffi.SIGINT ||
    sig == @ffi.SIGTERM ||
    sig == @ffi.SIGQUIT ||
    sig == @ffi.SIGHUP {
    // Relay termination-class signals to the child group.
    let _ = @ffi.proc_killpg(self.child_pid, sig)

  }
  // SIGWINCH is handled directly by the FFI handler, not via the self-pipe.
  true
}

///|
/// Poll once for events. Returns false when the loop should exit.
pub fn Agent::poll_once(self : Agent) -> Bool {
  // Build pollfd array:
  //   index 0: stdin (or -1 when closed)
  //   index 1: master_fd
  //   index 2: socket_fd
  //   index 3: self-pipe read fd
  let entries : Array[(Int, Int)] = []
  if self.stdin_open {
    entries.push((0, @ffi.POLLIN))
  } else {
    entries.push((-1, 0))
  }
  entries.push((self.master_fd, @ffi.POLLIN))
  entries.push((@ffi.sock_fd_raw(self.socket_fd), @ffi.POLLIN))
  entries.push((self.sig_read_fd, @ffi.POLLIN))
  let poll_fds = @ffi.pollfd_pack(entries)
  let timeout = self.poll_timeout_ms()
  let ready = @ffi.io_poll(poll_fds, 4, timeout)
  if ready == @ffi.POLL_EINTR {
    // Design rationale: EINTR is not fatal. A signal interrupted poll();
    // the self-pipe byte will be observed on the next iteration.
    return true
  }
  if ready < 0 {
    return false // genuine poll error
  }
  // Timeout / idle deadline check (independent of which fds are ready).
  self.check_deadlines()
  if self.exit_reason is Some(_) {
    self.terminate_child()
    return false
  }
  // self-pipe: process delivered signals first.
  let sig_revents = @ffi.pollfd_revents(poll_fds, 3)
  if (sig_revents & @ffi.POLLIN) != 0 {
    let signals = @ffi.sig_drain(self.sig_read_fd)
    for s in signals {
      if not(self.handle_signal(s)) {
        return false
      }
    }
  }
  // stdin
  if self.stdin_open {
    let stdin_revents = @ffi.pollfd_revents(poll_fds, 0)
    // POLLIN and POLLHUP can be reported together when a pipe writer closes.
    // Handle the readable case first; only fall through to the EOF path once.
    let mut eof = false
    if (stdin_revents & @ffi.POLLIN) != 0 {
      let buf = Bytes::make(4096, 0)
      let n = @ffi.io_read(0, buf, 4096)
      if n > 0 {
        let data = self.observer.on_input(bytes_take(buf, n))
        let _ = @ffi.io_write(self.master_fd, data)

      } else {
        eof = true
      }
    }
    if (stdin_revents & (@ffi.POLLHUP | @ffi.POLLERR)) != 0 {
      eof = true
    }
    if eof {
      // stdin EOF: forward ^D once so a canonical-mode child sees end-of-input.
      // Design rationale: writing 0x04 emulates the controlling terminal's
      // VEOF. If the child runs in raw (non-canonical) mode it receives a
      // literal 0x04 byte instead of an EOF; that is an accepted limitation
      // of proxying a pipe-fed stdin into a PTY.
      let eot = Bytes::make(1, 0x04)
      let _ = @ffi.io_write(self.master_fd, eot)
      self.stdin_open = false
    }
  }
  // master_fd (PTY output)
  let master_revents = @ffi.pollfd_revents(poll_fds, 1)
  if (master_revents & @ffi.POLLIN) != 0 {
    let buf = Bytes::make(4096, 0)
    let n = @ffi.io_read(self.master_fd, buf, 4096)
    if n > 0 {
      let chunk = bytes_take(buf, n)
      self.last_output_ms = @ffi.clock_monotonic_ms()
      self.scan_until(chunk)
      let data = self.observer.on_output(chunk)
      let _ = @ffi.io_write(1, data)
      if self.exit_reason is Some(_) {
        self.terminate_child()
        return false
      }
    }
  }
  if (master_revents & (@ffi.POLLHUP | @ffi.POLLERR)) != 0 {
    return false // child process ended
  }
  // socket (external input injection)
  let sock_revents = @ffi.pollfd_revents(poll_fds, 2)
  if (sock_revents & @ffi.POLLIN) != 0 {
    match @ffi.sock_accept(self.socket_fd) {
      Some(client) => {
        let client_raw = @ffi.sock_fd_raw(client)
        let _ = @ffi.io_set_nonblocking(client_raw)
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

///|
/// Run the agent event loop. Returns the process exit status.
pub fn Agent::run(self : Agent) -> Int {
  while self.poll_once() {

  }
  self.cleanup()
  // Determine the exit status.
  // Design rationale: exit codes follow coreutils `timeout`:
  //   - timeout / idle-timeout -> 124
  //   - --until hit            -> 0 (treated as a successful condition)
  //   - natural exit           -> the child's own status
  match self.exit_reason {
    Some(TimedOut) => 124
    Some(UntilHit) => 0
    Some(ChildExited(status)) => decode_wait_status(status)
    None =>
      // Loop ended via POLLHUP without a processed SIGCHLD; reap the child now.
      match @ffi.proc_waitpid(self.child_pid, false) {
        Some((_pid, status)) => decode_wait_status(status)
        None => 1
      }
  }
}

///|
/// Decode a raw wait(2) status into a process exit code.
/// Exited normally -> exit code; killed by signal -> 128 + signal number.
fn decode_wait_status(status : Int) -> Int {
  // POSIX layout: low 7 bits = termination signal (0 if exited normally),
  // bits 8..15 = exit code when the low bits are 0.
  let sig = status & 0x7F
  if sig == 0 {
    (status >> 8) & 0xFF
  } else {
    128 + sig
  }
}

///|
/// Read a length-prefixed message from a socket fd.
/// Protocol: [4B LE length][payload]
/// The client_fd should already be nonblocking.
/// Uses io_poll with timeout to wait for data.
/// Returns None on timeout, error, or if message exceeds max_len.
pub fn read_socket_message(
  client_fd : Int,
  timeout_ms? : Int = 100,
  max_len? : Int = 16384,
) -> Bytes? {
  let poll_fds = @ffi.pollfd_pack([(client_fd, @ffi.POLLIN)])
  let ready = @ffi.io_poll(poll_fds, 1, timeout_ms)
  // EINTR (-2) and timeout (0) and error (-1) are all "no message".
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
  let header = Bytes::make(4, 0)
  let n = @ffi.io_read(client_fd, header, 4)
  if n != 4 {
    return None
  }
  let len = header[0].to_int() |
    (header[1].to_int() << 8) |
    (header[2].to_int() << 16) |
    (header[3].to_int() << 24)
  if len <= 0 || len > max_len {
    return None
  }
  let payload = Bytes::make(len, 0)
  let n2 = @ffi.io_read(client_fd, payload, len)
  if n2 != len {
    return None
  }
  Some(payload)
}

///|
/// Write a length-prefixed message to a socket fd.
/// Protocol: [4B LE length][payload]
pub fn write_socket_message(fd : Int, data : Bytes) -> Bool {
  let len = data.length()
  let header : Array[Byte] = [
    (len & 0xFF).to_byte(),
    ((len >> 8) & 0xFF).to_byte(),
    ((len >> 16) & 0xFF).to_byte(),
    ((len >> 24) & 0xFF).to_byte(),
  ]
  let header_bytes = Bytes::from_array(header[:])
  let n1 = @ffi.io_write(fd, header_bytes)
  if n1 < 0 {
    return false
  }
  let n2 = @ffi.io_write(fd, data)
  if n2 < 0 {
    return false
  }
  true
}
