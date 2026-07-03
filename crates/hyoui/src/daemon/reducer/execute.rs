//! Effect executor (DR-0025 §Effect layer = reducer 出力 → 実 IO)。
//!
//! super-reducer [`super::handle`] が返す [`Effect`] 列を実 IO に変換し、各 effect の
//! 実行結果を [`DaemonMsg::EffectResult`] として返す (= caller が次 transaction の入力へ
//! feed-back する契約、DR-0025 §Effect 失敗の feedback)。reducer が pure を保つための IO
//! 境界層で、本 module だけが `send_control` / `broadcast_control` の実 socket write を触る。
//!
//! この片で配線する effect は client 送信系 ([`EffectKind::ClientReply`] /
//! [`EffectKind::ClientBroadcast`]) のみ。他 domain の effect (TtyWrite / Kill / Record /
//! SpawnChild / Timer 系) は各 domain reducer が当該 effect を発行し始める後続 Phase で
//! 本 module に配線する。未配線 effect の到達は設計違反として debug_assert で検出する。

// serve_loop (= `daemon::session`) への配線は次片で行うため、この片では execute の
// 呼び出し元がまだ無い。unit test だけが execute / outcome ヘルパを使う。配線までの間、
// pub fn を dead_code として許容する (= reducer/mod.rs の module allow と同じ扱い)。
#![allow(dead_code)]

use super::super::broadcast::{ClientHandle, broadcast_control, send_control};
use super::{DaemonMsg, Effect, EffectErrorKind, EffectKind, EffectOutcome, RetryAdvice};

/// [`Effect`] 列を実 IO に変換し、各 effect の [`DaemonMsg::EffectResult`] を返す。
///
/// `clients` は送信対象の client handle 群 (= [`EffectKind::ClientReply`] の宛先探索 /
/// [`EffectKind::ClientBroadcast`] の配信先)。返す `Vec<DaemonMsg>` は caller が
/// super-reducer [`super::handle`] に feed-back する (DR-0025 §Effect 失敗の feedback)。
///
/// send の実 IO (`send_control` / `broadcast_control`) は本関数が担い、その結果からの
/// [`EffectOutcome`] 生成は [`reply_outcome`] / [`broadcast_outcome`] の pure ヘルパに
/// 分離する (= clients 無しで outcome 生成ロジックを unit test 可能にする)。
pub(in crate::daemon) fn execute(
    effects: Vec<Effect>,
    clients: &mut [ClientHandle],
    overflow_ids: &mut Vec<u64>,
) -> Vec<DaemonMsg> {
    let mut results = Vec::with_capacity(effects.len());
    for Effect { id, kind } in effects {
        let outcome = match kind {
            EffectKind::ClientReply { client_id, message } => {
                let sent_ok = match clients.iter().find(|c| c.id == client_id.0) {
                    Some(ch) => send_control(ch, message),
                    // 宛先 client が既に集合から消えている = 送信不能。broken 扱いで
                    // feedback し、発行元 reducer が pending state を rollback できるようにする。
                    None => false,
                };
                reply_outcome(sent_ok)
            }
            EffectKind::ClientBroadcast { message, filter } => {
                // filter (All / SubscribersOnly) の実解釈は Client reducer の cap-aware
                // broadcast 実装 (Phase 2) で反映する。現状の `broadcast_control` は
                // subscribe filter を `ClientHandle.sub` から内部参照するため、ここでは
                // filter を渡さず subscribe 中の client へ配る。
                let _ = filter;
                // 送信失敗 (= backpressure overflow / writer dead) の client id は
                // caller の drop 予約 (`overflow_ids`) に積む (= serve_loop が次周回で
                // drop する既存規律の保存)。EffectResult::Failed 経由の Client domain
                // lifecycle 統合は Phase 2-γ の Backpressure sub-state で行う。
                let failed = broadcast_control(clients, &message);
                overflow_ids.extend(failed.iter().copied());
                broadcast_outcome(&failed)
            }
            // client 送信系以外の effect はこの片の execute では未配線。各 domain reducer が
            // 当該 effect を発行し始める Phase で配線する。未配線 effect の到達は「未実装
            // domain が client effect 以外を出した」設計違反なので debug_assert で検出し、
            // release build では黙って skip する (= feedback を出さない)。
            other => {
                debug_assert!(false, "execute: 未配線の EffectKind が到達した: {other:?}");
                continue;
            }
        };
        results.push(DaemonMsg::EffectResult {
            effect_id: id,
            outcome,
        });
    }
    results
}

/// [`EffectKind::ClientReply`] の送信結果 (bool) から [`EffectOutcome`] を導く。
///
/// clients を触らない pure ヘルパなので、実 socket IO 無しで outcome 生成ロジックを
/// unit test できる (= execute 本体は send の実 IO を担い、結果判定はここに集約)。
fn reply_outcome(sent_ok: bool) -> EffectOutcome {
    if sent_ok {
        EffectOutcome::Ok
    } else {
        // `send_control` の false は writer 破損 (= EPIPE / socket close) を意味する。
        // 再送しても直らないので rollback へ倒す (DR-0025 §Effect 失敗の feedback)。
        EffectOutcome::Failed {
            kind: EffectErrorKind::WriteBroken,
            retry_advice: RetryAdvice::DoNotRetry,
        }
    }
}

/// [`EffectKind::ClientBroadcast`] の送信結果 (= 失敗 client id 群) から [`EffectOutcome`] を導く。
///
/// Design rationale: broadcast は best-effort。個別 client への write 失敗は「その client を
/// drop すべき」という Client domain lifecycle の signal であって、broadcast effect 自体の
/// 失敗ではない (= mode.change は残りの client に既に届いており、発行元 reducer の state を
/// rollback する意味は無い)。よって broadcast effect は daemon 側で配信を試みた時点で成功
/// (`Ok`) とみなす。失敗 client の drop 通知経路 (= Client domain への feedback) は後続
/// Phase で配線する (= その時点で `_failed_client_ids` を drop 対象として使う)。
fn broadcast_outcome(_failed_client_ids: &[u64]) -> EffectOutcome {
    EffectOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, EffectId};
    use super::*;

    /// [`reply_outcome`]: 送信成功は `Ok`、失敗は `Failed { WriteBroken, DoNotRetry }`。
    ///
    /// なぜこの期待値か: `send_control` の false は writer 破損で、再送しても直らない
    /// (= 恒久的失敗)。発行元 reducer が pending state を rollback する判断材料になるよう、
    /// WriteBroken + DoNotRetry を返す。
    #[test]
    fn reply_outcome_maps_send_result() {
        assert!(matches!(reply_outcome(true), EffectOutcome::Ok));
        match reply_outcome(false) {
            EffectOutcome::Failed { kind, retry_advice } => {
                assert!(matches!(kind, EffectErrorKind::WriteBroken));
                assert!(matches!(retry_advice, RetryAdvice::DoNotRetry));
            }
            EffectOutcome::Ok => panic!("送信失敗は Failed になるべき"),
        }
    }

    /// [`broadcast_outcome`]: 全成功でも一部失敗でも `Ok` を返す。
    ///
    /// なぜこの期待値か: broadcast は best-effort で、個別 client の write 失敗は Client
    /// domain の drop 責務。broadcast effect 自体は配信を試みた時点で成功扱い (= 発行元
    /// state の rollback を誘発しない)。空 slice / 非空 slice の双方で Ok を固定する。
    #[test]
    fn broadcast_outcome_is_ok_regardless_of_partial_failure() {
        assert!(matches!(broadcast_outcome(&[]), EffectOutcome::Ok));
        assert!(matches!(broadcast_outcome(&[1, 2, 3]), EffectOutcome::Ok));
    }

    /// 未配線の [`EffectKind`] (= TtyWrite 等) が [`execute`] に到達したら debug build で
    /// panic する (= 設計違反の即検出)。
    ///
    /// なぜこの test か: この片の execute は client 送信系のみ配線する。未実装 domain が
    /// client effect 以外を出したら、それは「未配線 domain が effect を発行した」設計違反。
    /// debug build で fail-fast して検出する。`cargo test` は既定で `debug_assertions` on
    /// なのでこの panic 経路を通る (release 側の黙って skip は別 build profile を要するため
    /// 本 test では検証しない)。clients は空で良い (TtyWrite は client を触らない = 実 IO
    /// 無しで panic 経路のみを弁別できる)。
    #[test]
    #[should_panic(expected = "未配線の EffectKind")]
    fn execute_panics_on_unwired_effect_in_debug() {
        let mut clients: Vec<ClientHandle> = Vec::new();
        let mut overflow_ids = Vec::new();
        let _ = execute(
            vec![Effect {
                id: EffectId(Domain::Tty, 0),
                kind: EffectKind::TtyWrite { bytes: vec![b'x'] },
            }],
            &mut clients,
            &mut overflow_ids,
        );
    }
}
