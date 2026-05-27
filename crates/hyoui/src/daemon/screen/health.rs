//! `ScreenState` の health check (DR-0013 §5 + task A-8)。
//!
//! tmux `input.c` 標準と揃え、`last_feed_at` から 5 秒経過した stalled state を
//! 検出する。Phase A の方針: **検出のみ + warn-level log 用の `bool` を返す**。
//! 完全 reset (= `ScreenState::reset` を呼ぶ) は Phase B で再検討する保守的実装
//! (= 既存 partial sequence が後続 bytes で完成する可能性を捨てない)。
//!
//! 統合先: `daemon/session.rs` の serve_loop が poll timeout 経路で呼ぶ想定。
//! tokio 等の非同期 timer は使わず、既存 poll loop の自然なタイミングに乗せる
//! (= hyoui daemon の sync poll 設計と整合)。

use std::time::Instant;

use super::state::ScreenState;

/// stalled 検出結果。
///
/// `Detected` は当該 tick で「5 秒経過した partial sequence がある可能性」を
/// 示すサイン。caller は warn log を出すか、Phase B で `ScreenState::reset` を
/// 呼ぶか判断する。Phase A では log を出すだけで state は保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StalledOutcome {
    /// `last_feed_at` から `STALLED_RESET_TIMEOUT` 未満。健全。
    Healthy,
    /// 閾値を超過。Phase A は warn log のみで state を保持する。
    Detected,
}

/// 現時刻 `now` 基準で `state` の stalled 判定を行う。
pub(crate) fn check_stalled(state: &ScreenState, now: Instant) -> StalledOutcome {
    if state.is_stalled(now) {
        StalledOutcome::Detected
    } else {
        StalledOutcome::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::STALLED_RESET_TIMEOUT;
    use super::*;
    use std::time::Duration;

    #[test]
    fn fresh_state_is_healthy() {
        let s = ScreenState::new(5, 40, 100);
        assert_eq!(check_stalled(&s, Instant::now()), StalledOutcome::Healthy);
    }

    #[test]
    fn old_feed_returns_detected() {
        let s = ScreenState::new(5, 40, 100);
        let future = Instant::now() + STALLED_RESET_TIMEOUT + Duration::from_secs(1);
        assert_eq!(check_stalled(&s, future), StalledOutcome::Detected);
    }

    /// feed が来ると stalled timer はリセットされ Healthy に戻る。
    #[test]
    fn feed_resets_timer() {
        let mut s = ScreenState::new(5, 40, 100);
        let future = Instant::now() + STALLED_RESET_TIMEOUT + Duration::from_secs(1);
        assert_eq!(check_stalled(&s, future), StalledOutcome::Detected);
        // 新 feed で last_feed_at が更新される
        s.process(b"x");
        // 直後は healthy (= 評価時点の Instant::now() を基準にする)
        assert_eq!(check_stalled(&s, Instant::now()), StalledOutcome::Healthy);
    }
}
