//! Socket accept + handshake worker pool (DR-0009 Phase D で `session.rs` から分離)。
//!
//! ## 構成
//!
//! - [`PendingHandshake`]: serve_loop が握る in-flight な handshake worker entry
//! - [`AcceptedClient`]: 新規 client の accept 結果 (ClientHandle + leader 判定)
//! - [`HandshakeStageOk`]: worker thread → main thread 間の中間結果 type alias
//! - [`spawn_handshake_worker`]: 1 client accept + handshake 用 worker thread spawn
//! - [`do_handshake_stage`]: worker 内で走る handshake frame 受信 + token 検証
//! - [`finalize_accepted_client`]: main thread 側 leader 判定 + response 送信 +
//!   `ClientHandle` 構築
//! - [`process_pending_handshakes`]: serve_loop の各 iteration で完了 worker を取り込む
//! - [`unix_stream_from_owned_fd`]: `OwnedFd` → `UnixStream` の意図明示 helper
//! - [`HANDSHAKE_TIMEOUT`] / [`MAX_PENDING_HANDSHAKES`] const
//! - [`constant_time_eq`]: token 比較用の constant-time 比較
//!
//! ## session.rs / 他 module との接続
//!
//! - 入口: `serve_loop` は `spawn_handshake_worker` で 1 worker を spawn して
//!   `pending_handshakes` に push。`process_pending_handshakes` で完了済を `clients` に
//!   integrate する (= leader 判定 + mode.change broadcast)。
//! - 依存方向: accept.rs → broadcast.rs / lock.rs (片方向、DR-0009 §module DAG)。
//!   broadcast.rs から accept.rs を呼ぶ経路は無い。

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use crate::Error;
use crate::protocol::messages::{
    ErrorCode, ErrorMessage, LeaderNotify, MAX_CAP_LEN, MAX_CAPS_COUNT, MAX_TOKEN_LEN, ModeChange,
    SessionMode,
};
use crate::protocol::{
    ControlMessage, Frame, HandshakeRequest, HandshakeResponse, MVP_CAPS, TYPE_CBOR_CONTROL,
    Transport, UnixStreamTransport, intersect_caps,
};
use crate::sys::UnixSock;
use crate::sys::clock::now_unix_ms;

use super::DaemonConfig;
use super::broadcast::{
    ClientHandle, SharedBytes, Subscription, broadcast_control, enqueue_for_client, send_control,
    writer_pump,
};
use super::lock::{SessionState, should_assign_leader};
use super::screen::{ScreenState, build_attach_redraw};

/// R4-C3: handshake (= 1 client の HandshakeRequest 受信 + token 検証) を完了
/// させるまでの上限時間。これを超過した pending handshake は socket close して
/// 当該 worker thread の流れを中断する (= slow-loris DoS 防止)。
///
/// 旧実装は accept 後 `Frame::decode_from` を同期 blocking で呼び、悪意 client が
/// 1 byte ずつ送って handshake を遅延させると `serve_loop` 全体が止まっていた。
/// 現実装では handshake を別 thread に切り出し、本 timeout で個別に頭打ちする。
pub(super) const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// daemon が同時に走らせて良い pending handshake worker 数の上限。
///
/// R5-H2: 旧実装は `MAX_CLIENTS_PER_DAEMON` (= 64) と同じ値で **合算頭打ち**
/// していたため、正常 attach 64 client が居る状態では新規接続が無条件に reject
/// されていた (= 「64 client 占有しただけで以降の attach 不能」運用事故)。
/// 現実装では pending と attached を **独立 cap** にし、handshake 中の slow-loris
/// 攻撃面 (= pending) と legit client 数 (= attached) を別々に制御する:
///
/// - `MAX_CLIENTS_PER_DAEMON = 64`: 確立済 attach の上限
/// - `MAX_PENDING_HANDSHAKES = 16`: handshake 中 (= 確立前) の上限
///
/// 「64 client + 16 pending」が許容される設計。pending は worker thread + socket fd
/// の cost なので clients より小さく設定 (≈ 1/4) し、DoS 攻撃面を抑える。
pub(super) const MAX_PENDING_HANDSHAKES: usize = 16;

/// 2 つの byte slice を constant-time で比較 (= timing attack 耐性)。
///
/// 同 UID 信頼境界では timing leak の悪用余地は薄いが、token 比較に使う
/// 値は副作用ゼロの簡易実装。長さ違いは即 `false` で抜けるため厳密 constant
/// time ではないが、長さ自体を秘匿する必要はない。
pub(super) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 新規 client の accept 結果。
pub(super) struct AcceptedClient {
    pub(super) handle: ClientHandle,
    /// この client が leader として確定されたか (= Phase 10 leader assignment)。
    pub(super) became_leader: bool,
}

/// R4-C3: handshake worker thread が完了時に返す中間結果。
///
/// 成功時は (reader, writer_main, req, intersect) を main thread (= serve_loop) に
/// 渡し、main thread 側で leader 判定 + response 送信 + `ClientHandle` 構築を行う。
/// 失敗時 (= protocol error / token mismatch) は worker が socket に error frame を
/// 送ってから socket を drop し、本構造体は `Err` で main thread に届く。
pub(super) type HandshakeStageOk = (UnixStream, UnixStream, HandshakeRequest, Vec<String>);

/// R4-C3: pending handshake (= worker thread が走っている in-flight な handshake)。
///
/// `rx` に worker が完了結果を流す。`started_at` から `HANDSHAKE_TIMEOUT` を超えても
/// 完了しない場合は serve_loop が drop する (= socket を drop することで worker の
/// pending read が EBADF / read error で抜け、thread が自然終了する)。
///
/// **slow-loris 対策の本体**: worker thread は accepted UnixStream に
/// `set_read_timeout` / `set_write_timeout` を設定してから handshake を decode する。
/// 悪意 client が byte をだらだら送っても、socket の read/write が timeout で
/// 失敗するので thread は HANDSHAKE_TIMEOUT 以内に必ず終わる。
pub(super) struct PendingHandshake {
    rx: std::sync::mpsc::Receiver<Result<HandshakeStageOk, Error>>,
    started_at: Instant,
    /// 完了通知前に accept したことが分かる「socket を握っている worker」の
    /// JoinHandle。timeout 時に main thread から socket を切る経路は無いが、
    /// worker 自身が `set_*_timeout` で抜けるため放置で OK。drop で detached する
    /// (= join しない)。
    _worker: std::thread::JoinHandle<()>,
}

/// R4-C3: listener から 1 client を accept し、handshake を別 thread で進める。
///
/// 戻り値の [`PendingHandshake`] の `rx` が `Ok((reader, writer, req, intersect))` を
/// 通知してきたら、`finalize_accepted_client` で `ClientHandle` 構築 + response 送信
/// + leader 判定をする。`Err` なら worker が既に error frame 送信済 → drop で完了。
///
/// **同期 blocking 部分は `listener.accept()` のみ** (= kernel level)。handshake
/// frame 受信は worker thread に切り出すため、悪意 client が serve_loop を止める
/// ことはできない。
pub(super) fn spawn_handshake_worker(
    listener: &UnixSock,
    config: &DaemonConfig,
) -> Result<PendingHandshake, Error> {
    let fd: OwnedFd = listener.accept()?;
    let stream = unix_stream_from_owned_fd(fd);

    // R4-C3: handshake 用の read/write を時間で頭打ち。slow-loris client が
    // byte をだらだら送っても、socket I/O が EWOULDBLOCK で error 化して worker が
    // HANDSHAKE_TIMEOUT 以内に必ず終わる。
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));

    let transport = UnixStreamTransport::new(stream);
    let (mut reader, mut writer_main) = transport.split().map_err(Error::from)?;

    let expected_token = config.expected_token.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<HandshakeStageOk, Error>>(1);

    let worker = std::thread::Builder::new()
        .name("hyoui-handshake".into())
        .spawn(move || {
            let result =
                do_handshake_stage(&mut reader, &mut writer_main, expected_token.as_deref());
            match result {
                Ok((req, intersect)) => {
                    let _ = tx.send(Ok((reader, writer_main, req, intersect)));
                }
                Err(e) => {
                    // worker は error frame 送信済 (do_handshake_stage 内)。
                    // socket は ここで drop → close される。
                    let _ = tx.send(Err(e));
                }
            }
        })
        .map_err(|_| Error::Invalid("failed to spawn handshake worker"))?;

    Ok(PendingHandshake {
        rx,
        started_at: Instant::now(),
        _worker: worker,
    })
}

/// R4-C3: worker thread 側で実行する handshake 受信 + token 検証 stage。
///
/// 成功時: (req, intersect) を返す。**response はまだ送らない** (= leader 判定が
/// 必要なので main thread に任せる)。
/// 失敗時: 可能なら socket に error frame を送ってから `Err` を返す。
fn do_handshake_stage(
    reader: &mut UnixStream,
    writer_main: &mut UnixStream,
    expected_token: Option<&str>,
) -> Result<(HandshakeRequest, Vec<String>), Error> {
    let frame = Frame::decode_from(reader)
        .map_err(|_| Error::Invalid("failed to decode handshake frame"))?;
    if frame.ty != TYPE_CBOR_CONTROL {
        return Err(Error::Invalid("handshake frame must be CBOR control"));
    }
    let msg = ControlMessage::decode_from(frame.body.as_slice())
        .map_err(|_| Error::Invalid("handshake CBOR decode failed"))?;
    let req = match msg {
        ControlMessage::HandshakeRequest(r) => r,
        _ => return Err(Error::Invalid("first message must be handshake.request")),
    };

    // R5-H10: 認証 (token 検証) より前に caps / token の長さを cap する。
    // 旧実装は serde decode 通過した HandshakeRequest を無条件で握っており、
    // 1 worker あたり 16 MiB frame 上限ぎりぎりの caps/token を保持できた。
    // `MAX_PENDING_HANDSHAKES = 64` と組み合わさると認証前段階で
    // 1 GiB 級 transient peak が成立する (= memory exhaustion DoS)。
    // 違反は ProtocolMalformed として明示 error 通知 → worker drop で
    // 当該 memory を即解放する。
    if let Err(msg) = validate_handshake_lengths(&req) {
        let body = ControlMessage::Error(ErrorMessage {
            code: ErrorCode::ProtocolMalformed,
            message: msg.into(),
            details: None,
        })
        .encode_to_vec()
        .map_err(|_| Error::Invalid("handshake length error encode failed"))?;
        let _ = Frame::cbor_control(body).encode_to(writer_main);
        return Err(Error::Invalid("handshake field length exceeds limit"));
    }

    // token validation: `config.expected_token` が Some なら client が同一 token を
    // 提示する必要あり。不一致なら handshake を拒否。constant-time 比較で timing leak
    // 回避。
    // Round2 #10: 旧実装は `provided = req.token.as_deref().unwrap_or("")` で
    // `req.token = None` を空文字列と等価扱いしていたため、`expected_token = Some("")`
    // を運用ミスで設定すると全 client が free pass で通過する欠陥があった。
    // → `req.token` が `None` の場合は明示的に mismatch 扱い、`Some(s)` の場合のみ
    // constant_time_eq で比較する。
    if let Some(expected) = expected_token {
        let token_ok = match req.token.as_deref() {
            Some(provided) => constant_time_eq(expected.as_bytes(), provided.as_bytes()),
            None => false,
        };
        if !token_ok {
            let body = ControlMessage::Error(ErrorMessage {
                code: ErrorCode::AuthTokenMismatch,
                message: "handshake token does not match daemon configuration".into(),
                details: None,
            })
            .encode_to_vec()
            .map_err(|_| Error::Invalid("auth error encode failed"))?;
            let _ = Frame::cbor_control(body).encode_to(writer_main);
            return Err(Error::Invalid("handshake token mismatch"));
        }
    }

    let mvp: Vec<String> = MVP_CAPS.iter().map(|s| (*s).to_string()).collect();
    let intersect = intersect_caps(&req.caps, &mvp);

    Ok((req, intersect))
}

/// R5-H10: `HandshakeRequest` の caps / token の長さを cap する。
///
/// 違反時は人間可読な reason を `Err` で返す。caller は ProtocolMalformed の
/// error frame として client に通知してから worker を drop する。
fn validate_handshake_lengths(req: &HandshakeRequest) -> Result<(), &'static str> {
    if req.caps.len() > MAX_CAPS_COUNT {
        return Err("handshake.request.caps exceeds MAX_CAPS_COUNT");
    }
    for cap in &req.caps {
        if cap.len() > MAX_CAP_LEN {
            return Err("handshake.request.caps[*] exceeds MAX_CAP_LEN");
        }
    }
    if let Some(tok) = req.token.as_deref()
        && tok.len() > MAX_TOKEN_LEN
    {
        return Err("handshake.request.token exceeds MAX_TOKEN_LEN");
    }
    Ok(())
}

/// R4-C3: handshake worker から届いた中間結果を `AcceptedClient` に整える。
///
/// このタイミングで main thread の `clients` 列を見て leader 判定 + response を
/// 送信する (= leader 判定の snapshot を「handshake 完了時点」に揃えるため、
/// 並列 handshake 同士でも leader 重複は発生しない)。
///
/// Response 送信後、socket の read/write timeout は **解除** (= None) する。
/// serve_loop は poll 駆動なので blocking read は無いが、broadcast の write は
/// blocking write で行う。handshake 用 5s timeout のままだと正常 attach 中の
/// client への大量 broadcast で意図しない切断が起きうるため。
fn finalize_accepted_client(
    stage: HandshakeStageOk,
    config: &DaemonConfig,
    client_id: u64,
    clients: &[ClientHandle],
) -> Result<AcceptedClient, Error> {
    let (reader, mut writer_main, req, intersect) = stage;

    // R4-C3: 通常運用 (= broadcast write を含む) は timeout 無しに戻す。
    let _ = reader.set_read_timeout(None);
    let _ = reader.set_write_timeout(None);
    let _ = writer_main.set_read_timeout(None);
    let _ = writer_main.set_write_timeout(None);

    let became_leader = should_assign_leader(clients, req.mode);

    let response = HandshakeResponse {
        caps: intersect.clone(),
        session_id: config.session_id.clone(),
        client_id,
        leader: became_leader,
        mode: req.mode,
    };

    let body = ControlMessage::HandshakeResponse(response)
        .encode_to_vec()
        .map_err(|_| Error::Invalid("handshake.response encode failed"))?;
    Frame::cbor_control(body)
        .encode_to(&mut writer_main)
        .map_err(|_| Error::Invalid("handshake.response frame encode failed"))?;

    // writer thread を立ち上げ、broadcast 用 unbounded mpsc + atomic byte counter を作る。
    // queue capacity は byte 単位の `enqueue_for_client` で厳密に enforce する。
    let (tx, rx) = std::sync::mpsc::channel::<SharedBytes>();
    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let queued_bytes_for_pump = Arc::clone(&queued_bytes);
    let writer_thread =
        std::thread::spawn(move || writer_pump(rx, writer_main, queued_bytes_for_pump));
    let negotiated_caps = intersect;

    Ok(AcceptedClient {
        handle: ClientHandle {
            id: client_id,
            mode: req.mode,
            leader: became_leader,
            subscription: Subscription::Raw,
            negotiated_caps,
            writer_tx: tx,
            queued_bytes,
            buffer_limit: config.client_buffer_bytes,
            writer_thread: Some(writer_thread),
            reader,
        },
        became_leader,
    })
}

/// R4-C3: pending handshake worker の状態を更新する。
///
/// 各 entry に対し:
/// - `try_recv` で完了通知が来ていれば、`Ok` なら `finalize_accepted_client` →
///   `clients` に push + leader/mode.change broadcast。`Err` なら drop で完了。
/// - `started_at + HANDSHAKE_TIMEOUT` を超過していたら強制 drop (= 残った socket は
///   worker thread が `set_read_timeout` で抜け次第 close する)。
///
/// drop すべきものは即除去するため `Vec::retain_mut` で in-place 更新する。
///
/// DR-0013 §4 Phase A: new client を整列した直後に
/// [`build_attach_redraw`] で生成した raw bytes を送って画面を復元する。
/// `screen_state.sync_in_progress()` が true (= DEC sync update 中) の場合は
/// 即時 redraw を送らず、`pending_redraws` に client_id を積んで sync 終了後
/// に caller (= `serve_loop`) が flush する。
#[allow(clippy::too_many_arguments)]
pub(super) fn process_pending_handshakes(
    pending_handshakes: &mut Vec<PendingHandshake>,
    config: &DaemonConfig,
    next_client_id: &mut u64,
    clients: &mut Vec<ClientHandle>,
    state: &mut SessionState,
    overflow_ids: &mut Vec<u64>,
    screen_state: &ScreenState,
    pending_redraws: &mut Vec<u64>,
) {
    // 完了 / 失敗 / timeout の 3 状態に分岐して 1 つずつ処理する。
    let mut i = 0;
    while i < pending_handshakes.len() {
        // try_recv で完了確認
        match pending_handshakes[i].rx.try_recv() {
            Ok(Ok(stage)) => {
                let _entry = pending_handshakes.remove(i);
                // finalize: leader 判定 + response 送信 + ClientHandle 構築
                match finalize_accepted_client(stage, config, *next_client_id, clients) {
                    Ok(accepted) => {
                        *next_client_id += 1;
                        let new_id = accepted.handle.id;
                        let became_leader = accepted.became_leader;
                        let mode_change_for_locked = state.lock_holder.map(|holder| ModeChange {
                            session_mode: SessionMode::Locked,
                            lock_holder: Some(holder),
                            client_mode: None,
                        });
                        if let Some(mc) = mode_change_for_locked.as_ref() {
                            // accept した client に「現在 lock 中」を通知
                            let _ = send_control(&accepted.handle, ControlMessage::ModeChange(*mc));
                        }
                        // DR-0013 §4 Phase A: handshake response 送信完了直後に
                        // screen state からの redraw bytes を当該 client にだけ
                        // 送る。生 byte は raw_data frame (= TYPE_RAW_DATA) で送る
                        // ため、client 側は通常 attach フローでそのまま stdout に
                        // 流せば detach 時の画面が復元される (§4 + §10)。
                        if screen_state.sync_in_progress() {
                            // sync 中は中途半端な state を送らない (§6)。
                            // sync 終了後に caller が flush する。
                            pending_redraws.push(new_id);
                        } else {
                            send_attach_redraw(&accepted.handle, screen_state, overflow_ids);
                        }
                        let new_mode = accepted.handle.mode;
                        clients.push(accepted.handle);
                        // DR-0016 §3: client-attached lifecycle event。handshake 完了 +
                        // ClientHandle 登録完了の直後で push する (= 全 record sink に届く)。
                        state.record_registry.push_lifecycle(
                            super::record::LifecycleEvent::ClientAttached {
                                client_id: new_id,
                                mode: new_mode,
                                ts_unix_ms: now_unix_ms(),
                            },
                        );
                        if became_leader {
                            // 他 client に新 leader を通知 (= 新 client 自身は handshake.response
                            // で leader=true を受け取り済みだが、broadcast でも届く)
                            overflow_ids.extend(broadcast_control(
                                clients,
                                &ControlMessage::LeaderNotify(LeaderNotify {
                                    client_id: Some(new_id),
                                }),
                            ));
                        }
                    }
                    Err(_) => {
                        // response 送信失敗等。drop で client は弾く。
                    }
                }
                // remove したので i は変えない
            }
            Ok(Err(_)) => {
                // worker 側で error frame 送信済 → ここで drop で完了
                pending_handshakes.remove(i);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // 未完了。timeout 判定だけする
                if pending_handshakes[i].started_at.elapsed() >= HANDSHAKE_TIMEOUT {
                    // R4-C3: 5s 経過しても完了しない = slow-loris の可能性が高い。
                    // PendingHandshake を drop する (= rx を drop する)。worker は
                    // socket の read/write timeout (= 同じく HANDSHAKE_TIMEOUT) で
                    // ほぼ同時に抜けるため、deadlock せず thread は自然終了する。
                    pending_handshakes.remove(i);
                } else {
                    i += 1;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // worker thread が panic 等で消えた。drop で完了。
                pending_handshakes.remove(i);
            }
        }
    }
}

/// DR-0013 §4 Phase A: 1 client に attach 復元用 redraw bytes を 1 つの
/// `TYPE_RAW_DATA` frame で送る。
///
/// `build_attach_redraw` で alt mode prepend + `state_formatted` を組み立てた
/// bytes を raw_data frame に詰めて enqueue する (= 通常 broadcast の生 byte
/// 経路と同じ frame type)。enqueue が overflow / writer dead だった場合は
/// 当該 client_id を `overflow_ids` に積み、caller が drop する設計。
pub(super) fn send_attach_redraw(
    ch: &ClientHandle,
    screen_state: &ScreenState,
    overflow_ids: &mut Vec<u64>,
) {
    // pristine state なら `bytes` は空 Vec (= issue 2026-05-29-bug-attach-initial-
    // clear-on-empty-session.md の対策、`build_attach_redraw` 参照)。frame 自体は
    // 既存 client / test の「handshake 直後に必ず raw_data frame が 1 つ来る」契約を
    // 維持するため empty payload で送る。client 側は empty payload を stdout に書いても
    // 何も起きない (= 自然な no-op)、外側 shell の画面 history が clear されない。
    let bytes = build_attach_redraw(screen_state);
    let mut frame_bytes = Vec::new();
    if Frame::raw_data(bytes).encode_to(&mut frame_bytes).is_err() {
        return;
    }
    let payload: SharedBytes = Arc::new(frame_bytes);
    match enqueue_for_client(ch, payload) {
        super::broadcast::EnqueueOutcome::Sent => {}
        _ => overflow_ids.push(ch.id),
    }
}

/// `OwnedFd` を `std::os::unix::net::UnixStream` に変換する。
///
/// `UnixStream::from(OwnedFd)` は `From` impl が存在するが、明示的な
/// hyoui 内 helper を経由することで「ここで所有権が移る」点を可視化する。
pub(super) fn unix_stream_from_owned_fd(fd: OwnedFd) -> UnixStream {
    UnixStream::from(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Mode;

    fn req_with(caps: Vec<String>, token: Option<String>) -> HandshakeRequest {
        HandshakeRequest {
            caps,
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token,
        }
    }

    /// R5-H10: caps が `MAX_CAPS_COUNT` 以下なら通過。
    #[test]
    fn handshake_accepts_caps_at_max_count() {
        let caps: Vec<String> = (0..MAX_CAPS_COUNT).map(|i| format!("c{i}")).collect();
        validate_handshake_lengths(&req_with(caps, None)).expect("must accept");
    }

    /// R5-H10: caps の要素数が `MAX_CAPS_COUNT` を超えると reject。
    #[test]
    fn handshake_rejects_too_many_caps() {
        let caps: Vec<String> = (0..=MAX_CAPS_COUNT).map(|i| format!("c{i}")).collect();
        let err =
            validate_handshake_lengths(&req_with(caps, None)).expect_err("must reject excess caps");
        assert!(err.contains("MAX_CAPS_COUNT"), "reason was: {err}");
    }

    /// R5-H10: cap 1 個の byte 長が `MAX_CAP_LEN` を超えると reject。
    #[test]
    fn handshake_rejects_long_cap_string() {
        let long_cap = "x".repeat(MAX_CAP_LEN + 1);
        let err = validate_handshake_lengths(&req_with(vec![long_cap], None))
            .expect_err("must reject long cap");
        assert!(err.contains("MAX_CAP_LEN"), "reason was: {err}");
    }

    /// R5-H10: token が `MAX_TOKEN_LEN` 以下なら通過。
    #[test]
    fn handshake_accepts_token_at_max_len() {
        let tok = "a".repeat(MAX_TOKEN_LEN);
        validate_handshake_lengths(&req_with(Vec::new(), Some(tok))).expect("must accept");
    }

    /// R5-H10: token の byte 長が `MAX_TOKEN_LEN` を超えると reject。
    #[test]
    fn handshake_rejects_long_token() {
        let long_tok = "a".repeat(MAX_TOKEN_LEN + 1);
        let err = validate_handshake_lengths(&req_with(Vec::new(), Some(long_tok)))
            .expect_err("must reject long token");
        assert!(err.contains("MAX_TOKEN_LEN"), "reason was: {err}");
    }

    /// R5-H10: token = None は token 長検査をスキップ (= 通過)。
    #[test]
    fn handshake_accepts_no_token() {
        validate_handshake_lengths(&req_with(Vec::new(), None)).expect("must accept");
    }
}
