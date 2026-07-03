//! Serve domain reducer 骨格 (DR-0025 §Serve domain、Phase 1b 前半 stub)。
//!
//! serve プロセス自身の lifecycle + shutdown 調停を扱う。SIGTERM→SIGKILL 昇格の統一 /
//! linger phase 統合は Phase 4。Phase 1b 前半では骨格 stub のみ。

use nix::sys::signal::Signal;

use super::{DomainOutput, EffectOutcome};

/// Serve domain の state (DR-0025 §Serve domain の ServeState machine)。
///
/// serve lifecycle (Booting / Serving / ShuttingDown / ShutDown) + 内部 metric を将来
/// 持つ。Phase 1b 前半では空 stub。
#[derive(Debug, Default)]
pub(in crate::daemon) struct ServeState;

/// Serve domain の入力 event (DR-0025 §Serve domain の ServeEvent、stub 抜粋)。
#[derive(Debug)]
pub(in crate::daemon) enum ServeMsg {
    /// serve boot 完了。
    Booted,
    /// shutdown 開始 (= `reason` で契機を区別)。
    ShutdownInitiated { reason: ShutdownReason },
    /// shutdown 完了。
    ShutdownCompleted,
}

/// serve shutdown の契機 (DR-0025 §Serve domain の ShutdownReason)。
#[derive(Debug)]
pub(in crate::daemon) enum ShutdownReason {
    /// 子プロセス exit (= `Child ──→ Serve` dispatch)。
    ChildExited,
    /// signal 受信。
    SignalReceived(Signal),
    /// 全 client detach (= `Client ──→ Serve` dispatch、DR-0025 §cross-domain)。
    AllClientsDetached,
    /// config directive による停止。
    ConfigDirective,
}

/// Serve domain reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// [stub] Phase 1b 後半で配線されるまで空を返す。実装時は shutdown 調停と `Serve ──→ Client`
/// (= shutdown 開始を全 client に notify) の dispatch を担う。
pub(in crate::daemon) fn reduce(_state: &mut ServeState, _msg: ServeMsg) -> DomainOutput {
    DomainOutput::empty()
}

/// Serve domain の `EffectResult` 受口 (DR-0025 §Effect layer の pending state 戦略)。
///
/// [stub] Phase 1b 後半で shutdown notify 等の effect 完了を受けて state を遷移させる。
pub(in crate::daemon) fn reduce_effect_result(
    _state: &mut ServeState,
    _seq: u64,
    _outcome: &EffectOutcome,
) -> DomainOutput {
    DomainOutput::empty()
}
