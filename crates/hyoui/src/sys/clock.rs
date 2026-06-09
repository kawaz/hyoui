//! Clock utilities — monotonic clock for timeouts/heartbeats, and wall-clock
//! epoch helper for record event timestamps.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::sys::time::TimeSpec;
use nix::time::{ClockId, clock_gettime};

use super::error::{Error, Result};

/// DR-0016 §3 — record event の `ts_unix_ms` 用 (= epoch ms)。
///
/// `SystemTime::UNIX_EPOCH` より前の時刻 (= 時計がリセットされた等) は `0` を返す。
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
