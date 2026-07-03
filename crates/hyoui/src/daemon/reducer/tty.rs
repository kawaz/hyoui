//! TTY domain reducer 骨格 (DR-0025 §TTY domain、Phase 1b 前半 stub)。
//!
//! DR-0025 は byte stream を 3 layer (raw bytes → parsed event → semantic event) に
//! 段階的に意味化し、本 reducer は Layer 3 (= semantic event) を入力に取る。enum カタログ
//! (150-200 variant) の整備は Phase 6a/6b。Phase 1b 前半では骨格 stub のみ。

use super::{DomainOutput, EffectOutcome};

/// TTY domain の state (DR-0025 §TtyParserPipeline)。
///
/// parser pipeline の semantic state (= cursor / mode / alt screen 等の解釈状態) を将来
/// 持つ。Phase 1b 前半では空 stub。
#[derive(Debug, Default)]
pub(in crate::daemon) struct TtyState;

/// TTY domain の入力 event (DR-0025 §TTY domain の Layer 3 semantic event、stub 抜粋)。
///
/// カタログ全 variant は Phase 6a/6b。ここでは骨格として代表 2 種のみ置く。
#[derive(Debug)]
pub(in crate::daemon) enum TtyMsg {
    /// Layer 1 の raw byte 到着 (DR-0025 §IO boundary の translate 例
    /// `DaemonMsg::Tty(TtyLayer1Bytes { bytes })`)。実 pipeline は Layer 2/3 へ段階化する。
    Layer1Bytes { bytes: Vec<u8> },
    /// resize 要求 (= Client 認可後に dispatch され、Tty reducer が state 更新 +
    /// `Effect::TtyResize` を発行する経路の入口、DR-0025 §cross-domain `Client ──→ Tty`)。
    ResizeRequested { cols: u16, rows: u16 },
}

/// TTY domain reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// [stub] Phase 1b 後半 (serve_loop translate 層) で配線されるまで空を返す。実装時は Layer 3
/// semantic event を screen state 更新 (`Tty ──→ Screen` dispatch) / `Effect::TtyResize` 等へ
/// 写す。
pub(in crate::daemon) fn reduce(_state: &mut TtyState, _msg: TtyMsg) -> DomainOutput {
    DomainOutput::empty()
}

/// TTY domain の `EffectResult` 受口 (DR-0025 §Effect layer の pending state 戦略)。
///
/// [stub] Phase 1b 後半で `Effect::TtyResize` 等の完了/失敗を受けて state を Confirmed /
/// Rolled-back に遷移させる。
pub(in crate::daemon) fn reduce_effect_result(
    _state: &mut TtyState,
    _seq: u64,
    _outcome: &EffectOutcome,
) -> DomainOutput {
    DomainOutput::empty()
}
