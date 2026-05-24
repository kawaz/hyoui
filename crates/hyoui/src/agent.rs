//! Agent — the PTY-proxy event loop.
//!
//! [`Agent`] takes a parsed [`RunConfig`] (from [`crate::cli`]) and an
//! [`Observer`] (from [`crate::observer`]), spawns the configured child
//! inside a freshly-allocated PTY, and runs an asynchronous bridge:
//!
//! ```text
//!         stdin  ──▶  observer.on_input  ──▶  pty master (write)
//!   pty master ──▶  observer.on_output ──▶  stdout
//!         socket ──▶  protocol::read_message ──▶ observer.on_input ──▶ pty master
//!     self-pipe (SIGCHLD/SIGTSTP/SIGCONT/SIGINT/SIGTERM/SIGQUIT/SIGHUP)
//!                 ──▶  job-control + relay logic
//! ```
//!
//! The loop terminates when one of the following fires (in priority order):
//!
//! * The child exits or is killed by a signal.
//! * `--timeout` or `--idle-timeout` is reached. The child is then sent
//!   `SIGTERM` (grace ~2s) and escalated to `SIGKILL` if still alive.
//! * The `--until` substring is observed in PTY output (counts as success).
//!
//! ## RAII / Drop ordering
//!
//! Cleanup is the inverse of construction:
//!
//! 1. `TtyGuard` restores terminal modes (must happen *last*, so any final
//!    debug print remains readable).
//! 2. `UnixSock` removes its on-disk path.
//! 3. `Pty` closes the master fd (and clears the SIGWINCH global).
//! 4. `SelfPipe` closes both ends and clears the global write fd.
//!
//! Rust drops struct fields in declaration order, so the field order in
//! [`Agent`] is reverse-priority: `_tty_guard` first (dropped last), then
//! `socket`, `pty`, `self_pipe`.
//!
//! ## Unsafe policy
//!
//! This module contains zero `unsafe`. All syscalls go through the safe
//! wrappers in [`crate::sys`].

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::time::Duration;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::sys::signal::{self, Signal};
use nix::unistd::{Pid, getpid};

use crate::cli::{Mode as RunMode, OnChildSuspend, OnParentSuspend, RunConfig};
use crate::observer::Observer;
use crate::protocol;
use crate::sys::{
    Error, FdExt, Pty, Result, SelfPipe, TtyGuard, UnixSock, WaitOutcome, clock_monotonic,
    enter_raw, install_ignore, install_self_pipe, install_winch, is_tty, poll, raise,
    register_self_pipe, tty_size, wait_for_status,
};

/// Why the event loop ended.
#[derive(Debug, Clone, Copy)]
enum ExitReason {
    /// Child exited naturally; carry the encoded wait status (POSIX layout).
    ChildExited(i32),
    /// Either `--timeout` or `--idle-timeout` fired.
    TimedOut,
    /// The `--until` pattern was observed.
    UntilHit,
}

/// PTY proxy. Owns the child, the master fd, the listening socket, and
/// (in interactive mode) the parent's terminal raw-mode guard.
///
/// Construct with [`Agent::new`] and consume with [`Agent::run`]. The struct
/// is intentionally not [`Clone`] or [`Copy`]: a `run` call moves it so
/// `Drop` can release resources deterministically.
pub struct Agent {
    // -------------------------------------------------------------------
    // Resources (declared in REVERSE of desired Drop priority).
    // Rust drops fields top-to-bottom; we want `_tty_guard` to run LAST so
    // any panic / final stderr remains readable after the loop ends.
    // -------------------------------------------------------------------
    /// Raw-mode guard for the parent's controlling terminal (interactive
    /// mode only). `None` when stdin is not a tty or in headless mode.
    /// Dropped *last* so the terminal is restored after every other
    /// resource is released.
    _tty_guard: Option<TtyGuard>,
    /// Listening Unix-domain socket. `Drop` unlinks the path.
    socket: UnixSock,
    /// PTY master + child handle. `Drop` closes the master fd.
    pty: Pty,
    /// Self-pipe carrying SIGCHLD/SIGTSTP/... bytes from the signal handler
    /// to the poll loop. `Drop` clears the global write fd then closes both
    /// ends.
    self_pipe: SelfPipe,

    // -------------------------------------------------------------------
    // Non-resource state.
    // -------------------------------------------------------------------
    /// PID of the child spawned by `forkpty`.
    child_pid: Pid,
    /// Observer that sees / can transform every byte in both directions.
    observer: Box<dyn Observer + Send>,
    /// Configuration (copied; immutable for the loop's lifetime).
    config: RunConfig,
    /// `true` until stdin reports EOF / POLLHUP.
    stdin_open: bool,
    /// Monotonic start time (for `--timeout`).
    start: Duration,
    /// Last time PTY output was seen (for `--idle-timeout`).
    last_output: Duration,
    /// Trailing bytes of the previous chunk, kept so `--until` matches can
    /// straddle two reads.
    until_tail: Vec<u8>,
    /// Set once a stop condition fires; carried through to [`Agent::run`]
    /// to compute the exit code.
    exit_reason: Option<ExitReason>,
}

impl Agent {
    /// Construct an `Agent` from a parsed [`RunConfig`] and an [`Observer`].
    ///
    /// This performs all setup with side effects:
    ///
    /// * Opens a fresh PTY (size from real tty in interactive mode, from
    ///   `cfg.cols`/`cfg.rows` in headless).
    /// * Spawns the child via `forkpty` + `execvp`.
    /// * Binds a listening Unix-domain socket (auto-generated path if
    ///   `cfg.socket` is `None`).
    /// * Installs `SIGPIPE` ignore, `SIGWINCH` forwarder (interactive only),
    ///   and a self-pipe for job-control / termination signals.
    /// * Puts the parent's tty in raw mode (interactive only, when stdin
    ///   is a tty).
    ///
    /// Errors leave no partial resources behind: each fallible step
    /// happens after the previous successful resource is already wrapped
    /// in its RAII type, so an early `?` propagates a `Drop` cascade.
    pub fn new(config: RunConfig, observer: Box<dyn Observer + Send>) -> Result<Self> {
        let stdin = std::io::stdin();
        let stdin_fd: BorrowedFd<'_> = stdin.as_fd();

        // Determine PTY size.
        let (cols, rows) = match config.mode {
            RunMode::Interactive => match tty_size(stdin_fd)? {
                Some(ws) => (ws.cols, ws.rows),
                None => (clamp_u16(config.cols)?, clamp_u16(config.rows)?),
            },
            RunMode::Headless => (clamp_u16(config.cols)?, clamp_u16(config.rows)?),
        };

        // Spawn the child inside a fresh PTY.
        let argv_owned: Vec<&str> = config.command.iter().map(String::as_str).collect();
        let spawned = Pty::spawn(&argv_owned, cols, rows)?;
        let pty = spawned.pty;
        let child_pid = spawned.child;

        // Listening socket. `pty` is already in an owned local; if any
        // following step fails it will Drop here.
        let sock_path = match &config.socket {
            Some(p) => PathBuf::from(p),
            None => default_socket_path()?,
        };
        let socket = UnixSock::listen(&sock_path)?;
        socket.as_fd().set_nonblocking(true)?;

        // Signals.
        install_ignore(Signal::SIGPIPE)?;
        if matches!(config.mode, RunMode::Interactive) {
            install_winch(pty.master_fd())?;
        }
        let self_pipe = install_self_pipe()?;
        for sig in [
            Signal::SIGCHLD,
            Signal::SIGTSTP,
            Signal::SIGCONT,
            Signal::SIGINT,
            Signal::SIGTERM,
            Signal::SIGQUIT,
            Signal::SIGHUP,
        ] {
            register_self_pipe(sig)?;
        }

        // Raw-mode the parent's tty (interactive + real tty only).
        let tty_guard = if matches!(config.mode, RunMode::Interactive) && is_tty(stdin_fd) {
            Some(enter_raw(dup_stdin()?)?)
        } else {
            None
        };

        let now = clock_monotonic()?;
        Ok(Self {
            _tty_guard: tty_guard,
            socket,
            pty,
            self_pipe,
            child_pid,
            observer,
            config,
            stdin_open: true,
            start: now,
            last_output: now,
            until_tail: Vec::new(),
            exit_reason: None,
        })
    }

    /// Run the event loop to completion and return the child's exit code.
    ///
    /// Exit codes follow coreutils `timeout(1)`:
    ///
    /// * `--timeout` / `--idle-timeout` fired → **124**.
    /// * `--until` substring observed → **0** (treated as success).
    /// * Child exited normally → child's own exit code.
    /// * Child killed by signal `n` → **128 + n**.
    ///
    /// `self` is consumed so the [`Drop`] cascade runs immediately after
    /// the loop ends (terminal restored, socket unlinked, fds closed).
    pub fn run(mut self) -> Result<i32> {
        while self.poll_once()? {}
        // If the loop ended without ChildExited (e.g. POLLHUP on master),
        // reap the child here so we return its real status.
        let reason = self.exit_reason.unwrap_or_else(|| {
            match wait_for_status(self.child_pid, false) {
                Ok(WaitOutcome::Exited { status, .. }) => {
                    ExitReason::ChildExited(encode_exit_status(status))
                }
                Ok(WaitOutcome::Signaled { signal, .. }) => {
                    ExitReason::ChildExited(encode_signal_status(signal))
                }
                _ => ExitReason::ChildExited(1 << 8), // fallback: exit 1
            }
        });
        Ok(match reason {
            ExitReason::TimedOut => 124,
            ExitReason::UntilHit => 0,
            ExitReason::ChildExited(status) => decode_wait_status(status),
        })
    }

    /// One iteration of the loop. Returns `Ok(false)` when the loop should
    /// terminate.
    fn poll_once(&mut self) -> Result<bool> {
        // ---- Phase 1: poll (immutable borrows of self for fd accessors) ----
        // Snapshot fds as raw ints so we can re-borrow them after the poll,
        // even inside `&mut self` blocks. The fds remain valid for the whole
        // method body because their owners (self.pty, self.socket,
        // self.self_pipe) outlive it.
        let master_raw = self.pty.master_fd().as_raw_fd();
        let socket_raw = self.socket.as_fd().as_raw_fd();
        let sig_raw = self.self_pipe.read.as_fd().as_raw_fd();
        let stdin = std::io::stdin();
        let stdin_raw = stdin.as_fd().as_raw_fd();

        // Build pollfd slice. Indices are fixed so we can decode revents
        // by position below.
        let (revents_stdin, revents_master, revents_socket, revents_sig) = {
            let stdin_borrow = stdin.as_fd();
            let master_borrow = self.pty.master_fd();
            let socket_borrow = self.socket.as_fd();
            let sig_borrow = self.self_pipe.read.as_fd();
            let stdin_events = if self.stdin_open {
                PollFlags::POLLIN
            } else {
                // Stdin closed: poll with empty events so it never reports ready.
                // (PollFd doesn't accept fd=-1 in safe nix.)
                PollFlags::empty()
            };
            let mut fds = [
                PollFd::new(stdin_borrow, stdin_events),
                PollFd::new(master_borrow, PollFlags::POLLIN),
                PollFd::new(socket_borrow, PollFlags::POLLIN),
                PollFd::new(sig_borrow, PollFlags::POLLIN),
            ];
            let timeout = self.poll_timeout()?;
            match poll(&mut fds, timeout)? {
                crate::sys::PollOutcome::Interrupted => {
                    // EINTR: signal arrived; self-pipe byte surfaces next loop.
                    return Ok(true);
                }
                crate::sys::PollOutcome::Timeout | crate::sys::PollOutcome::Ready(_) => {}
            }
            (
                fds[0].revents().unwrap_or_else(PollFlags::empty),
                fds[1].revents().unwrap_or_else(PollFlags::empty),
                fds[2].revents().unwrap_or_else(PollFlags::empty),
                fds[3].revents().unwrap_or_else(PollFlags::empty),
            )
        };

        // ---- Phase 2: react. All immutable borrows of self are dropped. ----

        // Check timeouts; on hit, terminate child and exit.
        self.check_deadlines()?;
        if self.exit_reason.is_some() {
            self.terminate_child()?;
            return Ok(false);
        }

        // Self-pipe first: drain signals so subsequent reads see the latest
        // child state.
        if revents_sig.contains(PollFlags::POLLIN) {
            let signals = self.self_pipe.drain()?;
            for s in signals {
                if !self.handle_signal(s as i32)? {
                    return Ok(false);
                }
            }
        }

        // Stdin → PTY master.
        if self.stdin_open {
            let mut eof = false;
            if revents_stdin.contains(PollFlags::POLLIN) {
                let mut buf = [0u8; 4096];
                let stdin_fd = crate::sys::raw::borrow_raw_fd(stdin_raw);
                match stdin_fd.read_some(&mut buf) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        let out = self.observer.on_input(&buf[..n]);
                        if !out.is_empty() {
                            let master = crate::sys::raw::borrow_raw_fd(master_raw);
                            master.write_all(&out)?;
                        }
                    }
                    Err(Error::Errno(Errno::EAGAIN)) => {}
                    Err(e) => return Err(e),
                }
            }
            if revents_stdin.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
                eof = true;
            }
            if eof {
                // Design rationale: forward 0x04 (EOT) once so a canonical-mode
                // child sees end-of-input. If the child runs in raw mode it
                // receives a literal 0x04 byte instead — accepted limitation
                // of proxying a pipe-fed stdin into a PTY.
                let master = crate::sys::raw::borrow_raw_fd(master_raw);
                let _ = master.write_all(&[0x04u8]);
                self.stdin_open = false;
            }
        }

        // PTY master → stdout (+ until-scan).
        if revents_master.contains(PollFlags::POLLIN) {
            let mut buf = [0u8; 4096];
            let master = crate::sys::raw::borrow_raw_fd(master_raw);
            match master.read_some(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    self.last_output = clock_monotonic()?;
                    self.scan_until(&buf[..n]);
                    let out = self.observer.on_output(&buf[..n]);
                    if !out.is_empty() {
                        // stdout write errors are non-fatal for the loop.
                        let stdout = std::io::stdout();
                        let _ = stdout.as_fd().write_all(&out);
                    }
                    if self.exit_reason.is_some() {
                        self.terminate_child()?;
                        return Ok(false);
                    }
                }
                Err(Error::Errno(Errno::EAGAIN)) => {}
                // macOS: EIO after the slave side closes (child gone).
                Err(Error::Errno(Errno::EIO)) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        if revents_master.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
            return Ok(false);
        }

        // Socket: accept one connection, read a length-prefixed message,
        // feed it through `observer.on_input` into the child.
        if revents_socket.contains(PollFlags::POLLIN) {
            match self.socket.accept() {
                Ok(client) => {
                    client.set_nonblocking(false)?;
                    if let Ok(payload) = protocol::read_message(&client) {
                        let out = self.observer.on_input(&payload);
                        if !out.is_empty() {
                            let master = crate::sys::raw::borrow_raw_fd(master_raw);
                            master.write_all(&out)?;
                        }
                    }
                    drop(client);
                }
                Err(Error::Errno(Errno::EAGAIN)) => {}
                Err(e) => return Err(e),
            }
        }

        // Silence unused warnings for the snapshotted ints when the
        // corresponding branch never fires in a given build configuration.
        let _ = (socket_raw, sig_raw);

        Ok(true)
    }

    /// Compute the next poll timeout based on `--timeout` / `--idle-timeout`.
    fn poll_timeout(&self) -> Result<PollTimeout> {
        let now = clock_monotonic()?;
        let mut best: Option<i64> = None;
        if let Some(t_ms) = self.config.timeout_ms {
            let elapsed_ms = (now.saturating_sub(self.start)).as_millis() as i64;
            let remaining = t_ms - elapsed_ms;
            best = Some(remaining);
        }
        if let Some(t_ms) = self.config.idle_timeout_ms {
            let elapsed_ms = (now.saturating_sub(self.last_output)).as_millis() as i64;
            let remaining = t_ms - elapsed_ms;
            best = Some(match best {
                Some(b) => b.min(remaining),
                None => remaining,
            });
        }
        Ok(match best {
            None => PollTimeout::NONE,
            Some(ms) if ms <= 0 => PollTimeout::ZERO,
            // PollTimeout's u16 limit is plenty; clamp at u16::MAX ms (~65s)
            // so very large remainings still produce a wake-up to re-check.
            Some(ms) => PollTimeout::from(ms.min(u16::MAX as i64) as u16),
        })
    }

    /// If a deadline has been reached, record [`ExitReason::TimedOut`].
    fn check_deadlines(&mut self) -> Result<()> {
        if self.exit_reason.is_some() {
            return Ok(());
        }
        let now = clock_monotonic()?;
        if let Some(t_ms) = self.config.timeout_ms
            && now.saturating_sub(self.start).as_millis() as i64 >= t_ms
        {
            self.exit_reason = Some(ExitReason::TimedOut);
            return Ok(());
        }
        if let Some(t_ms) = self.config.idle_timeout_ms
            && now.saturating_sub(self.last_output).as_millis() as i64 >= t_ms
        {
            self.exit_reason = Some(ExitReason::TimedOut);
        }
        Ok(())
    }

    /// Scan freshly read PTY output for the `--until` pattern. Keeps a tail
    /// of `len(pattern) - 1` bytes so matches straddling two reads are
    /// caught.
    fn scan_until(&mut self, data: &[u8]) {
        let Some(pattern) = self.config.until.as_deref() else {
            return;
        };
        let needle = pattern.as_bytes();
        if needle.is_empty() {
            return;
        }
        let mut combined = Vec::with_capacity(self.until_tail.len() + data.len());
        combined.extend_from_slice(&self.until_tail);
        combined.extend_from_slice(data);
        if combined.windows(needle.len()).any(|w| w == needle) {
            self.exit_reason = Some(ExitReason::UntilHit);
        }
        let keep = needle.len().saturating_sub(1);
        if keep == 0 {
            self.until_tail.clear();
        } else if combined.len() > keep {
            let start = combined.len() - keep;
            self.until_tail.clear();
            self.until_tail.extend_from_slice(&combined[start..]);
        } else {
            self.until_tail = combined;
        }
    }

    /// Handle a single signal byte from the self-pipe. Returns `Ok(false)`
    /// if the loop should terminate.
    fn handle_signal(&mut self, sig: i32) -> Result<bool> {
        let signal = match Signal::try_from(sig) {
            Ok(s) => s,
            Err(_) => return Ok(true),
        };
        match signal {
            Signal::SIGCHLD => match wait_for_status(self.child_pid, true)? {
                WaitOutcome::Exited { pid, status } if pid == self.child_pid => {
                    self.exit_reason = Some(ExitReason::ChildExited(encode_exit_status(status)));
                    return Ok(false);
                }
                WaitOutcome::Signaled { pid, signal } if pid == self.child_pid => {
                    self.exit_reason = Some(ExitReason::ChildExited(encode_signal_status(signal)));
                    return Ok(false);
                }
                WaitOutcome::Stopped { pid, .. } if pid == self.child_pid => {
                    match self.config.on_child_suspend {
                        OnChildSuspend::AutoResume => {
                            let _ = signal::killpg(self.child_pid, Signal::SIGCONT);
                        }
                        OnChildSuspend::Follow => {
                            // Stop the parent too to preserve the invariant
                            // "parent runs → child runs".
                            let _ = raise(Signal::SIGSTOP);
                        }
                    }
                }
                _ => {}
            },
            Signal::SIGTSTP => match self.config.on_parent_suspend {
                OnParentSuspend::Transparent => {
                    let _ = signal::killpg(self.child_pid, Signal::SIGSTOP);
                    let _ = raise(Signal::SIGSTOP);
                }
                OnParentSuspend::Decouple => {
                    let _ = raise(Signal::SIGSTOP);
                }
            },
            Signal::SIGCONT => {
                if let Ok(WaitOutcome::Stopped { pid, .. }) = wait_for_status(self.child_pid, true)
                    && pid == self.child_pid
                {
                    let _ = signal::killpg(self.child_pid, Signal::SIGCONT);
                }
            }
            Signal::SIGINT | Signal::SIGTERM | Signal::SIGQUIT | Signal::SIGHUP => {
                let _ = signal::killpg(self.child_pid, signal);
            }
            _ => {}
        }
        Ok(true)
    }

    /// `SIGTERM` the child group, give a brief grace period, then `SIGKILL`.
    fn terminate_child(&mut self) -> Result<()> {
        let _ = signal::killpg(self.child_pid, Signal::SIGTERM);
        // Up to ~2s in 100ms ticks.
        for _ in 0..20 {
            match wait_for_status(self.child_pid, true)? {
                WaitOutcome::Exited { pid, .. } | WaitOutcome::Signaled { pid, .. }
                    if pid == self.child_pid =>
                {
                    return Ok(());
                }
                _ => {}
            }
            // Sleep 100 ms via poll on no fds.
            let mut empty: [PollFd<'_>; 0] = [];
            let _ = poll(&mut empty, PollTimeout::from(100u16));
        }
        let _ = signal::killpg(self.child_pid, Signal::SIGKILL);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// helpers (free functions; private)
// ---------------------------------------------------------------------------

/// Convert a [`RunConfig`] `i32` size into a `u16`, rejecting out-of-range
/// values with [`Error::Invalid`].
fn clamp_u16(n: i32) -> Result<u16> {
    if (0..=u16::MAX as i32).contains(&n) {
        Ok(n as u16)
    } else {
        Err(Error::Invalid("cols/rows out of u16 range"))
    }
}

/// `dup(2)` the inherited stdin fd into an owned descriptor. We need an
/// owned fd to pass to [`enter_raw`] (which restores termios in `Drop`),
/// but `std::io::stdin()` only hands out a borrow.
fn dup_stdin() -> Result<OwnedFd> {
    let stdin = std::io::stdin();
    nix::unistd::dup(stdin.as_fd()).map_err(Error::from)
}

/// Default socket path: `$XDG_RUNTIME_DIR/hyoui/agent-<pid>.sock` if the env
/// var is set, otherwise `$TMPDIR/hyoui-<uid>/agent-<pid>.sock` (TMPDIR
/// defaulting to `/tmp`). The parent directory is created with mode 0700.
fn default_socket_path() -> Result<PathBuf> {
    let pid = getpid().as_raw();
    let parent = if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(xdg).join("hyoui")
    } else {
        let base = std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into());
        let uid = nix::unistd::geteuid().as_raw();
        PathBuf::from(base).join(format!("hyoui-{uid}"))
    };
    ensure_private_dir(&parent)?;
    Ok(parent.join(format!("agent-{pid}.sock")))
}

/// Ensure `dir` exists and is mode 0700 owned by the current euid.
fn ensure_private_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::create_dir_all(dir) {
        // EEXIST is fine; anything else is a real error.
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(Error::from(e));
        }
    }
    // Force 0700 even if create_dir_all left a wider mode.
    // (UnixSock::listen also re-verifies parent dir mode + euid.)
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(Error::from)?;
    Ok(())
}

/// Encode an exit-only status into the POSIX wait(2) layout
/// (`bits 8..15` = exit code, low byte = 0).
fn encode_exit_status(status: i32) -> i32 {
    (status & 0xFF) << 8
}

/// Encode a termination-by-signal into the POSIX wait(2) layout
/// (low 7 bits = signal number, bits 8..15 = 0).
fn encode_signal_status(signal: Signal) -> i32 {
    (signal as i32) & 0x7F
}

/// Decode a raw `wait(2)` status into a process exit code.
///
/// Exited normally → `(status >> 8) & 0xFF`. Killed by signal `n` →
/// `128 + n`. Mirrors the bootstrap `decode_wait_status` (see
/// `bootstrap/lib/agent/agent.mbt`).
fn decode_wait_status(status: i32) -> i32 {
    let sig = status & 0x7F;
    if sig == 0 {
        (status >> 8) & 0xFF
    } else {
        128 + sig
    }
}

// ---------------------------------------------------------------------------
// unit tests (private helpers)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_wait_status_normal_exit_zero() {
        assert_eq!(decode_wait_status(0), 0);
    }

    #[test]
    fn decode_wait_status_normal_exit_one() {
        // exit code 1 lives in bits 8..15
        assert_eq!(decode_wait_status(1 << 8), 1);
    }

    #[test]
    fn decode_wait_status_killed_by_signal_9() {
        assert_eq!(decode_wait_status(9), 128 + 9);
    }

    #[test]
    fn clamp_u16_accepts_in_range() {
        assert_eq!(clamp_u16(0).expect("0"), 0);
        assert_eq!(clamp_u16(80).expect("80"), 80);
        assert_eq!(clamp_u16(u16::MAX as i32).expect("max"), u16::MAX);
    }

    #[test]
    fn clamp_u16_rejects_out_of_range() {
        assert!(matches!(clamp_u16(-1), Err(Error::Invalid(_))));
        assert!(matches!(
            clamp_u16(u16::MAX as i32 + 1),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn encode_exit_status_roundtrip() {
        let s = encode_exit_status(7);
        assert_eq!(decode_wait_status(s), 7);
    }

    #[test]
    fn encode_signal_status_roundtrip() {
        let s = encode_signal_status(Signal::SIGKILL);
        assert_eq!(decode_wait_status(s), 128 + (Signal::SIGKILL as i32));
    }

    /// `windows().any()` is the Rust replacement for the bootstrap
    /// `bytes_contains` helper; verify the substring semantics match.
    #[test]
    fn substring_search_matches_bootstrap_semantics() {
        let haystack = b"hello world";
        let needle = b"o w";
        assert!(haystack.windows(needle.len()).any(|w| w == needle));

        let abc = b"abc";
        // empty needle: degenerate; we treat as "no scan" in scan_until.
        assert!(abc.windows(1).any(|w| w == &abc[0..1]));
        assert!(!abc.windows(4).any(|w| w == b"abcd"));
        assert!(!abc.windows(3).any(|w| w == b"xyz"));
    }
}
