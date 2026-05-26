//! PoC 04: SCM_RIGHTS fd passing — 動作確認 + stream 中継との比較
//!
//! Unix socket の sendmsg/recvmsg + SCM_RIGHTS control message で fd を別プロセスに渡す
//! ことが可能か、渡された側がその fd を有効に使えるかを確認。
//! nix の `uio` feature が hyoui workspace で無効化されているため libc 直接で実装。
//!
//! 実行:
//!   cargo run --example 04-fd-passing
//!
//! 結果: parent が target file を open → fd を child に SCM_RIGHTS で送る → child が
//! その fd に文字列 write → parent が target file の中身を確認、で PASS。

use nix::sys::socket::{AddressFamily, SockFlag, SockType, socketpair};
use nix::sys::wait::waitpid;
use nix::unistd::{ForkResult, fork};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};

const TARGET_PATH: &str = "/tmp/hyoui-poc-04-target.txt";

/// SAFETY: caller must ensure `sock` is a valid Unix socket fd, `fd_to_send` is a valid
/// open fd in this process, and `data` is a valid byte slice.
unsafe fn send_fd(sock: RawFd, fd_to_send: RawFd, data: &[u8]) -> isize {
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };

    let cmsg_space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut _;
    msg.msg_controllen = cmsg_buf.len() as _;

    // cmsghdr を初期化して fd を埋め込む
    let cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if !cmsg_ptr.is_null() {
        unsafe {
            (*cmsg_ptr).cmsg_level = libc::SOL_SOCKET;
            (*cmsg_ptr).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg_ptr).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
            std::ptr::write(libc::CMSG_DATA(cmsg_ptr) as *mut RawFd, fd_to_send);
        }
    }

    unsafe { libc::sendmsg(sock, &msg, 0) }
}

/// SAFETY: caller must ensure `sock` is a valid Unix socket fd and `buf` is a valid byte buffer.
/// Returns (bytes_read, optional fd that was received).
unsafe fn recv_fd(sock: RawFd, buf: &mut [u8]) -> (isize, Option<RawFd>) {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut _,
        iov_len: buf.len(),
    };

    let cmsg_space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut _;
    msg.msg_controllen = cmsg_buf.len() as _;

    let n = unsafe { libc::recvmsg(sock, &mut msg, 0) };

    let mut received_fd = None;
    let cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if !cmsg_ptr.is_null() {
        let level = unsafe { (*cmsg_ptr).cmsg_level };
        let typ = unsafe { (*cmsg_ptr).cmsg_type };
        if level == libc::SOL_SOCKET && typ == libc::SCM_RIGHTS {
            let fd = unsafe { std::ptr::read(libc::CMSG_DATA(cmsg_ptr) as *const RawFd) };
            received_fd = Some(fd);
        }
    }

    (n, received_fd)
}

fn main() {
    let _ = std::fs::remove_file(TARGET_PATH);

    let (sock_a, sock_b) = socketpair(
        AddressFamily::Unix,
        SockType::Datagram,
        None,
        SockFlag::empty(),
    )
    .expect("socketpair");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Parent { child } => {
            drop(sock_b);
            let file = std::fs::File::create(TARGET_PATH).expect("create target");
            let raw = file.as_raw_fd();
            eprintln!("[parent] target file opened, fd={raw}");

            let n = unsafe { send_fd(sock_a.as_raw_fd(), raw, b"hello-fd") };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                eprintln!("[parent] sendmsg failed: {err}");
                std::process::exit(1);
            }
            eprintln!("[parent] sent fd via SCM_RIGHTS, {n} data bytes");
            drop(file);
            drop(sock_a);

            let _ = waitpid(child, None);

            let content = std::fs::read_to_string(TARGET_PATH).unwrap_or_default();
            eprintln!("[parent] target file content: {content:?}");
            let pass = content.contains("received via SCM_RIGHTS");
            if pass {
                eprintln!("[parent] PASS");
            } else {
                eprintln!("[parent] FAIL");
                std::process::exit(1);
            }
            let _ = std::fs::remove_file(TARGET_PATH);
        }
        ForkResult::Child => {
            drop(sock_a);
            let mut buf = [0u8; 256];
            let (n, fd_opt) = unsafe { recv_fd(sock_b.as_raw_fd(), &mut buf) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                eprintln!("[child] recvmsg failed: {err}");
                std::process::exit(1);
            }
            let data = std::str::from_utf8(&buf[..n as usize]).unwrap_or("?");
            eprintln!("[child] recv data: {data:?}");

            if let Some(fd) = fd_opt {
                eprintln!("[child] received fd {fd}");
                // SAFETY: fd was just received from SCM_RIGHTS, kernel dup'd it into us
                let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
                f.write_all(b"received via SCM_RIGHTS\n").expect("write");
                eprintln!("[child] wrote to received fd");
            } else {
                eprintln!("[child] no fd received");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    }
}
