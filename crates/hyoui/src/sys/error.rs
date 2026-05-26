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
}

/// `sys`-layer result alias.
pub type Result<T> = std::result::Result<T, Error>;
