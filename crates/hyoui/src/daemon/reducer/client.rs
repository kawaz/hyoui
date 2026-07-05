//! Client domain reducer 骨格 (DR-0025 §Client domain、Phase 1b 前半 stub)。
//!
//! client 集合 / lifecycle / leader-follower に加え、Transport (socket / framing) / Auth
//! (handshake / cap nego) / Backpressure (write idle / stale) を内部 sub-state として持つ
//! (DR-0025 §Transport / Auth / Backpressure の扱い)。現状 `Detached` のみ Lock domain への
//! disconnect dispatch を配線し、`Connected` / `FrameReceived` の本実装・detach cascade 1 本化・
//! protocol kind 1:1 mapping・Transport / Auth / Backpressure sub-state は Phase 2。
//!
//! Client reducer は他 domain と異なり [`DomainViews`] を追加で受け取る (= raw_data /
//! input 系 request の holder 認可判定で Lock state を read する、DR-0025 §read-only view の
//! `Client ──read──→ Lock`)。

use crate::protocol::Mode;
use crate::protocol::messages::{
    CODE_CLIENT_LOCK_NOT_HELD, CODE_CLIENT_RO_REJECTED, CODE_MASTER_WRITE_ERROR,
    CODE_MASTER_WRITE_PARTIAL, CODE_MASTER_WRITE_TIMEOUT, ControlMessage, ErrorCode, ErrorMessage,
    RawAck,
};

use super::super::lock::LockMsg;
use super::execute::MASTER_WRITE_IDLE_TIMEOUT_MS;
use super::{
    ClientId, DaemonMsg, Domain, DomainOutput, DomainViews, Effect, EffectId, EffectKind,
    EffectOutcome, RecordEntry, RecordInRejectedReason, RecordWriteErrorKind, TtyWriteErrorKind,
};

/// Client domain の state (DR-0025 §ClientRegistry)。
///
/// client 集合 + Transport / Auth / Backpressure sub-state は Phase 2-γ で持つ。現状は
/// raw_data 経路の pending state (= DR-0025 §state rollback / pending state 戦略) と
/// effect 採番 counter のみ。
#[derive(Debug, Default)]
pub(in crate::daemon) struct ClientRegistry {
    /// in-flight の TtyWrite (= raw_data の write 要求を発行してから EffectResult を
    /// 受けるまでの pending)。単一 thread + 同期 feedback (= execute_with_feedback が
    /// 同一呼び出し内で EffectResult を戻す) なので同時に高々 1 件。
    pending_write: Option<PendingWrite>,
    /// in-flight の RawAck (= 送信失敗時に disconnect が必要な ack)。同上で高々 1 件。
    pending_ack: Option<PendingAck>,
    /// effect 採番 counter (= EffectId(Domain::Client, n) の n、DR-0025 §Effect layer)。
    next_effect_seq: u64,
}

/// in-flight の TtyWrite。EffectResult の `seq` で相関し、bytes は record (in /
/// in-write-error) の組み立てに使う。
#[derive(Debug)]
struct PendingWrite {
    seq: u64,
    client_id: u64,
    bytes: Vec<u8>,
}

/// in-flight の RawAck のうち「送信失敗 = client 終端 (disconnect)」の扱いが必要なもの
/// (= 既存挙動: reject ack / ok ack の enqueue 失敗は DropClient。write 失敗後の err ack
/// は成否無視なので登録しない)。
#[derive(Debug)]
struct PendingAck {
    seq: u64,
    client_id: u64,
}

impl ClientRegistry {
    fn next_effect_id(&mut self) -> EffectId {
        let seq = self.next_effect_seq;
        self.next_effect_seq += 1;
        EffectId(Domain::Client, seq)
    }
}

/// Client domain の入力 event (DR-0025 §Client domain の ClientEvent、stub 抜粋)。
#[derive(Debug)]
pub(in crate::daemon) enum ClientMsg {
    /// client 接続確立。
    Connected { client_id: ClientId },
    /// client からの frame 到着 (DR-0025 §IO boundary の translate 例
    /// `DaemonMsg::Client(FrameReceived..)`)。CBOR control の kind 1:1 mapping は
    /// Phase 2-γ、raw_data は [`ClientMsg::RawDataReceived`] が担う。
    FrameReceived { client_id: ClientId },
    /// raw_data (= `TYPE_RAW_DATA` frame) の処理要求 (DR-0025 §Phase 2-β の実体化点)。
    ///
    /// `mode` は translate 時の client mode スナップショット (= ClientRegistry pure
    /// ミラー導入 (Phase 2-γ) までの中間形)。認可 (Ro reject / lock holder reject) は
    /// 本 reducer が judge し、許可なら `Effect::TtyWrite` を発行する。
    RawDataReceived {
        client_id: u64,
        mode: Mode,
        bytes: Vec<u8>,
    },
    /// client detach。
    Detached { client_id: ClientId },
}

/// Client domain reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// `Detached` は Lock domain へ [`LockMsg::ClientDisconnected`] を cross-domain dispatch
/// する (= holder なら process-bound GC で auto-release、DR-0025 §許可された cross-domain
/// 方向 `Client ──→ Lock`)。holder 判定と Released broadcast は Lock reducer 側が担い、
/// Client は「切れた client id」を渡すだけの弱結合を保つ (DR-0025 §Lock domain)。
///
/// `Connected` / `FrameReceived` は空を返す (= detach cascade 一本化 / raw_data 認可、
/// および `views` の Lock read は Phase 2)。
pub(in crate::daemon) fn reduce(
    state: &mut ClientRegistry,
    views: DomainViews<'_>,
    msg: ClientMsg,
) -> DomainOutput {
    match msg {
        ClientMsg::Detached { client_id } => {
            let mut out = DomainOutput::empty();
            out.cross.push(DaemonMsg::Lock(LockMsg::ClientDisconnected {
                client_id: client_id.0,
            }));
            out
        }
        ClientMsg::RawDataReceived {
            client_id,
            mode,
            bytes,
        } => reduce_raw_data(state, views, client_id, mode, bytes),
        ClientMsg::Connected { .. } | ClientMsg::FrameReceived { .. } => DomainOutput::empty(),
    }
}

/// raw_data の認可判定 (DR-0025 §Phase 2-β / DR-0021 / DR-0022)。
///
/// 既存挙動の写像 (= 応答順序・文言を保存):
/// - `Ro` mode → record (in-rejected: ro-client) → err ack (`client.ro-rejected`)。
///   ack の送信失敗は client 終端なので pending_ack に登録し、Failed で disconnect
/// - lock holder ≠ 自分 → record (in-rejected: lock-not-held) → err ack
///   (`client.lock-not-held`)。同上
/// - 許可 → bytes を pending_write に退避して `TtyWrite` を発行 (= write 結果は
///   [`reduce_effect_result`] が受けて record / ack / disconnect に写す)
fn reduce_raw_data(
    state: &mut ClientRegistry,
    views: DomainViews<'_>,
    client_id: u64,
    mode: Mode,
    bytes: Vec<u8>,
) -> DomainOutput {
    let mut out = DomainOutput::empty();
    // DR-0021 改訂: ack の意味論は「bytes が子の input stream に確実に到達した」。
    // Ro / lock 不一致は master fd に書かれていないので Error ack を返す
    // (= silent-drop や嘘の Ok ack にしない)。
    if matches!(mode, Mode::Ro) {
        out.effects.push(Effect {
            id: state.next_effect_id(),
            kind: EffectKind::Record {
                entry: RecordEntry::InRejected {
                    client_id,
                    client_mode: mode,
                    lock_holder_client_id: views.lock.holder(),
                    reason: RecordInRejectedReason::RoClient,
                    bytes,
                },
            },
        });
        let ack_id = state.next_effect_id();
        state.pending_ack = Some(PendingAck {
            seq: ack_id.seq(),
            client_id,
        });
        out.effects.push(Effect {
            id: ack_id,
            kind: EffectKind::ClientRawAck {
                client_id: ClientId(client_id),
                ack: RawAck::err(
                    CODE_CLIENT_RO_REJECTED,
                    "client is read-only; input rejected by daemon",
                ),
            },
        });
        return out;
    }
    if let Some(holder) = views.lock.holder()
        && holder != client_id
    {
        out.effects.push(Effect {
            id: state.next_effect_id(),
            kind: EffectKind::Record {
                entry: RecordEntry::InRejected {
                    client_id,
                    client_mode: mode,
                    lock_holder_client_id: Some(holder),
                    reason: RecordInRejectedReason::LockNotHeld,
                    bytes,
                },
            },
        });
        let ack_id = state.next_effect_id();
        state.pending_ack = Some(PendingAck {
            seq: ack_id.seq(),
            client_id,
        });
        out.effects.push(Effect {
            id: ack_id,
            kind: EffectKind::ClientRawAck {
                client_id: ClientId(client_id),
                ack: RawAck::err(
                    CODE_CLIENT_LOCK_NOT_HELD,
                    "lock is held by another client; input rejected",
                ),
            },
        });
        return out;
    }
    // 許可: bytes を pending に退避して write を依頼。record (in) は write 結果
    // (= written_len) が分かってから push する (= 既存の「written prefix を in event」)。
    let write_id = state.next_effect_id();
    debug_assert!(
        state.pending_write.is_none(),
        "同時に複数の TtyWrite が in-flight になるのは設計違反 (単一 thread + 同期 feedback)"
    );
    state.pending_write = Some(PendingWrite {
        seq: write_id.seq(),
        client_id,
        bytes: bytes.clone(),
    });
    out.effects.push(Effect {
        id: write_id,
        kind: EffectKind::TtyWrite { bytes },
    });
    out
}

/// Client domain の `EffectResult` 受口 (DR-0025 §Effect layer の pending state 戦略)。
///
/// - pending_write の TtyWrite 結果 → 既存挙動の写像: written prefix を record (in)、
///   error を record (in-write-error)、complete なら ok ack (DR-0021 完了点)、partial /
///   error なら err ack + (IdleTimeout のみ CBOR Error 併送) + disconnect。write dispatch
///   自体の失敗 (`Failed`) は ack 無しで disconnect (= 既存 `Err(_) => DropClient`)
/// - pending_ack の RawAck 送信結果 → 失敗なら disconnect (= 既存「ack enqueue 失敗は
///   client 終端」)
pub(in crate::daemon) fn reduce_effect_result(
    state: &mut ClientRegistry,
    _views: DomainViews<'_>,
    seq: u64,
    outcome: &EffectOutcome,
) -> DomainOutput {
    if state.pending_write.as_ref().is_some_and(|pw| pw.seq == seq) {
        let pw = state.pending_write.take().expect("直前に Some 確認済み");
        return finish_write(state, pw, outcome);
    }
    if state.pending_ack.as_ref().is_some_and(|pa| pa.seq == seq) {
        let pa = state.pending_ack.take().expect("直前に Some 確認済み");
        let mut out = DomainOutput::empty();
        if !matches!(outcome, EffectOutcome::Ok) {
            // ack を届けられない client は終端 (= 既存 `if !send_raw_ack { DropClient }`)。
            out.effects.push(Effect {
                id: state.next_effect_id(),
                kind: EffectKind::ClientDisconnect {
                    client_id: ClientId(pa.client_id),
                },
            });
        }
        return out;
    }
    DomainOutput::empty()
}

/// TtyWrite の結果を record / ack / disconnect の effect 列に写す (既存 raw_data arm の
/// write 後半部の写像、DR-0016 §4 / DR-0021)。
fn finish_write(
    state: &mut ClientRegistry,
    pw: PendingWrite,
    outcome: &EffectOutcome,
) -> DomainOutput {
    let mut out = DomainOutput::empty();
    let (written_len, requested_len, error) = match outcome {
        EffectOutcome::TtyWrite {
            written_len,
            requested_len,
            error,
        } => (*written_len, *requested_len, error.as_ref()),
        // write の dispatch 自体が失敗 (= 既存 `Err(_) => DropClient`、ack 無し・record 無し)。
        _ => {
            out.effects.push(Effect {
                id: state.next_effect_id(),
                kind: EffectKind::ClientDisconnect {
                    client_id: ClientId(pw.client_id),
                },
            });
            return out;
        }
    };
    // DR-0016 §4 hook: written prefix を `in` event、partial / error は `in-write-error`
    // event に分けて push する (= 「成功した bytes」と「失敗の root cause」を別 record
    // event として正本化、順序も既存どおり in → in-write-error)。
    if written_len > 0 {
        out.effects.push(Effect {
            id: state.next_effect_id(),
            kind: EffectKind::Record {
                entry: RecordEntry::BytesIn {
                    client_id: pw.client_id,
                    bytes: pw.bytes[..written_len].to_vec(),
                },
            },
        });
    }
    if let Some(err) = error {
        let kind = match err {
            TtyWriteErrorKind::IdleTimeout => RecordWriteErrorKind::IdleTimeout,
            TtyWriteErrorKind::Io(msg) => RecordWriteErrorKind::IoError(msg.clone()),
        };
        out.effects.push(Effect {
            id: state.next_effect_id(),
            kind: EffectKind::Record {
                entry: RecordEntry::InWriteError {
                    client_id: pw.client_id,
                    requested_len,
                    written_len,
                    error: kind,
                    unwritten_bytes: pw.bytes[written_len..].to_vec(),
                },
            },
        });
    }
    let is_complete = error.is_none() && written_len == requested_len;
    if is_complete {
        // DR-0021: bytes 系 spec の完了点として ack を返す。ack enqueue 失敗 (= overflow /
        // writer dead) は当該 client の終端なので pending_ack で失敗検知して disconnect。
        let ack_id = state.next_effect_id();
        state.pending_ack = Some(PendingAck {
            seq: ack_id.seq(),
            client_id: pw.client_id,
        });
        out.effects.push(Effect {
            id: ack_id,
            kind: EffectKind::ClientRawAck {
                client_id: ClientId(pw.client_id),
                ack: RawAck::ok(),
            },
        });
        return out;
    }
    // partial / failure。DR-0021: client が ack 待ちで hang しないよう、disconnect する
    // 前に失敗 ack を必ず送る (= client は CLI exit 1 で abort できる)。送信成否は
    // 問わない (= どうせ disconnect するので pending_ack に登録しない、既存の
    // `let _ = send_raw_ack(..)` と同じ)。
    let (ack_code, ack_msg) = match error {
        Some(TtyWriteErrorKind::IdleTimeout) => (
            CODE_MASTER_WRITE_TIMEOUT,
            format!(
                "master PTY write made no forward progress for {MASTER_WRITE_IDLE_TIMEOUT_MS} ms \
                (child is a slow reader); disconnecting client (written={written_len}/{requested_len})"
            ),
        ),
        Some(TtyWriteErrorKind::Io(errno)) => (
            CODE_MASTER_WRITE_ERROR,
            format!(
                "master PTY write failed with I/O error {errno} \
                 (written={written_len}/{requested_len})"
            ),
        ),
        None => (
            CODE_MASTER_WRITE_PARTIAL,
            format!(
                "master PTY write returned partial without error (written={written_len}/{requested_len})"
            ),
        ),
    };
    out.effects.push(Effect {
        id: state.next_effect_id(),
        kind: EffectKind::ClientRawAck {
            client_id: ClientId(pw.client_id),
            ack: RawAck::err(ack_code, ack_msg),
        },
    });
    // 既存の CBOR Error message も IdleTimeout 経路では送る (= 旧 client / 観測者にも
    // 理由が伝わるよう retain。新 client は RAW_ACK で検出済なので無視される)。
    if matches!(error, Some(TtyWriteErrorKind::IdleTimeout)) {
        out.effects.push(Effect {
            id: state.next_effect_id(),
            kind: EffectKind::ClientReply {
                client_id: ClientId(pw.client_id),
                message: ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::MasterWriteTimeout,
                    message: format!(
                        "master PTY write made no forward progress for {MASTER_WRITE_IDLE_TIMEOUT_MS} ms \
                        (child is a slow reader); disconnecting client (written={written_len}/{requested_len})"
                    ),
                    details: None,
                }),
            },
        });
    }
    out.effects.push(Effect {
        id: state.next_effect_id(),
        kind: EffectKind::ClientDisconnect {
            client_id: ClientId(pw.client_id),
        },
    });
    out
}

#[cfg(test)]
mod tests {
    use super::super::{ClientId, DaemonMsg, DaemonState, DomainViews, EffectKind, handle};
    use super::*;
    use crate::daemon::lock::{LockMsg, LockState, reduce as lock_reduce};
    use crate::protocol::messages::ControlMessage;

    /// `Detached` は Lock domain へ [`LockMsg::ClientDisconnected`] を cross に 1 件積み、
    /// effect 自体は出さない (= DR-0025 §許可された cross-domain 方向 `Client ──→ Lock`)。
    ///
    /// なぜこの期待値か: Client reducer は holder 判定を持たず (= 弱結合)、「切れた client
    /// id」を Lock reducer へ渡すだけ。holder GC / Released broadcast は Lock 側の責務。
    /// dispatch は cross-domain queue 経由なので、この単体では cross に Lock msg が積まれる
    /// ことだけを固定する (holder GC の実効は下の handle 統合 test で検証)。
    #[test]
    fn detached_dispatches_client_disconnected_to_lock() {
        let mut reg = ClientRegistry::default();
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::Detached {
                client_id: ClientId(7),
            },
        );
        assert!(out.effects.is_empty(), "Detached 自体は effect を出さない");
        assert_eq!(out.cross.len(), 1, "Lock への cross dispatch が 1 件");
        match &out.cross[0] {
            DaemonMsg::Lock(LockMsg::ClientDisconnected { client_id }) => {
                assert_eq!(*client_id, 7, "切断された client id が Lock へ渡る");
            }
            other => panic!("Lock(ClientDisconnected) を期待: {other:?}"),
        }
    }

    /// handle 統合: holder client の `Detached` → Lock の process-bound GC が Released を
    /// 出し → `ClientBroadcast(ModeChange)` Effect が 1 件出る (PTY / socket 不要、
    /// `DaemonState` だけで検証)。
    ///
    /// なぜこの test か: Client ──→ Lock の cross-domain dispatch と Lock ──→ Client の
    /// Effect 翻訳が super-reducer 上で連結することを end-to-end で固定する。holder=7 を
    /// lock reduce で直接セットアップし、client 7 の detach で mode.change broadcast が
    /// 1 件生成されることを確認する (= lock GC が発火した証跡)。
    #[test]
    fn handle_holder_detach_emits_mode_change_broadcast() {
        let mut state = DaemonState::default();
        // holder=7 をセットアップ (lock reduce を直接使い state を作る、token 供給で grant)。
        let _ = lock_reduce(
            &mut state.lock,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("t".into()),
            },
        );
        let effects = handle(
            &mut state,
            DaemonMsg::Client(ClientMsg::Detached {
                client_id: ClientId(7),
            }),
        );
        assert_eq!(
            effects.len(),
            1,
            "holder 切断で mode.change broadcast が 1 件"
        );
        match &effects[0].kind {
            EffectKind::ClientBroadcast { message, .. } => {
                assert!(
                    matches!(message, ControlMessage::ModeChange(_)),
                    "ClientBroadcast の payload は ModeChange"
                );
            }
            other => panic!("ClientBroadcast を期待: {other:?}"),
        }
    }

    /// handle 統合 (対極): 非 holder の `Detached` は Lock GC が no-op なので Effect が
    /// 出ない (= holder=1 の lock は維持され mode.change を発火しない)。
    ///
    /// なぜこの test か: Client ──→ Lock dispatch が常に broadcast を生むわけではないこと
    /// (= 弱結合で「切れた id」を渡すだけで、broadcast の要否は Lock reducer の holder 照合
    /// 次第) を固定する。holder GC の false-positive で無関係な mode.change が飛ばないこと
    /// の回帰基準。
    #[test]
    fn handle_non_holder_detach_emits_no_effect() {
        let mut state = DaemonState::default();
        let _ = lock_reduce(
            &mut state.lock,
            LockMsg::Acquire {
                client_id: 1,
                token: Some("t".into()),
            },
        );
        let effects = handle(
            &mut state,
            DaemonMsg::Client(ClientMsg::Detached {
                client_id: ClientId(2),
            }),
        );
        assert!(
            effects.is_empty(),
            "非 holder 切断は Lock GC が no-op で mode.change を出さない"
        );
    }

    // ==== raw_data 経路 (DR-0025 §Phase 2-β) ====

    /// Ro client の raw_data は「record (in-rejected: ro-client) → err ack
    /// (client.ro-rejected)」の 2 effect をこの順で出し、TtyWrite を出さない。
    ///
    /// なぜこの期待値か: record は state 変化 (= reject 判定) の直後・応答 IO より前に
    /// push する規律 (DR-0016 §3、観測順序を判定と一致させる) の保存。ack が Error なのは
    /// DR-0021 改訂「master fd に書かれていない bytes に Ok ack を返さない (= 嘘応答禁止)」
    /// による。lock_holder は未 lock なので None が record に載る。
    #[test]
    fn raw_data_ro_client_rejected_record_then_err_ack() {
        let mut reg = ClientRegistry::default();
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 7,
                mode: crate::protocol::Mode::Ro,
                bytes: b"hi".to_vec(),
            },
        );
        assert_eq!(out.effects.len(), 2, "record + err ack の 2 effect");
        assert!(
            matches!(
                &out.effects[0].kind,
                EffectKind::Record {
                    entry: RecordEntry::InRejected {
                        client_id: 7,
                        reason: RecordInRejectedReason::RoClient,
                        lock_holder_client_id: None,
                        ..
                    }
                }
            ),
            "先頭は in-rejected (ro-client) record: {:?}",
            out.effects[0].kind
        );
        match &out.effects[1].kind {
            EffectKind::ClientRawAck { client_id, ack } => {
                assert_eq!(client_id.0, 7);
                assert_eq!(ack.code.as_deref(), Some("client.ro-rejected"));
            }
            other => panic!("err ack を期待: {other:?}"),
        }
    }

    /// lock holder 以外の rw client の raw_data は「record (in-rejected: lock-not-held、
    /// holder id 付き) → err ack (client.lock-not-held)」を出し、TtyWrite を出さない。
    ///
    /// なぜこの期待値か: DR-0022 の lock gate (= holder のみ raw_data を master fd に
    /// 書ける)。holder 判定は DomainViews 経由の read (DR-0025 §read-only view の
    /// `Client ──read──→ Lock`) で行われることの実配線検証でもある。
    #[test]
    fn raw_data_non_holder_rejected_with_lock_not_held() {
        let mut reg = ClientRegistry::default();
        let mut lock = LockState::default();
        let _ = lock_reduce(
            &mut lock,
            LockMsg::Acquire {
                client_id: 1,
                token: Some("t".into()),
            },
        );
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 2,
                mode: crate::protocol::Mode::Rw,
                bytes: b"hi".to_vec(),
            },
        );
        assert_eq!(out.effects.len(), 2);
        assert!(matches!(
            &out.effects[0].kind,
            EffectKind::Record {
                entry: RecordEntry::InRejected {
                    client_id: 2,
                    reason: RecordInRejectedReason::LockNotHeld,
                    lock_holder_client_id: Some(1),
                    ..
                }
            }
        ));
        match &out.effects[1].kind {
            EffectKind::ClientRawAck { ack, .. } => {
                assert_eq!(ack.code.as_deref(), Some("client.lock-not-held"));
            }
            other => panic!("err ack を期待: {other:?}"),
        }
    }

    /// lock holder 自身の raw_data は認可され、TtyWrite 1 effect (Domain::Client 採番) が
    /// 出て bytes が pending_write に退避される。
    ///
    /// なぜこの期待値か: DR-0022 の「holder のみ受理」の受理側。record (in) は
    /// written_len が判明する write 結果 (EffectResult) 後に出すため、この時点では
    /// TtyWrite のみ (= 既存の「written prefix を in event」規律の保存)。
    #[test]
    fn raw_data_holder_passes_authorization_and_emits_tty_write() {
        let mut reg = ClientRegistry::default();
        let mut lock = LockState::default();
        let _ = lock_reduce(
            &mut lock,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("t".into()),
            },
        );
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 7,
                mode: crate::protocol::Mode::Rw,
                bytes: b"abc".to_vec(),
            },
        );
        assert_eq!(
            out.effects.len(),
            1,
            "TtyWrite のみ (record は write 結果後)"
        );
        assert_eq!(out.effects[0].id.domain(), Domain::Client);
        assert!(matches!(
            &out.effects[0].kind,
            EffectKind::TtyWrite { bytes } if bytes == b"abc"
        ));
    }

    /// write 完全成功 (complete) の EffectResult は「record (in: written prefix 全体) →
    /// ok ack」の 2 effect をこの順で出す (= DR-0021 の完了点 = master fd write の return)。
    ///
    /// なぜこの期待値か: 既存 raw arm の complete 経路の写像。ok ack の送信失敗は client
    /// 終端なので pending_ack に登録され、続く Failed で disconnect が出ること (下の
    /// ack 失敗 test) とセットで DR-0021 の ack 意味論を固定する。
    #[test]
    fn write_complete_emits_bytes_in_record_then_ok_ack() {
        let mut reg = ClientRegistry::default();
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 7,
                mode: crate::protocol::Mode::Rw,
                bytes: b"abc".to_vec(),
            },
        );
        let write_seq = out.effects[0].id.seq();
        let out2 = reduce_effect_result(
            &mut reg,
            DomainViews { lock: &lock },
            write_seq,
            &EffectOutcome::TtyWrite {
                written_len: 3,
                requested_len: 3,
                error: None,
            },
        );
        assert_eq!(out2.effects.len(), 2, "record (in) + ok ack");
        assert!(matches!(
            &out2.effects[0].kind,
            EffectKind::Record {
                entry: RecordEntry::BytesIn { client_id: 7, bytes }
            } if bytes == b"abc"
        ));
        match &out2.effects[1].kind {
            EffectKind::ClientRawAck { ack, .. } => {
                assert!(ack.code.is_none(), "ok ack は code を持たない");
            }
            other => panic!("ok ack を期待: {other:?}"),
        }
    }

    /// partial + IdleTimeout の EffectResult は「record (in: written prefix) → record
    /// (in-write-error: 残 bytes) → err ack (master.write-timeout) → CBOR Error 併送 →
    /// disconnect」の 5 effect をこの順で出す。
    ///
    /// なぜこの期待値か: 既存 raw arm の IdleTimeout 経路の写像。CBOR Error の併送は
    /// IdleTimeout のみ (= 旧 client / 観測者への理由伝達の retain)。err ack は送信成否を
    /// 問わない (= どうせ disconnect) ので pending_ack に登録されない。
    #[test]
    fn write_partial_idle_timeout_emits_records_err_ack_cbor_error_disconnect() {
        let mut reg = ClientRegistry::default();
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 7,
                mode: crate::protocol::Mode::Rw,
                bytes: b"abcde".to_vec(),
            },
        );
        let write_seq = out.effects[0].id.seq();
        let out2 = reduce_effect_result(
            &mut reg,
            DomainViews { lock: &lock },
            write_seq,
            &EffectOutcome::TtyWrite {
                written_len: 2,
                requested_len: 5,
                error: Some(TtyWriteErrorKind::IdleTimeout),
            },
        );
        assert_eq!(out2.effects.len(), 5);
        assert!(matches!(
            &out2.effects[0].kind,
            EffectKind::Record { entry: RecordEntry::BytesIn { bytes, .. } } if bytes == b"ab"
        ));
        assert!(matches!(
            &out2.effects[1].kind,
            EffectKind::Record {
                entry: RecordEntry::InWriteError {
                    requested_len: 5,
                    written_len: 2,
                    unwritten_bytes,
                    ..
                }
            } if unwritten_bytes == b"cde"
        ));
        match &out2.effects[2].kind {
            EffectKind::ClientRawAck { ack, .. } => {
                assert_eq!(ack.code.as_deref(), Some("master.write-timeout"));
            }
            other => panic!("err ack を期待: {other:?}"),
        }
        assert!(matches!(
            &out2.effects[3].kind,
            EffectKind::ClientReply {
                message: ControlMessage::Error(_),
                ..
            }
        ));
        assert!(matches!(
            &out2.effects[4].kind,
            EffectKind::ClientDisconnect { client_id } if client_id.0 == 7
        ));
    }

    /// write dispatch 自体の失敗 (EffectOutcome::Failed) は ack も record も出さず
    /// disconnect のみ (= 既存 raw arm の `Err(_) => DropClient` の写像)。
    #[test]
    fn write_dispatch_failure_disconnects_without_ack() {
        let mut reg = ClientRegistry::default();
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 7,
                mode: crate::protocol::Mode::Rw,
                bytes: b"x".to_vec(),
            },
        );
        let write_seq = out.effects[0].id.seq();
        let out2 = reduce_effect_result(
            &mut reg,
            DomainViews { lock: &lock },
            write_seq,
            &EffectOutcome::Failed {
                kind: super::super::EffectErrorKind::WriteBroken,
                retry_advice: super::super::RetryAdvice::DoNotRetry,
            },
        );
        assert_eq!(out2.effects.len(), 1);
        assert!(matches!(
            &out2.effects[0].kind,
            EffectKind::ClientDisconnect { client_id } if client_id.0 == 7
        ));
    }

    /// ok ack の送信失敗 (= ack enqueue 失敗) は client 終端として disconnect を出す
    /// (= 既存 raw arm の `if !send_raw_ack { DropClient }` の写像)。
    ///
    /// なぜこの期待値か: DR-0021 で client は ack を同期待ちするため、ack を届けられない
    /// client は残しても hang するだけ (= 終端が正)。
    #[test]
    fn ok_ack_send_failure_disconnects_client() {
        let mut reg = ClientRegistry::default();
        let lock = LockState::default();
        let out = reduce(
            &mut reg,
            DomainViews { lock: &lock },
            ClientMsg::RawDataReceived {
                client_id: 7,
                mode: crate::protocol::Mode::Rw,
                bytes: b"x".to_vec(),
            },
        );
        let write_seq = out.effects[0].id.seq();
        let out2 = reduce_effect_result(
            &mut reg,
            DomainViews { lock: &lock },
            write_seq,
            &EffectOutcome::TtyWrite {
                written_len: 1,
                requested_len: 1,
                error: None,
            },
        );
        let ack_seq = out2.effects[1].id.seq();
        let out3 = reduce_effect_result(
            &mut reg,
            DomainViews { lock: &lock },
            ack_seq,
            &EffectOutcome::Failed {
                kind: super::super::EffectErrorKind::WriteBroken,
                retry_advice: super::super::RetryAdvice::DoNotRetry,
            },
        );
        assert_eq!(out3.effects.len(), 1);
        assert!(matches!(
            &out3.effects[0].kind,
            EffectKind::ClientDisconnect { client_id } if client_id.0 == 7
        ));
    }
}
