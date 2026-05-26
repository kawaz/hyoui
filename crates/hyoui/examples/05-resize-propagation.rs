//! PoC 05: leader resize 伝播 検証
//!
//! 子 pty に対して TIOCSWINSZ で resize → 子 (bash) が SIGWINCH を受けて
//! $COLUMNS / $LINES を更新できることを確認。
//!
//! Protocol (簡易 1 byte type):
//!   0x01 + u16 LE cols + u16 LE rows  = resize request
//!   0x02 + bytes                       = data (子 stdin へ)
//!
//! 実行:
//!   cargo run --example 05-resize-propagation -- test
//!   cargo run --example 05-resize-propagation -- daemon [sock_path]

use hyoui::sys::{Pty, UnixSock};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_SOCK: &str = "/tmp/hyoui-poc-05.sock";

fn main() {
    let role = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test".to_string());
    let path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| DEFAULT_SOCK.to_string());
    match role.as_str() {
        "daemon" => daemon_role(&path),
        "test" => test_role(),
        _ => {
            eprintln!("usage: 05-resize-propagation <daemon|test>");
            std::process::exit(2);
        }
    }
}

fn daemon_role(sock_path: &str) {
    let _ = std::fs::remove_file(sock_path);
    let listener = UnixSock::listen(sock_path).expect("listen");
    let spawned = Pty::spawn(
        &[
            "bash",
            "-c",
            // stty size は "rows cols"。SIGWINCH trap で都度出力、初回も。
            // read -t は signal で即時中断するので trap がすぐ走る。
            // trap 後の "continue" で read を続行、SIGWINCH を何回でも受ける。
            r#"trap 'echo "size: $(stty size)"' WINCH; echo "size: $(stty size)"; while read -r -t 30 _; do :; done; sleep 30"#,
        ],
        80,
        24,
    )
    .expect("spawn");
    let master_raw = spawned.pty.master_fd().as_raw_fd();
    fcntl(
        spawned.pty.master_fd(),
        FcntlArg::F_SETFL(OFlag::O_NONBLOCK),
    )
    .expect("nonblock");
    eprintln!(
        "[daemon] listening {sock_path}, child pid {} (80x24 initial)",
        spawned.child
    );

    // 1 client only (simplicity)
    let client = listener.accept().expect("accept");
    fcntl(client.as_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).ok();
    let client_raw = client.as_raw_fd();
    eprintln!("[daemon] client connected");

    loop {
        let revents: Vec<PollFlags> = {
            let mut polls = [
                PollFd::new(client.as_fd(), PollFlags::POLLIN),
                PollFd::new(spawned.pty.master_fd(), PollFlags::POLLIN),
            ];
            let timeout = PollTimeout::try_from(500i32).unwrap();
            match nix::poll::poll(&mut polls, timeout) {
                Ok(0) => continue,
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    eprintln!("[daemon] poll err {e:?}");
                    break;
                }
            }
            polls
                .iter()
                .map(|p| p.revents().unwrap_or(PollFlags::empty()))
                .collect()
        };

        if revents[0].contains(PollFlags::POLLIN) {
            let mut buf = [0u8; 4096];
            // SAFETY: client_raw is owned by `client`
            let n = unsafe { libc::read(client_raw, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                eprintln!("[daemon] client closed");
                break;
            }
            let n = n as usize;
            if buf[0] == 0x01 && n >= 5 {
                let cols = u16::from_le_bytes([buf[1], buf[2]]);
                let rows = u16::from_le_bytes([buf[3], buf[4]]);
                eprintln!("[daemon] resize req {cols}x{rows}");
                if let Err(e) = spawned.pty.resize(cols, rows) {
                    eprintln!("[daemon] resize err: {e:?}");
                }
            } else if buf[0] == 0x02 && n > 1 {
                // SAFETY: master_raw is owned by spawned.pty
                unsafe {
                    libc::write(master_raw, buf.as_ptr().add(1) as *const _, n - 1);
                }
            }
        }

        if revents[1].contains(PollFlags::POLLIN) {
            let mut buf = [0u8; 4096];
            // SAFETY: master_raw is owned by spawned.pty
            let n = unsafe { libc::read(master_raw, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                eprintln!("[daemon] master EOF");
                break;
            }
            // broadcast to client
            // SAFETY: client_raw owned
            unsafe {
                libc::write(client_raw, buf.as_ptr() as *const _, n as usize);
            }
        }
    }
}

fn test_role() {
    // socket dir 作成 (0700)
    use std::os::unix::fs::PermissionsExt;
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!(
        "{}/hyoui-poc-05-{}",
        tmp.trim_end_matches('/'),
        std::process::id()
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    let sock_path = format!("{dir}/sock");

    let exe = std::env::current_exe().expect("current_exe");
    let mut daemon = std::process::Command::new(&exe)
        .args(["daemon", &sock_path])
        .spawn()
        .expect("spawn daemon");

    let start = Instant::now();
    while !Path::new(&sock_path).exists() {
        if start.elapsed() > Duration::from_secs(3) {
            eprintln!("[test] FAIL: socket did not appear");
            let _ = daemon.kill();
            std::process::exit(1);
        }
        thread::sleep(Duration::from_millis(50));
    }

    let mut client = UnixStream::connect(&sock_path).expect("connect");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    thread::sleep(Duration::from_millis(200));

    // 初回 size の echo を吸う
    let mut buf = [0u8; 1024];
    let initial = client.read(&mut buf).unwrap_or(0);
    let initial_str = String::from_utf8_lossy(&buf[..initial]).into_owned();
    eprintln!("[test] initial output ({initial} bytes): {initial_str:?}");
    // stty size は "rows cols" 順 (= "24 80")
    let initial_ok = initial_str.contains("size: 24 80");

    // resize to 160x48
    let mut msg = [0u8; 5];
    msg[0] = 0x01;
    msg[1..3].copy_from_slice(&160u16.to_le_bytes());
    msg[3..5].copy_from_slice(&48u16.to_le_bytes());
    client.write_all(&msg).expect("write resize");
    eprintln!("[test] sent resize 160x48");

    // SIGWINCH trap が echo するのを待つ
    thread::sleep(Duration::from_millis(500));
    let n = client.read(&mut buf).unwrap_or(0);
    let after = String::from_utf8_lossy(&buf[..n]).into_owned();
    eprintln!("[test] after resize ({n} bytes): {after:?}");
    // stty size は "rows cols" 順 (= "48 160")
    let after_ok = after.contains("size: 48 160");

    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_dir(&dir);

    eprintln!("[test] initial_ok={initial_ok}, after_ok={after_ok}");
    if initial_ok && after_ok {
        eprintln!("[test] PASS");
    } else {
        eprintln!("[test] FAIL");
        std::process::exit(1);
    }
}
