//! `handshake.request` / `handshake.response` の payload struct (DR-0008 §2.3)。
//!
//! kind の dispatch (= `kind = "handshake.request"` で variant 選択) は
//! 親 [`super::ControlMessage`] enum 側で扱う。本 module は payload struct のみ。

use std::fmt;

use serde::{Deserialize, Serialize};

// === handshake field 長さの上限 (R5-H10) ===
//
// HandshakeRequest は **認証前** に CBOR decode される (= token 検証より前)。
// schema 上は `caps: Vec<String>` / `token: Option<String>` で長さ無制限のため、
// 16 MiB frame 上限ぎりぎりまで自由に詰められる。これと
// `MAX_PENDING_HANDSHAKES = MAX_CLIENTS_PER_DAEMON (64)` を組み合わせると、
// 認証前段階で 1 GiB 級の transient peak (= 64 worker × 16 MiB) が成立し、
// memory exhaustion 経由の daemon kill / 同居 process 巻き添えが起きる。
//
// 対策として decode 直後に caps / token の長さを cap する:
//
// - [`MAX_CAPS_COUNT`]: `caps` Vec の要素数上限 (= MVP_CAPS の 4 倍弱の余裕)
// - [`MAX_CAP_LEN`]: 1 cap string の byte 長上限 (DR-0008 §2.2 の dotted name は
//   実運用で 16 byte 程度、64 で十分)
// - [`MAX_TOKEN_LEN`]: token string の byte 長上限 (128-bit hex = 32 byte が
//   `generate_lock_token` の出力、256 で 8 倍の余裕)
//
// 違反時は [`super::ErrorCode::ProtocolMalformed`] を返して当該 worker を即終了
// (= memory を抱え込まない)。serde の decode 自体は通過するため、validate は
// `daemon::accept::do_handshake_stage` 側で行う。

/// `HandshakeRequest.caps` の最大要素数。
pub const MAX_CAPS_COUNT: usize = 32;

/// `HandshakeRequest.caps` の各 cap string の最大 byte 長。
pub const MAX_CAP_LEN: usize = 64;

/// `HandshakeRequest.token` の最大 byte 長。`generate_lock_token` 出力は 32 byte、
/// 8 倍の余裕を持たせる (= 将来 token format を変えても 256 byte 以内で済む想定)。
pub const MAX_TOKEN_LEN: usize = 256;

/// client / daemon の動作 mode (DR-0006)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Mode {
    /// read/write、子 pty への入力可。
    Rw,
    /// read-only、子 pty への入力なし、winsize 計算除外。
    Ro,
    /// rw だが leader 取らない (= 他の rw client が leader)。
    RwNoLeader,
}

/// `handshake.request` payload。
///
/// `Debug` は手書き impl で `token` を `<redacted>` に置換する (secret 漏洩防止)。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl fmt::Debug for HandshakeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandshakeRequest")
            .field("caps", &self.caps)
            .field("mode", &self.mode)
            .field("exclusive", &self.exclusive)
            .field("detach_others", &self.detach_others)
            .field("token", &redact_token(self.token.as_deref()))
            .finish()
    }
}

/// token を Debug 出力用に redact する (Some → `"<redacted>"`、None → `None`)。
fn redact_token(token: Option<&str>) -> &'static str {
    match token {
        Some(_) => "Some(\"<redacted>\")",
        None => "None",
    }
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
    /// handshake 完了時点で子 process group が stopped か。
    #[serde(default)]
    pub child_stopped: bool,
}
