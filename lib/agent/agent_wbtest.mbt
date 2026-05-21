///|
#borrow(path)
extern "C" fn hyoui_test_mkdir(path : Bytes) -> Int = "hyoui_test_mkdir"

///|
#borrow(path)
extern "C" fn hyoui_test_rmdir(path : Bytes) -> Int = "hyoui_test_rmdir"

///|
test "socket message roundtrip" {
  // Create test directory with 0700 permissions
  let pid = @ffi.getpid()
  let dir = "/tmp/hyoui_test_agent_" + pid.to_string()
  let dir_bytes = @ffi.to_utf8(dir)
  let _ = hyoui_test_mkdir(dir_bytes)
  let path = dir + "/test.sock"
  let path_bytes = @ffi.to_utf8(path)

  // Listen
  let server = @ffi.sock_listen(path_bytes)
  assert_true(server is Some(_))
  let server_fd = server.unwrap()

  // Connect
  let client = @ffi.sock_connect(path_bytes)
  assert_true(client is Some(_))
  let client_fd = client.unwrap()

  // Write message from client side
  let message = @ffi.to_utf8("hello hyoui agent")
  let write_ok = write_socket_message(@ffi.sock_fd_raw(client_fd), message)
  assert_true(write_ok)

  // Accept connection on server side
  let accepted = @ffi.sock_accept(server_fd)
  assert_true(accepted is Some(_))
  let accepted_fd = accepted.unwrap()

  // Set accepted fd to nonblocking
  assert_true(@ffi.io_set_nonblocking(@ffi.sock_fd_raw(accepted_fd)))

  // Read message from accepted connection
  let result = read_socket_message(
    @ffi.sock_fd_raw(accepted_fd),
    timeout_ms=1000,
  )
  assert_true(result is Some(_))
  let received = result.unwrap()
  assert_eq(received, message)

  // Cleanup
  let _ = @ffi.sock_close(accepted_fd)
  let _ = @ffi.sock_close(client_fd)
  let _ = @ffi.sock_close(server_fd)
  let _ = @ffi.sock_unlink(path_bytes)
  let _ = hyoui_test_rmdir(dir_bytes)
}

///|
/// Build a minimal headless Config running `command` for tests.
/// Headless mode avoids touching the real tty in CI.
fn test_config(command : Array[String]) -> Config {
  match parse_args(["--mode=headless", "--", ..command]) {
    Run(cfg) => cfg
    _ => abort("test_config: parse failed")
  }
}

///|
test "Agent new and cleanup" {
  let obs : @observer.NullObserver = @observer.NullObserver::new()
  // Use echo to create a short-lived process (PATH-resolved).
  let agent = Agent::new(obs, test_config(["echo", "hi"]))
  assert_true(agent.master_fd >= 0)
  assert_true(agent.child_pid > 0)
  assert_true(agent.socket_path.length() > 0)
  agent.cleanup()
}

///|
test "Agent run with echo exits cleanly" {
  let obs : @observer.NullObserver = @observer.NullObserver::new()
  let agent = Agent::new(obs, test_config(["echo", "hello"]))
  let status = agent.run()
  assert_eq(status, 0)
}

///|
test "Agent run with false returns nonzero exit status" {
  let obs : @observer.NullObserver = @observer.NullObserver::new()
  let agent = Agent::new(obs, test_config(["false"]))
  let status = agent.run()
  assert_eq(status, 1)
}

///|
test "decode_wait_status: normal exit and signalled" {
  // exited with code 0
  assert_eq(decode_wait_status(0), 0)
  // exited with code 1 (status bits 8..15)
  assert_eq(decode_wait_status(1 << 8), 1)
  // killed by signal 9
  assert_eq(decode_wait_status(9), 128 + 9)
}

///|
test "bytes_contains: substring matching" {
  assert_true(bytes_contains(b"hello world", b"o w"))
  assert_true(bytes_contains(b"abc", b""))
  assert_false(bytes_contains(b"abc", b"abcd"))
  assert_false(bytes_contains(b"abc", b"xyz"))
}
