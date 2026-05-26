//! hyoui daemon (DR-0008 §Consequences、DR-0007 MVP scope)。
//!
//! 1 session = 子 PTY 1 つ + Unix socket bind + scrollback ring buffer +
//! attach 中の N clients。protocol module (`crate::protocol`) を使って各 client と
//! frame レベルで通信する。
//!
//! ## 段階実装の進捗 (Phase)
//!
//! - [x] Phase 6: skeleton (`DaemonConfig`、module 構成)
//! - [ ] Phase 7: 子 PTY spawn + listener bind + 1 client + handshake
//! - [ ] Phase 8: raw data 中継 + detach + clean shutdown
//! - [ ] Phase 9: multi-attach + bounded mpsc (backpressure DR-0008 §8.2)
//! - [ ] Phase 10: lock + leader + mode change
//! - [ ] Phase 11: status / tail / wait
//!
//! ## 関連
//!
//! - DR-0005 (思想), DR-0006 (CLI ground rules), DR-0007 (MVP scope), DR-0008 (protocol)
//! - finding 2026-05-26-multi-attach (poll パターン)
//! - finding 2026-05-26-daemon-fork (`--detached` 起動の pipe 同期、Phase 7 で使用予定)

mod config;
mod session;

pub use config::DaemonConfig;
pub use session::Session;
