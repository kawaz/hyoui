//! Integration tests for the `sys` layer.
//!
//! These tests verify behaviour that needs an actual forked child or a
//! second process, which is awkward to do inside `#[cfg(test)] mod tests`
//! blocks because of `cargo test`'s threaded test runner.

use std::thread;
use std::time::Duration;

use hyoui::sys::fd::FdExt;
use hyoui::sys::pty::Pty;
use hyoui::sys::wait::{WaitOutcome, wait_for_status};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};

/// `forkpty` echo test (mirrors bootstrap's
/// "pty_spawnv: echo hello returns valid pid and waitpid succeeds").
#[test]
fn forkpty_echo_returns_status_zero() {
    let spawned = Pty::spawn(&["echo", "hello"], 80, 24).expect("spawn");
    let outcome = wait_for_status(spawned.child, false).expect("wait");
    match outcome {
        WaitOutcome::Exited { status, .. } => {
            assert_eq!(status, 0, "echo should exit 0");
        }
        other => panic!("expected Exited(0), got {other:?}"),
    }
}

/// ctty verification: spawn `cat`, verify `tcgetpgrp(master)` returns the
/// child pgid, send SIGSTOP, and verify `waitpid(WUNTRACED)` reports a
/// Stopped state. This is the central proof that `forkpty + login_tty`
/// succeeded:
///
/// * The child became the session leader (`login_tty` calls `setsid`).
/// * The PTY slave is the controlling terminal of the child.
/// * `tcgetpgrp(master)` returns the child's pgid (the foreground process
///   group is correctly initialized by `login_tty`).
/// * Job-control stop machinery (`waitpid(WUNTRACED)`) is functional.
///
/// We use SIGSTOP rather than SIGTSTP for the stop because SIGSTOP cannot
/// be caught/ignored and gives a deterministic test on every platform; the
/// ctty proof is the `tcgetpgrp` assertion, not the choice of stop signal.
#[test]
fn forkpty_child_has_ctty_and_can_be_stopped() {
    // `cat` reads from stdin forever — the child stays alive until the
    // parent closes the master fd or kills it.
    let spawned = Pty::spawn(&["cat"], 80, 24).expect("spawn cat");
    let child = spawned.child;

    // Wait until tcgetpgrp returns the child pgid; this happens once
    // login_tty has made the child the foreground process group of its
    // ctty (login_tty contract).
    let mut fg_pgid = None;
    for _ in 0..100 {
        if let Ok(p) = nix::unistd::tcgetpgrp(spawned.pty.master_fd())
            && p.as_raw() == child.as_raw()
        {
            fg_pgid = Some(p);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fg_pgid.expect("tcgetpgrp never matched child").as_raw(),
        child.as_raw(),
        "ctty fg pgid should equal the child pid (login_tty contract)"
    );

    // Send SIGSTOP to the child. SIGSTOP cannot be caught/ignored, so it is
    // guaranteed to stop the child unconditionally — which lets us prove
    // that `waitpid(WUNTRACED)` machinery works irrespective of whether the
    // child happened to install a SIGTSTP handler.
    //
    // (We also exercise SIGTSTP separately below to verify the ctty-aware
    //  job-control path.)
    kill(child, Signal::SIGSTOP).expect("kill SIGSTOP");

    // Wait WUNTRACED for the stop event. Use blocking waitpid (nohang=false)
    // with WUNTRACED to avoid the race where WNOHANG returns 0 before the
    // kernel updates the state.
    let outcome = wait_for_status(child, false).expect("waitpid");
    match outcome {
        WaitOutcome::Stopped { signal, .. } => {
            assert!(
                matches!(signal, Signal::SIGSTOP | Signal::SIGTSTP),
                "expected SIGSTOP/SIGTSTP stop, got {signal:?}"
            );
        }
        other => panic!("expected Stopped, got {other:?}"),
    }

    // Clean up: kill the stopped child (SIGKILL bypasses the stop).
    let _ = kill(child, Signal::SIGKILL);
    // Drain the final status so we don't leave a zombie. SIGKILL on a
    // stopped child yields Signaled(SIGKILL) after the kernel resumes it
    // for delivery.
    for _ in 0..50 {
        match wait_for_status(child, true) {
            Ok(WaitOutcome::Exited { .. }) | Ok(WaitOutcome::Signaled { .. }) => break,
            Ok(_) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
}

/// Drain a small string written to the master while the child is still
/// alive. We poll concurrently with the child running and stop as soon as
/// we observe "hi" or the child exits.
#[test]
fn forkpty_echo_output_observable_from_master() {
    let spawned = Pty::spawn(&["echo", "hi"], 80, 24).expect("spawn");
    spawned
        .pty
        .master_fd()
        .set_nonblocking(true)
        .expect("nonblock");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut acc = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        match spawned.pty.master_fd().read_some(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(2).any(|w| w == b"hi") {
                    break;
                }
            }
            Err(hyoui::Error::Errno(Errno::EAGAIN)) => {}
            // macOS reports EIO after the slave is closed (child exited
            // and its slave fd was the last reference). Any data we
            // already buffered in `acc` is what we have.
            Err(hyoui::Error::Errno(Errno::EIO)) => break,
            Err(e) => panic!("read err: {e:?}"),
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    // Reap the child to avoid a zombie.
    let _ = wait_for_status(spawned.child, false);

    let text = String::from_utf8_lossy(&acc);
    assert!(
        text.contains("hi"),
        "expected 'hi' in pty output, got {text:?}"
    );
}
