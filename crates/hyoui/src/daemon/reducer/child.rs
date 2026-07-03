//! Child domain reducer 骨格 (DR-0025 §Child domain、Phase 1b 前半 stub)。
//!
//! 子プロセスの state machine + lifecycle event を扱う。DR-0001 axis 1 / DR-0017 anchor /
//! DR-0019 OnChildSuspend policy の formal 化は Phase 3。Phase 1b 前半では骨格 stub のみ。
//!
//! 命名: state 型は [`ChildDomainState`] とする。`daemon::pty::ChildState` (= latched な
//! 子状態 enum) と衝突するため domain reducer 側を明示的に別名にする。

use nix::unistd::Pid;

use super::{DomainOutput, EffectOutcome};

/// Child domain の state (DR-0025 §Child domain の ChildState machine)。
///
/// 子の formal state machine (Spawning / Running / Stopped / Exited / Reaped) + DR-0019
/// OnChildSuspend policy を将来持つ。Phase 1b 前半では空 stub。
///
/// 命名は `daemon::pty::ChildState` との衝突回避のため `ChildDomainState`。
#[derive(Debug, Default)]
pub(in crate::daemon) struct ChildDomainState;

/// Child domain の入力 event (DR-0025 §Child domain の ChildEvent、stub 抜粋)。
#[derive(Debug)]
pub(in crate::daemon) enum ChildMsg {
    /// `SIGCHLD` 受信 (DR-0025 §IO boundary の translate 例
    /// `DaemonMsg::Child(SigchldReceived { pid })`)。reducer は waitpid 結果を state 遷移へ写す。
    SigchldReceived { pid: Pid },
    /// 子 exit を reap した (`Some(code)` は実 exit code、`None` は取得不能)。
    Reaped { exit_status: Option<i32> },
    /// `SIGHUP` 受信 (DR-0025 §Child domain の `HungUp`)。
    HungUp,
}

/// Child domain reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// [stub] Phase 1b 後半で配線されるまで空を返す。実装時は子 exit を `Child ──→ Serve` /
/// `Child ──→ Client` へ dispatch し、reap を `Effect::Kill` 等の EffectResult 経由で扱う。
pub(in crate::daemon) fn reduce(_state: &mut ChildDomainState, _msg: ChildMsg) -> DomainOutput {
    DomainOutput::empty()
}

/// Child domain の `EffectResult` 受口 (DR-0025 §Effect layer の pending state 戦略)。
///
/// [stub] Phase 1b 後半で `Effect::Kill` / `Effect::SpawnChild` の完了/失敗を受けて state を
/// 遷移させる。
pub(in crate::daemon) fn reduce_effect_result(
    _state: &mut ChildDomainState,
    _seq: u64,
    _outcome: &EffectOutcome,
) -> DomainOutput {
    DomainOutput::empty()
}
