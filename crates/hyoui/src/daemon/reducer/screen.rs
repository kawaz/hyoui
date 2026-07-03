//! Screen domain reducer 骨格 (DR-0025 §Screen domain、Phase 1b 前半 stub)。
//!
//! 仮想 screen state (rows-base) + byte-base tail/history (DR-0013 §scrollback) + watch
//! (region / matcher / flow の 3 軸直交) を扱う。ScreenWriteEvent broadcast は Phase 5、
//! watch は Phase 7。Phase 1b 前半では骨格 stub のみ。
//!
//! 命名: state 型は [`ScreenDomainState`] とする。`daemon::screen::state::ScreenState`
//! (= vt100 backing の仮想 screen struct) と衝突するため domain reducer 側を明示的に別名にする。

use super::{DomainOutput, EffectOutcome};

/// Screen domain の state (DR-0025 §Screen domain)。
///
/// 仮想 screen state + byte-base tail/history + watch registration を将来持つ。Phase 1b
/// 前半では空 stub。
///
/// 命名は `daemon::screen::state::ScreenState` との衝突回避のため `ScreenDomainState`。
#[derive(Debug, Default)]
pub(in crate::daemon) struct ScreenDomainState;

/// Screen domain の入力 event (DR-0025 §Screen domain の ScreenWriteEvent、stub 抜粋)。
#[derive(Debug)]
pub(in crate::daemon) enum ScreenMsg {
    /// 仮想 screen への write 通知 (= cell 更新の生 event、DR-0025 §ScreenWriteEvent)。
    /// payload / layer は Phase 5、watch 連携は Phase 7。骨格では座標のみ。
    WriteEvent { x: u16, y: u16 },
}

/// Screen domain reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// [stub] Phase 1b 後半で配線されるまで空を返す。実装時は ScreenWriteEvent を subscribe
/// client へ配信する `Screen ──→ Client` dispatch と watch matcher 実行を担う。
pub(in crate::daemon) fn reduce(_state: &mut ScreenDomainState, _msg: ScreenMsg) -> DomainOutput {
    DomainOutput::empty()
}

/// Screen domain の `EffectResult` 受口 (DR-0025 §Effect layer の pending state 戦略)。
///
/// [stub] Phase 1b 後半で watch 配信等の effect 完了を受けて state を遷移させる。
pub(in crate::daemon) fn reduce_effect_result(
    _state: &mut ScreenDomainState,
    _seq: u64,
    _outcome: &EffectOutcome,
) -> DomainOutput {
    DomainOutput::empty()
}
