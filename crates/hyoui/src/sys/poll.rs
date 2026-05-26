//! `poll(2)` wrapper that surfaces `EINTR` distinctly so the agent loop can
//! drain the self-pipe and re-poll.

use nix::errno::Errno;
use nix::poll::{PollFd, PollTimeout};

use super::error::{Error, Result};

pub use nix::poll::PollFlags;

/// Result of a single `poll(2)` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PollOutcome {
    /// `n` fds are ready.
    Ready(usize),
    /// `poll(2)` returned 0.
    Timeout,
    /// `poll(2)` returned `-1 / EINTR`. Caller should drain the self-pipe and
    /// re-poll.
    Interrupted,
}

/// `poll(2)` wrapper. `timeout` may be:
///
/// * `Some(ms)` — bounded wait (any value `nix::poll::PollTimeout::try_from`
///   accepts).
/// * `None` — block indefinitely.
pub fn poll(fds: &mut [PollFd<'_>], timeout: PollTimeout) -> Result<PollOutcome> {
    match nix::poll::poll(fds, timeout) {
        Ok(0) => Ok(PollOutcome::Timeout),
        Ok(n) => Ok(PollOutcome::Ready(n as usize)),
        Err(Errno::EINTR) => Ok(PollOutcome::Interrupted),
        Err(e) => Err(Error::from(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::pty::Pty;

    #[test]
    fn poll_timeout_zero_on_pty_master_does_not_error() {
        // mirrors ffi_wbtest.mbt:
        // "io_poll: timeout 0 on pty master_fd returns without error"
        let pty = Pty::open(80, 24).expect("open");
        let mut fds = [PollFd::new(pty.master_fd(), PollFlags::POLLIN)];
        let outcome = poll(&mut fds, PollTimeout::ZERO).expect("poll");
        // Any of Ready/Timeout is valid; Interrupted is unlikely with timeout 0.
        match outcome {
            PollOutcome::Ready(_) | PollOutcome::Timeout => {}
            other => panic!("unexpected outcome {other:?}"),
        }
    }
}
