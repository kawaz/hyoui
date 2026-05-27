//! `daemon/screen/` — vt100 ベースの screen state wrapper (DR-0013 Phase A)。
//!
//! daemon が「screen state の唯一の正本」を持つための core module
//! (DR-0013 §1 + §3)。子 PTY bytes は本 module の [`ScreenState`] を経由してから
//! broadcast / wait / tail に流れる設計。生 byte の直接 broadcast はしない。
//!
//! ## 構成
//!
//! - [`state`] — vt100 `Parser` を内包する [`ScreenState`]、process / 各 getter /
//!   DEC sync flag / stalled timer を提供
//! - [`redraw`] — attach 復元用 bytes を組み立てる [`build_attach_redraw`]
//!   (= alt mode prepend + `state_formatted()`)
//! - [`health`] — stalled sequence 検出 [`check_stalled`] (Phase A: detect only)
//!
//! ## Phase A の責務
//!
//! 1. PTY read loop が `ScreenState::process` を呼んで bytes を反映する経路 (§3)
//! 2. attach handshake 直後に `build_attach_redraw` を送って画面を復元する (§4)
//! 3. DEC sync update (`?2026h`) 同期中は redraw を blocking する (§6)
//! 4. 5 秒 stalled sequence 検出 (`check_stalled`、Phase A は warn のみ) (§5)
//!
//! ## Phase B 以降に持ち越す項目
//!
//! - input bytes log (= resize 救済策、§7) — `crates/hyoui/src/daemon/screen/input_log.rs`
//! - scrollback 統合 (= `crates/hyoui/src/scrollback.rs` 置換、§8)
//! - structured snapshot (= §11) — `crates/hyoui/src/daemon/screen/snapshot.rs`
//! - per-line SequenceNo + pull 型 protocol (= §4 Phase B)
//! - debug / inspection protocol (= §9) + DR-0008 cap flag negotiation (§10)

pub(crate) mod health;
pub(crate) mod input_log;
pub(crate) mod redraw;
pub(crate) mod snapshot;
pub(crate) mod state;

pub(crate) use health::{StalledOutcome, check_stalled};
pub(crate) use redraw::build_attach_redraw;
pub(crate) use snapshot::{
    ScreenDumpFormat, ScreenDumpLayer, build_screen_dump, build_screen_snapshot,
};
// ScreenSnapshot は session.rs の test (CBOR decode で確認) で参照する。test では
// `crate::daemon::screen::snapshot::ScreenSnapshot` フルパス経由でも届くが、
// session.rs の既存スタイルに合わせて re-export しておく。
#[cfg(test)]
pub(crate) use snapshot::ScreenSnapshot;
// snapshot::ScreenDumpError は control.rs 側 module path で参照する想定
// (= `super::screen::snapshot::ScreenDumpError`)。daemon/screen/snapshot.rs が
// pub(crate) なので path 経由でアクセスできる。
pub(crate) use state::ScreenState;
