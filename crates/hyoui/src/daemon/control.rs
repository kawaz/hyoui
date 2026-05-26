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
//! - `handle_wait_request` / `handle_tail_request`: predicate / subscription
//!   セットアップ (= 本 module からは cap check 後に呼ぶだけ、本体は Phase E で
//!   wait.rs / tail.rs に分離予定)
//! - [`super::lock`] の `generate_lock_token` / `SessionState`
//!
//! frame I/O や PTY write の I/O 部分は `session.rs::serve_loop` 側に残る (= 本
//! module は protocol-level の意思決定に集中)。

use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::protocol::messages::{
    ClientInfo, ErrorCode, ErrorMessage, LockResponse, LockResult, ModeChange, SessionMode,
    StatusResponse, TailRequest, WaitRequest,
};
use crate::protocol::{ControlMessage, Frame, Mode, TYPE_CBOR_CONTROL, TYPE_RAW_DATA};
use crate::scrollback::Scrollback;
use crate::sys::{FdExt, Pty};

use super::DaemonConfig;
use super::broadcast::{ClientHandle, broadcast_control, send_control};
use super::lock::{SessionState, generate_lock_token};
use super::session::{RelayOutcome, handle_tail_request};
use super::wait::{PendingWait, handle_wait_request};

// === Frame 処理 outcome ===

/// 1 client から受け取った frame の処理結果。
pub(super) enum ClientFrameOutcome {
    /// 通常処理完了、loop 継続。
    Continue,
    /// この client は detach / protocol error → list から remove。
    DropClient,
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
    config: &DaemonConfig,
    pending_waits: &mut Vec<PendingWait>,
) -> ClientFrameOutcome {
    match frame.ty {
        TYPE_RAW_DATA => {
            let ch_id = clients[idx].id;
            let ch_mode = clients[idx].mode;
            // 書き込み authorization:
            // - Ro mode は書けない (silently drop)
            // - lock 中は lock holder のみ書ける (= 他 rw も silently drop)
            if matches!(ch_mode, Mode::Ro) {
                return ClientFrameOutcome::Continue;
            }
            if let Some(holder) = state.lock_holder {
                if holder != ch_id {
                    return ClientFrameOutcome::Continue;
                }
            }
            if pty.master_fd().write_all(&frame.body).is_err() {
                return ClientFrameOutcome::DropClient;
            }
            ClientFrameOutcome::Continue
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
                config,
                pending_waits,
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
/// 通じて必要な state (`SessionState`, `ClientHandle` slice, scrollback, config,
/// pending_waits) を受け取る。
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
    config: &DaemonConfig,
    pending_waits: &mut Vec<PendingWait>,
) -> ClientFrameOutcome {
    match msg {
        ControlMessage::Detach(d) => handle_detach_target(idx, d, clients),
        ControlMessage::Kill(k) => handle_kill(child, idx, k, clients),
        ControlMessage::Signal(s) => handle_signal(child, idx, s, clients),
        ControlMessage::Resize(r) => handle_resize(pty, idx, r, clients),
        ControlMessage::LockAcquire(req) => handle_lock_acquire(idx, req, clients, state),
        ControlMessage::LockRelease(rel) => handle_lock_release(idx, rel, clients, state),
        ControlMessage::TailRequest(req) => {
            handle_tail_request_dispatch(idx, req, clients, scrollback)
        }
        ControlMessage::WaitRequest(req) => {
            handle_wait_request_dispatch(idx, req, clients, pending_waits)
        }
        ControlMessage::StatusQuery(_) => {
            handle_status_query(child, idx, clients, state, scrollback, config)
        }
        // daemon → client 方向のはずの message が client → daemon に来た or 未実装 kind。
        // DR-0008 §3.2 「未知 kind は decode error」だが、ここに来るのは serde で既知
        // variant なので decode 段階では catch されない。protocol violation として
        // 明示 error を返す (= silent skip しない)。
        ControlMessage::HandshakeRequest(_)
        | ControlMessage::HandshakeResponse(_)
        | ControlMessage::Error(_)
        | ControlMessage::LockResponse(_)
        | ControlMessage::LeaderNotify(_)
        | ControlMessage::ModeChange(_)
        | ControlMessage::StatusResponse(_)
        | ControlMessage::TailData(_)
        | ControlMessage::TailEnd(_)
        | ControlMessage::WaitResult(_) => reject_unexpected_kind(idx, clients),
    }
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

/// 整数 signum から nix `Signal` を返す。POSIX `kill(pid, 0)` semantic に従い
/// `signum == 0` も範囲外として扱う (= "existence probe" は wire protocol で
/// サポートしない、必要なら別 message を新設)。範囲外なら `None`。
pub(super) fn nix_signal_from_signum(signum: u8) -> Option<Signal> {
    if signum == 0 {
        return None;
    }
    Signal::try_from(signum as i32).ok()
}

/// `detach` message の target に応じて drop 対象を決める。
///
/// - `Myself`: 自分 1 client のみ drop (= DropClient)
/// - `Others`: 自分以外の全 client を drop (= caller が複数 drop できる API が
///   無いため、本実装では `error: not-implemented` を返して継続)
/// - `All`: 全 client + session 終了 (= 同様に `error: not-implemented` で継続)
fn handle_detach_target(
    idx: usize,
    detach: crate::protocol::messages::Detach,
    clients: &mut [ClientHandle],
) -> ClientFrameOutcome {
    use crate::protocol::messages::DetachTarget;
    match detach.target {
        DetachTarget::Myself => ClientFrameOutcome::DropClient,
        DetachTarget::Others | DetachTarget::All => {
            // Round2 #7: error 通知 + 自分も drop する。
            // 旧 Round1 実装は `Continue` で client を生かしたまま error だけ送って
            // いたが、旧仕様 (silent skip → DropClient だった) を仮定する client は
            // socket open のまま hang する。「target が `Others`/`All` で部分実装」
            // という事実を error で伝えつつ、後方互換 (= 最低限自分は drop される)
            // を維持する。Others/All の本来の動作 (= 他 client / 全 client drop)
            // は v0.2.0+ で実装予定。
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::DetachTargetPartial,
                    message: "detach target=others/all not fully implemented yet; \
                              only self will be detached (Phase 11 MVP は Myself のみ対応、\
                              v0.2.0+ で他 client も drop する semantic に拡張予定)"
                        .into(),
                    details: None,
                }),
            );
            ClientFrameOutcome::DropClient
        }
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
    // signum 解釈: None なら SIGTERM、invalid 値 (0 や 範囲外) は error。
    let signum = k.signum.unwrap_or(libc::SIGTERM as u8);
    let sig = match nix_signal_from_signum(signum) {
        Some(s) => s,
        None => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::SignalInvalid,
                    message: format!("invalid signum: {signum}"),
                    details: None,
                }),
            );
            return ClientFrameOutcome::Continue;
        }
    };
    let _ = kill(child, sig);
    ClientFrameOutcome::TerminateSession(RelayOutcome::ClientDetachedOrKilled)
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
    let sig = match nix_signal_from_signum(s.signum) {
        Some(s) => s,
        None => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::SignalInvalid,
                    message: format!("invalid signum: {}", s.signum),
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
fn handle_resize(
    pty: &Pty,
    idx: usize,
    r: crate::protocol::messages::Resize,
    clients: &mut [ClientHandle],
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
    // R4-C9: idempotency — 同じ client が既に lock を保持している場合は、
    // 旧 token をそのまま返して Acquired を返す。
    // 旧実装は `state.lock_holder.is_some()` だけで Denied を返していたため、
    // 既に保持中の client が再 LockAcquire すると **自分の lock に弾かれる**
    // footgun が発生していた。idempotent operation の標準的な挙動 (= 既に
    // 同じ state なら success) に合わせる。
    // mode.change broadcast は **行わない** (= state 変化なし)。
    if state.lock_holder == Some(ch_id) {
        let token = state.lock_token.clone();
        let _ = req; // wait / timeout / process_bound は idempotent 再取得では未使用
        let _ = send_control(
            &clients[idx],
            ControlMessage::LockResponse(LockResponse {
                result: LockResult::Acquired,
                token,
                queue_position: None,
            }),
        );
        return ClientFrameOutcome::Continue;
    }
    if state.lock_holder.is_some() {
        // 「1 request → 1 response」契約を守るため、wait=true / wait=false
        // どちらも LockResponse(Denied) 1 frame のみで応答する (Round2 #3
        // = `error` を併送して 2 frame にすると client が後続 frame と
        // mis-align するため)。MVP では wait queue 未実装なので wait=true
        // でも Queued を返せない事実は DR-0008 に明記する想定。
        let _ = req; // process_bound / timeout / wait は queue 実装まで未使用
        let _ = send_control(
            &clients[idx],
            ControlMessage::LockResponse(LockResponse {
                result: LockResult::Denied,
                token: None,
                queue_position: None,
            }),
        );
        return ClientFrameOutcome::Continue;
    }
    let token = generate_lock_token();
    state.lock_holder = Some(ch_id);
    state.lock_token = Some(token.clone());
    let _ = send_control(
        &clients[idx],
        ControlMessage::LockResponse(LockResponse {
            result: LockResult::Acquired,
            token: Some(token),
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
    // token + holder 両方を照合してから解放
    let valid =
        state.lock_holder == Some(ch_id) && state.lock_token.as_deref() == Some(rel.token.as_str());
    if !valid {
        let _ = send_control(
            &clients[idx],
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::LockNotHeld,
                message: "lock token mismatch or not the lock holder".into(),
                details: None,
            }),
        );
        return ClientFrameOutcome::Continue;
    }
    state.lock_holder = None;
    state.lock_token = None;
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

/// `ControlMessage::WaitRequest` の cap check + handler 呼び出し。
fn handle_wait_request_dispatch(
    idx: usize,
    req: WaitRequest,
    clients: &mut [ClientHandle],
    pending_waits: &mut Vec<PendingWait>,
) -> ClientFrameOutcome {
    if ensure_cap(
        &clients[idx],
        "wait-l0",
        "wait.request requires `wait-l0` cap",
    )
    .is_err()
    {
        return ClientFrameOutcome::Continue;
    }
    handle_wait_request(idx, req, clients, pending_waits);
    ClientFrameOutcome::Continue
}

/// `ControlMessage::StatusQuery` を処理する。
///
/// 子 pid の生死は waitpid(WNOHANG) で確認 (= reap せず存在チェックのみ)。
fn handle_status_query(
    child: Pid,
    idx: usize,
    clients: &mut [ClientHandle],
    state: &SessionState,
    scrollback: &Scrollback,
    config: &DaemonConfig,
) -> ClientFrameOutcome {
    let child_pid = match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => Some(child.as_raw() as u32),
        _ => None,
    };
    let clients_info: Vec<ClientInfo> = clients
        .iter()
        .map(|c| ClientInfo {
            client_id: c.id,
            mode: c.mode,
            leader: c.leader,
        })
        .collect();
    let resp = StatusResponse {
        session_id: config.session_id.clone(),
        child_pid,
        clients: clients_info,
        scrollback_bytes: scrollback.total_bytes() as u64,
        lock_holder: state.lock_holder,
    };
    let _ = send_control(&clients[idx], ControlMessage::StatusResponse(resp));
    ClientFrameOutcome::Continue
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
