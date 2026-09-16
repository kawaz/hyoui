//! web endpoint の passkey 認証 (DR-0036)。
//!
//! **gateway は自分の endpoint を知らない。** endpoint URL を知るのは CLI
//! (`--endpoint` で受け取る) とブラウザ (`location` から計算する) の 2 者だけで、
//! gateway はその値を受け取って record を引く (決定 3)。endpoint リストも RP ID の
//! 指定口も origin の allowlist も config に置かない。
//!
//! record は endpoint を **key の 1 段** にした file で、2 つの unit が `flock` で
//! 共有する (決定 4)。instance 間で複製する protocol は持たない。
//!
//! 失効は削除ではなく tombstone で表す。CLI は gateway に通知せず file を書き、
//! gateway は読むだけである (決定 2 / 決定 4)。

mod record;
mod routes;
mod store;
pub mod token;
mod webauthn;

pub use record::{
    ACCESS_TTL_MS, Access, AuthFile, CODE_ATTEMPT_LIMIT, CodeOutcome, CredentialRecord,
    FamilyRecord, PendingChallenge, PendingFile, PendingRegistration, REFRESH_REPLAY_GRACE_MS,
    REFRESH_TTL_MS, REGISTRATION_TTL_MS, RefreshOutcome, RetiredRefresh, TokenGeneration,
};
pub use routes::{AuthContext, Identity, WS_TOKEN_PROTOCOL_PREFIX, WsAuth};
pub(crate) use routes::{require_auth, routes, ws_token_protocol};
pub use store::{StateDir, StateFile, StoreError};
pub use webauthn::{Rp, WebauthnFailure};
