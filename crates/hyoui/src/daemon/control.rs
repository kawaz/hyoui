//! Control message dispatcher と kind 別 handler 群 (DR-0009 Phase B 後半で
//! `session.rs` から分離)。
//!
//! ## 構成
//!
//! - [`handle_client_frame`]: client から受け取った 1 frame を kind 別に振り分け
//! - [`handle_control_message`]: CBOR control message の kind 別 dispatcher
//!   (R4-M2 解消: 311 行の単一 match を 36 行 dispatcher + 10 個の handler に
//!   分解、cap / mode check を helper 化)
//! - [`ClientFrameOutcome`]: frame 処理結果 (= Continue / DropClient /
//!   TerminateSession)
//! - [`FrameOrError`]: serve_loop が frame 取得を Vec に集めるための中間型
//!
//! ## cap / mode helper
//!
//! - [`ensure_cap`]: handshake で intersect された capability に該当 string が
//!   含まれているか
//! - [`ensure_rw_mode`]: `Mode::Rw` 限定 (= leader 取りうる主導 client のみ)
//! - [`ensure_not_ro`]: `Mode::Ro` 不可 (= Rw / RwNoLeader は OK)
//! - [`ensure_leader`]: leader 限定
//!
//! いずれも `Result<(), ()>` を返し、`Err(())` 時には error message を当該
//! client に送って caller が `ClientFrameOutcome::Continue` で抜ける pattern。
//!
//! ## session.rs との接続
//!
//! 本 module は `session.rs` の下記 item を経由して broadcast / state mutation
//! を行う:
//!
//! - `send_control` / `broadcast_control`: writer queue 経由の 1 client / 全 client 送信
//! - `handle_tail_request`: subscription セットアップ (= 本 module からは
//!   cap check 後に呼ぶだけ)
//! - [`super::lock`] の `generate_lock_token` / `SessionState`
//!
//! frame I/O や PTY write の I/O 部分は `session.rs::serve_loop` 側に残る (= 本
//! module は protocol-level の意思決定に集中)。

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::protocol::messages::{
    CODE_CLIENT_LOCK_NOT_HELD, CODE_CLIENT_RO_REJECTED, CODE_MASTER_WRITE_ERROR,
    CODE_MASTER_WRITE_PARTIAL, CODE_MASTER_WRITE_TIMEOUT, ClientInfo, ErrorCode, ErrorMessage,
    LockResponse, LockResult, ModeChange, RawAck, RecordInfo, RecordListResponse,
    RecordStartRequest, RecordStartResponse, RecordStopRequest, RecordStopResponse,
    ScreenBufferKind, ScreenCursorSnap, ScreenDumpRequest, ScreenDumpResponse, ScreenModeSnap,
    ScreenWindowSize, SessionMode, SnapshotComponent, StateSnapshotRequest, StateSnapshotResponse,
    StatusResponse, TailRequest,
};
use crate::protocol::{ControlMessage, Frame, Mode, TYPE_CBOR_CONTROL, TYPE_RAW_DATA};
use crate::scrollback::Scrollback;
use crate::sys::clock::now_unix_ms;
use crate::sys::{FdExt, Pty, WriteError};

use super::broadcast::{ClientHandle, broadcast_control, send_control, send_raw_ack};
use super::lock::{LockEvent, LockMsg, SessionState, generate_lock_token};
use super::record::{
    InRejectedReason, LifecycleEvent, RecordStartError, RecordStopError, SessionInfo,
    WriteErrorKind,
};
use super::screen::{
    ScreenDumpFormat as InternalDumpFormat, ScreenDumpLayer as InternalDumpLayer, ScreenState,
    build_screen_dump, build_screen_snapshot,
};
use super::session::RelayOutcome;
use super::tail::handle_tail_request;
use super::{ChildSuspendPolicy, DaemonConfig};

// === Tunables ===

/// client → master PTY への raw_data write が、子の slow-reader (= line
/// discipline buffer 満杯) のために forward progress を立てられない時の
/// **per-chunk idle timeout** (ms)。
///
/// 値の根拠 (R5-C3): Linux の N_TTY line discipline buffer は典型 4–8 KiB
/// で、子が `read(2)` を 1 回呼べばその分すぐ空く。**500 ms** は「子が完全に
/// 読まなくなった (= SIGSTOP 中 / dead / 永久 loop) のを検出するに十分」と
/// 同時に「子が真に slow だが進行中の場合に間違って disconnect しない」
/// 距離。R4 で writer_pump 側 backpressure (8 MiB queue) との非対称が
/// 1000× だった指摘 (R5-KER-C1) に対し、ここで上限を持たせる。
///
/// 注意: timeout は **forward progress が無い時間** であり、絶対 deadline
/// ではない。子が steady に読んでいれば MB 級 paste も完走する。
const MASTER_WRITE_IDLE_TIMEOUT_MS: u32 = 500;

// === Frame 処理 outcome ===

/// 1 client から受け取った frame の処理結果。
pub(super) enum ClientFrameOutcome {
    /// 通常処理完了、loop 継続。
    Continue,
    /// この client は detach / protocol error → list から remove。
    DropClient,
    /// 指定した複数 client (= clients 内 index) を remove する (DR-0020 §4)。
    /// `detach --target=others/all` で「自分以外」「全員」を引き剥がす用途。
    /// serve_loop が `indices` を `indices_to_drop` に展開し、leader cascade /
    /// lock auto-release を既存経路で処理する (= 単一 `DropClient` と同じ後段
    /// ロジックに乗る)。
    DropClients {
        /// drop する client の `clients` 内 index 集合。
        indices: Vec<usize>,
        /// in-flight handshake (= pending) もキャンセルするか (codex review
        /// 2026-06-12)。others/all の意味は「この時点で session に attach
        /// しよう/している接続を引き剥がす」なので、成立しかけの pending を
        /// 生き残らせると race 下で意味が崩れる。serve_loop が true を見たら
        /// `pending_handshakes` を clear する (= entry drop で worker 側 socket
        /// が close、timeout 強制 drop と同じ経路に一本化)。
        cancel_pending: bool,
    },
    /// session 全体終了 (= kill received など)。
    TerminateSession(RelayOutcome),
}

/// `frames_to_process` 用の中間型 (= frame 取得成功 / 失敗を持ち回る)。
pub(super) enum FrameOrError {
    Frame(Frame),
    Error,
}

// === Frame dispatcher (raw_data vs cbor_control) ===

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_client_frame(
    pty: &Pty,
    child: Pid,
    idx: usize,
    frame: Frame,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
    scrollback: &Scrollback,
    screen_state: &mut ScreenState,
    config: &DaemonConfig,
) -> ClientFrameOutcome {
    match frame.ty {
        TYPE_RAW_DATA => {
            let ch_id = clients[idx].id;
            let ch_mode = clients[idx].mode;
            // 書き込み authorization:
            // - Ro mode は書けない (= reject)
            // - lock 中は lock holder のみ書ける (= 他 rw も reject)
            // DR-0016 §3: reject 経路は `in-rejected` lifecycle event として記録する。
            //
            // DR-0021 改訂: ack の意味論を「bytes が子の input stream に確実に到達した」に
            // 統一する。Ro / lock 不一致は master fd に書かれていないので Error ack
            // (`client.ro-rejected` / `client.lock-not-held`) を返し、client は CLI exit 1
            // で abort する。旧実装は silent-drop 互換のため `Ok` を返していたが、
            // ack:Ok = 嘘応答になっていたため改訂 (= v1.0 前の breaking、許容方針)。
            if matches!(ch_mode, Mode::Ro) {
                state.record_registry.push_in_rejected(
                    ch_id,
                    ch_mode,
                    state.lock.holder(),
                    InRejectedReason::RoClient,
                    &frame.body,
                );
                let ack = RawAck::err(
                    CODE_CLIENT_RO_REJECTED,
                    "client is read-only; input rejected by daemon",
                );
                if !send_raw_ack(&clients[idx], &ack) {
                    return ClientFrameOutcome::DropClient;
                }
                return ClientFrameOutcome::Continue;
            }
            if let Some(holder) = state.lock.holder()
                && holder != ch_id
            {
                state.record_registry.push_in_rejected(
                    ch_id,
                    ch_mode,
                    Some(holder),
                    InRejectedReason::LockNotHeld,
                    &frame.body,
                );
                let ack = RawAck::err(
                    CODE_CLIENT_LOCK_NOT_HELD,
                    "lock is held by another client; input rejected",
                );
                if !send_raw_ack(&clients[idx], &ack) {
                    return ClientFrameOutcome::DropClient;
                }
                return ClientFrameOutcome::Continue;
            }
            // R5-C3: master fd は NONBLOCK なので `write_all` だと EAGAIN
            // (= 子の line discipline buffer 4–8 KiB が満杯の瞬間) を即
            // disconnect 扱いし、slow-reader 経由の client DoS が成立する。
            // `write_all_with_idle_timeout` は EAGAIN を poll(POLLOUT) で
            // 待ち、forward progress が `MASTER_WRITE_IDLE_TIMEOUT_MS` 続け
            // ないときだけ idle-timeout を返す (DR-0016 §4)。
            //
            // DR-0016 §4: 戻り値が `Result<WriteOutcome, _>` に変更された。
            // partial write 時の written byte 数 + error kind を両方保持し、
            // record sink には `bytes[0..written_len]` を `in` event、残りを
            // `in-write-error` event として push する。
            match pty
                .master_fd()
                .write_all_with_idle_timeout(&frame.body, MASTER_WRITE_IDLE_TIMEOUT_MS)
            {
                Ok(outcome) => {
                    // DR-0016 §4 hook: written prefix を `in` event、partial / error は
                    // `in-write-error` event に分けて push する。complete / partial を
                    // 区別する前に push することで「成功した bytes」と「失敗の root cause」
                    // を別 record event として正本化する。
                    if outcome.written_len > 0 {
                        state
                            .record_registry
                            .push_bytes_in(ch_id, &frame.body[..outcome.written_len]);
                    }
                    if let Some(err) = outcome.error.as_ref() {
                        let kind = match err {
                            WriteError::IdleTimeout => WriteErrorKind::IdleTimeout,
                            WriteError::Io(errno) => WriteErrorKind::IoError(format!("{errno}")),
                        };
                        state.record_registry.push_in_write_error(
                            ch_id,
                            outcome.requested_len,
                            outcome.written_len,
                            kind,
                            &frame.body[outcome.written_len..],
                        );
                    }
                    if outcome.is_complete() {
                        // DR-0021: bytes 系 spec の完了点として ack を返す。
                        // ack enqueue 失敗 (= overflow / writer dead) は当該 client の
                        // 終端なので drop する (既存 backpressure cascade に合流)。
                        if !send_raw_ack(&clients[idx], &RawAck::ok()) {
                            return ClientFrameOutcome::DropClient;
                        }
                        ClientFrameOutcome::Continue
                    } else {
                        // partial / failure (= written_len < requested_len、または error)。
                        // DR-0021: client が ack 待ちで hang しないよう、disconnect する前に
                        // 失敗 ack を必ず送る (= client は CLI exit 1 で abort できる)。
                        let (ack_code, ack_msg) = match outcome.error.as_ref() {
                            Some(WriteError::IdleTimeout) => (
                                CODE_MASTER_WRITE_TIMEOUT,
                                format!(
                                    "master PTY write made no forward progress for {MASTER_WRITE_IDLE_TIMEOUT_MS} ms \
                                    (child is a slow reader); disconnecting client (written={}/{})",
                                    outcome.written_len, outcome.requested_len
                                ),
                            ),
                            Some(WriteError::Io(errno)) => (
                                CODE_MASTER_WRITE_ERROR,
                                format!(
                                    "master PTY write failed with I/O error {errno} \
                                     (written={}/{})",
                                    outcome.written_len, outcome.requested_len
                                ),
                            ),
                            None => (
                                CODE_MASTER_WRITE_PARTIAL,
                                format!(
                                    "master PTY write returned partial without error (written={}/{})",
                                    outcome.written_len, outcome.requested_len
                                ),
                            ),
                        };
                        let _ = send_raw_ack(&clients[idx], &RawAck::err(ack_code, ack_msg));
                        // 既存の CBOR Error message も IdleTimeout 経路では送る (= 旧 client
                        // / 観測者にも理由が伝わるよう retain)。新 client は RAW_ACK で
                        // 検出済なので無視される (= 後続 frame の skip 経路で吸収)。
                        if matches!(outcome.error, Some(WriteError::IdleTimeout)) {
                            let _ = send_control(
                                &clients[idx],
                                ControlMessage::Error(ErrorMessage {
                                    code: ErrorCode::MasterWriteTimeout,
                                    message: format!(
                                        "master PTY write made no forward progress for {MASTER_WRITE_IDLE_TIMEOUT_MS} ms \
                                        (child is a slow reader); disconnecting client (written={}/{})",
                                        outcome.written_len, outcome.requested_len
                                    ),
                                    details: None,
                                }),
                            );
                        }
                        ClientFrameOutcome::DropClient
                    }
                }
                Err(_) => ClientFrameOutcome::DropClient,
            }
        }
        TYPE_CBOR_CONTROL => {
            let msg = match ControlMessage::decode_from(frame.body.as_slice()) {
                Ok(m) => m,
                Err(_) => return ClientFrameOutcome::Continue,
            };
            handle_control_message(
                pty,
                child,
                idx,
                msg,
                clients,
                state,
                scrollback,
                screen_state,
                config,
            )
        }
        _ => ClientFrameOutcome::DropClient,
    }
}

// === CBOR control message dispatcher (R4-M2 解消) ===

/// CBOR control message を kind 別 handler に dispatch する (R4-M2 解消)。
///
/// 旧実装の 311 行単一 `match` を、kind ごとの `handle_*` 関数 +
/// 共通 cap_check / mode_check helper (= [`ensure_cap`] / [`ensure_rw_mode`] /
/// [`ensure_leader`]) に分解した。各 handler は self-contained で、引数を
/// 通じて必要な state (`SessionState`, `ClientHandle` slice, scrollback, config)
/// を受け取る。
///
/// Phase 10-11 の state 更新と broadcast を担う。
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_control_message(
    pty: &Pty,
    child: Pid,
    idx: usize,
    msg: ControlMessage,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
    scrollback: &Scrollback,
    screen_state: &mut ScreenState,
    config: &DaemonConfig,
) -> ClientFrameOutcome {
    match msg {
        ControlMessage::Detach(d) => handle_detach_target(idx, d, clients),
        ControlMessage::Kill(k) => handle_kill(child, idx, k, clients),
        ControlMessage::Signal(s) => handle_signal(child, idx, s, clients),
        ControlMessage::Resize(r) => handle_resize(pty, idx, r, clients, screen_state),
        ControlMessage::LockAcquire(req) => handle_lock_acquire(idx, req, clients, state),
        ControlMessage::LockRelease(rel) => handle_lock_release(idx, rel, clients, state),
        ControlMessage::TailRequest(req) => {
            handle_tail_request_dispatch(idx, req, clients, scrollback)
        }
        ControlMessage::StatusQuery(_) => {
            handle_status_query(child, idx, clients, state, scrollback, config)
        }
        ControlMessage::ScreenDumpRequest(req) => {
            handle_screen_dump_request(idx, req, clients, screen_state)
        }
        ControlMessage::StateSnapshotRequest(req) => {
            handle_state_snapshot_request(idx, req, clients, screen_state)
        }
        ControlMessage::SessionChildResumeRequest(_) => {
            handle_session_child_resume_request(child, idx, clients, state, screen_state)
        }
        // DR-0016 Phase 4: record handler 配線 (= Phase 1 で reject されていた variant を
        // 取り出して個別 handler に振る)。
        ControlMessage::RecordStartRequest(req) => {
            handle_record_start_request(idx, req, clients, state, config)
        }
        ControlMessage::RecordStopRequest(req) => {
            handle_record_stop_request(idx, req, clients, state)
        }
        ControlMessage::RecordStopAllRequest(_req) => {
            handle_record_stop_all_request(idx, clients, state)
        }
        ControlMessage::RecordListRequest(_req) => handle_record_list_request(idx, clients, state),
        ControlMessage::SetRequest(req) => handle_set_request(idx, req, clients, state),
        // daemon → client 方向のはずの message が client → daemon に来た or 未実装 kind。
        // DR-0008 §3.2 「未知 kind は decode error」だが、ここに来るのは serde で既知
        // variant なので decode 段階では catch されない。protocol violation として
        // 明示 error を返す (= silent skip しない)。
        ControlMessage::HandshakeRequest(_)
        | ControlMessage::HandshakeResponse(_)
        | ControlMessage::Error(_)
        | ControlMessage::KillAck(_)
        | ControlMessage::DetachAck(_)
        | ControlMessage::LockResponse(_)
        | ControlMessage::LeaderNotify(_)
        | ControlMessage::ModeChange(_)
        | ControlMessage::StatusResponse(_)
        | ControlMessage::TailData(_)
        | ControlMessage::TailEnd(_)
        | ControlMessage::ScreenDumpResponse(_)
        | ControlMessage::StateSnapshotResponse(_)
        | ControlMessage::SessionExitNotify(_)
        | ControlMessage::SessionChildStoppedNotify(_)
        | ControlMessage::RecordStartResponse(_)
        | ControlMessage::RecordStopResponse(_)
        | ControlMessage::RecordListResponse(_)
        | ControlMessage::SetAck(_) => reject_unexpected_kind(idx, clients),
    }
}

/// `session.child.resume.request` (DR-0015 §2.2、cap `child-state-v1`)。
///
/// leader が follow / auto-resume 政策の延長で「子を SIGCONT で起こせ」と daemon に
/// 要求する経路。daemon は `killpg(child_pgid, SIGCONT)` で子 pgrp 全体に SIGCONT。
///
/// suspend/resume の外側端末状態管理 (= issue 2026-06-11): SIGCONT で子を起こす
/// **前に** 要求元 client へ attach redraw bytes (`build_attach_redraw` = DR-0013
/// Phase A と同一機構) を送る。これにより client は子が出力を再開する前に
/// 画面・端末モード (alt screen / cursor 可視 / bracketed paste off 等) を
/// screen state から復元できる。順序を「redraw → SIGCONT」とするのは、子復帰
/// 直後の出力が redraw bytes より後に届くことを保証するため。
///
/// cap 未保持 client は `UnsupportedCapability` で reject (= leader 選定で本来弾かれる
/// はずだが defense-in-depth)。
fn handle_session_child_resume_request(
    child: Pid,
    idx: usize,
    clients: &mut [ClientHandle],
    state: &SessionState,
    screen_state: &ScreenState,
) -> ClientFrameOutcome {
    let ch = &clients[idx];
    if ensure_cap(
        ch,
        "child-state-v1",
        "session.child.resume.request requires `child-state-v1`",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    let client_id = ch.id;
    // DR-0016 §3 — 4 段階 lifecycle event の 2 段階目 (= resume-request-received)。
    state
        .record_registry
        .push_lifecycle(LifecycleEvent::ResumeRequestReceived {
            client_id,
            ts_unix_ms: now_unix_ms(),
        });
    // resume 後の画面・モード復元: SIGCONT より前に要求元 client へ attach redraw を
    // push (= handshake redraw と同じ DR-0013 Phase A 機構を再利用)。enqueue が
    // overflow / writer dead だった場合は当該 client を drop する。
    let mut overflow_ids: Vec<u64> = Vec::new();
    super::accept::send_attach_redraw(&clients[idx], screen_state, &mut overflow_ids);
    if !overflow_ids.is_empty() {
        return ClientFrameOutcome::DropClient;
    }
    // 子 pgrp に SIGCONT。DR-0001 §実装ノート「子は独立セッションリーダーなので
    // 子の pgid == 子の pid」を踏襲。
    let _ = nix::sys::signal::killpg(child, nix::sys::signal::Signal::SIGCONT);
    // DR-0016 §3 — 4 段階 lifecycle event の 3 段階目 (= sigcont-sent)。
    state
        .record_registry
        .push_lifecycle(LifecycleEvent::SigcontSent {
            pid: child.as_raw() as u32,
            ts_unix_ms: now_unix_ms(),
        });
    ClientFrameOutcome::Continue
}

// === cap / mode 共通 helper ===

/// negotiated_caps に `cap` が含まれていれば `Ok(())`、無ければ
/// `Error(UnsupportedCapability)` を送って `Err(())` を返す。
///
/// caller は `Err(())` を受けたら handler を `ClientFrameOutcome::Continue`
/// で抜ける。`message` は cap が無い時に error message として使われる。
fn ensure_cap(ch: &ClientHandle, cap: &str, message: &str) -> Result<(), ()> {
    if ch.negotiated_caps.iter().any(|c| c == cap) {
        Ok(())
    } else {
        let _ = send_control(
            ch,
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::UnsupportedCapability,
                message: message.into(),
                details: None,
            }),
        );
        Err(())
    }
}

/// client の mode が `Mode::Rw` であれば `Ok(())`、それ以外なら
/// `Error(ModeNotAllowed)` を送って `Err(())` を返す。
///
/// kill / lock.acquire など「Rw 限定」操作で使う。RwNoLeader を含めて
/// rw 系を許可したい場合は [`ensure_not_ro`] を使う。
fn ensure_rw_mode(ch: &ClientHandle, message: &str) -> Result<(), ()> {
    if matches!(ch.mode, Mode::Rw) {
        Ok(())
    } else {
        let _ = send_control(
            ch,
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::ModeNotAllowed,
                message: message.into(),
                details: None,
            }),
        );
        Err(())
    }
}

/// client の mode が `Mode::Ro` でなければ `Ok(())` (= Rw / RwNoLeader は OK)、
/// `Ro` なら `Error(ModeNotAllowed)` を送って `Err(())` を返す。
///
/// signal 送信のように「観察者 Ro は不可だが Rw 系は全部 OK」な操作で使う。
fn ensure_not_ro(ch: &ClientHandle, message: &str) -> Result<(), ()> {
    if matches!(ch.mode, Mode::Ro) {
        let _ = send_control(
            ch,
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::ModeNotAllowed,
                message: message.into(),
                details: None,
            }),
        );
        Err(())
    } else {
        Ok(())
    }
}

/// client が leader であれば `Ok(())`、そうでなければ
/// `Error(ModeNotLeader)` を送って `Err(())` を返す。
///
/// resize など「leader 限定」操作で使う。
fn ensure_leader(ch: &ClientHandle, message: &str) -> Result<(), ()> {
    if ch.leader {
        Ok(())
    } else {
        let _ = send_control(
            ch,
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::ModeNotLeader,
                message: message.into(),
                details: None,
            }),
        );
        Err(())
    }
}

// === kind 別 handler 群 ===

/// 「殺す意図」の signal を stopped な子に送る前に SIGCONT 併送すべきか。
///
/// shell の job control 慣行 (= job への TERM/HUP に CONT を併送して起こす) に倣う。
/// stopped のまま terminate 系を送ると signal が pending になって配送されない。
/// SIGCONT / SIGKILL / SIGSTOP / SIGTSTP それ自体や SIGUSR 等は対象外:
/// - SIGCONT: 併送する意味がない (= まさに起こす signal)
/// - SIGKILL: stopped でも無条件で殺せる (= 併送不要)
/// - SIGSTOP/SIGTSTP: 止める意図なので起こさない
fn signal_should_cont_first(sig: Signal) -> bool {
    matches!(
        sig,
        Signal::SIGTERM | Signal::SIGINT | Signal::SIGHUP | Signal::SIGQUIT | Signal::SIGABRT
    )
}

/// signal name string から nix `Signal` を返す (DR-0012)。
///
/// wire protocol は signal 数値ではなく **signal name** (`"SIGTERM"` / `"SIGINT"` 等)
/// を送るため、daemon 側で OS native 値に解決する。
///
/// - 受理する name は SIG-prefix 大文字 (正規表記)。`"sigterm"` / `"TERM"` / `"15"`
///   等は **reject** (= 大文字小文字緩和や省略・数値表現を入れると spec が曖昧化)
/// - daemon の OS で `nix::Signal` variant が定義されていない signal name
///   (例: Linux 上で `"SIGINFO"` = macOS 専用) は自動的に `None` → `signal.invalid`
/// - POSIX `kill(pid, 0)` semantic の existence probe (= signum=0) は wire protocol
///   ではサポートしない (必要なら別 message を新設)
pub(super) fn signal_name_to_nix_signal(name: &str) -> Option<Signal> {
    // nix::Signal の Display は `Signal::SIGTERM.as_str() -> "SIGTERM"` 形式を返す。
    // FromStr は dotted text を受け取る形ではなく、`"SIGTERM"` / `"SIGINT"` 等
    // 正規表記文字列で variant を返す。daemon の running OS で定義されている
    // signal variant のみが対象。
    name.parse::<Signal>().ok()
}

/// `detach` message の target に応じて drop 対象を決める (DR-0020 §4)。
///
/// - `Myself`: 自分 1 client のみ drop (= DropClient)
/// - `Others`: 自分以外の全 client を drop (= 自分は残る、attach 中の端末から
///   「自分以外を蹴る」用途)
/// - `All`: 全 client を drop (= 自分含む全員引き剥がし、TUI 直起動からの脱出)。
///   全 client が消えても daemon は子 PTY 接続を維持して常駐継続する
///   (= DR-0015 §2.3.1、新規 attach 待ち)
///
/// Others/All は複数 client を drop するため [`ClientFrameOutcome::DropClients`] を
/// 返す。serve_loop が `indices` を `indices_to_drop` に展開し、leader cascade /
/// lock auto-release を既存経路で処理する。`cancel_pending: true` により in-flight
/// handshake (= pending) も同時にキャンセルされる (= 成立しかけの接続が detach を
/// すり抜ける race を防ぐ、codex review 2026-06-12)。
fn handle_detach_target(
    idx: usize,
    detach: crate::protocol::messages::Detach,
    clients: &mut [ClientHandle],
) -> ClientFrameOutcome {
    use crate::protocol::messages::DetachTarget;
    // Fable review M2 (2026-06-12): Others/All は他 client を引き剥がす破壊的操作
    // なので Signal と同じ権限ゲート (= Rw / RwNoLeader 可、Ro 観察者は不可) を
    // 適用する。Myself は自分が抜けるだけなので mode 不問 (= 従来通り)。
    if !matches!(detach.target, DetachTarget::Myself)
        && ensure_not_ro(
            &clients[idx],
            "detach target=others/all requires rw mode (= read-only client cannot \
             detach other clients)",
        )
        .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    match detach.target {
        DetachTarget::Myself => ClientFrameOutcome::DropClient,
        DetachTarget::Others => {
            // 自分以外の全 client index を集める (= 自分は残す)。
            let others: Vec<usize> = (0..clients.len()).filter(|&i| i != idx).collect();
            send_detach_ack_and_flush(&clients[idx], others.len() as u64);
            ClientFrameOutcome::DropClients {
                indices: others,
                cancel_pending: true,
            }
        }
        DetachTarget::All => {
            // 自分含む全 client を drop。daemon は serve_loop を継続 (= 子 PTY 接続維持、
            // DR-0015 §2.3.1)。
            send_detach_ack_and_flush(&clients[idx], clients.len() as u64);
            ClientFrameOutcome::DropClients {
                indices: (0..clients.len()).collect(),
                cancel_pending: true,
            }
        }
    }
}

/// `detach.ack` を要求元に enqueue し、writer thread が flush し切るまで bounded wait
/// する (DR-0020 §4 / Fable Minor3)。
///
/// drop は serve_loop 後段で `ClientHandle::Drop` が `shutdown(Both)` を呼ぶため、
/// enqueue 直後に drop されると writer thread が ack を書く前に socket が閉じ、
/// client が ack を読む前に EOF を観測する race が起きる (= `SessionExitNotify` の
/// 終端 drain と同根、issue 2026-06-11)。同じ流儀の per-client bounded drain
/// (= 200ms 上限) を ack 送信直後に行い、要求元が drop 対象 (= All) でも ack が
/// 確実に socket に書かれてから drop されるようにする。
fn send_detach_ack_and_flush(ch: &ClientHandle, dropped_count: u64) {
    let _ = send_control(
        ch,
        ControlMessage::DetachAck(crate::protocol::messages::DetachAck { dropped_count }),
    );
    const ACK_DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(200);
    let deadline = std::time::Instant::now() + ACK_DRAIN_BUDGET;
    while ch.queued_bytes.load(std::sync::atomic::Ordering::Acquire) > 0
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// `ControlMessage::Kill` を処理する。
///
/// Kill は session 全体 terminate なので `Mode::Rw` (= leader 取りうる
/// 主導 client) のみ許可。`Mode::Ro` (観察者) と `Mode::RwNoLeader`
/// (入力可だが leader 取らない補助 client) は session を畳む権限なし。
/// (Round2 #6: 旧実装は `!Ro` ガードで RwNoLeader も通過していた)
fn handle_kill(
    child: Pid,
    idx: usize,
    k: crate::protocol::messages::Kill,
    clients: &mut [ClientHandle],
) -> ClientFrameOutcome {
    if ensure_rw_mode(&clients[idx], "kill requires rw mode (= leader-eligible)").is_err() {
        return ClientFrameOutcome::Continue;
    }
    // DR-0012: signal name 解釈。None なら SIGTERM (default)、未知 name は error。
    let sig = match k.signal.as_deref() {
        None => Signal::SIGTERM,
        Some(name) => match signal_name_to_nix_signal(name) {
            Some(s) => s,
            None => {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::SignalInvalid,
                        message: format!("invalid signal name: {name}"),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
        },
    };
    // stopped な子に terminate 系 signal を送ると、stopped のまま signal が
    // pending になり配送されない (= shell の job control 慣行)。SIGTERM/SIGINT/
    // SIGHUP 等の「殺す意図」の signal は SIGCONT を併送して子を起こしてから
    // 効かせる。SIGCONT / SIGKILL / SIGSTOP 自体や SIGUSR 等は併送しない。
    if signal_should_cont_first(sig) {
        let _ = kill(child, Signal::SIGCONT);
    }
    let _ = kill(child, sig);

    // === wait 軸の分岐 (= 即時応答 / 終了見届け) ===
    //
    // [default = 即時応答 (k.wait == false)]: `kill(1)` と同じ直感。signal 送信
    // 受理を `KillAck` で即 ack し、session は **畳まず serve を継続**する
    // (= `Continue`)。子が signal で死ねば既存の PTY-EOF → ChildExited 経路で
    // session は自然終了する。子が signal を catch / ignore して生き残れば
    // session はそのまま残る (= `kill(1)` で 1 発で死なない app を撃っても
    // process が残るのと同じ)。daemon を block しないため、続けて
    // `hyoui kill --signal=KILL <session>` で始末する経路も生きる。
    //
    // [wait: true (= legacy client 互換)]: ack を送らず `TerminateSession` を返し、
    // serve 後段の `finalize_child` が子 exit を見届けて socket を close する。
    // finalize は bounded escalation (= SIGCONT+SIGTERM → grace → SIGKILL) で
    // 必ず有限時間で返るため、TERM を ignore する子でも daemon は孤児化しない。
    //
    // 注: 現行 `hyoui kill --wait` はこの経路を **使わない** (= `wait: false` を
    // 送って KillAck 後に client 側 deadline で session.exit.notify / EOF を待つ)。
    // timeout 時に session を畳まず残せるのは client 駆動だからこそ。
    if k.wait {
        return ClientFrameOutcome::TerminateSession(RelayOutcome::ClientDetachedOrKilled);
    }

    // ack の signal 名は正規表記で返す (= client 表示用)。`Signal::as_str()` は
    // SIG-prefix 大文字 (= `"SIGTERM"`) を返す。
    let _ = send_control(
        &clients[idx],
        ControlMessage::KillAck(crate::protocol::messages::KillAck {
            signal: sig.as_str().to_string(),
        }),
    );
    ClientFrameOutcome::Continue
}

/// `ControlMessage::Signal` を処理する。
///
/// Ro 観察者は signal 送信不可 (= 子を SIGKILL できると Ro の前提が壊れる)。
/// Rw / RwNoLeader は raw mode 中の Ctrl-C 等を CBOR 経由でも送れる必要があるので OK。
fn handle_signal(
    child: Pid,
    idx: usize,
    s: crate::protocol::messages::Signal,
    clients: &mut [ClientHandle],
) -> ClientFrameOutcome {
    if ensure_not_ro(&clients[idx], "signal requires rw mode").is_err() {
        return ClientFrameOutcome::Continue;
    }
    // DR-0012: signal name 解釈。未知 name は error。
    let sig = match signal_name_to_nix_signal(&s.signal) {
        Some(sig) => sig,
        None => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::SignalInvalid,
                    message: format!("invalid signal name: {}", s.signal),
                    details: None,
                }),
            );
            return ClientFrameOutcome::Continue;
        }
    };
    let _ = kill(child, sig);
    ClientFrameOutcome::Continue
}

/// `ControlMessage::Resize` を処理する (DR-0008 §2.3: leader 限定)。
///
/// PTY 側の TIOCSWINSZ に加え、DR-0013 Phase B から **ScreenState 側も同期 resize**
/// する (= input log replay で primary buffer の cell grid を再構築、§7)。
fn handle_resize(
    pty: &Pty,
    idx: usize,
    r: crate::protocol::messages::Resize,
    clients: &mut [ClientHandle],
    screen_state: &mut ScreenState,
) -> ClientFrameOutcome {
    if ensure_leader(&clients[idx], "resize requires leader role").is_err() {
        return ClientFrameOutcome::Continue;
    }
    // sanitize: 0×0 や巨大値で curses 系子が壊れることがあるので clamp。
    // 上限 4096 (= 一般 terminal で見ない値) で十分、下限 1 で 0 を排除。
    const COLS_MIN: u16 = 1;
    const COLS_MAX: u16 = 4096;
    const ROWS_MIN: u16 = 1;
    const ROWS_MAX: u16 = 4096;
    let cols = r.cols.clamp(COLS_MIN, COLS_MAX);
    let rows = r.rows.clamp(ROWS_MIN, ROWS_MAX);
    let _ = pty.resize(cols, rows);
    // DR-0013 Phase B §7: ScreenState 側も同サイズに揃え、input log を新 Parser に
    // replay する (primary buffer 中は cell が再構築、alt screen 中は子側 redraw を
    // 期待して flag のみ復元)。
    screen_state.resize(rows, cols);
    ClientFrameOutcome::Continue
}

/// `ControlMessage::LockAcquire` を処理する。
///
/// - D7: lock cap が必要
/// - R4-C7: Mode::Ro は lock を取れない
/// - R4-C9: idempotency — 同じ client が既に holder ならそのまま Acquired を返す
/// - 他 client が holder なら Denied (wait queue は MVP 未実装、wait=true でも Denied)
/// - lock 取得成功時は mode.change を broadcast
fn handle_lock_acquire(
    idx: usize,
    req: crate::protocol::messages::LockAcquire,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
) -> ClientFrameOutcome {
    let ch_id = clients[idx].id;
    // D7: lock cap が無いと LockAcquire 受理しない
    if ensure_cap(&clients[idx], "lock", "lock.acquire requires `lock` cap").is_err() {
        return ClientFrameOutcome::Continue;
    }
    // R4-C7: Mode::Ro (= 観察者) は lock を取れない。
    // 旧実装は mode をチェックしておらず、Ro client が LockAcquire を送ると
    // session 全体を Locked 化して rw client の書き込みを止められる
    // session DoS が成立していた。DR-0008 §2.3 の意図 (= leader/lock は
    // rw 系のみ) に揃え、Ro は mode.not-allowed で reject する。
    if matches!(clients[idx].mode, Mode::Ro) {
        let _ = send_control(
            &clients[idx],
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::ModeNotAllowed,
                message: "lock.acquire requires rw mode (= Ro cannot hold lock)".into(),
                details: None,
            }),
        );
        return ClientFrameOutcome::Continue;
    }
    let _ = req; // wait / timeout / process_bound は wait queue 実装まで未使用 (DR-0008)

    // DR-0025 Phase 1a: lock 判定と state 遷移は lock reducer に集約する。token 生成
    // (urandom IO) は reducer の外なので、まず token なしで peek し、grant 確定
    // (= TokenRequired) のときだけ生成して再 reduce する (= 既存の「grant 経路だけ
    // generate_lock_token を呼ぶ」挙動 = urandom 失敗の影響範囲を保存する)。
    let peek = super::lock::reduce(
        &mut state.lock,
        LockMsg::Acquire {
            client_id: ch_id,
            token: None,
        },
    );
    match peek.into_iter().next() {
        // R4-C9 idempotency: 既に holder の client の再取得。発行済 token をそのまま
        // 返し、mode.change / lifecycle は発火しない (= state 変化なし、newly_acquired
        // false)。旧実装で自分の lock に自分が弾かれる footgun を防ぐ標準挙動。
        Some(LockEvent::Acquired {
            token,
            newly_acquired: false,
            ..
        }) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::LockResponse(LockResponse {
                    result: LockResult::Acquired,
                    token,
                    queue_position: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
        // 他 client 保持中: 「1 request → 1 response」契約を守るため wait=true /
        // wait=false どちらも LockResponse(Denied) 1 frame のみで応答する (Round2 #3、
        // MVP は wait queue 未実装で Queued を返せない、DR-0008)。
        Some(LockEvent::Denied { .. }) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::LockResponse(LockResponse {
                    result: LockResult::Denied,
                    token: None,
                    queue_position: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
        // free → grant 確定。token を生成して再 reduce で holder を確定させる。
        Some(LockEvent::TokenRequired { .. }) => {
            // R5-H11: 旧実装は `generate_lock_token()` 内で urandom 失敗時 `.expect()`
            // panic していたため、`panic = "abort"` 設定下では daemon 全体が abort して
            // 全 client が巻き添え切断される (= 同 UID の攻撃者が EMFILE/ENFILE を
            // 作り出せれば session DoS が成立)。Result 化して I/O 失敗時は
            // `LockResponse(Denied)` + `ErrorCode::InternalError` notify で当該 client
            // にだけ返し、session 自体は継続する。
            let token = match generate_lock_token() {
                Ok(t) => t,
                Err(e) => {
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::LockResponse(LockResponse {
                            result: LockResult::Denied,
                            token: None,
                            queue_position: None,
                        }),
                    );
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::Error(ErrorMessage {
                            code: ErrorCode::InternalError,
                            message: format!(
                                "lock token generation failed (urandom unavailable): {e}; retry later"
                            ),
                            details: None,
                        }),
                    );
                    return ClientFrameOutcome::Continue;
                }
            };
            let granted = super::lock::reduce(
                &mut state.lock,
                LockMsg::Acquire {
                    client_id: ch_id,
                    token: Some(token),
                },
            );
            match granted.into_iter().next() {
                Some(LockEvent::Acquired {
                    token,
                    newly_acquired: true,
                    ..
                }) => {
                    // DR-0016 §3: lock-acquired lifecycle event。state 遷移 (grant) の
                    // 直後で push する (= 観測順序を state 変化と一致させるため)。
                    state
                        .record_registry
                        .push_lifecycle(LifecycleEvent::LockAcquired {
                            client_id: ch_id,
                            ts_unix_ms: now_unix_ms(),
                        });
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::LockResponse(LockResponse {
                            result: LockResult::Acquired,
                            token,
                            queue_position: None,
                        }),
                    );
                    broadcast_control(
                        clients,
                        &ControlMessage::ModeChange(ModeChange {
                            session_mode: SessionMode::Locked,
                            lock_holder: Some(ch_id),
                            client_mode: None,
                        }),
                    );
                    ClientFrameOutcome::Continue
                }
                // 単一 thread なので peek と grant の間に他 client の割り込みは起きない
                // (= 到達不能)。防御的に state 非確定として Continue で抜ける。
                _ => ClientFrameOutcome::Continue,
            }
        }
        // peek は token:None なので grant (Acquired newly_acquired:true) / Released を
        // 返さない (= 到達不能)。防御的に Continue。
        _ => ClientFrameOutcome::Continue,
    }
}

/// `ControlMessage::LockRelease` を処理する。
///
/// token + holder 両方を照合してから解放し、mode.change を broadcast する。
fn handle_lock_release(
    idx: usize,
    rel: crate::protocol::messages::LockRelease,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
) -> ClientFrameOutcome {
    let ch_id = clients[idx].id;
    // DR-0025 Phase 1a: token + holder 照合と state クリアは lock reducer に集約する。
    let events = super::lock::reduce(
        &mut state.lock,
        LockMsg::Release {
            client_id: ch_id,
            token: rel.token,
        },
    );
    match events.into_iter().next() {
        Some(LockEvent::Released { .. }) => {
            // DR-0016 §3: lock-released lifecycle event。
            state
                .record_registry
                .push_lifecycle(LifecycleEvent::LockReleased {
                    client_id: ch_id,
                    ts_unix_ms: now_unix_ms(),
                });
            broadcast_control(
                clients,
                &ControlMessage::ModeChange(ModeChange {
                    session_mode: state.session_mode(),
                    lock_holder: None,
                    client_mode: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
        // Denied (holder でない or token 不一致): 現状どおり Error(LockNotHeld) を返す。
        _ => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::LockNotHeld,
                    message: "lock token mismatch or not the lock holder".into(),
                    details: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
    }
}

// === DR-0016 record handler (Phase 4 配線) ===

/// `RecordStartRequest` の daemon 側 entry (DR-0016 §7)。
///
/// `record-v1` cap を強制 + `SessionInfo` を組み立てて registry に start を委譲する。
/// 成功時は `RecordStartResponse` を、失敗時は `ErrorMessage` を当該 client に返す。
fn handle_record_start_request(
    idx: usize,
    req: RecordStartRequest,
    clients: &mut [ClientHandle],
    state: &SessionState,
    config: &DaemonConfig,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "record-v1",
        "record.start.request requires `record-v1` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    let started_by_client_id = clients[idx].id;
    // TODO(Phase 6): argv / cwd を sensitive pattern で sanitize する。本 Phase は
    // unsanitized で渡す (= jsonl header に raw 値が乗る、kawaz の認可境界内前提)。
    let session_info = SessionInfo {
        session_id: config.session_id.clone(),
        daemon_pid: std::process::id(),
        daemon_boot_id: config.daemon_boot_id.clone(),
        argv: config.cmd.clone(),
        cwd: config
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string()),
    };
    match state
        .record_registry
        .start(&req, started_by_client_id, session_info)
    {
        Ok(record_id) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::RecordStartResponse(RecordStartResponse { record_id }),
            );
        }
        Err(e) => {
            let (code, message) = record_start_error_to_protocol(&e);
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code,
                    message,
                    details: None,
                }),
            );
        }
    }
    ClientFrameOutcome::Continue
}

fn handle_record_stop_request(
    idx: usize,
    req: RecordStopRequest,
    clients: &mut [ClientHandle],
    state: &SessionState,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "record-v1",
        "record.stop.request requires `record-v1` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    match state.record_registry.stop(req.record_id) {
        Ok(()) => {
            // 成功も ACK を返す (= client は成功 / 失敗とも recv で待つ。無音にすると
            // client が永久 hang する、DR-0016 §7)。single stop の成功は stopped=1。
            let _ = send_control(
                &clients[idx],
                ControlMessage::RecordStopResponse(RecordStopResponse { stopped: 1 }),
            );
        }
        Err(RecordStopError(id)) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::RecordNotFound,
                    message: format!("record not found: id={id}"),
                    details: None,
                }),
            );
        }
    }
    ClientFrameOutcome::Continue
}

fn handle_record_stop_all_request(
    idx: usize,
    clients: &mut [ClientHandle],
    state: &SessionState,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "record-v1",
        "record.stop.all.request requires `record-v1` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    // 成功 ACK を返す (= client は recv で待つ、無音だと hang、DR-0016 §7)。
    // stopped に実際の停止数を載せて `record stop --all` で件数表示できるようにする。
    let stopped = state.record_registry.stop_all();
    let _ = send_control(
        &clients[idx],
        ControlMessage::RecordStopResponse(RecordStopResponse {
            stopped: stopped as u32,
        }),
    );
    ClientFrameOutcome::Continue
}

fn handle_record_list_request(
    idx: usize,
    clients: &mut [ClientHandle],
    state: &SessionState,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "record-v1",
        "record.list.request requires `record-v1` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    let records: Vec<RecordInfo> = state.record_registry.list();
    let _ = send_control(
        &clients[idx],
        ControlMessage::RecordListResponse(RecordListResponse { records }),
    );
    ClientFrameOutcome::Continue
}

/// `RecordStartError` を protocol error code + 説明文に写像する (DR-0016 §7)。
fn record_start_error_to_protocol(e: &RecordStartError) -> (ErrorCode, String) {
    match e {
        RecordStartError::PathNotAbsolute => (
            ErrorCode::RecordPathNotAbsolute,
            "record output path must be absolute".to_string(),
        ),
        RecordStartError::OutputAlreadyExists => (
            ErrorCode::RecordOutputAlreadyExists,
            "record output file already exists".to_string(),
        ),
        RecordStartError::OutputPermissionDenied(msg) => (
            ErrorCode::RecordOutputPermissionDenied,
            format!("record output denied: {msg}"),
        ),
        RecordStartError::UnsupportedDirectionForFormat => (
            ErrorCode::RecordUnsupportedDirectionForFormat,
            "record raw format requires single direction (stdin or stdout, not both)".to_string(),
        ),
        RecordStartError::InvalidPromptPattern(msg) => (
            ErrorCode::RecordInvalidPromptPattern,
            format!("invalid prompt pattern regex: {msg}"),
        ),
        RecordStartError::Io(e) => (ErrorCode::InternalError, format!("record io error: {e}")),
    }
}

/// `ControlMessage::TailRequest` の cap check + handler 呼び出し。
fn handle_tail_request_dispatch(
    idx: usize,
    req: TailRequest,
    clients: &mut [ClientHandle],
    scrollback: &Scrollback,
) -> ClientFrameOutcome {
    // D7: tail-v1 cap が intersect から落ちている client は reject
    if ensure_cap(
        &clients[idx],
        "tail-v1",
        "tail.request requires `tail-v1` cap, but it was not negotiated at handshake",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    handle_tail_request(idx, req, clients, scrollback);
    ClientFrameOutcome::Continue
}

/// `ControlMessage::StatusQuery` を処理する。
///
/// 子 pid の生死チェックに waitpid は **使わない** (= WNOHANG でも exit 済みの子を
/// その場で reap してしまい、serve_loop の lifecycle が exit を観測できなくなる =
/// SessionExitNotify が永遠に出ない)。`kill(pid, 0)` (= signal 送らず存在確認のみ) を
/// 使う。zombie (= exit 済み・lifecycle 未観測) は alive 扱いになるが、それで正しい
/// (= reap と exit 観測は lifecycle の責務。直後の serve_loop 周回で exit 処理される)。
fn handle_status_query(
    child: Pid,
    idx: usize,
    clients: &mut [ClientHandle],
    state: &SessionState,
    scrollback: &Scrollback,
    config: &DaemonConfig,
) -> ClientFrameOutcome {
    let child_pid = nix::sys::signal::kill(child, None)
        .is_ok()
        .then(|| child.as_raw() as u32);
    // 子が生存中のみ pgid を引く (= 子は独立 session leader なので通常 pid と一致、
    // DR-0001 §実装ノート)。getpgid 失敗 (= 既に exit 等) は None に倒す。
    let child_pgid = child_pid.and_then(|_| {
        nix::unistd::getpgid(Some(child))
            .ok()
            .map(|p| p.as_raw() as u32)
    });
    let clients_info: Vec<ClientInfo> = clients
        .iter()
        .map(|c| ClientInfo {
            client_id: c.id,
            mode: c.mode,
            leader: c.leader,
            connected_at_unix_ms: c.connected_at_unix_ms,
        })
        .collect();
    // cwd / argv は required field (= v1.0 breaking OK 方針)。daemonize 経路で
    // `DaemonConfig::cwd` を `current_dir()` で必ず埋めている (= 失敗時も `/` で
    // fallback)。test 経路で `cwd` が `None` の場合は防衛的に `/` を入れる
    // (= `/` は POSIX で必ず存在、空文字は invalid value)。
    // DR-0017 §柱2: 子が exit 済 (= child_pid None) なら stopped は意味を持たない
    // ので false。生存中のみ `SessionState` の観測フラグを反映する。
    let child_stopped = child_pid.is_some() && state.child_stopped();
    // child_state は child_pid (None=exited) + child_stopped から導出 (= 旧 2 field
    // と矛盾しない正本)。exit code は status query 経路では未取得 (= reap してない)
    // なので None。
    let child_state =
        crate::protocol::messages::ChildLiveState::from_legacy(child_pid, child_stopped);
    let resp = StatusResponse {
        session_id: config.session_id.clone(),
        child_pid,
        child_pgid,
        daemon_pid: std::process::id(),
        child_stopped,
        child_state,
        clients: clients_info,
        scrollback_bytes: scrollback.total_bytes() as u64,
        lock_holder: state.lock.holder(),
        cwd: config
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string()),
        argv: config.cmd.clone(),
        // DR-0019 Update: policy は runtime 変更可能なので SessionState の現在値を載せる
        // (= config 固定値ではない)。`hyoui set` 後は変更後の値が反映される。
        // 新 daemon は必ず Some (= None は「field を送らない旧 daemon」専用)。
        on_child_suspend: Some(match state.child_suspend_policy() {
            ChildSuspendPolicy::Notify => crate::protocol::messages::OnChildSuspendPolicy::Notify,
            ChildSuspendPolicy::AutoResume => {
                crate::protocol::messages::OnChildSuspendPolicy::AutoResume
            }
        }),
        // DR-0019 Update: daemon 自身のバイナリ version (= 人間の診断用)。
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let _ = send_control(&clients[idx], ControlMessage::StatusResponse(resp));
    ClientFrameOutcome::Continue
}

/// `ControlMessage::SetRequest` を処理する (DR-0019 Update、cap `set-v1`)。
///
/// 汎用 key=value の runtime 設定変更。Ro 以外の書き込み可能 client (= Rw /
/// RwNoLeader) なら誰でも変更可 (= leader 限定にしない)。
///
/// - cap `set-v1` 未保持 → `UnsupportedCapability`
/// - mode が Ro → `ModeNotAllowed` (= 観察者は変更不可、Rw / RwNoLeader は OK)
/// - 未知 key → `SetInvalidKey`
/// - 不正 value → `SetInvalidValue`
/// - 成功 → SessionState を更新 + lifecycle event `PolicyChanged` を push + `SetAck` を返す
///
/// 初期サポート key は `on-child-suspend` のみ (値 `notify` / `auto-resume`)。
/// 将来の runtime 設定は本 handler の match 枝を増やすだけで載る。
///
/// **反映タイミング (= happened-before)**: `SetAck` は「適用完了」を意味する。
/// serve loop は child transition (= stop 観測) を client frame より先に処理する
/// 意図的な順序のため、**ack より前に daemon が観測済みの child stop は旧 policy で
/// 処理され得る**。新 policy が効くのは ack 以降に観測される stop から。
fn handle_set_request(
    idx: usize,
    req: crate::protocol::messages::SetRequest,
    clients: &mut [ClientHandle],
    state: &SessionState,
) -> ClientFrameOutcome {
    let ch = &clients[idx];
    if ensure_cap(ch, "set-v1", "set.request requires `set-v1` cap").is_err() {
        return ClientFrameOutcome::Continue;
    }
    // Ro のみ拒否 (= Rw / RwNoLeader は OK)。set は「子への書き込み」ではなく
    // daemon 設定の変更だが、観察者 (Ro) に session の挙動を変えさせない境界は
    // signal 送信 (= ensure_not_ro) と同じ。
    if ensure_not_ro(ch, "set requires a writable mode (= rw / rw-no-leader)").is_err() {
        return ClientFrameOutcome::Continue;
    }
    let client_id = ch.id;

    // 設定 key の dispatch。現状 `on-child-suspend` のみ。
    match req.key.as_str() {
        "on-child-suspend" => {
            let Some(policy) = ChildSuspendPolicy::from_wire(&req.value) else {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::SetInvalidValue,
                        message: format!(
                            "invalid value for on-child-suspend: {} (expected notify|auto-resume)",
                            req.value
                        ),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            };
            state.set_child_suspend_policy(policy);
            // 誰がいつ何を変えたか追える lifecycle event (DR-0019 Update)。
            state
                .record_registry
                .push_lifecycle(LifecycleEvent::PolicyChanged {
                    key: "on-child-suspend".to_string(),
                    value: policy.as_str().to_string(),
                    changed_by_client_id: client_id,
                    ts_unix_ms: now_unix_ms(),
                });
            let _ = send_control(
                &clients[idx],
                ControlMessage::SetAck(crate::protocol::messages::SetAck {
                    key: "on-child-suspend".to_string(),
                    value: policy.as_str().to_string(),
                }),
            );
            ClientFrameOutcome::Continue
        }
        other => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::SetInvalidKey,
                    message: format!("unknown set key: {other}"),
                    details: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
    }
}

/// daemon → client 方向のはずの message が client → daemon に来た or 未実装 kind を
/// `Error(ProtocolUnexpectedKind)` で reject する。
fn reject_unexpected_kind(idx: usize, clients: &[ClientHandle]) -> ClientFrameOutcome {
    let _ = send_control(
        &clients[idx],
        ControlMessage::Error(ErrorMessage {
            code: ErrorCode::ProtocolUnexpectedKind,
            message: "this kind is daemon→client only or not accepted in this direction".into(),
            details: None,
        }),
    );
    ClientFrameOutcome::Continue
}

/// `ControlMessage::ScreenDumpRequest` を処理する (DR-0013 §9)。
///
/// - cap `screen-dump-v1` が必要
/// - format = json は format-not-implemented を返す (= MVP scope 外)
/// - layer = scrollback / both は config の `screen_vt100_scrollback_rows` に従って
///   `daemon/screen/snapshot.rs` の `build_screen_dump` で配線済
/// - rect は受信するが現状全画面のみ対応 (= 無視、forward-compat field)
///
/// `&mut ScreenState` を要求する理由: scrollback layer 抽出のため vt100
/// `set_scrollback` を一時的に操作する必要があり、論理的には副作用なしだが
/// API 制約で mutable 借用が必要 (= [`super::screen::snapshot::build_screen_dump`] 参照)。
fn handle_screen_dump_request(
    idx: usize,
    req: ScreenDumpRequest,
    clients: &mut [ClientHandle],
    screen_state: &mut ScreenState,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "screen-dump-v1",
        "screen.dump.request requires `screen-dump-v1` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    // 同 crate 内では `#[non_exhaustive]` が wildcard を作らないため、全 variant を
    // 列挙する (= 将来 variant 追加時に match exhaustiveness で気付く)。クレート外
    // からの version skew は protocol 層の `decode_from` 段階で reject されるため、
    // ここに「未知 variant」は到達しない。
    let format = match req.format {
        crate::protocol::messages::ScreenDumpFormat::Ansi => InternalDumpFormat::Ansi,
        crate::protocol::messages::ScreenDumpFormat::Binary => InternalDumpFormat::Binary,
        crate::protocol::messages::ScreenDumpFormat::Json => InternalDumpFormat::Json,
        crate::protocol::messages::ScreenDumpFormat::Cbor => InternalDumpFormat::Cbor,
        crate::protocol::messages::ScreenDumpFormat::TextPlain => InternalDumpFormat::TextPlain,
    };
    let layer = match req.layer {
        crate::protocol::messages::ScreenDumpLayer::Visible => InternalDumpLayer::Visible,
        crate::protocol::messages::ScreenDumpLayer::Scrollback => InternalDumpLayer::Scrollback,
        crate::protocol::messages::ScreenDumpLayer::Both => InternalDumpLayer::Both,
    };
    match build_screen_dump(screen_state, format, layer) {
        Ok(payload) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::ScreenDumpResponse(ScreenDumpResponse {
                    payload,
                    serial: req.serial,
                }),
            );
        }
        Err(super::screen::snapshot::ScreenDumpError::FormatNotImplemented(_)) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::ProtocolMalformed,
                    message: "screen.dump format not implemented in MVP (json)".into(),
                    details: None,
                }),
            );
        }
        Err(super::screen::snapshot::ScreenDumpError::EncodeFailed) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::InternalError,
                    message: "screen.dump cbor encode failed".into(),
                    details: None,
                }),
            );
        }
    }
    ClientFrameOutcome::Continue
}

/// `ControlMessage::StateSnapshotRequest` を処理する (DR-0013 §9)。
///
/// - cap `state-snapshot-v1` が必要
/// - include が空なら ProtocolMalformed
/// - 各 component を ScreenState から引いて Response を組み立てる
fn handle_state_snapshot_request(
    idx: usize,
    req: StateSnapshotRequest,
    clients: &mut [ClientHandle],
    screen_state: &ScreenState,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "state-snapshot-v1",
        "screen.snapshot.request requires `state-snapshot-v1` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    if req.include.is_empty() {
        let _ = send_control(
            &clients[idx],
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::ProtocolMalformed,
                message: "screen.snapshot.request requires at least one include component".into(),
                details: None,
            }),
        );
        return ClientFrameOutcome::Continue;
    }
    let want = |c: SnapshotComponent| req.include.contains(&c);

    let mode_snap_internal = screen_state.snapshot_mode();
    let cursor_snap_internal = screen_state.snapshot_cursor();
    let (rows, cols) = screen_state.size();

    let cells_payload = if want(SnapshotComponent::Cells) {
        let snap = build_screen_snapshot(screen_state);
        let mut buf = Vec::new();
        match ciborium::ser::into_writer(&snap, &mut buf) {
            Ok(()) => Some(buf),
            Err(_) => {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::InternalError,
                        message: "snapshot cells cbor encode failed".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
        }
    } else {
        None
    };

    let cursor_field = if want(SnapshotComponent::Cursor) {
        Some(ScreenCursorSnap {
            row: cursor_snap_internal.row,
            col: cursor_snap_internal.col,
            visible: cursor_snap_internal.visible,
        })
    } else {
        None
    };

    let mode_field = if want(SnapshotComponent::Mode) {
        Some(ScreenModeSnap {
            alternate_screen: mode_snap_internal.alternate_screen,
            application_keypad: mode_snap_internal.application_keypad,
            application_cursor: mode_snap_internal.application_cursor,
            bracketed_paste: mode_snap_internal.bracketed_paste,
            hide_cursor: mode_snap_internal.hide_cursor,
        })
    } else {
        None
    };

    let window_field = if want(SnapshotComponent::WindowSize) {
        Some(ScreenWindowSize { rows, cols })
    } else {
        None
    };

    let buffer_field = if want(SnapshotComponent::Buffer) {
        Some(if mode_snap_internal.alternate_screen {
            ScreenBufferKind::Alternate
        } else {
            ScreenBufferKind::Primary
        })
    } else {
        None
    };

    let seq_field = if want(SnapshotComponent::SequenceNo) {
        Some(screen_state.current_seqno())
    } else {
        None
    };

    // scrollback は Phase B では未実装 → include されていたら error
    if want(SnapshotComponent::Scrollback) {
        let _ = send_control(
            &clients[idx],
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::ProtocolMalformed,
                message: "snapshot component `scrollback` not implemented in MVP".into(),
                details: None,
            }),
        );
        return ClientFrameOutcome::Continue;
    }

    let resp = StateSnapshotResponse {
        cells: cells_payload,
        cursor: cursor_field,
        mode: mode_field,
        scrollback: None,
        window_size: window_field,
        buffer: buffer_field,
        sequence_no: seq_field,
        serial: req.serial,
    };
    let _ = send_control(&clients[idx], ControlMessage::StateSnapshotResponse(resp));
    ClientFrameOutcome::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::{InputSecrecy, RecordDirection, RecordFormat};

    /// SIGCONT 併送対象: 「殺す意図」signal のみ (= shell job control 慣行)。
    /// CONT/KILL/STOP/TSTP/USR 系は併送しない。
    #[test]
    fn signal_should_cont_first_targets_terminate_intent_only() {
        // 併送する (= stopped な子を起こしてから効かせる)
        for sig in [
            Signal::SIGTERM,
            Signal::SIGINT,
            Signal::SIGHUP,
            Signal::SIGQUIT,
            Signal::SIGABRT,
        ] {
            assert!(signal_should_cont_first(sig), "{sig} should cont-first");
        }
        // 併送しない
        for sig in [
            Signal::SIGCONT, // まさに起こす signal
            Signal::SIGKILL, // stopped でも無条件で殺せる
            Signal::SIGSTOP, // 止める意図
            Signal::SIGTSTP, // 止める意図
            Signal::SIGUSR1, // 殺す意図ではない
            Signal::SIGWINCH,
        ] {
            assert!(
                !signal_should_cont_first(sig),
                "{sig} should NOT cont-first"
            );
        }
    }

    /// DR-0016 §7: `RecordStartError` を protocol `ErrorCode` に写像する table。
    #[test]
    fn record_start_error_mapping_uses_dr0016_codes() {
        let cases = [
            (
                RecordStartError::PathNotAbsolute,
                ErrorCode::RecordPathNotAbsolute,
            ),
            (
                RecordStartError::OutputAlreadyExists,
                ErrorCode::RecordOutputAlreadyExists,
            ),
            (
                RecordStartError::OutputPermissionDenied("p".into()),
                ErrorCode::RecordOutputPermissionDenied,
            ),
            (
                RecordStartError::UnsupportedDirectionForFormat,
                ErrorCode::RecordUnsupportedDirectionForFormat,
            ),
            (
                RecordStartError::InvalidPromptPattern("x".into()),
                ErrorCode::RecordInvalidPromptPattern,
            ),
        ];
        for (input, expected_code) in cases {
            let (code, _msg) = record_start_error_to_protocol(&input);
            assert_eq!(code, expected_code, "input = {input:?}");
        }
        // io error → InternalError fallback (= path validation 経由でなく writer thread 等の I/O)
        let io = RecordStartError::Io(std::io::Error::other("boom"));
        let (code, _) = record_start_error_to_protocol(&io);
        assert_eq!(code, ErrorCode::InternalError);
    }

    /// DR-0016 §3: `in-rejected` event の bytes は reject 時に hex で記録される。
    /// SessionState 経由で registry を共有し、Ro client の reject path 相当を
    /// 直接 `push_in_rejected` で再現する (= TYPE_RAW_DATA arm の hook と等価)。
    #[test]
    fn in_rejected_event_records_bytes_and_reason_via_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rej.jsonl");
        let state = SessionState::default();
        let req = crate::protocol::messages::RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            input_secrecy: InputSecrecy::RedactAfterPrompt,
            prompt_pattern: None,
        };
        let session = super::super::record::SessionInfo {
            session_id: "t".into(),
            daemon_pid: 1,
            daemon_boot_id: "boot".into(),
            argv: vec!["sh".into()],
            cwd: "/".into(),
        };
        let id = state.record_registry.start(&req, 1, session).unwrap();
        state.record_registry.push_in_rejected(
            7,
            Mode::Ro,
            None,
            InRejectedReason::RoClient,
            b"\x1b[A",
        );
        state.record_registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["ev"], "in-rejected");
        assert_eq!(v["client_id"], 7);
        assert_eq!(v["client_mode"], "ro");
        assert_eq!(v["reason"], "ro-client");
        assert_eq!(v["bytes"], hex::encode(b"\x1b[A"));
    }

    /// DR-0016 §4: partial write は `in` event (= written prefix) + `in-write-error`
    /// event (= requested/written/error/unwritten) の **両方** を別 line で記録する。
    /// `WriteOutcome::written_len > 0` の場合に push_bytes_in、`error.is_some()`
    /// で push_in_write_error が両方発火する hot path の意味論を再現する。
    #[test]
    fn partial_write_records_both_in_and_in_write_error_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pw.jsonl");
        let state = SessionState::default();
        let req = crate::protocol::messages::RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            input_secrecy: InputSecrecy::RedactAfterPrompt,
            prompt_pattern: None,
        };
        let session = super::super::record::SessionInfo {
            session_id: "t".into(),
            daemon_pid: 1,
            daemon_boot_id: "boot".into(),
            argv: vec!["sh".into()],
            cwd: "/".into(),
        };
        let id = state.record_registry.start(&req, 1, session).unwrap();
        // hot path の partial write 経路と同じ shape で 2 event 発火する。
        state.record_registry.push_bytes_in(5, b"prefix-written");
        state.record_registry.push_in_write_error(
            5,
            20,
            14,
            WriteErrorKind::IdleTimeout,
            b"unwritten",
        );
        state.record_registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 body events");
        let in_evt: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(in_evt["dir"], "in");
        assert_eq!(in_evt["client_id"], 5);
        assert_eq!(in_evt["bytes"], hex::encode("prefix-written"));
        let err_evt: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(err_evt["ev"], "in-write-error");
        assert_eq!(err_evt["error"], "timeout");
        assert_eq!(err_evt["requested_len"], 20);
        assert_eq!(err_evt["written_len"], 14);
        assert_eq!(err_evt["unwritten_bytes"], hex::encode("unwritten"));
    }

    /// DR-0016 §3 4 段階目の `child-stopped-observed` は registry が記録する。
    #[test]
    fn child_stopped_observed_lifecycle_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stop.jsonl");
        let state = SessionState::default();
        let req = crate::protocol::messages::RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            input_secrecy: InputSecrecy::RedactAfterPrompt,
            prompt_pattern: None,
        };
        let session = super::super::record::SessionInfo {
            session_id: "t".into(),
            daemon_pid: 1,
            daemon_boot_id: "boot".into(),
            argv: vec!["sh".into()],
            cwd: "/".into(),
        };
        let id = state.record_registry.start(&req, 1, session).unwrap();
        // SIGTSTP は OS 依存 (Linux=20, Darwin=18) なので nix の Signal 列挙値で照合する。
        let expected_signum = nix::sys::signal::Signal::SIGTSTP as i32;
        state
            .record_registry
            .push_lifecycle(LifecycleEvent::ChildStoppedObserved {
                sig_name: "SIGTSTP".into(),
                sig_num: expected_signum,
                pid: 1234,
                ts_unix_ms: 0,
            });
        state.record_registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["ev"], "child-stopped-observed");
        assert_eq!(v["sig_name"], "SIGTSTP");
        assert_eq!(v["sig_num"], expected_signum);
        assert_eq!(v["pid"], 1234);
    }

    // ===== record stop / stop --all の ACK 回帰 (= hang bug の核心) =====

    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::Receiver;

    use super::super::broadcast::{SharedBytes, Subscription};

    /// `record-v1` cap を持つ test 用 `ClientHandle` を、writer thread 無しの素の
    /// mpsc channel 付きで組み立てる。`send_control` の enqueue 先 (= rx) を test が
    /// 直接 drain して daemon の応答 frame を覗ける。
    fn record_test_client() -> (ClientHandle, Receiver<SharedBytes>) {
        let (tx, rx) = std::sync::mpsc::channel::<SharedBytes>();
        // ClientHandle は UnixStream (reader) を要求するがダミーで足りる。writer_tx は
        // 素の mpsc で writer thread も spawn しないため、reader は Drop 以外で触られない
        // (= 両端そのまま drop して問題ない)。
        let (_a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let ch = ClientHandle {
            id: 1,
            mode: Mode::Ro,
            leader: false,
            subscription: Subscription::Raw,
            negotiated_caps: vec!["record-v1".into()],
            writer_tx: tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            buffer_limit: 1 << 20,
            writer_thread: None,
            reader: b,
            connected_at_unix_ms: 0,
        };
        (ch, rx)
    }

    // ===== DR-0020 §4: handle_detach_target の target 別 drop 対象 =====

    /// `handle_detach_target` の outcome を `(drop idx 集合, cancel_pending)` に
    /// 正規化する test helper。`DropClient` は「自分の idx 1 個 + pending 不干渉」、
    /// `DropClients` はソート済 `indices` + `cancel_pending`、それ以外は panic。
    fn detach_outcome_indices(out: ClientFrameOutcome, self_idx: usize) -> (Vec<usize>, bool) {
        match out {
            ClientFrameOutcome::DropClient => (vec![self_idx], false),
            ClientFrameOutcome::DropClients {
                mut indices,
                cancel_pending,
            } => {
                indices.sort_unstable();
                (indices, cancel_pending)
            }
            _ => panic!("detach は DropClient / DropClients を返すべき"),
        }
    }

    /// detach target 別に「drop 対象 client index 集合 + pending キャンセル」が正しいか
    /// (DR-0020 §4 + codex review 2026-06-12)。3 client・self_idx=1 (= 中央、rw) で:
    /// - Myself → 自分のみ ([1])、pending 不干渉
    /// - Others → 自分以外全部 ([0, 2]) + pending キャンセル
    /// - All    → 全部 ([0, 1, 2]) + pending キャンセル
    #[test]
    fn handle_detach_target_resolves_drop_set_per_target() {
        use crate::protocol::messages::{Detach, DetachTarget};

        let make_clients = || {
            let (c0, _r0) = record_test_client();
            // self (= idx 1) は rw (= Others/All の権限ゲートを通す、Fable M2)。
            let (mut c1, _r1) = record_test_client();
            c1.mode = Mode::Rw;
            let (c2, _r2) = record_test_client();
            vec![c0, c1, c2]
        };
        let self_idx = 1;

        let mut clients = make_clients();
        let out = handle_detach_target(
            self_idx,
            Detach {
                target: DetachTarget::Myself,
            },
            &mut clients,
        );
        assert_eq!(detach_outcome_indices(out, self_idx), (vec![1], false));

        let mut clients = make_clients();
        let out = handle_detach_target(
            self_idx,
            Detach {
                target: DetachTarget::Others,
            },
            &mut clients,
        );
        // Others は in-flight handshake もキャンセル (= 成立しかけの接続が detach を
        // すり抜ける race を防ぐ)。
        assert_eq!(detach_outcome_indices(out, self_idx), (vec![0, 2], true));

        let mut clients = make_clients();
        let out = handle_detach_target(
            self_idx,
            Detach {
                target: DetachTarget::All,
            },
            &mut clients,
        );
        assert_eq!(detach_outcome_indices(out, self_idx), (vec![0, 1, 2], true));
    }

    /// Fable review M2 regression: Ro 観察者は Others/All で他 client を蹴れない
    /// (= Signal と同じ権限ゲート)。ModeNotAllowed が返り、誰も drop されない。
    /// Myself は自分が抜けるだけなので Ro でも許容 (= 従来通り)。
    #[test]
    fn handle_detach_target_rejects_others_all_from_ro_client() {
        use crate::protocol::messages::{Detach, DetachTarget};

        for target in [DetachTarget::Others, DetachTarget::All] {
            // 全員 Ro (= record_test_client の default)。self_idx=0 が Ro で要求。
            let (c0, r0) = record_test_client();
            let (c1, _r1) = record_test_client();
            let mut clients = vec![c0, c1];
            let out = handle_detach_target(0, Detach { target }, &mut clients);
            assert!(
                matches!(out, ClientFrameOutcome::Continue),
                "ro client の {target:?} は Continue (= 誰も drop しない) になるべき"
            );
            // 要求元には ModeNotAllowed error が返る。
            match recv_control_from_queue(&r0) {
                ControlMessage::Error(e) => {
                    assert_eq!(e.code, crate::protocol::messages::ErrorCode::ModeNotAllowed);
                }
                other => panic!("expected ModeNotAllowed error, got {other:?}"),
            }
        }

        // Myself は Ro でも許容 (= 自分が抜けるだけ)。
        let (c0, _r0) = record_test_client();
        let mut clients = vec![c0];
        let out = handle_detach_target(
            0,
            Detach {
                target: DetachTarget::Myself,
            },
            &mut clients,
        );
        assert!(matches!(out, ClientFrameOutcome::DropClient));
    }

    /// queue に積まれた次の frame を decode して `ControlMessage` を返す。
    /// 何も積まれていなければ panic (= 「無音 return = hang」を test failure に変える)。
    fn recv_control_from_queue(rx: &Receiver<SharedBytes>) -> ControlMessage {
        let payload = rx
            .try_recv()
            .expect("daemon must enqueue a response (silent return = client hang)");
        let frame = Frame::decode_from(&mut payload.as_slice()).expect("decode frame");
        assert_eq!(frame.ty, TYPE_CBOR_CONTROL);
        ControlMessage::decode_from(frame.body.as_slice()).expect("decode control message")
    }

    fn start_record(state: &SessionState, path: &std::path::Path) -> u32 {
        let req = crate::protocol::messages::RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            input_secrecy: InputSecrecy::RedactAfterPrompt,
            prompt_pattern: None,
        };
        let session = super::super::record::SessionInfo {
            session_id: "t".into(),
            daemon_pid: 1,
            daemon_boot_id: "boot".into(),
            argv: vec!["sh".into()],
            cwd: "/".into(),
        };
        state.record_registry.start(&req, 1, session).unwrap()
    }

    /// 成功時に `RecordStopResponse { stopped: 1 }` を enqueue する (= 無音 return
    /// しない)。これが今回の hang bug の核心の固定。
    #[test]
    fn record_stop_request_sends_stop_response_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let state = SessionState::default();
        let id = start_record(&state, &dir.path().join("a.jsonl"));
        let (mut ch, rx) = record_test_client();
        let clients = std::slice::from_mut(&mut ch);

        handle_record_stop_request(0, RecordStopRequest { record_id: id }, clients, &state);

        match recv_control_from_queue(&rx) {
            ControlMessage::RecordStopResponse(resp) => assert_eq!(resp.stopped, 1),
            other => panic!("expected RecordStopResponse {{ stopped: 1 }}, got {other:?}"),
        }
    }

    /// stop --all は停止件数を `stopped` に載せて返す (= 2 件停止 → stopped=2)。
    #[test]
    fn record_stop_all_sends_stop_response_with_count() {
        let dir = tempfile::tempdir().unwrap();
        let state = SessionState::default();
        start_record(&state, &dir.path().join("a.jsonl"));
        start_record(&state, &dir.path().join("b.jsonl"));
        let (mut ch, rx) = record_test_client();
        let clients = std::slice::from_mut(&mut ch);

        handle_record_stop_all_request(0, clients, &state);

        match recv_control_from_queue(&rx) {
            ControlMessage::RecordStopResponse(resp) => assert_eq!(resp.stopped, 2),
            other => panic!("expected RecordStopResponse {{ stopped: 2 }}, got {other:?}"),
        }
    }

    /// 存在しない record_id への stop も無音 return せず、`RecordNotFound` error を
    /// 返す (= 失敗経路も client が hang しないことの固定)。
    #[test]
    fn record_stop_nonexistent_id_sends_error_not_silent() {
        let state = SessionState::default();
        let (mut ch, rx) = record_test_client();
        let clients = std::slice::from_mut(&mut ch);

        handle_record_stop_request(0, RecordStopRequest { record_id: 999 }, clients, &state);

        match recv_control_from_queue(&rx) {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::RecordNotFound),
            other => panic!("expected Error {{ RecordNotFound }}, got {other:?}"),
        }
    }

    /// `child-state-v1` cap を持つ test 用 `ClientHandle` を組み立てる
    /// (= `record_test_client` の cap 違い版、resume request 用)。
    fn child_state_test_client() -> (ClientHandle, Receiver<SharedBytes>) {
        let (tx, rx) = std::sync::mpsc::channel::<SharedBytes>();
        let (_a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let ch = ClientHandle {
            id: 1,
            mode: Mode::Rw,
            leader: true,
            subscription: Subscription::Raw,
            negotiated_caps: vec!["child-state-v1".into()],
            writer_tx: tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            buffer_limit: 1 << 20,
            writer_thread: None,
            reader: b,
            connected_at_unix_ms: 0,
        };
        (ch, rx)
    }

    /// issue 2026-06-11: `session.child.resume.request` 受信時、daemon は SIGCONT
    /// より前に要求元 client へ attach redraw bytes (= raw_data frame) を push する。
    /// screen state に出力がある状態なら redraw frame が非空で届くことを固定する。
    #[test]
    fn resume_request_pushes_attach_redraw_before_sigcont() {
        let state = SessionState::default();
        let (mut ch, rx) = child_state_test_client();
        let clients = std::slice::from_mut(&mut ch);

        // 子が何か出力した state を作る (= pristine ではない → redraw 非空)。
        let mut screen_state = ScreenState::new(24, 80, 100);
        screen_state.process(b"hello world");

        // killpg は self pgrp に SIGCONT (= 実行中 process には無害な no-op)。
        let pgid = nix::unistd::getpgrp();
        let outcome = handle_session_child_resume_request(pgid, 0, clients, &state, &screen_state);
        assert!(
            matches!(outcome, ClientFrameOutcome::Continue),
            "resume request must not drop a healthy client"
        );

        // 最初に届く frame は raw_data の redraw bytes。
        let payload = rx
            .try_recv()
            .expect("resume must enqueue an attach redraw raw_data frame");
        let frame = Frame::decode_from(&mut payload.as_slice()).expect("decode frame");
        assert_eq!(frame.ty, TYPE_RAW_DATA, "redraw must be a raw_data frame");
        assert!(
            !frame.body.is_empty(),
            "redraw body must be non-empty for a non-pristine screen state"
        );
        // build_attach_redraw は primary buffer 復元の `?1049l` から始まる。
        assert!(
            frame.body.starts_with(b"\x1b[?1049l"),
            "redraw must start with primary-buffer restore (?1049l), got: {:?}",
            &frame.body[..frame.body.len().min(16)]
        );
    }

    /// pristine な screen state (= 子が 1 byte も出力していない) では redraw body は
    /// 空 raw_data frame になる (= `build_attach_redraw` の pristine 早期 return、
    /// 外側 shell 画面 history を clear しない契約)。
    #[test]
    fn resume_request_pushes_empty_redraw_for_pristine_state() {
        let state = SessionState::default();
        let (mut ch, rx) = child_state_test_client();
        let clients = std::slice::from_mut(&mut ch);

        let screen_state = ScreenState::new(24, 80, 100);

        let pgid = nix::unistd::getpgrp();
        let outcome = handle_session_child_resume_request(pgid, 0, clients, &state, &screen_state);
        assert!(
            matches!(outcome, ClientFrameOutcome::Continue),
            "resume request must not drop a healthy client"
        );

        let payload = rx
            .try_recv()
            .expect("resume must still enqueue a (possibly empty) raw_data frame");
        let frame = Frame::decode_from(&mut payload.as_slice()).expect("decode frame");
        assert_eq!(frame.ty, TYPE_RAW_DATA);
        assert!(
            frame.body.is_empty(),
            "pristine screen state must yield an empty redraw body"
        );
    }

    /// status の生死チェックが exit 済みの子を reap してはいけない (= reap すると
    /// serve_loop の lifecycle が exit を観測できず session が永遠に畳まれない)。
    /// zombie の子に対して生死チェックを行った後でも、waitpid が Exited を返せる
    /// ことを検証する (旧実装 = waitpid(WNOHANG) ベースだとここで ECHILD になる)。
    #[test]
    // zombie を意図的に作って「reap せずに観測できるか」を検証するテストなので、
    // wait() しない spawn が本体 (実際の reap は末尾の waitpid で行われる)。
    #[allow(clippy::zombie_processes)]
    fn status_liveness_check_must_not_reap_exited_child() {
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        // 即 exit する子を spawn して zombie にする (= std の Child は drop でも
        // reap しないので、waitpid で観測するまで zombie が残る)
        let spawned = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let child = nix::unistd::Pid::from_raw(spawned.id() as i32);
        // 子が exit して zombie になるのを少し待つ
        std::thread::sleep(std::time::Duration::from_millis(50));

        // handle_status_query と同じ生死チェック (= kill(pid, 0))。zombie は
        // 「alive 扱い」(= kill 0 が成功) でよい — exit の観測は lifecycle の責務。
        let alive = nix::sys::signal::kill(child, None).is_ok();
        assert!(alive, "zombie child must still be visible to kill(pid, 0)");

        // 生死チェックの後でも lifecycle 相当の waitpid が exit を観測できること。
        let status = waitpid(child, Some(WaitPidFlag::WNOHANG)).expect(
            "waitpid must still observe the exit (ECHILD here means the liveness \
             check reaped the child)",
        );
        assert!(
            matches!(status, WaitStatus::Exited(_, 0)),
            "exit must be observable exactly once by the lifecycle path, got {status:?}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // DR-0019 Update: `set.request` handler (= on-child-suspend runtime 変更)
    // ──────────────────────────────────────────────────────────────────────

    /// `set-v1` cap + `Rw` mode を持つ test 用 `ClientHandle`。
    fn set_test_client(caps: Vec<String>, mode: Mode) -> (ClientHandle, Receiver<SharedBytes>) {
        let (tx, rx) = std::sync::mpsc::channel::<SharedBytes>();
        let (_a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let ch = ClientHandle {
            id: 5,
            mode,
            leader: false,
            subscription: Subscription::Raw,
            negotiated_caps: caps,
            writer_tx: tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            buffer_limit: 1 << 20,
            writer_thread: None,
            reader: b,
            connected_at_unix_ms: 0,
        };
        (ch, rx)
    }

    fn set_req(value: &str) -> crate::protocol::messages::SetRequest {
        crate::protocol::messages::SetRequest {
            key: "on-child-suspend".into(),
            value: value.into(),
        }
    }

    /// rw + set-v1 で `auto-resume` を set → SessionState の policy が更新され、
    /// SetAck が返り、lifecycle event `policy-changed` が記録される。
    #[test]
    fn set_on_child_suspend_updates_state_and_acks() {
        let state = SessionState::default();
        // default は Notify。
        assert_eq!(state.child_suspend_policy(), ChildSuspendPolicy::Notify);
        let (mut ch, rx) = set_test_client(vec!["set-v1".into()], Mode::Rw);
        let clients = std::slice::from_mut(&mut ch);

        let outcome = handle_set_request(0, set_req("auto-resume"), clients, &state);
        assert!(matches!(outcome, ClientFrameOutcome::Continue));

        // state が更新されている (= runtime 変更が実効)。
        assert_eq!(state.child_suspend_policy(), ChildSuspendPolicy::AutoResume);

        // SetAck が返る。
        match recv_control_from_queue(&rx) {
            ControlMessage::SetAck(ack) => {
                assert_eq!(ack.key, "on-child-suspend");
                assert_eq!(ack.value, "auto-resume");
            }
            other => panic!("expected SetAck, got {other:?}"),
        }
    }

    /// `set` 成功時に lifecycle event `policy-changed` が record sink (jsonl) に
    /// 書かれる (= 「誰がいつ変えたか追える」要件の固定。push_lifecycle は sink 無し
    /// だと no-op なので、record を張った上で実出力を検証する)。
    #[test]
    fn set_on_child_suspend_records_policy_changed_lifecycle_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("set.jsonl");
        let state = SessionState::default();
        let id = start_record(&state, &path);
        let (mut ch, rx) = set_test_client(vec!["set-v1".into()], Mode::Rw);
        let clients = std::slice::from_mut(&mut ch);

        handle_set_request(0, set_req("auto-resume"), clients, &state);
        // ack を drain (= enqueue 確認も兼ねる)。
        let _ = recv_control_from_queue(&rx);
        state.record_registry.stop(id).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let line = content
            .lines()
            .find(|l| l.contains("policy-changed"))
            .expect("policy-changed lifecycle event must be written to jsonl");
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["ev"], "policy-changed");
        assert_eq!(v["key"], "on-child-suspend");
        assert_eq!(v["value"], "auto-resume");
        assert_eq!(v["changed_by_client_id"], 5); // set_test_client の id
    }

    /// 不正値は `SetInvalidValue` で reject、state は変わらない。
    #[test]
    fn set_on_child_suspend_invalid_value_rejected() {
        let state = SessionState::default();
        let (mut ch, rx) = set_test_client(vec!["set-v1".into()], Mode::Rw);
        let clients = std::slice::from_mut(&mut ch);

        handle_set_request(0, set_req("bogus"), clients, &state);

        assert_eq!(
            state.child_suspend_policy(),
            ChildSuspendPolicy::Notify,
            "invalid value must not mutate state"
        );
        match recv_control_from_queue(&rx) {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::SetInvalidValue),
            other => panic!("expected Error(SetInvalidValue), got {other:?}"),
        }
    }

    /// 未知 key は `SetInvalidKey` で reject。
    #[test]
    fn set_unknown_key_rejected() {
        let state = SessionState::default();
        let (mut ch, rx) = set_test_client(vec!["set-v1".into()], Mode::Rw);
        let clients = std::slice::from_mut(&mut ch);

        handle_set_request(
            0,
            crate::protocol::messages::SetRequest {
                key: "no-such-key".into(),
                value: "x".into(),
            },
            clients,
            &state,
        );
        match recv_control_from_queue(&rx) {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::SetInvalidKey),
            other => panic!("expected Error(SetInvalidKey), got {other:?}"),
        }
    }

    /// cap `set-v1` 未保持は `UnsupportedCapability` で reject。
    #[test]
    fn set_without_cap_rejected() {
        let state = SessionState::default();
        let (mut ch, rx) = set_test_client(vec!["data".into()], Mode::Rw);
        let clients = std::slice::from_mut(&mut ch);

        handle_set_request(0, set_req("auto-resume"), clients, &state);
        assert_eq!(state.child_suspend_policy(), ChildSuspendPolicy::Notify);
        match recv_control_from_queue(&rx) {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::UnsupportedCapability),
            other => panic!("expected Error(UnsupportedCapability), got {other:?}"),
        }
    }

    /// RwNoLeader は set 可 (= Ro 以外の書き込み可能 client なら誰でも変更できる。
    /// leader を取らない自動化 client が policy を直すユースケース)。
    #[test]
    fn set_rw_no_leader_mode_allowed() {
        let state = SessionState::default();
        let (mut ch, rx) = set_test_client(vec!["set-v1".into()], Mode::RwNoLeader);
        let clients = std::slice::from_mut(&mut ch);

        handle_set_request(0, set_req("auto-resume"), clients, &state);

        assert_eq!(
            state.child_suspend_policy(),
            ChildSuspendPolicy::AutoResume,
            "RwNoLeader client must be able to change policy"
        );
        match recv_control_from_queue(&rx) {
            ControlMessage::SetAck(ack) => {
                assert_eq!(ack.key, "on-child-suspend");
                assert_eq!(ack.value, "auto-resume");
            }
            other => panic!("expected SetAck, got {other:?}"),
        }
    }

    /// Ro mode は `ModeNotAllowed` で reject (= 書き込み権限が必要)。
    #[test]
    fn set_ro_mode_rejected() {
        let state = SessionState::default();
        let (mut ch, rx) = set_test_client(vec!["set-v1".into()], Mode::Ro);
        let clients = std::slice::from_mut(&mut ch);

        handle_set_request(0, set_req("auto-resume"), clients, &state);
        assert_eq!(state.child_suspend_policy(), ChildSuspendPolicy::Notify);
        match recv_control_from_queue(&rx) {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::ModeNotAllowed),
            other => panic!("expected Error(ModeNotAllowed), got {other:?}"),
        }
    }
}
