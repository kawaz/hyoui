//! Crate-wide error type. Thin wrapper around `nix::errno::Errno` plus a few
//! categorical variants the agent loop wants to distinguish.

use std::io;

use nix::errno::Errno;
use thiserror::Error;

/// `sys`-layer error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A raw `errno` from a syscall.
    #[error("syscall failed: {0}")]
    Errno(#[from] Errno),

    /// A `std::io::Error` (used for file metadata, paths, etc.).
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Caller provided an invalid argument (length 0, NUL inside a path, etc.).
    #[error("invalid argument: {0}")]
    Invalid(&'static str),

    /// A precondition for a syscall failed (parent dir mode/owner, ctty
    /// unavailable, ...).
    #[error("precondition violated: {0}")]
    Precondition(&'static str),

    /// daemon (= remote 側) が返した拒否理由をそのまま運ぶ (Fable review M3
    /// 2026-06-12)。`Invalid` は `&'static str` 限定のため、`ErrorMessage.message`
    /// のような動的文字列 (= 例: `attach --exclusive denied: ...`) を client の
    /// stderr まで中継する用途で使う。
    #[error("{0}")]
    Remote(String),
}

/// `sys`-layer result alias.
pub type Result<T> = std::result::Result<T, Error>;
