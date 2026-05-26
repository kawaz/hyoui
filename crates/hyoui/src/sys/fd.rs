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
use nix::poll::{PollFd, PollFlags, PollTimeout};

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

    /// Write `data` in full on a NONBLOCK fd, waiting on EAGAIN via
    /// `poll(POLLOUT)` until each chunk either succeeds or the **per-chunk**
    /// timeout `idle_timeout_ms` elapses without forward progress.
    ///
    /// Semantics (R5-C3): `idle_timeout_ms` is **reset on every successful
    /// `write(2)`**, so a slow-but-progressing reader (= legitimate large
    /// paste being consumed steadily by the child's line discipline) is not
    /// punished. Only a reader that makes **zero progress** for the entire
    /// timeout window is reported as `Error::Errno(ETIMEDOUT)`.
    ///
    /// EINTR (poll or write) is retried transparently. POLLHUP/POLLERR on
    /// the fd is treated as a hard write error (`EIO`) — the peer is gone or
    /// the fd is unusable, no point in looping.
    ///
    /// Design rationale: the master PTY in `Session::start` is set
    /// NONBLOCK so the daemon never blocks on a slow-reader child. Without
    /// this helper, `write_all` would surface EAGAIN as an error and the
    /// daemon would silently DropClient on any large input frame whenever
    /// the child's PTY line discipline buffer (typically 4–8 KiB) is
    /// transiently full. See `docs/decisions/DR-0008-protocol-design.md`
    /// (raw_data hot path) and R5-Kernel C1.
    fn write_all_with_idle_timeout(&self, data: &[u8], idle_timeout_ms: u32) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let timeout = PollTimeout::try_from(idle_timeout_ms)
            .map_err(|_| Error::Invalid("idle_timeout_ms out of range for PollTimeout"))?;
        let mut off = 0;
        while off < data.len() {
            match nix::unistd::write(self.as_fd(), &data[off..]) {
                Ok(0) => return Err(Error::Errno(Errno::EIO)),
                Ok(n) => {
                    off += n;
                }
                Err(Errno::EINTR) => continue,
                // POSIX permits EAGAIN and EWOULDBLOCK to be distinct values,
                // but on Linux and macOS they alias (= the `Errno` variant is
                // the same). We only need to match `EAGAIN`.
                Err(Errno::EAGAIN) => {
                    // Wait until fd is writable again, or until the idle
                    // timeout elapses. `poll(2)` itself can be interrupted
                    // by signals (EINTR) — loop until it returns a real
                    // status. Each EINTR is *not* counted as forward
                    // progress, but we re-issue `poll` with the same
                    // timeout so a flurry of signals can extend the wait;
                    // that is acceptable (DoS bound is bounded by signal
                    // delivery rate, which the daemon controls itself).
                    let fd = self.as_fd();
                    let mut pfd = [PollFd::new(fd, PollFlags::POLLOUT)];
                    loop {
                        match nix::poll::poll(&mut pfd, timeout) {
                            Ok(0) => return Err(Error::Errno(Errno::ETIMEDOUT)),
                            Ok(_) => {
                                let revents = pfd[0].revents().unwrap_or(PollFlags::empty());
                                if revents.intersects(
                                    PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL,
                                ) {
                                    return Err(Error::Errno(Errno::EIO));
                                }
                                // POLLOUT or some other ready bit — go retry write.
                                break;
                            }
                            Err(Errno::EINTR) => continue,
                            Err(e) => return Err(Error::from(e)),
                        }
                    }
                }
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

    /// R5-C3 regression: a NONBLOCK fd whose peer drains data steadily must
    /// not be flagged as a slow-reader. `write_all_with_idle_timeout` should
    /// transparently retry on EAGAIN via `poll(POLLOUT)` and complete the
    /// full write.
    #[test]
    fn master_write_retries_on_eagain() {
        use crate::sys::pty::Pty;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let pty = Pty::open(80, 24).expect("openpty");
        pty.master_fd().set_nonblocking(true).expect("nonblock");

        // Drain the slave from a background thread, slowly enough that the
        // master fd's NONBLOCK write will hit EAGAIN at least once for any
        // payload larger than the line discipline buffer. We use the slave
        // fd via `nix::unistd::read` directly.
        let slave_raw = pty.slave_fd().expect("slave fd").as_raw_fd();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let reader = std::thread::spawn(move || {
            // SAFETY: we use a BorrowedFd via `raw::borrow_raw_fd`; the slave
            // fd is owned by `pty` which outlives this thread (we join below
            // before dropping pty).
            let bfd = raw::borrow_raw_fd(slave_raw);
            let mut buf = [0u8; 1024];
            let mut total = 0usize;
            while !stop_t.load(Ordering::Relaxed) {
                match nix::unistd::read(bfd, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EINTR) => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            total
        });

        // Payload large enough to overflow the typical 4–8 KiB line
        // discipline buffer several times over.
        let payload = vec![b'A'; 64 * 1024];
        // 500 ms idle timeout matches the daemon's constant.
        pty.master_fd()
            .write_all_with_idle_timeout(&payload, 500)
            .expect("write should complete despite EAGAIN");

        stop.store(true, Ordering::Relaxed);
        // Drop the master so the reader sees EOF eventually.
        drop(pty);
        // OwnedFd is dropped above; ensure the thread terminates.
        let _ = reader.join();
    }

    /// R5-C3 regression: when the peer never reads, the helper must give up
    /// after `idle_timeout_ms` of zero forward progress instead of looping
    /// forever or returning a misleading EAGAIN error.
    ///
    /// We use a pipe (instead of a PTY pair) because pipes have a small,
    /// well-defined kernel buffer (typically 16–64 KiB on Linux, 16 KiB on
    /// macOS). PTY slave buffers can be surprisingly large on macOS even
    /// without a reader, which makes the EAGAIN condition hard to provoke
    /// in a unit test. The helper itself is fd-generic, so this is a
    /// faithful regression for the daemon's master-write code path.
    #[test]
    fn master_write_times_out_after_threshold() {
        let (rd, wr) = nix::unistd::pipe().expect("pipe");
        wr.set_nonblocking(true).expect("nonblock");
        // Hold rd to keep the pipe open (no reader means SIGPIPE on write).
        // But we never read from it, so the kernel buffer fills up and any
        // further write returns EAGAIN forever.

        // Payload bigger than any plausible pipe buffer (4 MiB).
        let payload = vec![b'A'; 4 * 1024 * 1024];

        let start = std::time::Instant::now();
        // 100 ms idle timeout — short enough for the test to be fast.
        let err = wr
            .write_all_with_idle_timeout(&payload, 100)
            .expect_err("write should ETIMEDOUT when peer never reads");
        let elapsed = start.elapsed();

        match err {
            Error::Errno(Errno::ETIMEDOUT) => {}
            other => panic!("expected ETIMEDOUT, got {other:?}"),
        }
        // Should give up within a small multiple of the idle timeout. We
        // allow generous slack because the first write(2) call may write
        // a partial chunk before EAGAIN, but the idle window starts only
        // after that.
        assert!(
            elapsed < std::time::Duration::from_millis(2000),
            "took too long: {elapsed:?}"
        );
        drop(rd);
    }

    /// R5-C3 corollary: empty payload is a no-op and never times out.
    #[test]
    fn master_write_empty_payload_is_noop() {
        use crate::sys::pty::Pty;
        let pty = Pty::open(80, 24).expect("openpty");
        pty.master_fd().set_nonblocking(true).expect("nonblock");
        pty.master_fd()
            .write_all_with_idle_timeout(&[], 100)
            .expect("empty write should succeed");
    }
}
