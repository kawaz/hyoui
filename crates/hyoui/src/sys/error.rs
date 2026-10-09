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

    /// bind しようとした path に socket (か他のファイル) が既にある、または同じ name
    /// lock を別のプロセスが持っている (DR-0041 決定 3)。相手の daemon の生死は問わない
    /// (= 死んだ socket の片付けは片付けの経路の責務)。
    #[error("socket already exists: {}", .0.display())]
    SocketExists(std::path::PathBuf),

    /// daemon (= remote 側) が返した拒否理由をそのまま運ぶ (Fable review M3
    /// 2026-06-12)。`Invalid` は `&'static str` 限定のため、`ErrorMessage.message`
    /// のような動的文字列 (= 例: `attach --exclusive denied: ...`) を client の
    /// stderr まで中継する用途で使う。
    #[error("{0}")]
    Remote(String),

    /// daemon が接続を閉じた (= frame の区切りか途中で EOF、または接続の reset)。
    ///
    /// 応答を待つ client が受けるのは、session の子が終わって daemon が後始末に入った時
    /// (= 接続を閉じてから socket を消すまでの間に繋いだ場合を含む)。利用者に要るのは
    /// 「session が終わっている」ことなので、frame の decode の失敗とは分けて伝える。
    #[error(
        "daemon が接続を閉じました。session は既に終わっています (`hyoui list` で確かめてください)"
    )]
    ConnectionClosed,
}

/// `sys`-layer result alias.
pub type Result<T> = std::result::Result<T, Error>;
