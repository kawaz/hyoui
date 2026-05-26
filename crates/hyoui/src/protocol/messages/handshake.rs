//! `handshake.request` / `handshake.response` の payload struct (DR-0008 §2.3)。
//!
//! kind の dispatch (= `kind = "handshake.request"` で variant 選択) は
//! 親 [`super::ControlMessage`] enum 側で扱う。本 module は payload struct のみ。

use serde::{Deserialize, Serialize};

/// client / daemon の動作 mode (DR-0006)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// read/write、子 pty への入力可。
    Rw,
    /// read-only、子 pty への入力なし、winsize 計算除外。
    Ro,
    /// rw だが leader 取らない (= 他の rw client が leader)。
    RwNoLeader,
}

/// `handshake.request` payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HandshakeRequest {
    /// 自分 (client) が話せる capability 一覧。
    pub caps: Vec<String>,
    /// 動作 mode。
    pub mode: Mode,
    /// session 起動時の占有要求 (起動 race を意識する場合)。
    pub exclusive: bool,
    /// attach 時に既存 client を奪取する。
    pub detach_others: bool,
    /// HYOUI_LOCK_TOKEN env から継承した token (= null なら未提示)。
    pub token: Option<String>,
}

/// `handshake.response` payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HandshakeResponse {
    /// daemon 側が話せる capability 一覧 (= client の caps と intersect して有効 cap を決める)。
    pub caps: Vec<String>,
    /// session 名 (DR-0006)。
    pub session_id: String,
    /// daemon が割り当てた client 番号。
    pub client_id: u64,
    /// leader 取得結果 (rw mode の場合のみ true になりうる)。
    pub leader: bool,
    /// daemon が認証した実 mode (request の mode から変更されうる)。
    pub mode: Mode,
}
