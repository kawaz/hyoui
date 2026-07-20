//! `tail.request` 処理 + tail-follow cleanup helper
//! (DR-0009 Phase E 後半で `session.rs` から分離)。
//!
//! ## 構成
//!
//! - [`handle_tail_request`]: `tail.request` 受信時の scrollback snapshot 取り出し +
//!   strip_ansi 適用 + TailData 送信 + follow=true なら subscription を `TailFollow`
//!   に切替、follow=false なら即 `TailEnd(Eof)`
//! - [`tail_end_reason_from_outcome`]: `serve_loop` 終了後 cleanup 段の helper。
//!   `RelayOutcome` から TailEndReason を導出する (= ChildExited / ClientCancel /
//!   Error は None)
//! - [`broadcast_tail_end_to_followers`]: tail follower への TailEnd 一括送信
//!
//! ## session.rs / 他 module との接続
//!
//! - `handle_tail_request` は control.rs の dispatcher (`handle_tail_request_dispatch`)
//!   から呼ばれる (= cap check 後)
//! - `tail_end_reason_from_outcome` / `broadcast_tail_end_to_followers` は
//!   session.rs の `Session::serve` cleanup 段から呼ばれる
//! - TailFollow subscription への per-master-bytes broadcast 経路は
//!   broadcast.rs の `broadcast_master_bytes` 内に閉じる (= 本 module には置かない)

use std::time::Instant;

use crate::protocol::ControlMessage;
use crate::protocol::messages::{TailData, TailEnd, TailEndReason, TailRequest};
use crate::scrollback::Scrollback;

use super::broadcast::{ClientHandle, Subscription, instant_to_epoch_ms, send_control};
use super::session::RelayOutcome;

/// `tail.request` を処理する (Phase 11)。
///
/// 流れ:
/// 1. since_ms / last_bytes でフィルタした bytes を scrollback から取り出す
/// 2. strip_ansi が true なら ANSI escape を strip (per-snapshot で 1 回だけ)
/// 3. 取り出した bytes を 1 個の `TailData` として送信 (= chunk 境界は失う、
///    timestamp_ms = now)
/// 4. follow=false なら即 `TailEnd(Eof)`、follow=true なら subscription を
///    `TailFollow` に切り替えて以降の master 出力も `TailData` で送り続ける
///
/// since_strict=true で since 範囲が ring buffer から押し出されていれば
/// `TailEnd(BufferTruncated)` を返して subscription は変更しない。
pub(super) fn handle_tail_request(
    idx: usize,
    req: TailRequest,
    clients: &mut [ClientHandle],
    scrollback: &Scrollback,
) {
    let now = Instant::now();
    let bytes_opt: Option<Vec<u8>> = if let Some(since_ms) = req.since_ms {
        let dur = std::time::Duration::from_millis(since_ms);
        if req.since_strict {
            match scrollback.since_strict(now, dur) {
                Ok(b) => Some(b),
                Err(_) => {
                    // since 範囲が buffer から evict 済 → BufferTruncated で即終了
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::TailEnd(TailEnd {
                            reason: TailEndReason::BufferTruncated,
                        }),
                    );
                    return;
                }
            }
        } else {
            Some(scrollback.since(now, dur))
        }
    } else if let Some(last_bytes) = req.last_bytes {
        Some(scrollback.last_n_bytes(last_bytes as usize))
    } else {
        Some(scrollback.last_n_bytes(scrollback.total_bytes()))
    };

    let mut snapshot = bytes_opt.unwrap_or_default();
    if let Some(last_bytes) = req.last_bytes {
        let lb = last_bytes as usize;
        if snapshot.len() > lb {
            snapshot = snapshot[snapshot.len() - lb..].to_vec();
        }
    }
    if req.strip_ansi {
        snapshot = crate::strip::strip_ansi(&snapshot);
    }

    if !snapshot.is_empty() {
        let _ = send_control(
            &clients[idx],
            ControlMessage::TailData(TailData {
                bytes: snapshot,
                timestamp_ms: instant_to_epoch_ms(now),
            }),
        );
    }

    if req.follow {
        clients[idx].subscription = Subscription::TailFollow {
            strip_ansi: req.strip_ansi,
        };
    } else {
        let _ = send_control(
            &clients[idx],
            ControlMessage::TailEnd(TailEnd {
                reason: TailEndReason::Eof,
            }),
        );
    }
}

/// `serve_loop` 終了後 cleanup 段の helper:
/// `RelayOutcome` から tail-follower に送る `TailEndReason` を導出する。
///
/// - `ChildExited` → `ChildExited` (= 子 PTY が exit した)
/// - `ClientDetachedOrKilled` → `ClientCancel` (= client が detach/kill で session 終了)
/// - `Error(_)` → `None` (= 致命 error 時は送らない、cleanup の socket close で済ます)
pub(super) fn tail_end_reason_from_outcome(outcome: &RelayOutcome) -> Option<TailEndReason> {
    match outcome {
        RelayOutcome::ChildExited(_) => Some(TailEndReason::ChildExited),
        RelayOutcome::ClientDetachedOrKilled => Some(TailEndReason::ClientCancel),
        RelayOutcome::Error(_) => None,
        // DR-0028 Phase 1: upgrade は Session::serve 側で早期 return するため本
        // helper には到達しないが、型網羅性のため None を返す (= tail-end 送信は
        // upgrade でも不要、client 側は socket 切断で気づく)。
        RelayOutcome::UpgradeRequested => None,
    }
}

/// tail-follow subscription な client 全員に `TailEnd(reason)` を best-effort 送信。
///
/// `Session::serve` の cleanup 段で 1 度だけ呼ばれる。送信失敗 (= socket already
/// closed 等) は無視 (= cleanup で socket close するため誠実な接続終了は不要)。
pub(super) fn broadcast_tail_end_to_followers(clients: &[ClientHandle], reason: TailEndReason) {
    for ch in clients.iter() {
        if matches!(ch.subscription, Subscription::TailFollow { .. }) {
            let _ = send_control(ch, ControlMessage::TailEnd(TailEnd { reason }));
        }
    }
}
