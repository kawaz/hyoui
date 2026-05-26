//! PoC 02: 複数 attach + broadcast 検証
//!
//! 子 pty (`sh -c "stty -icanon -echo; cat"`) を forkpty で起動、Unix socket で複数 client
//! から接続を受け、daemon が子 pty 出力を全 client に broadcast + 各 client の stdin を
//! 子 pty に multiplex で write することを確認。
//!
//! 実行:
//!   cargo run --example 02-multi-attach -- test
//!   # test 親が daemon (子プロセス) を起動、2 client connect、broadcast 検証、exit
//!
//!   cargo run --example 02-multi-attach -- daemon [sock_path]
//!   # daemon 単独起動 (手動で client 接続するとき)

use hyoui::sys::{Pty, UnixSock};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_SOCK: &str = "/tmp/hyoui-poc-02.sock";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).cloned().unwrap_or_else(|| "test".to_string());
    let path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| DEFAULT_SOCK.to_string());

    match role.as_str() {
        "daemon" => daemon_role(&path),
        "test" => test_role(&path),
        _ => {
            eprintln!("usage: 02-multi-attach <daemon|test> [sock_path]");
            std::process::exit(2);
        }
    }
}

fn set_nonblocking(fd: &OwnedFd) {
    fcntl(fd.as_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).expect("nonblock");
}

fn daemon_role(sock_path: &str) {
    let _ = std::fs::remove_file(sock_path);
    let listener = UnixSock::listen(sock_path).expect("listen");
    let spawned =
        Pty::spawn(&["sh", "-c", "stty -icanon -echo 2>/dev/null; cat"], 80, 24).expect("spawn");
    let master = spawned.pty.into_master();
    set_nonblocking(&master);
    let master_raw = master.as_raw_fd();
    eprintln!(
        "[daemon] listening {sock_path}, child pid {}",
        spawned.child
    );

    let mut clients: Vec<OwnedFd> = Vec::new();
    let mut loop_count = 0u64;

    loop {
        loop_count += 1;
        // poll set 構築 + poll、revents を別 Vec にコピーして polls を drop (= borrow 解放)
        let revents: Vec<PollFlags> = {
            let mut polls: Vec<PollFd> = Vec::with_capacity(2 + clients.len());
            polls.push(PollFd::new(listener.as_fd(), PollFlags::POLLIN));
            polls.push(PollFd::new(master.as_fd(), PollFlags::POLLIN));
            for c in &clients {
                polls.push(PollFd::new(c.as_fd(), PollFlags::POLLIN));
            }
            let timeout = PollTimeout::try_from(1000i32).unwrap();
            match nix::poll::poll(&mut polls, timeout) {
                Ok(0) => continue,
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    eprintln!("[daemon] poll error: {e:?}");
                    break;
                }
            }
            polls
                .iter()
                .map(|p| p.revents().unwrap_or(PollFlags::empty()))
                .collect()
        };

        // listener accept
        if revents[0].contains(PollFlags::POLLIN) {
            match listener.accept() {
                Ok(fd) => {
                    set_nonblocking(&fd);
                    eprintln!("[daemon] +client, total {}", clients.len() + 1);
                    clients.push(fd);
                }
                Err(e) => eprintln!("[daemon] accept err: {e:?}"),
            }
        }

        // master read → broadcast
        if revents[1].contains(PollFlags::POLLIN) {
            let mut buf = [0u8; 4096];
            // SAFETY: master_raw valid while `master` owned. read(2) signature satisfied.
            let n = unsafe { libc::read(master_raw, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                eprintln!("[daemon] master EOF (n={n}), exit loop");
                break;
            }
            let data = &buf[..n as usize];
            eprintln!(
                "[daemon] master read {} bytes: {:?}",
                n,
                String::from_utf8_lossy(data)
            );
            let mut alive = Vec::with_capacity(clients.len());
            for c in clients.drain(..) {
                // SAFETY: c.as_raw_fd() valid for owned client fd
                let w =
                    unsafe { libc::write(c.as_raw_fd(), data.as_ptr() as *const _, data.len()) };
                if w >= 0 {
                    alive.push(c);
                } else {
                    eprintln!("[daemon] client write failed, drop");
                }
            }
            clients = alive;
        }

        // client reads → write to master
        let mut alive = Vec::with_capacity(clients.len());
        for (i, c) in clients.drain(..).enumerate() {
            let poll_idx = 2 + i;
            let ready = poll_idx < revents.len() && revents[poll_idx].contains(PollFlags::POLLIN);
            if !ready {
                alive.push(c);
                continue;
            }
            let mut buf = [0u8; 4096];
            // SAFETY: c.as_raw_fd() valid
            let n = unsafe { libc::read(c.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                let data = &buf[..n as usize];
                eprintln!(
                    "[daemon] client read {} bytes: {:?}",
                    n,
                    String::from_utf8_lossy(data)
                );
                // SAFETY: master_raw valid
                unsafe { libc::write(master_raw, data.as_ptr() as *const _, data.len()) };
                alive.push(c);
            } else if n == 0 {
                eprintln!("[daemon] client EOF, drop");
            } else {
                let err = std::io::Error::last_os_error();
                if matches!(err.kind(), std::io::ErrorKind::WouldBlock) {
                    alive.push(c);
                } else {
                    eprintln!("[daemon] client read err {err}, drop");
                }
            }
        }
        clients = alive;

        if loop_count % 10000 == 0 {
            // 進捗 mark (デバッグ用、通常出ない)
        }
    }
    eprintln!("[daemon] exit");
}

fn test_role(_default_path: &str) {
    // UnixSock::listen は parent dir 0700 必須 → subdir を作って渡す
    use std::os::unix::fs::PermissionsExt;
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!(
        "{}/hyoui-poc-02-{}",
        tmp.trim_end_matches('/'),
        std::process::id()
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    let sock_path = format!("{dir}/sock");

    let exe = std::env::current_exe().expect("current_exe");
    eprintln!(
        "[test] starting daemon: {} daemon {sock_path}",
        exe.display()
    );
    let mut daemon = std::process::Command::new(&exe)
        .args(["daemon", &sock_path])
        .spawn()
        .expect("spawn daemon");

    // クリーンアップは後の処理 (=daemon kill 後) と一緒
    let cleanup_dir = dir.clone();
    let cleanup = move || {
        let _ = std::fs::remove_file(format!("{cleanup_dir}/sock"));
        let _ = std::fs::remove_dir(&cleanup_dir);
    };
    // shadow して以降 sock_path を local 名にする
    let sock_path = sock_path.as_str();

    let start = Instant::now();
    while !Path::new(sock_path).exists() {
        if start.elapsed() > Duration::from_secs(3) {
            eprintln!("[test] FAIL: socket did not appear in 3s");
            let _ = daemon.kill();
            let _ = daemon.wait();
            std::process::exit(1);
        }
        thread::sleep(Duration::from_millis(50));
    }
    eprintln!("[test] socket appeared in {:?}", start.elapsed());

    let mut client_a = UnixStream::connect(sock_path).expect("connect a");
    thread::sleep(Duration::from_millis(50));
    let mut client_b = UnixStream::connect(sock_path).expect("connect b");
    eprintln!("[test] both clients connected");
    thread::sleep(Duration::from_millis(200));

    // client A から write
    client_a.write_all(b"hello\n").expect("write a");
    eprintln!("[test] wrote 'hello\\n' from client A");

    // 両 client から read (timeout 付き)
    client_a
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client_b
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut buf_a = [0u8; 1024];
    let mut buf_b = [0u8; 1024];
    let na = client_a.read(&mut buf_a).unwrap_or(0);
    let nb = client_b.read(&mut buf_b).unwrap_or(0);
    let recv_a = String::from_utf8_lossy(&buf_a[..na]).into_owned();
    let recv_b = String::from_utf8_lossy(&buf_b[..nb]).into_owned();
    eprintln!("[test] client A received ({} bytes): {recv_a:?}", na);
    eprintln!("[test] client B received ({} bytes): {recv_b:?}", nb);

    let a_ok = recv_a.contains("hello");
    let b_ok = recv_b.contains("hello");

    // client B から write、両 client から read で multiplex も確認
    client_b.write_all(b"world\n").expect("write b");
    eprintln!("[test] wrote 'world\\n' from client B");
    let na2 = client_a.read(&mut buf_a).unwrap_or(0);
    let nb2 = client_b.read(&mut buf_b).unwrap_or(0);
    let recv_a2 = String::from_utf8_lossy(&buf_a[..na2]).into_owned();
    let recv_b2 = String::from_utf8_lossy(&buf_b[..nb2]).into_owned();
    eprintln!(
        "[test] (after B write) A received ({} bytes): {recv_a2:?}",
        na2
    );
    eprintln!(
        "[test] (after B write) B received ({} bytes): {recv_b2:?}",
        nb2
    );
    let a2_ok = recv_a2.contains("world");
    let b2_ok = recv_b2.contains("world");

    let _ = daemon.kill();
    let _ = daemon.wait();
    cleanup();

    let pass = a_ok && b_ok && a2_ok && b2_ok;
    eprintln!(
        "[test] result: A_recv_from_A={a_ok}, B_recv_from_A={b_ok}, A_recv_from_B={a2_ok}, B_recv_from_B={b2_ok}"
    );
    if pass {
        eprintln!("[test] PASS");
        std::process::exit(0);
    } else {
        eprintln!("[test] FAIL");
        std::process::exit(1);
    }
}
