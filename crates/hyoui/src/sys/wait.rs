//! `waitpid(2)` wrapper.

use nix::errno::Errno;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use super::error::{Error, Result};

/// Categorized outcome of `waitpid(WUNTRACED | flags)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// No state change pending (only possible with `WNOHANG`).
    NoChange,
    /// Child exited with the given status.
    Exited {
        /// PID of the child that exited.
        pid: Pid,
        /// Exit status (0..=255).
        status: i32,
    },
    /// Child was killed by `signal`.
    Signaled {
        /// PID of the child that was signaled.
        pid: Pid,
        /// Signal that terminated the child.
        signal: nix::sys::signal::Signal,
    },
    /// Child was stopped by `signal` (job control).
    Stopped {
        /// PID of the stopped child.
        pid: Pid,
        /// Signal that caused the stop (typically `SIGTSTP`/`SIGSTOP`).
        signal: nix::sys::signal::Signal,
    },
    /// Child resumed (rare, only with `WCONTINUED` which we don't request).
    Continued {
        /// PID of the resumed child.
        pid: Pid,
    },
}

/// `waitpid(pid, WUNTRACED [| WNOHANG])` with EINTR retry. Returns categorized
/// outcome.
pub fn wait_for_status(pid: Pid, nohang: bool) -> Result<WaitOutcome> {
    let mut flags = WaitPidFlag::WUNTRACED;
    if nohang {
        flags |= WaitPidFlag::WNOHANG;
    }
    loop {
        match waitpid(pid, Some(flags)) {
            Ok(WaitStatus::StillAlive) => return Ok(WaitOutcome::NoChange),
            Ok(WaitStatus::Exited(pid, status)) => {
                return Ok(WaitOutcome::Exited { pid, status });
            }
            Ok(WaitStatus::Signaled(pid, signal, _core)) => {
                return Ok(WaitOutcome::Signaled { pid, signal });
            }
            Ok(WaitStatus::Stopped(pid, signal)) => {
                return Ok(WaitOutcome::Stopped { pid, signal });
            }
            Ok(WaitStatus::Continued(pid)) => return Ok(WaitOutcome::Continued { pid }),
            #[allow(unreachable_patterns)]
            Ok(_) => {
                // PtraceEvent / PtraceSyscall only exist with the `ptrace`
                // feature; the wildcard catches them when that feature is
                // enabled in dependent crates.
                return Err(Error::Precondition("unexpected waitpid result"));
            }
            Err(Errno::EINTR) => continue,
            Err(Errno::ECHILD) if nohang => return Ok(WaitOutcome::NoChange),
            Err(e) => return Err(Error::from(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waitpid_no_children_with_nohang_returns_no_change() {
        // mirrors ffi_wbtest.mbt: "proc_waitpid: pid -1 with nohang returns None"
        let outcome = wait_for_status(Pid::from_raw(-1), true).expect("ok");
        assert!(matches!(outcome, WaitOutcome::NoChange));
    }
}
