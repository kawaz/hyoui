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
    /// `waitpid(WNOHANG | WUNTRACED | WCONTINUED)` で子の状態 transition を拾い、
    /// 累積状態を踏まえて [`ChildState`] を返す。
    ///
    /// 旧 `child_actually_exited` 単体関数からの差し替え。state を持つので
    /// caller は `ChildLifecycle` インスタンスを 1 つ loop 全体で使い回す。
    pub(super) fn poll(&mut self, child: Pid) -> ChildState {
        let flags = WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED;
        match waitpid(child, Some(flags)) {
            Ok(WaitStatus::Exited(_, code)) => ChildState::Exited(Some(code)),
            Ok(WaitStatus::Signaled(_, sig, _)) => ChildState::Exited(Some(128 + (sig as i32))),
            Ok(WaitStatus::Stopped(_, _)) => {
                self.stopped = true;
                ChildState::Stopped
            }
            Ok(WaitStatus::Continued(_)) => {
                self.stopped = false;
                ChildState::Alive
            }
            Ok(WaitStatus::StillAlive) => {
                // No new transition; report the latched state.
                if self.stopped {
                    ChildState::Stopped
                } else {
                    ChildState::Alive
                }
            }
            // ptrace event 等の wildcard (= `ptrace` feature 有効時) を defensively alive 扱い
            #[allow(unreachable_patterns)]
            Ok(_) => ChildState::Alive,
            Err(_) => {
                // transient error (= ECHILD で reap 済の可能性等)。state を維持。
                if self.stopped {
                    ChildState::Stopped
                } else {
                    ChildState::Alive
                }
            }
        }
    }
}
