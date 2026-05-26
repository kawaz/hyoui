//! Monotonic clock — used by the agent loop for timeouts/heartbeats.

use std::time::Duration;

use nix::sys::time::TimeSpec;
use nix::time::{ClockId, clock_gettime};

use super::error::{Error, Result};

/// `clock_gettime(CLOCK_MONOTONIC)` as a `Duration` since some unspecified
/// epoch.
///
/// # Suspend behavior (OS-dependent, R5-M21)
///
/// `CLOCK_MONOTONIC` does **not** have a portable definition for what happens
/// while the system is suspended (S3/S4):
///
/// * **Linux**: paused during suspend (use `CLOCK_BOOTTIME` to include suspend
///   time).
/// * **macOS** (Darwin): paused during suspend.
/// * **FreeBSD**: continues to advance through suspend
///   (`CLOCK_MONOTONIC` on FreeBSD ≈ Linux's `CLOCK_BOOTTIME`).
///
/// `wait` timeouts and heartbeats inherit this behavior. A laptop that is
/// suspended for an hour and resumed will see `wait` time out one hour later
/// than wall-clock would suggest on Linux/macOS, and roughly on time on
/// FreeBSD. See `docs/DESIGN.md` §5 (Portability) for the broader matrix.
pub fn clock_monotonic() -> Result<Duration> {
    let ts: TimeSpec = clock_gettime(ClockId::CLOCK_MONOTONIC).map_err(Error::from)?;
    let secs = ts.tv_sec() as u64;
    let nanos = ts.tv_nsec() as u32;
    Ok(Duration::new(secs, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_is_nonzero_and_nondecreasing() {
        // mirrors ffi_wbtest.mbt: "clock_monotonic_ms: monotonic and positive"
        let t1 = clock_monotonic().expect("clock");
        assert!(t1 > Duration::ZERO);
        let t2 = clock_monotonic().expect("clock");
        assert!(t2 >= t1, "monotonic clock went backwards: {t1:?} -> {t2:?}");
    }
}
