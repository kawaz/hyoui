//! Effect executor (DR-0025 §Effect layer = reducer 出力 → 実 IO)。
//!
//! super-reducer [`super::handle`] が返す [`Effect`] 列を実 IO に変換し、各 effect の
//! 実行結果を [`DaemonMsg::EffectResult`] として返す (= caller が次 transaction の入力へ
//! feed-back する契約、DR-0025 §Effect 失敗の feedback)。reducer が pure を保つための IO
//! 境界層で、本 module だけが `send_control` / `broadcast_control` の実 socket write を触る。
//!
//! 配線済み effect: client 送信系 ([`EffectKind::ClientReply`] / [`ClientBroadcast`] /
//! [`ClientRawAck`]([`EffectKind::ClientRawAck`]))、raw_data hot path の
//! [`EffectKind::TtyWrite`] (DR-0025 §Phase 2-β)、[`EffectKind::Record`] (raw_data 系
//! 3 種)、[`EffectKind::ClientDisconnect`] (切断予約)。未配線 (Kill / SpawnChild /
//! Timer 系 / TtyResize) は各 domain reducer が発行し始める Phase で配線し、それまでの
//! 到達は設計違反として debug_assert で検出する。

use super::super::broadcast::{ClientHandle, broadcast_control, send_control, send_raw_ack};
use super::super::record::RecordRegistry;
use super::{
    DaemonMsg, Effect, EffectErrorKind, EffectKind, EffectOutcome, RecordEntry, RetryAdvice,
    TtyWriteErrorKind,
};
use crate::sys::{FdExt, Pty, WriteError};

/// execute の実行資源 (DR-0025 §Phase 2-β の実体化点)。
///
/// - `clients`: [`EffectKind::ClientReply`] / [`ClientRawAck`] の宛先探索、
///   [`ClientBroadcast`] の配信先
/// - `overflow_ids`: 送信失敗 / [`EffectKind::ClientDisconnect`] の切断予約先
/// - `pty`: [`EffectKind::TtyWrite`] の書き込み先。持たない呼び出し元 (= linger 等) は
///   `None` (= TtyWrite が来たら設計違反として debug_assert)
/// - `record_registry`: [`EffectKind::Record`] の push 先。`None` なら同上
pub(in crate::daemon) struct ExecuteCtx<'a> {
    pub(in crate::daemon) clients: &'a mut [ClientHandle],
    pub(in crate::daemon) overflow_ids: &'a mut Vec<u64>,
    pub(in crate::daemon) pty: Option<&'a Pty>,
    pub(in crate::daemon) record_registry: Option<&'a RecordRegistry>,
}

/// [`EffectKind::TtyWrite`] の idle timeout (ms)。
///
/// 値の正本は raw_data 経路の既存挙動 (= `control.rs` が使っていた
/// `MASTER_WRITE_IDLE_TIMEOUT_MS = 500`、DR-0016 §4 / R5-C3)。execute layer への移設に
/// 伴い本 module で保持する。
pub(in crate::daemon) const MASTER_WRITE_IDLE_TIMEOUT_MS: u32 = 500;

/// [`Effect`] 列を実 IO に変換し、各 effect の [`DaemonMsg::EffectResult`] を返す。
///
/// 返す `Vec<DaemonMsg>` は caller が super-reducer [`super::handle`] に feed-back する
/// (DR-0025 §Effect 失敗の feedback)。fire-and-forget の effect ([`EffectKind::Record`] /
/// [`ClientDisconnect`]) は EffectResult を返さない。
///
/// send の実 IO は本関数が担い、その結果からの [`EffectOutcome`] 生成は
/// [`reply_outcome`] / [`broadcast_outcome`] / [`tty_write_outcome`] の pure ヘルパに
/// 分離する (= 実 IO 無しで outcome 生成ロジックを unit test 可能にする)。
pub(in crate::daemon) fn execute(effects: Vec<Effect>, ctx: &mut ExecuteCtx<'_>) -> Vec<DaemonMsg> {
    let mut results = Vec::with_capacity(effects.len());
    for Effect { id, kind } in effects {
        let outcome = match kind {
            EffectKind::ClientReply { client_id, message } => {
                let sent_ok = match ctx.clients.iter().find(|c| c.id == client_id.0) {
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
                let failed = broadcast_control(ctx.clients, &message);
                ctx.overflow_ids.extend(failed.iter().copied());
                broadcast_outcome(&failed)
            }
            EffectKind::TtyWrite { bytes } => {
                let Some(pty) = ctx.pty else {
                    debug_assert!(false, "execute: TtyWrite が来たが ctx.pty が None");
                    continue;
                };
                // DR-0016 §4 / R5-C3: master fd は NONBLOCK。EAGAIN (= 子の line
                // discipline buffer 満杯) は poll(POLLOUT) で待ち、forward progress が
                // idle timeout 続けて無いときだけ IdleTimeout を返す。結果の解釈
                // (record / ack / disconnect) は EffectOutcome::TtyWrite を受けた
                // 発行元 reducer が行う。
                match pty
                    .master_fd()
                    .write_all_with_idle_timeout(&bytes, MASTER_WRITE_IDLE_TIMEOUT_MS)
                {
                    Ok(outcome) => tty_write_outcome(
                        outcome.written_len,
                        outcome.requested_len,
                        outcome.error.as_ref(),
                    ),
                    // write の dispatch 自体の失敗 (= 既存挙動では ack 無しの即
                    // DropClient)。詳細を持たない Failed で返し、発行元 reducer が
                    // 「ack 無しで disconnect」に写す。
                    Err(_) => EffectOutcome::Failed {
                        kind: EffectErrorKind::WriteBroken,
                        retry_advice: RetryAdvice::DoNotRetry,
                    },
                }
            }
            EffectKind::ClientRawAck { client_id, ack } => {
                let sent_ok = match ctx.clients.iter().find(|c| c.id == client_id.0) {
                    Some(ch) => send_raw_ack(ch, &ack),
                    None => false,
                };
                reply_outcome(sent_ok)
            }
            EffectKind::ClientDisconnect { client_id } => {
                // 切断予約のみ (= 実 IO なし、実 drop は caller の既存 cascade)。
                // fire-and-forget で EffectResult は返さない。
                ctx.overflow_ids.push(client_id.0);
                continue;
            }
            EffectKind::Record { entry } => {
                let Some(registry) = ctx.record_registry else {
                    debug_assert!(
                        false,
                        "execute: Record が来たが ctx.record_registry が None"
                    );
                    continue;
                };
                match entry {
                    RecordEntry::BytesIn { client_id, bytes } => {
                        registry.push_bytes_in(client_id, &bytes);
                    }
                    RecordEntry::InWriteError {
                        client_id,
                        requested_len,
                        written_len,
                        error,
                        unwritten_bytes,
                    } => {
                        registry.push_in_write_error(
                            client_id,
                            requested_len,
                            written_len,
                            error,
                            &unwritten_bytes,
                        );
                    }
                    RecordEntry::InRejected {
                        client_id,
                        client_mode,
                        lock_holder_client_id,
                        reason,
                        bytes,
                    } => {
                        registry.push_in_rejected(
                            client_id,
                            client_mode,
                            lock_holder_client_id,
                            reason,
                            &bytes,
                        );
                    }
                }
                // fire-and-forget (= 既存の push 系も返り値なし)。
                continue;
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

/// [`EffectKind::TtyWrite`] の `WriteOutcome` から [`EffectOutcome::TtyWrite`] を導く。
///
/// `sys::WriteError` を pure data の [`TtyWriteErrorKind`] に写す (= reducer が sys 型に
/// 依存しないための境界変換)。実 IO を触らない pure ヘルパなので unit test 可能。
fn tty_write_outcome(
    written_len: usize,
    requested_len: usize,
    error: Option<&WriteError>,
) -> EffectOutcome {
    EffectOutcome::TtyWrite {
        written_len,
        requested_len,
        error: error.map(|e| match e {
            WriteError::IdleTimeout => TtyWriteErrorKind::IdleTimeout,
            WriteError::Io(errno) => TtyWriteErrorKind::Io(format!("{errno}")),
        }),
    }
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
            other => panic!("送信失敗は Failed になるべき: {other:?}"),
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

    /// 未配線の [`EffectKind`] (= Kill / SpawnChild / Timer 系 / TtyResize) が
    /// [`execute`] に到達したら debug build で panic する (= 設計違反の即検出)。
    ///
    /// なぜこの test か: これらの effect はまだどの reducer も発行しない。到達したら
    /// 「未実装 domain が effect を発行した」設計違反なので debug build で fail-fast
    /// する。`cargo test` は既定で `debug_assertions` on なのでこの panic 経路を通る
    /// (release 側の黙って skip は別 build profile を要するため本 test では検証しない)。
    /// Kill は実 IO 前に match arm で弾かれるため pid/signal はダミー値でよい。
    #[test]
    #[should_panic(expected = "未配線の EffectKind")]
    fn execute_panics_on_unwired_effect_in_debug() {
        let mut clients: Vec<ClientHandle> = Vec::new();
        let mut overflow_ids = Vec::new();
        let mut ctx = ExecuteCtx {
            clients: &mut clients,
            overflow_ids: &mut overflow_ids,
            pty: None,
            record_registry: None,
        };
        let _ = execute(
            vec![Effect {
                id: EffectId(super::super::Domain::Child, 0),
                kind: EffectKind::Kill {
                    pid: nix::unistd::Pid::from_raw(1),
                    signal: nix::sys::signal::Signal::SIGTERM,
                },
            }],
            &mut ctx,
        );
    }

    /// 配線済みの [`EffectKind::TtyWrite`] が pty 無しの ctx (= 後方互換 wrapper 経路等)
    /// に到達したら debug build で panic する (= 実行資源の欠落を設計違反として検出)。
    ///
    /// なぜこの test か: TtyWrite を発行しうる reducer 経路 (= raw_data) は必ず pty 付き
    /// ctx (= control.rs の raw arm) から execute される契約 (DR-0025 §Phase 2-β)。
    /// pty: None の ctx に来るのは配線ミスなので fail-fast する。
    #[test]
    #[should_panic(expected = "ctx.pty が None")]
    fn execute_panics_on_tty_write_without_pty_in_debug() {
        let mut clients: Vec<ClientHandle> = Vec::new();
        let mut overflow_ids = Vec::new();
        let mut ctx = ExecuteCtx {
            clients: &mut clients,
            overflow_ids: &mut overflow_ids,
            pty: None,
            record_registry: None,
        };
        let _ = execute(
            vec![Effect {
                id: EffectId(Domain::Tty, 0),
                kind: EffectKind::TtyWrite { bytes: vec![b'x'] },
            }],
            &mut ctx,
        );
    }
}
