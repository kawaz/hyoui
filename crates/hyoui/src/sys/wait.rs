//! `waitpid(2)` wrapper.

use nix::errno::Errno;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use super::error::{Error, Result};

/// Categorized outcome of `waitpid(WUNTRACED | flags)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    fn waitpid_with_nohang_returns_no_change_for_alive_child() {
        // pid=-1 (= any child) は parallel test 環境で他テストの子と race する
        // (= 他テストが exit 直後の child を持つと、本テストの waitpid(-1) が
        // それを拾って Exited を返してしまう、CI で観測された flaky)。
        // 特定 pid を spawn して「alive child を WNOHANG で wait」シナリオを
        // 再現すれば NoChange path を race-free に検証できる。
        use std::process::Command;
        let mut child = Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .expect("spawn sleep");
        let pid = Pid::from_raw(child.id() as i32);
        let outcome = wait_for_status(pid, true).expect("ok");
        assert!(
            matches!(outcome, WaitOutcome::NoChange),
            "alive child + WNOHANG should return NoChange, got {outcome:?}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
