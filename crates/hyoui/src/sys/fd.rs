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

/// `write_all_with_idle_timeout` の write 失敗種別 (DR-0016 §4)。
///
/// partial write 後の error kind を caller / record sink で区別するために用意する:
/// - `IdleTimeout` は client → 子 PTY 経路の slow-reader 検知 (= 旧 `Errno::ETIMEDOUT`)、
///   client にも `master.write-timeout` で notify されて DropClient される。
/// - `Io` はそれ以外の transport failure (= EIO / POLLHUP / POLLERR 等)。
///
/// `WriteOutcome` の `error` field に格納される。`written_len > 0` で error あり、
/// `written_len == 0` で error あり、`written_len == requested_len` で error なし、の
/// 3 パターンを 1 値で表現する (DR-0016 §4)。
#[derive(Debug)]
#[non_exhaustive]
pub enum WriteError {
    /// idle timeout が経過し、forward progress が立たなかった (= 旧 ETIMEDOUT)。
    IdleTimeout,
    /// IO 失敗 (= POLLHUP / POLLERR / write の errno 等)。`Errno` を保持。
    Io(Errno),
}

impl WriteError {
    /// errno があれば返す (= record sink で wire 文字列化するときに使う)。
    pub fn errno(&self) -> Option<Errno> {
        match self {
            Self::IdleTimeout => Some(Errno::ETIMEDOUT),
            Self::Io(e) => Some(*e),
        }
    }
}

/// `write_all_with_idle_timeout` の戻り値 (DR-0016 §4)。
///
/// partial write 時の **written byte 数 + error kind** を両方保持する:
/// - `written_len == requested_len && error.is_none()`: 完全成功
/// - `written_len > 0 && error.is_some()`: partial write (= 成功 prefix と error 両方)
/// - `written_len == 0 && error.is_some()`: 何も書けず失敗
///
/// caller (= daemon control.rs) は `written_len` と `error.is_some()` で
/// partial / failure を判定する。record sink には `bytes[0..written_len]` を `in`
/// event、残りを `in-write-error` event として push する想定 (Phase 4)。
#[derive(Debug)]
pub struct WriteOutcome {
    /// 呼び出し時に渡した bytes 長 (= data.len())。
    pub requested_len: usize,
    /// 子に write 成功した byte 数 (= 0 ≤ written_len ≤ requested_len)。
    pub written_len: usize,
    /// error 種別 (= None なら完全成功)。
    pub error: Option<WriteError>,
}

impl WriteOutcome {
    /// 全 bytes を書き終え、error も無い完全成功状態か。
    pub fn is_complete(&self) -> bool {
        self.error.is_none() && self.written_len == self.requested_len
    }
}

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
    fn write_all_with_idle_timeout(
        &self,
        data: &[u8],
        idle_timeout_ms: u32,
    ) -> Result<WriteOutcome> {
        // DR-0016 §4: 戻り値を `Result<WriteOutcome, _>` に変更。`written_len` と
        // `error` を両方保持し、partial write 時の written byte prefix を caller /
        // record sink で再現できるようにする。本 fn 開始前の error (= invalid arg)
        // のみ outer `Err(_)`、write 中に発生した error はすべて `WriteOutcome.error`
        // に集約する (= 「何 byte 書けたか」を捨てないため)。
        if data.is_empty() {
            return Ok(WriteOutcome {
                requested_len: 0,
                written_len: 0,
                error: None,
            });
        }
        let timeout = PollTimeout::try_from(idle_timeout_ms)
            .map_err(|_| Error::Invalid("idle_timeout_ms out of range for PollTimeout"))?;
        let mut off = 0;
        // helper: WriteOutcome を組み立てる。
        let make = |off: usize, err: Option<WriteError>| WriteOutcome {
            requested_len: data.len(),
            written_len: off,
            error: err,
        };
        while off < data.len() {
            match nix::unistd::write(self.as_fd(), &data[off..]) {
                Ok(0) => return Ok(make(off, Some(WriteError::Io(Errno::EIO)))),
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
                            Ok(0) => return Ok(make(off, Some(WriteError::IdleTimeout))),
                            Ok(_) => {
                                let revents = pfd[0].revents().unwrap_or(PollFlags::empty());
                                if revents.intersects(
                                    PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL,
                                ) {
                                    return Ok(make(off, Some(WriteError::Io(Errno::EIO))));
                                }
                                // POLLOUT or some other ready bit — go retry write.
                                break;
                            }
                            Err(Errno::EINTR) => continue,
                            Err(e) => return Ok(make(off, Some(WriteError::Io(e)))),
                        }
                    }
                }
                Err(e) => return Ok(make(off, Some(WriteError::Io(e)))),
            }
        }
        Ok(make(off, None))
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
        let outcome = pty
            .master_fd()
            .write_all_with_idle_timeout(&payload, 500)
            .expect("write should not surface invalid-arg error");
        assert!(
            outcome.is_complete(),
            "expected complete write, got {outcome:?}"
        );
        assert_eq!(outcome.written_len, payload.len());

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
        let outcome = wr
            .write_all_with_idle_timeout(&payload, 100)
            .expect("idle-timeout is a WriteOutcome.error, not an outer Err");
        let elapsed = start.elapsed();

        assert!(
            !outcome.is_complete(),
            "expected partial / failed write, got {outcome:?}"
        );
        match outcome.error {
            Some(WriteError::IdleTimeout) => {}
            other => panic!("expected IdleTimeout, got {other:?}"),
        }
        // 一部 byte は line discipline buffer に書けている可能性がある (= partial)。
        assert!(outcome.written_len < outcome.requested_len);
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
        let outcome = pty
            .master_fd()
            .write_all_with_idle_timeout(&[], 100)
            .expect("empty write should succeed");
        assert_eq!(outcome.requested_len, 0);
        assert_eq!(outcome.written_len, 0);
        assert!(outcome.error.is_none());
        assert!(outcome.is_complete());
    }

    /// DR-0016 §4 regression: writing to a closed peer (= read end dropped) must
    /// surface `WriteError::Io(EPIPE)` in `WriteOutcome.error`、`written_len` は
    /// `requested_len` 未満 (= 多くの場合 0、kernel が EPIPE を即返すケース)。
    /// outer `Err(_)` ではない。
    #[test]
    fn master_write_io_error_carries_written_len_and_kind() {
        let (rd, wr) = nix::unistd::pipe().expect("pipe");
        wr.set_nonblocking(true).expect("nonblock");
        // read end を即 drop すると、wr への write は EPIPE を返す (= SIGPIPE は
        // process-wide ignore でなくても EPIPE は errno で返ってくる)。
        drop(rd);
        // SIGPIPE で process が死ぬのを防ぐため、ここではテストプロセスの SIGPIPE
        // 既定 disposition (= terminate) を一時 ignore に揃える。test 内のみ、unsafe を
        // 持ち込まずに `crate::sys::signal::install_ignore` を使う。
        let _ = crate::sys::signal::install_ignore(nix::sys::signal::Signal::SIGPIPE);
        let payload = b"hello-pipe-closed";
        let outcome = wr
            .write_all_with_idle_timeout(payload, 50)
            .expect("io-error is captured in WriteOutcome, not outer Err");
        assert_eq!(outcome.requested_len, payload.len());
        match &outcome.error {
            Some(WriteError::Io(Errno::EPIPE)) => {}
            other => panic!("expected Io(EPIPE), got {other:?}"),
        }
        // partial write は kernel 実装次第 (= macOS の pipe は write の途中で EPIPE
        // を返すことがある)、ここでは「complete でない」ことだけ確認する。
        assert!(!outcome.is_complete());
    }

    /// DR-0016 §4 corollary: 完全成功時の WriteOutcome の field 値。
    #[test]
    fn master_write_complete_outcome_shape() {
        // self-pipe で完全成功させる (= kernel pipe buffer に十分余裕がある小サイズ)。
        let (rd, wr) = nix::unistd::pipe().expect("pipe");
        wr.set_nonblocking(true).expect("nonblock");
        let payload = b"abc";
        let outcome = wr.write_all_with_idle_timeout(payload, 100).expect("ok");
        assert_eq!(outcome.requested_len, payload.len());
        assert_eq!(outcome.written_len, payload.len());
        assert!(outcome.error.is_none());
        assert!(outcome.is_complete());
        drop(wr);
        drop(rd);
    }
}
