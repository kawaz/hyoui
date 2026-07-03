//! Client domain reducer 骨格 (DR-0025 §Client domain、Phase 1b 前半 stub)。
//!
//! client 集合 / lifecycle / leader-follower に加え、Transport (socket / framing) / Auth
//! (handshake / cap nego) / Backpressure (write idle / stale) を内部 sub-state として持つ
//! (DR-0025 §Transport / Auth / Backpressure の扱い)。detach cascade 1 本化 / protocol kind
//! 1:1 mapping は Phase 2。Phase 1b 前半では骨格 stub のみ。
//!
//! Client reducer は他 domain と異なり [`DomainViews`] を追加で受け取る (= raw_data /
//! input 系 request の holder 認可判定で Lock state を read する、DR-0025 §read-only view の
//! `Client ──read──→ Lock`)。

use super::{ClientId, DomainOutput, DomainViews, EffectOutcome};

/// Client domain の state (DR-0025 §ClientRegistry)。
///
/// client 集合 + Transport / Auth / Backpressure sub-state を将来持つ。Phase 1b 前半では
/// 空 stub。
#[derive(Debug, Default)]
pub(in crate::daemon) struct ClientRegistry;

/// Client domain の入力 event (DR-0025 §Client domain の ClientEvent、stub 抜粋)。
#[derive(Debug)]
pub(in crate::daemon) enum ClientMsg {
    /// client 接続確立。
    Connected { client_id: ClientId },
    /// client からの frame 到着 (DR-0025 §IO boundary の translate 例
    /// `DaemonMsg::Client(FrameReceived..)`)。raw_data は本 reducer が認可判定
    /// (mode / cap / lock holder) 後に `Effect::TtyWrite` へ写す。
    FrameReceived { client_id: ClientId },
    /// client detach。
    Detached { client_id: ClientId },
}

/// Client domain reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// `views` で Lock state を read し raw_data の holder 認可を判定する (DR-0022、Phase 2 で
/// 実装)。[stub] Phase 1b 後半で配線されるまで空を返す。
pub(in crate::daemon) fn reduce(
    _state: &mut ClientRegistry,
    _views: DomainViews<'_>,
    _msg: ClientMsg,
) -> DomainOutput {
    DomainOutput::empty()
}

/// Client domain の `EffectResult` 受口 (DR-0025 §Effect layer の pending state 戦略)。
///
/// [stub] Phase 1b 後半で `Effect::TtyWrite` の EffectResult を受けて raw ack (= DR-0021 drain
/// ack 意味論) を発行する等に配線する。認可 read のため `views` を受け取る。
pub(in crate::daemon) fn reduce_effect_result(
    _state: &mut ClientRegistry,
    _views: DomainViews<'_>,
    _seq: u64,
    _outcome: &EffectOutcome,
) -> DomainOutput {
    DomainOutput::empty()
}
