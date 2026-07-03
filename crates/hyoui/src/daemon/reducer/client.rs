//! Client domain reducer 骨格 (DR-0025 §Client domain、Phase 1b 前半 stub)。
//!
//! client 集合 / lifecycle / leader-follower に加え、Transport (socket / framing) / Auth
//! (handshake / cap nego) / Backpressure (write idle / stale) を内部 sub-state として持つ
//! (DR-0025 §Transport / Auth / Backpressure の扱い)。現状 `Detached` のみ Lock domain への
//! disconnect dispatch を配線し、`Connected` / `FrameReceived` の本実装・detach cascade 1 本化・
//! protocol kind 1:1 mapping・Transport / Auth / Backpressure sub-state は Phase 2。
//!
//! Client reducer は他 domain と異なり [`DomainViews`] を追加で受け取る (= raw_data /
//! input 系 request の holder 認可判定で Lock state を read する、DR-0025 §read-only view の
//! `Client ──read──→ Lock`)。

use super::super::lock::LockMsg;
use super::{ClientId, DaemonMsg, DomainOutput, DomainViews, EffectOutcome};

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
/// `Detached` は Lock domain へ [`LockMsg::ClientDisconnected`] を cross-domain dispatch
/// する (= holder なら process-bound GC で auto-release、DR-0025 §許可された cross-domain
/// 方向 `Client ──→ Lock`)。holder 判定と Released broadcast は Lock reducer 側が担い、
/// Client は「切れた client id」を渡すだけの弱結合を保つ (DR-0025 §Lock domain)。
///
/// `Connected` / `FrameReceived` は空を返す (= detach cascade 一本化 / raw_data 認可、
/// および `views` の Lock read は Phase 2)。
pub(in crate::daemon) fn reduce(
    _state: &mut ClientRegistry,
    _views: DomainViews<'_>,
    msg: ClientMsg,
) -> DomainOutput {
    match msg {
        ClientMsg::Detached { client_id } => {
            let mut out = DomainOutput::empty();
            out.cross.push(DaemonMsg::Lock(LockMsg::ClientDisconnected {
                client_id: client_id.0,
            }));
            out
        }
        ClientMsg::Connected { .. } | ClientMsg::FrameReceived { .. } => DomainOutput::empty(),
    }
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

#[cfg(test)]
mod tests {
    use super::super::{ClientId, DaemonMsg, DaemonState, DomainViews, EffectKind, handle};
    use super::*;
    use crate::daemon::lock::{LockMsg, LockState, reduce as lock_reduce};
    use crate::protocol::messages::ControlMessage;

    /// `Detached` は Lock domain へ [`LockMsg::ClientDisconnected`] を cross に 1 件積み、
    /// effect 自体は出さない (= DR-0025 §許可された cross-domain 方向 `Client ──→ Lock`)。
    ///
    /// なぜこの期待値か: Client reducer は holder 判定を持たず (= 弱結合)、「切れた client
    /// id」を Lock reducer へ渡すだけ。holder GC / Released broadcast は Lock 側の責務。
    /// dispatch は cross-domain queue 経由なので、この単体では cross に Lock msg が積まれる
    /// ことだけを固定する (holder GC の実効は下の handle 統合 test で検証)。
    #[test]
    fn detached_dispatches_client_disconnected_to_lock() {
        let mut reg = ClientRegistry;
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::Detached {
                client_id: ClientId(7),
            },
        );
        assert!(out.effects.is_empty(), "Detached 自体は effect を出さない");
        assert_eq!(out.cross.len(), 1, "Lock への cross dispatch が 1 件");
        match &out.cross[0] {
            DaemonMsg::Lock(LockMsg::ClientDisconnected { client_id }) => {
                assert_eq!(*client_id, 7, "切断された client id が Lock へ渡る");
            }
            other => panic!("Lock(ClientDisconnected) を期待: {other:?}"),
        }
    }

    /// handle 統合: holder client の `Detached` → Lock の process-bound GC が Released を
    /// 出し → `ClientBroadcast(ModeChange)` Effect が 1 件出る (PTY / socket 不要、
    /// `DaemonState` だけで検証)。
    ///
    /// なぜこの test か: Client ──→ Lock の cross-domain dispatch と Lock ──→ Client の
    /// Effect 翻訳が super-reducer 上で連結することを end-to-end で固定する。holder=7 を
    /// lock reduce で直接セットアップし、client 7 の detach で mode.change broadcast が
    /// 1 件生成されることを確認する (= lock GC が発火した証跡)。
    #[test]
    fn handle_holder_detach_emits_mode_change_broadcast() {
        let mut state = DaemonState::default();
        // holder=7 をセットアップ (lock reduce を直接使い state を作る、token 供給で grant)。
        let _ = lock_reduce(
            &mut state.lock,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("t".into()),
            },
        );
        let effects = handle(
            &mut state,
            DaemonMsg::Client(ClientMsg::Detached {
                client_id: ClientId(7),
            }),
        );
        assert_eq!(
            effects.len(),
            1,
            "holder 切断で mode.change broadcast が 1 件"
        );
        match &effects[0].kind {
            EffectKind::ClientBroadcast { message, .. } => {
                assert!(
                    matches!(message, ControlMessage::ModeChange(_)),
                    "ClientBroadcast の payload は ModeChange"
                );
            }
            other => panic!("ClientBroadcast を期待: {other:?}"),
        }
    }

    /// handle 統合 (対極): 非 holder の `Detached` は Lock GC が no-op なので Effect が
    /// 出ない (= holder=1 の lock は維持され mode.change を発火しない)。
    ///
    /// なぜこの test か: Client ──→ Lock dispatch が常に broadcast を生むわけではないこと
    /// (= 弱結合で「切れた id」を渡すだけで、broadcast の要否は Lock reducer の holder 照合
    /// 次第) を固定する。holder GC の false-positive で無関係な mode.change が飛ばないこと
    /// の回帰基準。
    #[test]
    fn handle_non_holder_detach_emits_no_effect() {
        let mut state = DaemonState::default();
        let _ = lock_reduce(
            &mut state.lock,
            LockMsg::Acquire {
                client_id: 1,
                token: Some("t".into()),
            },
        );
        let effects = handle(
            &mut state,
            DaemonMsg::Client(ClientMsg::Detached {
                client_id: ClientId(2),
            }),
        );
        assert!(
            effects.is_empty(),
            "非 holder 切断は Lock GC が no-op で mode.change を出さない"
        );
    }
}
