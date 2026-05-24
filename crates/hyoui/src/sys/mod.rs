//! `sys` — the only layer in `hyoui` allowed to talk to the OS.
//!
//! Higher layers depend on the safe wrappers re-exported below. The two
//! `unsafe`-containing modules ([`raw`] and [`signal`]) are pub so they can be
//! invoked from the safe wrappers, but their `unsafe` bodies stay encapsulated.

pub mod clock;
pub mod error;
pub mod fd;
pub mod poll;
pub mod pty;
pub mod raw;
pub mod signal;
pub mod socket;
pub mod tty;
pub mod wait;

pub use clock::clock_monotonic;
pub use error::{Error, Result};
pub use fd::FdExt;
pub use poll::{PollOutcome, poll};
pub use pty::Pty;
pub use signal::{
    SelfPipe, SigMaskGuard, install_default, install_ignore, install_self_pipe, install_winch,
    raise, register_self_pipe,
};
pub use socket::{UmaskGuard, UnixSock};
pub use tty::{TtyGuard, enter_raw, is_tty, tty_size};
pub use wait::{WaitOutcome, wait_for_status};
