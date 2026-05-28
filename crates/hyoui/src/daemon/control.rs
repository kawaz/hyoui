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
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::protocol::messages::{
    ClientInfo, ErrorCode, ErrorMessage, LockResponse, LockResult, ModeChange, ScreenBufferKind,
    ScreenCursorSnap, ScreenDumpRequest, ScreenDumpResponse, ScreenModeSnap, ScreenWindowSize,
    SessionMode, SnapshotComponent, StateSnapshotRequest, StateSnapshotResponse, StatusResponse,
    TailRequest,
};
use crate::protocol::{ControlMessage, Frame, Mode, TYPE_CBOR_CONTROL, TYPE_RAW_DATA};
use crate::scrollback::Scrollback;
use crate::sys::{FdExt, Pty};

use super::DaemonConfig;
use super::broadcast::{ClientHandle, broadcast_control, send_control};
use super::lock::{SessionState, generate_lock_token};
use super::screen::{
    ScreenDumpFormat as InternalDumpFormat, ScreenDumpLayer as InternalDumpLayer, ScreenState,
    build_screen_dump, build_screen_snapshot,
};
use super::session::RelayOutcome;
use super::tail::handle_tail_request;

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
            // R5-C3: master fd は NONBLOCK なので `write_all` だと EAGAIN
            // (= 子の line discipline buffer 4–8 KiB が満杯の瞬間) を即
            // disconnect 扱いし、slow-reader 経由の client DoS が成立する。
            // `write_all_with_idle_timeout` は EAGAIN を poll(POLLOUT) で
            // 待ち、forward progress が `MASTER_WRITE_IDLE_TIMEOUT_MS` 続け
            // ないときだけ ETIMEDOUT を返す。タイムアウト時は明示 error
            // (= `master.write-timeout`) を通知してから DropClient する。
            match pty
                .master_fd()
                .write_all_with_idle_timeout(&frame.body, MASTER_WRITE_IDLE_TIMEOUT_MS)
            {
                Ok(()) => ClientFrameOutcome::Continue,
                Err(crate::sys::Error::Errno(nix::errno::Errno::ETIMEDOUT)) => {
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::Error(ErrorMessage {
                            code: ErrorCode::MasterWriteTimeout,
                            message: format!(
                                "master PTY write made no forward progress for {MASTER_WRITE_IDLE_TIMEOUT_MS} ms \
                                (child is a slow reader); disconnecting client"
                            ),
                            details: None,
                        }),
                    );
                    ClientFrameOutcome::DropClient
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
            handle_session_child_resume_request(child, idx, clients)
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
        | ControlMessage::ScreenDumpResponse(_)
        | ControlMessage::StateSnapshotResponse(_)
        | ControlMessage::SessionExitNotify(_)
        | ControlMessage::SessionChildStoppedNotify(_) => reject_unexpected_kind(idx, clients),
    }
}

/// `session.child.resume.request` (DR-0015 §2.2、cap `child-state-v1`)。
///
/// leader が follow / auto-resume 政策の延長で「子を SIGCONT で起こせ」と daemon に
/// 要求する経路。daemon は `killpg(child_pgid, SIGCONT)` で子 pgrp 全体に SIGCONT。
///
/// cap 未保持 client は `UnsupportedCapability` で reject (= leader 選定で本来弾かれる
/// はずだが defense-in-depth)。
fn handle_session_child_resume_request(
    child: Pid,
    idx: usize,
    clients: &mut [ClientHandle],
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
    // 子 pgrp に SIGCONT。DR-0001 §実装ノート「子は独立セッションリーダーなので
    // 子の pgid == 子の pid」を踏襲。
    let _ = nix::sys::signal::killpg(child, nix::sys::signal::Signal::SIGCONT);
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
