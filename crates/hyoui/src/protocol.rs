//! Length-prefixed socket message protocol.
//!
//! Wire format: `[4 byte little-endian u32 length][payload]`.
//!
//! Ported from `bootstrap/lib/agent/agent.mbt`'s `read_socket_message` /
//! `write_socket_message`. The bootstrap version was tailored to its FFI:
//! it polled the fd, accepted a single `read` returning the full header /
//! payload, and silently dropped messages on partial reads. Here we read
//! and write in loops so that the protocol works with both blocking and
//! nonblocking sockets that happen to return ready (callers that need a
//! timeout layer should `poll` the fd themselves before calling
//! [`read_message`]).
//!
//! Errors are reported via [`crate::Error`]:
//!
//! * [`crate::Error::Errno`] for syscall failures (propagated from
//!   [`crate::sys::FdExt`]).
//! * [`crate::Error::Invalid`] for protocol violations (oversized payload,
//!   unexpected EOF, attempt to send an oversized payload).
//!
//! The peer-closed-cleanly case (EOF before any header byte arrives) is
//! reported as `Invalid("eof before header")` so the caller can distinguish
//! it from a real syscall error if needed.
//!
//! Design rationale: the bootstrap version capped payloads at 16 KiB to
//! match the agent's RPC envelopes. We raise that to 1 MiB here so the
//! protocol can carry larger snapshots (terminal screen dumps, etc.) while
//! still bounding the per-message allocation. Callers that want a tighter
//! limit should validate the payload after decoding.
//!
//! Note that little-endian was chosen by the bootstrap implementation (it
//! shifts byte 0 into bits 0..7), and we keep it for wire compatibility
//! with the agent that already speaks this protocol.

use std::os::fd::AsFd;

use crate::sys::FdExt;
use crate::{Error, Result};

/// Maximum payload size accepted by [`read_message`] / [`write_message`].
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

/// Read a length-prefixed message from `fd`.
///
/// Reads exactly 4 bytes of little-endian length, then exactly that many
/// payload bytes. Loops over short reads (which are normal on sockets and
/// pipes). EINTR is handled inside [`FdExt::read_some`].
///
/// # Errors
///
/// * [`Error::Errno`] — underlying syscall failed.
/// * [`Error::Invalid`] — peer closed the fd before the message was
///   complete, or the declared length exceeds [`MAX_PAYLOAD_LEN`].
pub fn read_message<F: AsFd>(fd: &F) -> Result<Vec<u8>> {
    let mut header = [0u8; 4];
    read_exact(fd, &mut header, "eof before header")?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_PAYLOAD_LEN {
        return Err(Error::Invalid("payload exceeds MAX_PAYLOAD_LEN"));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        read_exact(fd, &mut payload, "eof inside payload")?;
    }
    Ok(payload)
}

/// Write a length-prefixed message to `fd`.
///
/// Sends the 4-byte little-endian length followed by `payload` in a single
/// allocation (so a blocking peer doesn't see the header without the body).
/// Short writes are looped inside [`FdExt::write_all`].
///
/// # Errors
///
/// * [`Error::Errno`] — underlying syscall failed.
/// * [`Error::Invalid`] — `payload` is larger than [`MAX_PAYLOAD_LEN`].
pub fn write_message<F: AsFd>(fd: &F, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(Error::Invalid("payload exceeds MAX_PAYLOAD_LEN"));
    }
    // Safe cast: bounded above by MAX_PAYLOAD_LEN which fits in u32.
    let len = payload.len() as u32;
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(payload);
    fd.write_all(&buf)
}

/// Read into `buf` until full, treating `Ok(0)` as EOF.
fn read_exact<F: AsFd>(fd: &F, buf: &mut [u8], eof_msg: &'static str) -> Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let n = fd.read_some(&mut buf[off..])?;
        if n == 0 {
            return Err(Error::Invalid(eof_msg));
        }
        off += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe_pair() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        nix::unistd::pipe().expect("pipe")
    }

    #[test]
    fn roundtrip_small_payload() {
        let (rd, wr) = pipe_pair();
        let payload = b"hello, hyoui";
        write_message(&wr, payload).expect("write");
        let got = read_message(&rd).expect("read");
        assert_eq!(got, payload);
    }

    #[test]
    fn roundtrip_empty_payload() {
        let (rd, wr) = pipe_pair();
        write_message(&wr, b"").expect("write");
        let got = read_message(&rd).expect("read");
        assert!(got.is_empty());
    }

    #[test]
    fn roundtrip_max_payload() {
        let (rd, wr) = pipe_pair();
        // Use a thread for the writer so the pipe buffer (typically 64 KiB)
        // doesn't deadlock on a 1 MiB payload.
        let payload = vec![0xABu8; MAX_PAYLOAD_LEN];
        let payload_clone = payload.clone();
        let writer = std::thread::spawn(move || {
            write_message(&wr, &payload_clone).expect("write");
        });
        let got = read_message(&rd).expect("read");
        writer.join().expect("writer thread");
        assert_eq!(got.len(), MAX_PAYLOAD_LEN);
        assert_eq!(got, payload);
    }

    #[test]
    fn write_rejects_oversized_payload() {
        let (_rd, wr) = pipe_pair();
        let payload = vec![0u8; MAX_PAYLOAD_LEN + 1];
        let err = write_message(&wr, &payload).expect_err("must reject");
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn read_rejects_oversized_length() {
        let (rd, wr) = pipe_pair();
        // Manually craft a header that declares MAX_PAYLOAD_LEN + 1 bytes.
        let bogus_len = (MAX_PAYLOAD_LEN as u32) + 1;
        wr.write_all(&bogus_len.to_le_bytes()).expect("write hdr");
        drop(wr);
        let err = read_message(&rd).expect_err("must reject");
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn read_errors_on_eof_before_header() {
        let (rd, wr) = pipe_pair();
        drop(wr);
        let err = read_message(&rd).expect_err("must error");
        match err {
            Error::Invalid(msg) => assert_eq!(msg, "eof before header"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn read_errors_on_eof_inside_payload() {
        let (rd, wr) = pipe_pair();
        // Announce 16 bytes but only deliver 4 then close.
        let len: u32 = 16;
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(b"abcd");
        wr.write_all(&buf).expect("write");
        drop(wr);
        let err = read_message(&rd).expect_err("must error");
        match err {
            Error::Invalid(msg) => assert_eq!(msg, "eof inside payload"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn read_handles_short_reads() {
        // Write the header and payload in two separate syscalls to force
        // read_exact to loop. (Use a thread so the writer can sleep between
        // chunks without deadlocking the reader.)
        let (rd, wr) = pipe_pair();
        let writer = std::thread::spawn(move || {
            let len: u32 = 5;
            wr.write_all(&len.to_le_bytes()).expect("hdr");
            std::thread::sleep(std::time::Duration::from_millis(20));
            wr.write_all(b"abcde").expect("body");
        });
        let got = read_message(&rd).expect("read");
        writer.join().expect("writer thread");
        assert_eq!(got, b"abcde");
    }

    #[test]
    fn header_is_little_endian() {
        // Wire-compat check: writing payload of length 0x01020304 must
        // produce header bytes 04 03 02 01.
        // We don't actually want to allocate 16 MiB, so we just craft the
        // expected header inline and compare against to_le_bytes.
        let len: u32 = 0x01020304;
        assert_eq!(len.to_le_bytes(), [0x04, 0x03, 0x02, 0x01]);
    }
}
