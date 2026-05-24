//! File-descriptor helpers built on top of `nix::unistd::{read,write}` +
//! `nix::fcntl`. EINTR is retried; partial writes are looped.
//!
//! We deliberately do NOT wrap `OwnedFd` in a newtype — the standard
//! [`std::os::fd::OwnedFd`] already gives RAII close, `From<OwnedFd> for File`
//! interop, and `AsRawFd`. We only need an extension trait for the syscalls
//! we want to be uniform across all owned descriptors.

use std::os::fd::AsFd;

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};

use super::error::{Error, Result};

/// Methods we want available on any owned descriptor.
pub trait FdExt: AsFd {
    /// `read(2)` with EINTR retry. Returns `Ok(0)` on EOF.
    fn read_some(&self, buf: &mut [u8]) -> Result<usize> {
        loop {
            match nix::unistd::read(self.as_fd(), buf) {
                Ok(n) => return Ok(n),
                Err(Errno::EINTR) => continue,
                Err(e) => return Err(Error::from(e)),
            }
        }
    }

    /// Write `data` in full. EINTR is retried; short writes are looped.
    fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut off = 0;
        while off < data.len() {
            match nix::unistd::write(self.as_fd(), &data[off..]) {
                Ok(0) => return Err(Error::Errno(Errno::EIO)),
                Ok(n) => off += n,
                Err(Errno::EINTR) => continue,
                Err(e) => return Err(Error::from(e)),
            }
        }
        Ok(())
    }

    /// Toggle `O_NONBLOCK` on this fd.
    fn set_nonblocking(&self, on: bool) -> Result<()> {
        let fd = self.as_fd();
        let cur = fcntl(fd, FcntlArg::F_GETFL).map_err(Error::from)?;
        let mut flags = OFlag::from_bits_truncate(cur);
        flags.set(OFlag::O_NONBLOCK, on);
        fcntl(fd, FcntlArg::F_SETFL(flags)).map_err(Error::from)?;
        Ok(())
    }
}

impl<T: AsFd> FdExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::raw;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

    #[test]
    fn write_empty_to_stdout_succeeds() {
        // mirrors ffi_wbtest.mbt: "io_write: empty bytes to stdout returns 0"
        let stdout = std::io::stdout();
        let fd: BorrowedFd<'_> = stdout.as_fd();
        fd.write_all(b"").expect("write 0 bytes");
    }

    #[test]
    fn read_closed_fd_errors() {
        // mirrors ffi_wbtest.mbt: "io_read: invalid fd returns -1".
        //
        // `BorrowedFd::borrow_raw(-1)` panics in modern std (`-1` is a
        // reserved sentinel). Reproduce the spirit by reading from a fd
        // that has been explicitly closed (which yields EBADF). The
        // `borrow_raw` is funnelled through [`raw::borrow_raw_fd`] so this
        // file remains `unsafe`-free.
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        let raw_fd = rd.as_fd().as_raw_fd();
        drop(rd);
        let bad = raw::borrow_raw_fd(raw_fd);
        let mut buf = [0u8; 8];
        let err = bad.read_some(&mut buf).expect_err("EBADF expected");
        match err {
            Error::Errno(Errno::EBADF) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn set_nonblocking_on_pipe() {
        // mirrors ffi_wbtest.mbt: "io_set_nonblocking: works on pty master fd"
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        rd.set_nonblocking(true).expect("set nonblocking");
    }
}
