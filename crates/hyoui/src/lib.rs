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

// v0.1.0 modules (PoC 07/08 から正規実装に取り込み):
//
//   * `scrollback` — daemon が子 pty 出力を timestamped chunks の ring buffer に蓄積
//                    (DR-0006 §11.6, finding 2026-05-26-scrollback-ring-buffer.md)
//   * `strip`      — ANSI escape (CSI/OSC/DCS/single char) を strip して raw text を返す
//                    (DR-0006 §11 装飾除去 default, finding 2026-05-26-ansi-strip.md)
pub mod scrollback;
pub mod strip;

/// Library version (matches `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use sys::error::{Error, Result};
