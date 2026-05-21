// ===== helpers =====

///|
test "to_utf8: converts String to UTF-8 Bytes" {
  inspect(to_utf8("hello"), content="b\"hello\"")
  inspect(to_utf8(""), content="b\"\"")
  // マルチバイト: "あ" = 0xE3 0x81 0x82 (3 bytes)
  let a = to_utf8("あ")
  inspect(a.length(), content="3")
}

// ===== pty =====

///|
test "pty_open: returns valid handle" {
  let handle = pty_open(80, 24)
  inspect(not(handle is None), content="true")
  // cleanup
  let _ = pty_close(handle.unwrap())
}

///|
test "pty_master_fd: returns valid fd" {
  let handle = pty_open(80, 24).unwrap()
  let fd = pty_master_fd(handle)
  inspect(fd >= 0, content="true")
  let _ = pty_close(handle)
}

///|
test "pty_close: succeeds" {
  let handle = pty_open(80, 24).unwrap()
  inspect(pty_close(handle), content="true")
}

///|
test "pty_spawnv: empty argv returns None" {
  let handle = pty_open(80, 24).unwrap()
  let pid = pty_spawnv(handle, [])
  inspect(pid, content="None")
  let _ = pty_close(handle)
}

///|
test "pty_resize: succeeds on valid handle" {
  let handle = pty_open(80, 24).unwrap()
  inspect(pty_resize(handle, 120, 40), content="true")
  let _ = pty_close(handle)
}

// W4: pty_spawnv normal case

///|
test "pty_spawnv: echo hello returns valid pid and waitpid succeeds" {
  let handle = pty_open(80, 24).unwrap()
  // execvp-style: argv[0] resolved against $PATH
  let pid = pty_spawnv(handle, ["echo", "hello"])
  inspect(not(pid is None), content="true")
  let child_pid = pid.unwrap()
  inspect(child_pid > 0, content="true")
  // Wait for child to finish (blocking)
  let result = proc_waitpid(child_pid, false)
  inspect(not(result is None), content="true")
  // Verify exit status is 0
  match result {
    Some((_, status)) => inspect(status, content="0")
    None => inspect(false, content="true") // waitpid failed unexpectedly
  }
  let _ = pty_close(handle)
}

// ===== tty =====

// W6: Fix tautological test - use meaningful assertions

///|
test "tty_is_tty: invalid fd returns false" {
  inspect(tty_is_tty(-1), content="false")
}

///|
test "tty_is_tty: does not crash on stdin" {
  // CI/test environments may not have a tty on stdin, so just verify no crash
  let _ = tty_is_tty(0)
}

// W5: tty_set_raw + tty_restore on pty fd

///|
test "tty_set_raw and tty_restore: works on pty master fd" {
  let handle = pty_open(80, 24).unwrap()
  let master_fd = pty_master_fd(handle)
  // master_fd is a pty device, so tty_save/set_raw/restore should work
  let tty_handle = tty_save(master_fd)
  inspect(not(tty_handle is None), content="true")
  let saved = tty_handle.unwrap()
  inspect(tty_set_raw(master_fd), content="true")
  inspect(tty_restore(saved), content="true")
  let _ = pty_close(handle)
}

///|
test "tty_size: returns None or valid size" {
  // テスト環境では tty でない場合 None を返す
  match tty_size(0) {
    None => inspect(true, content="true")
    Some((rows, cols)) => {
      inspect(rows > 0, content="true")
      inspect(cols > 0, content="true")
    }
  }
}

///|
test "tty_size: invalid fd returns None" {
  inspect(tty_size(-1), content="None")
}

///|
test "tty_save: invalid fd returns None" {
  // save していない無効なハンドルで restore → Rust 側で -1 を返すはず
  // TtyHandle を直接作れないので tty_save を無効 fd で呼ぶ
  let handle = tty_save(-1)
  inspect(handle is None, content="true")
}

// ===== io =====

///|
test "io_write: empty bytes to stdout returns 0" {
  inspect(io_write(1, b""), content="0")
}

///|
test "io_read: invalid fd returns -1" {
  let buf = Bytes::make(64, 0)
  inspect(io_read(-1, buf, 64), content="-1")
}

///|
test "io_close: invalid fd returns false" {
  inspect(io_close(-1), content="false")
}

// W5: io_set_nonblocking on pty master_fd

///|
test "io_set_nonblocking: works on pty master fd" {
  let handle = pty_open(80, 24).unwrap()
  let master_fd = pty_master_fd(handle)
  inspect(io_set_nonblocking(master_fd), content="true")
  let _ = pty_close(handle)
}

// W5: io_poll with pty master_fd

///|
test "io_poll: timeout 0 on pty master_fd returns without error" {
  let handle = pty_open(80, 24).unwrap()
  let master_fd = pty_master_fd(handle)
  let fds = pollfd_pack([(master_fd, POLLIN)])
  // timeout=0 means immediate return; result >= 0 means no error
  let result = io_poll(fds, 1, 0)
  inspect(result >= 0, content="true")
  let _ = pty_close(handle)
}

///|
test "pollfd_pack: creates correct byte layout" {
  let buf = pollfd_pack([(5, POLLIN)])
  // 8 bytes per entry
  inspect(buf.length(), content="8")
  // fd = 5 at offset 0 (LE i32)
  inspect(buf[0].to_int(), content="5")
  inspect(buf[1].to_int(), content="0")
  inspect(buf[2].to_int(), content="0")
  inspect(buf[3].to_int(), content="0")
  // events = POLLIN (0x0001) at offset 4 (LE i16)
  inspect(buf[4].to_int(), content="1")
  inspect(buf[5].to_int(), content="0")
  // revents = 0 at offset 6 (LE i16)
  inspect(buf[6].to_int(), content="0")
  inspect(buf[7].to_int(), content="0")
}

///|
test "pollfd_revents: reads revents from buffer" {
  let buf = pollfd_pack([(5, POLLIN)])
  // revents is initially 0
  inspect(pollfd_revents(buf, 0), content="0")
}

///|
test "pollfd_pack: multiple entries" {
  let buf = pollfd_pack([(3, POLLIN), (7, POLLHUP)])
  inspect(buf.length(), content="16")
  // entry 0: fd=3
  inspect(buf[0].to_int(), content="3")
  // entry 1: fd=7
  inspect(buf[8].to_int(), content="7")
  // entry 1: events = POLLHUP (0x0010)
  inspect(buf[12].to_int(), content="16")
  inspect(buf[13].to_int(), content="0")
}

// ===== sock =====

// sock テストは親ディレクトリが 0700 である必要がある（Rust 側のセキュリティチェック）。
// /tmp は 1777 なので直接使えない。テストでは 0700 のサブディレクトリを利用する。

///|
#borrow(path)
extern "C" fn hyoui_test_mkdir(path : Bytes) -> Int = "hyoui_test_mkdir"

///|
#borrow(path)
extern "C" fn hyoui_test_rmdir(path : Bytes) -> Int = "hyoui_test_rmdir"

///|
test "sock_listen: creates socket and cleanup" {
  let dir = to_utf8("/tmp/hyoui_test")
  let _ = hyoui_test_rmdir(dir) // 前回残りを削除
  let rc = hyoui_test_mkdir(dir)
  inspect(rc, content="0")
  let path = to_utf8("/tmp/hyoui_test/test.sock")
  // まず既存を削除
  let _ = sock_unlink(path)
  let sock = sock_listen(path)
  inspect(not(sock is None), content="true")
  // cleanup: close fd, then unlink
  match sock {
    Some(SockFd(fd)) => {
      let _ = io_close(fd)
    }
    None => ()
  }
  inspect(sock_unlink(path), content="true")
  let _ = hyoui_test_rmdir(dir)
}

// W5: sock_connect + sock_accept roundtrip

///|
test "sock_connect and sock_accept: roundtrip" {
  let dir = to_utf8("/tmp/hyoui_test_rt")
  let _ = hyoui_test_rmdir(dir)
  let rc = hyoui_test_mkdir(dir)
  inspect(rc, content="0")
  let path = to_utf8("/tmp/hyoui_test_rt/rt.sock")
  let _ = sock_unlink(path)
  let server = sock_listen(path)
  inspect(not(server is None), content="true")
  let server_sock = server.unwrap()
  // Set server to non-blocking for accept
  let SockFd(server_fd) = server_sock
  inspect(io_set_nonblocking(server_fd), content="true")
  // Connect from client side
  let client = sock_connect(path)
  inspect(not(client is None), content="true")
  let client_sock = client.unwrap()
  // Accept the connection
  let accepted = sock_accept(server_sock)
  inspect(not(accepted is None), content="true")
  // Cleanup
  let SockFd(client_fd) = client_sock
  let _ = io_close(client_fd)
  match accepted {
    Some(SockFd(afd)) => {
      let _ = io_close(afd)
    }
    None => ()
  }
  let _ = io_close(server_fd)
  inspect(sock_unlink(path), content="true")
  let _ = hyoui_test_rmdir(dir)
}

// ===== sig =====

///|
test "sig_ignore: SIGPIPE succeeds" {
  inspect(sig_ignore(SIGPIPE), content="true")
}

// W5: sig_setup_winch on pty master_fd

///|
test "sig_setup_winch: succeeds on pty master fd" {
  let handle = pty_open(80, 24).unwrap()
  let master_fd = pty_master_fd(handle)
  inspect(sig_setup_winch(master_fd), content="true")
  let _ = pty_close(handle)
}

// ===== proc =====

///|
test "proc_waitpid: pid -1 with nohang returns None" {
  // pid -1 + WNOHANG: 子プロセスがないので None
  inspect(proc_waitpid(-1, true), content="None")
}

///|
test "proc_kill: pid 0 with signal 0 checks process group" {
  // signal 0 は kill のエラーチェック用。pid 0 は自プロセスグループ → 成功するはず
  inspect(proc_kill(0, 0), content="true")
}

///|
test "clock_monotonic_ms: monotonic and positive" {
  let t1 = clock_monotonic_ms()
  inspect(t1 > 0L, content="true")
  let t2 = clock_monotonic_ms()
  // monotonic clock never goes backwards
  inspect(t2 >= t1, content="true")
}

///|
test "sig_selfpipe_init and sig_drain: roundtrip via raise" {
  match sig_selfpipe_init() {
    None => inspect(false, content="true") // init must succeed
    Some((read_fd, _write_fd)) => {
      // Register SIGUSR1 (30 on macOS / Linux) to the self-pipe, then raise it.
      let sigusr1 = 30
      inspect(sig_register_selfpipe(sigusr1), content="true")
      inspect(sig_raise(sigusr1), content="true")
      let signals = sig_drain(read_fd)
      inspect(signals.length() >= 1, content="true")
      inspect(signals[0], content="30")
      let _ = io_close(read_fd)
    }
  }
}

// ===== helpers: getenv, getpid, getuid, ensure_dir =====

///|
test "getenv returns non-empty for PATH" {
  let path = getenv("PATH")
  assert_true(path.length() > 0)
}

///|
test "getenv returns empty for nonexistent var" {
  let val = getenv("SHIMUX_NONEXISTENT_VAR_12345")
  assert_eq(val, "")
}

///|
test "getpid returns positive" {
  assert_true(getpid() > 0)
}

///|
test "getuid returns valid uid" {
  // uid is unsigned, any value is valid but should be consistent
  let uid = getuid()
  let uid2 = getuid()
  assert_eq(uid, uid2)
}

///|
test "ensure_dir creates directory" {
  let dir = "/tmp/hyoui_test_ensure_dir_" + getpid().to_string()
  assert_true(ensure_dir(dir))
  // cleanup
  let _ = hyoui_test_rmdir(to_utf8(dir))
}
