//! `hyoui` core library.
//!
//! v0.1.0 に向けて再構築中。DR-0008 (CBOR ハイブリッド framing) 確定後、
//! 旧 PoC 産物の `agent` / 旧 length-prefixed `protocol` モジュールを削除済み。
//! 新規 `protocol` / `daemon` / `client` モジュールが追って実装される。
//!
//! ## Unsafe policy
//!
//! `unsafe` is confined to:
//!
//! * [`sys::raw`] — child process spawn (`forkpty`/`login_tty`), winsize
//!   `ioctl`, and libc constants that are not available through `nix`.
//! * [`sys::signal`] — `sigaction` installation, async-signal-safe handlers,
//!   and the self-pipe write end.
//!
//! Every other module must reach syscalls through the safe wrappers
//! re-exported from `sys`.

#![cfg_attr(not(test), warn(missing_docs))]

pub mod sys;

pub mod cli;

pub mod config;

// v0.1.0 modules (PoC 07/08 から正規実装に取り込み済み):
//
//   * `scrollback` — daemon が子 pty 出力を timestamped chunks の ring buffer に蓄積
//                    (DR-0006 §11.6, finding 2026-05-26-scrollback-ring-buffer.md)
//   * `strip`      — ANSI escape (CSI/OSC/DCS/single char) を strip して raw text を返す
//                    (DR-0006 §11 装飾除去 default, finding 2026-05-26-ansi-strip.md)
pub mod scrollback;
pub mod strip;

// v0.1.0 wire protocol (DR-0008 確定版):
//
//   * Frame layout: [u32 LE size][u8 type][body]、wire 外枠は永久固定
//   * type=0x00 raw PTY data / 0x01 CBOR control message / 0x02+ 予約
//   * 各 control message は `protocol::messages::*` で型付き
pub mod protocol;

// v0.1.0 daemon (DR-0008 §Consequences、DR-0007 MVP):
//
// 段階実装中。1 session = 子 PTY + Unix socket + scrollback + attach clients の
// 集合体。protocol module を frame レベルで使う。
pub mod daemon;

// v0.1.0 client (= attach 側、DR-0006 §3):
//
// daemon と対称構造で stdin/stdout を中継。protocol module を frame レベルで使う。
pub mod client;

// DR-0027 Phase 1: session discovery for hyoui-web (= socket dir walk + status.query 統合)。
pub mod discovery;

// DR-0027 Phase 1: input spec bytes 化 helpers (= 旧 `hyoui-cli::input_handlers` を core 側に
// 引き上げ、hyoui-web からも同じ変換を使う)。
pub mod input_bytes;

/// Library version (matches `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use sys::error::{Error, Result};
