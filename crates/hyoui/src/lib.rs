//! `hyoui` core library.
//!
//! Stage 2 ships the `sys` layer only: low-level syscall wrappers, RAII
//! resource handles, and a typed `Error` enum that the upper layers
//! (observer, agent, cli) will consume.
//!
//! ## Unsafe policy
//!
//! `unsafe` is confined to exactly two modules:
//!
//! * [`sys::raw`] — child process spawn (`forkpty`/`login_tty`), winsize
//!   `ioctl`, and libc constants that are not available through `nix`.
//! * [`sys::signal`] — `sigaction` installation, async-signal-safe handlers,
//!   and the self-pipe write end.
//!
//! Every other module (including everything in the future agent/cli crates)
//! must reach syscalls through the safe wrappers re-exported from `sys`.

#![cfg_attr(not(test), warn(missing_docs))]

pub mod sys;

// Stage 3 modules (doc-complete; lints enabled).
pub mod cli;
pub mod observer;
pub mod protocol;

// Stage 4: agent — the PTY-proxy event loop wired together from the
// `sys`, `observer`, `protocol`, and `cli` modules above.
pub mod agent;

/// Library version (matches `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use sys::error::{Error, Result};
