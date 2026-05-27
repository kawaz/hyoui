//! 子 PTY の lifecycle 追跡 (DR-0009 Phase A 前半で `session.rs` から分離)。
//!
//! - [`ChildLifecycle`] / [`ChildState`]: SIGSTOP/SIGCONT を取りこぼさない
//!   子 process 状態 machine (R4-H14)
//! - [`ALIVE_RETRY_INTERVAL`] / [`STOPPED_POLL_INTERVAL`]: serve_loop 内で
//!   状態に応じて使い分ける poll interval
//!
//! `Session::Drop` の SIGTERM→SIGKILL 経路や `finalize_child` (= 正常 path の
//! waitpid 集約) は `session.rs` に残る (= Session 自身の責務)。本 module は
//! 「子 PID の現在状態 polling/state machine」のみを担当する。

use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

/// 子が通常 alive 時の master read=0/EIO retry 間隔。forkpty 直後の
/// transient 偽 EOF を吸収する用途なので短く (= 200Hz)。
pub(super) const ALIVE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// 子が SIGTSTP/SIGSTOP で stopped 中の master poll 間隔 (R4-H14)。
/// 子から出力が来る見込みが無いため大きめに (= ~2Hz)。SIGCONT を検出する
/// `waitpid(WCONTINUED)` の latency もこの上限になる。
pub(super) const STOPPED_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// 子 process の状態判定結果 (R4-H14: Stopped と Continued を区別)。
#[derive(Debug)]
pub(super) enum ChildState {
    /// 子は exit 済。`Some(code)` は実 exit code、`None` は transient で取得不能。
    Exited(Option<i32>),
    /// 子は通常 alive (= StillAlive、または直前に Continued を受けた、transient error)。
    /// caller は短い sleep で次の poll に戻る。
    Alive,
    /// 子は SIGTSTP / SIGSTOP で stopped 中。master PTY 出力は事実上止まり、
    /// poll → read=0 / EIO の busy spin になる可能性が高いので caller は
    /// **長めの sleep** (= STOPPED_POLL_INTERVAL) で待機する。
    Stopped,
}

/// DR-0001 軸 1: 子の **新規 state transition イベント** (= `waitpid` で初めて
/// 取り出した遷移)。latch された状態とは別物で、軸 1 policy の発火判定に使う。
///
/// `ChildState` は「現在の latched 状態」を返すため、loop で何度呼んでも
/// `Stopped` のままになる (= SIGCONT を毎周送ってしまう)。本 enum は新規 transition
/// のみを 1 度返し、以降は `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildTransition {
    /// 子が新しく STOPPED 状態に遷移した (= 軸 1 policy を発火させる)。
    Stopped,
    /// 子が STOPPED 状態から復帰した (= invariant 回復の確認用、外部介入で
    /// SIGCONT が送られた場合の log/trace 用途)。
    Continued,
    /// 子が exit した。`Some(code)` が判明している場合のみ含む。
    Exited(Option<i32>),
}

/// 子の lifecycle 追跡 (R4-H14)。
///
/// `waitpid` の Stopped / Continued は **state transition でしか報告されない**
/// (= kernel が wait queue から消費したら以降は StillAlive)。busy-wait を避ける
/// ため自前で `stopped` フラグを保持し、次の Continued / Exited が来るまで
/// 「stopped 中」として扱う。`WUNTRACED | WCONTINUED` を指定して waitpid を呼ぶ
/// ことで transition を取りこぼさない。
///
/// 用途: serve_loop の中で master read が 0 / EIO を返した時に
/// `poll(child)` を呼んで状態を更新し、ChildState で caller に sleep 間隔を
/// 委ねる。
#[derive(Debug, Default)]
pub(super) struct ChildLifecycle {
    /// 直近の waitpid で Stopped を観測してから、まだ Continued / Exited を
    /// 観測していない時のみ true。
    stopped: bool,
}

impl ChildLifecycle {
    /// `poll_with_transition` の latched state だけを返す薄い wrapper。
    /// transition 情報を必要としない呼び出し元 (= test 内 helper 等) 向け。
    ///
    /// 通常の serve_loop 配線では transition を policy 発火に使うため、
    /// 直接 `poll_with_transition` を呼ぶこと。
    #[cfg(test)]
    pub(super) fn poll(&mut self, child: Pid) -> ChildState {
        self.poll_with_transition(child).0
    }

    /// `waitpid(WNOHANG | WUNTRACED | WCONTINUED)` で子の状態 transition を拾い、
    /// 累積状態を踏まえて `(state, transition)` を返す。
    ///
    /// 返り値 `.1` (= `Option<ChildTransition>`) は `waitpid` が今回呼び出しで
    /// **初めて** 取り出した遷移のみ `Some` (= `WaitStatus::Stopped` /
    /// `Continued` / `Exited` を初めて drain した瞬間)。`StillAlive` や
    /// transient error では `None`。caller はこの transition を見て軸 1
    /// policy (= `OnChildSuspend::Follow` で `raise(SIGSTOP)` / `AutoResume`
    /// で `killpg(child, SIGCONT)`) を発火させる。
    ///
    /// 旧 `child_actually_exited` 単体関数からの差し替え。state を持つので
    /// caller は `ChildLifecycle` インスタンスを 1 つ loop 全体で使い回す。
    ///
    /// 通常の latched state だけを返す path は廃止 (= 毎周期で `Stopped` を
    /// 返してしまい、policy 側で何度も SIGCONT を投げる事故の温床になる)。
    /// caller は本 method の返り値 `.1` を毎回 `Some` 判定し、必要なら policy
    /// を発火させる。
    pub(super) fn poll_with_transition(
        &mut self,
        child: Pid,
    ) -> (ChildState, Option<ChildTransition>) {
        let flags = WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED;
        match waitpid(child, Some(flags)) {
            Ok(WaitStatus::Exited(_, code)) => (
                ChildState::Exited(Some(code)),
                Some(ChildTransition::Exited(Some(code))),
            ),
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                let code = 128 + (sig as i32);
                (
                    ChildState::Exited(Some(code)),
                    Some(ChildTransition::Exited(Some(code))),
                )
            }
            Ok(WaitStatus::Stopped(_, _)) => {
                self.stopped = true;
                (ChildState::Stopped, Some(ChildTransition::Stopped))
            }
            Ok(WaitStatus::Continued(_)) => {
                self.stopped = false;
                (ChildState::Alive, Some(ChildTransition::Continued))
            }
            Ok(WaitStatus::StillAlive) => {
                // No new transition; report the latched state without a transition event.
                let state = if self.stopped {
                    ChildState::Stopped
                } else {
                    ChildState::Alive
                };
                (state, None)
            }
            // ptrace event 等の wildcard (= `ptrace` feature 有効時) を defensively alive 扱い
            #[allow(unreachable_patterns)]
            Ok(_) => (ChildState::Alive, None),
            Err(_) => {
                // transient error (= ECHILD で reap 済の可能性等)。state を維持。
                let state = if self.stopped {
                    ChildState::Stopped
                } else {
                    ChildState::Alive
                };
                (state, None)
            }
        }
    }
}
