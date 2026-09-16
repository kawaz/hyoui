//! DR-0027 Phase 3: WebSocket フルターミナル attach。
//!
//! 1 WS 接続 = 1 `hyoui::client::ClientConnection` (Rw)。daemon → WS は raw_data
//! frame の body を binary message として 1:1 転写、WS → daemon は受信 bytes を
//! `send_raw_bytes` (DR-0021 raw_ack 同期) で daemon に届ける。
//!
//! ## bridge の trans-async 経路
//!
//! `ClientConnection` は blocking API なので、`spawn_blocking` で単一 bridge
//! thread を起こし、そこに WS 側の tokio task から:
//!
//! - 入力 (WS → daemon) は `std::sync::mpsc` + self-pipe (wake_fd) で通知
//! - 出力 (daemon → WS) は `tokio::sync::mpsc::UnboundedSender` で non-blocking
//!   に送る (= bridge thread は sync 呼び出しでよい)
//!
//! bridge thread は `poll(reader_fd, wake_fd)` で 2 fd を同時に待ち、
//! reader ready → `recv_frame` → output_tx、wake ready → input_rx を drain → 全
//! byte を 1 frame に結合して `send_raw_bytes`。
//!
//! ## detach / エラー扱い
//!
//! - WS close (client 起因) → shutdown コマンドで bridge を終わらせ、connection
//!   drop で daemon 側 attach が cleanup される (= 明示 Detach message は送らない、
//!   socket 切断で daemon の `handle_client_disconnect` に任せる)。
//! - daemon 切断 / recv error → bridge Err で終了、writer task が WS close を送る。
//! - `send_raw_bytes` の `Error::Remote` (= daemon が ro-rejected / lock-not-held を
//!   ack で返した) は log 出すのみで bridge は継続 (= 意味論的失敗であって protocol
//!   fatal ではない)。それ以外の I/O / poison / timeout は fatal で終了。
//!
//! ## DR-0022 との関係 (WS attach と POST /input の competing)
//!
//! WS attach は `hyoui attach` と同じ意味論 (= Rw connect、auto-lock は取らない、
//! leader は daemon が決める)。POST /input 側は従来通り WEB_INPUT_AUTO_LOCK_TIMEOUT
//! (5s) で auto-lock を取る。WS attach client が leader を握っていて lock は
//! 持っていない状態では POST /input は 200 で通り、逆に WS attach 側から普通に
//! typing しても daemon の TX serialize (= master PTY への書き込み順) で
//! interleave されるだけ。「WS が入力中は POST /input を弾く」等の追加調停は
//! しない (= 既存 attach の意味論を踏襲する、kawaz 明示 2026-07-22)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use futures_util::{SinkExt, StreamExt};

use crate::contract::{
    AttachMode, ClientFrame, ErrorInfo, ServerFrame, WEB_PROTOCOL_VERSION, code,
};

/// bridge thread への入力コマンド。
enum BridgeCmd {
    /// WS から受け取った raw bytes (= xterm.js の key input)。daemon の PTY input へ。
    Bytes(Vec<u8>),
    /// Browser が提案した grid を同じ leader connection から daemon へ反映する。
    Resize {
        request_id: u64,
        cols: u16,
        rows: u16,
    },
    /// 当該 WS に対応する daemon client への leader 奪取要求。
    LeaderRequest { request_id: u64 },
    /// WS close 相当。bridge を Ok で終わらせる。
    Shutdown,
}

/// bridge thread から WS writer task への出力。
enum BridgeOutput {
    Bytes(Vec<u8>),
    Control(String),
}

/// 1 回の wake drain 内で受け取った制御操作。browser が送った順序を保存する。
enum PendingControl {
    Resize {
        request_id: u64,
        cols: u16,
        rows: u16,
    },
    LeaderRequest {
        request_id: u64,
    },
}

/// WS handler の中身 (= `on_upgrade` 内で呼ぶ本体)。socket_path は事前解決済み。
///
/// # Errors
/// 内部の pipe 作成 / bridge join / connect 失敗を文字列で返す (= caller は log 出力のみ、
/// レスポンス body には反映されない = WS 既に upgrade 済み)。
pub async fn run_bridge(socket: WebSocket, sock_path: PathBuf) -> Result<(), String> {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (input_tx, input_rx) = std::sync::mpsc::channel::<BridgeCmd>();
    let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<BridgeOutput>();
    let (wake_r, wake_w) = nix::unistd::pipe().map_err(|e| format!("pipe: {e}"))?;
    let wake_w = Arc::new(wake_w);

    // Task A: WS → input queue + wake pipe。
    //
    // 未知 `kind` / 不正 JSON の応答 (DR-0035 決定 2) は bridge thread を経由させず、
    // writer queue へ直接積む。bridge は daemon との往復を担っており、browser の
    // 形式違反はそこへ持ち込む理由が無い。
    let wake_w_a = wake_w.clone();
    let input_tx_a = input_tx;
    let reject_tx = output_tx.clone();
    let reader_task = tokio::spawn(async move {
        while let Some(msg) = ws_rx.next().await {
            let cmd = match msg {
                Ok(Message::Binary(b)) => BridgeCmd::Bytes(b.to_vec()),
                Ok(Message::Text(s)) => match serde_json::from_str::<ClientFrame>(s.as_str()) {
                    Ok(ClientFrame::Resize {
                        request_id,
                        cols,
                        rows,
                    }) => BridgeCmd::Resize {
                        request_id,
                        cols,
                        rows,
                    },
                    Ok(ClientFrame::LeaderRequest { request_id }) => {
                        BridgeCmd::LeaderRequest { request_id }
                    }
                    // 契約表には載っているが、この gateway では認証が有効に
                    // なっていない (DR-0035 決定 1)。daemon へは持ち込まない。
                    Ok(ClientFrame::AuthExtend { request_id, .. }) => {
                        let frame = ServerFrame::Error {
                            request_id: Some(request_id),
                            error: ErrorInfo::new(
                                code::UNSUPPORTED,
                                "authentication is not enabled on this gateway",
                            ),
                        };
                        if !send_or_stop(&frame, &reject_tx) {
                            break;
                        }
                        continue;
                    }
                    Err(e) => {
                        // 黙殺すると「新しい browser + 古い gateway」が検出不能に
                        // なる (DR-0035 決定 2)。requestId は JSON が壊れている
                        // 可能性があるので相関させず null で返す。
                        let frame = ServerFrame::Error {
                            request_id: None,
                            error: ErrorInfo::new(
                                code::UNKNOWN_KIND,
                                format!("unrecognized WS text frame: {e}"),
                            ),
                        };
                        if !send_or_stop(&frame, &reject_tx) {
                            break;
                        }
                        continue;
                    }
                },
                Ok(Message::Ping(_) | Message::Pong(_)) => continue, // axum が自動応答
                Ok(Message::Close(_)) => break,
                Err(_) => break,
            };
            if input_tx_a.send(cmd).is_err() {
                break;
            }
            // wake pipe に 1 byte 書いて bridge thread の poll を起こす。
            let _ = nix::unistd::write(wake_w_a.as_ref(), &[1u8]);
        }
        let _ = input_tx_a.send(BridgeCmd::Shutdown);
        let _ = nix::unistd::write(wake_w_a.as_ref(), &[1u8]);
    });

    // Task B: bridge output → WS binary。
    let writer_task = tokio::spawn(async move {
        while let Some(output) = output_rx.recv().await {
            let message = match output {
                BridgeOutput::Bytes(bytes) => Message::Binary(bytes.into()),
                BridgeOutput::Control(text) => Message::Text(text.into()),
            };
            if ws_tx.send(message).await.is_err() {
                break;
            }
        }
        // bridge 側が閉じたら WS も明示的に close。
        let _ = ws_tx.send(Message::Close(None)).await;
    });

    // Task C: blocking bridge (= ClientConnection を保持するのは単一 thread のみ、
    // reader/writer を跨ぐ split は不要 = pending_frames の順序が保たれる)。
    let sock_owned = sock_path;
    let bridge_res =
        tokio::task::spawn_blocking(move || bridge_loop(&sock_owned, input_rx, wake_r, output_tx))
            .await;

    // どちらの task も bridge 終了で自然に自走停止するが、念のため abort で明示閉じ。
    reader_task.abort();
    writer_task.abort();

    match bridge_res {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("bridge join: {e}")),
    }
}

/// intersect 済みの cap 集合が `cap` を含むか確かめる (DR-0035 決定 4)。
///
/// 渡すのは `conn.response.caps` = daemon が handshake 応答で返した **intersect
/// 済み** の集合。cap 名ごとに手で書くのをやめ、全 cap を同じ形で判定する。
/// browser は `hello.caps` から同じ事実を先に知れるので、操作を灰色に落とせる。
fn require_cap(caps: &[String], cap: &str) -> Result<(), ErrorInfo> {
    hyoui::protocol::require_cap(caps, cap).map_err(|missing| {
        ErrorInfo::new(
            hyoui::protocol::messages::ErrorCode::UnsupportedCapability.as_str(),
            format!("daemon does not support {}", missing.cap),
        )
    })
}

/// reader task から error frame を 1 つ返す。送れなくなったら `false`。
///
/// encode 失敗は契約型の不整合 (= bug) で、この経路では bridge を畳む手段が
/// 無いので log に出して読み続ける (= browser の形式違反 1 件で接続を切らない)。
fn send_or_stop(
    frame: &ServerFrame,
    tx: &tokio::sync::mpsc::UnboundedSender<BridgeOutput>,
) -> bool {
    match encode_server_frame(frame) {
        Ok(text) => tx.send(BridgeOutput::Control(text)).is_ok(),
        Err(encode_err) => {
            eprintln!("hyoui-web: encode error frame: {encode_err}");
            true
        }
    }
}

/// gateway → browser の text frame を JSON にする。
///
/// encode 失敗は契約型の不整合 (= bug) なので bridge を畳む fatal として扱い、
/// WS writer の閉塞とは区別する (= 後者は client 離脱で正常終了)。
fn encode_server_frame(frame: &ServerFrame) -> Result<String, String> {
    serde_json::to_string(frame).map_err(|e| format!("encode gateway → browser frame: {e}"))
}

/// gateway → browser の text frame を 1 つ WS へ流す。
fn send_server_frame(
    frame: &ServerFrame,
    output_tx: &tokio::sync::mpsc::UnboundedSender<BridgeOutput>,
) -> Result<(), String> {
    output_tx
        .send(BridgeOutput::Control(encode_server_frame(frame)?))
        .map_err(|_| "WS writer closed".to_string())
}

fn send_attach_info(
    response: &hyoui::protocol::messages::HandshakeResponse,
    output_tx: &tokio::sync::mpsc::UnboundedSender<BridgeOutput>,
) -> Result<(), String> {
    send_server_frame(
        &ServerFrame::AttachInfo {
            mode: AttachMode::from(response.mode),
            leader: response.leader,
        },
        output_tx,
    )
}

/// blocking bridge の本体。`ClientConnection` を Rw で connect し、
/// `reader_fd` + `wake_fd` を poll しながら双方向転送する。
fn bridge_loop(
    sock_path: &Path,
    input_rx: std::sync::mpsc::Receiver<BridgeCmd>,
    wake_r: std::os::fd::OwnedFd,
    output_tx: tokio::sync::mpsc::UnboundedSender<BridgeOutput>,
) -> Result<(), String> {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::{ControlMessage, MVP_CAPS, Mode, TYPE_CBOR_CONTROL, TYPE_RAW_DATA};
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::os::fd::AsFd;

    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: MVP_CAPS.iter().map(|s| (*s).to_string()).collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = ClientConnection::connect(sock_path, opts)
        .map_err(|e| format!("connect/handshake: {e}"))?;
    // 世代の検出点 (DR-0035 決定 3)。`attach.info` より前に 1 回だけ送る。
    // `caps` は daemon が handshake 応答で返した intersect 済みの集合で、
    // browser はこれを見て操作の可否を表示に落とす (決定 4)。
    send_server_frame(
        &ServerFrame::Hello {
            protocol: WEB_PROTOCOL_VERSION,
            version: hyoui::VERSION.to_string(),
            build_id: hyoui::BUILD_ID.map(str::to_string),
            caps: conn.response.caps.clone(),
            // 認証は DR-0036 で載る。無効な間は null (決定 1)。
            auth_expires_at: None,
        },
        &output_tx,
    )?;
    send_attach_info(&conn.response, &output_tx)?;
    // 非 leader の resize は daemon へ送れないが、browser の現在 grid 提案として保持する。
    // takeover 成功後に同じ connection から再送し、新 leader の viewport に PTY を合わせる。
    let mut latest_grid: Option<(u16, u16)> = None;

    loop {
        let reader_fd = conn.reader_fd();
        let wake_fd = wake_r.as_fd();
        let mut fds = [
            PollFd::new(reader_fd, PollFlags::POLLIN),
            PollFd::new(wake_fd, PollFlags::POLLIN),
        ];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(format!("poll: {e}")),
        }

        let reader_ready = fds[0]
            .revents()
            .map(|r| r.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR))
            .unwrap_or(false);
        let wake_ready = fds[1]
            .revents()
            .map(|r| r.contains(PollFlags::POLLIN))
            .unwrap_or(false);

        if reader_ready {
            match conn.recv_frame() {
                Ok(frame) => {
                    if frame.ty == TYPE_RAW_DATA {
                        if output_tx.send(BridgeOutput::Bytes(frame.body)).is_err() {
                            // WS 側 writer が閉じた = client 離脱、正常終了。
                            return Ok(());
                        }
                    } else if frame.ty == TYPE_CBOR_CONTROL {
                        match ControlMessage::decode_from(frame.body.as_slice()) {
                            Ok(ControlMessage::LeaderNotify(notify)) => {
                                conn.response.leader =
                                    notify.client_id == Some(conn.response.client_id);
                                send_attach_info(&conn.response, &output_tx)?;
                            }
                            Ok(ControlMessage::ModeChange(change)) => {
                                if let Some(mode) = change.client_mode {
                                    conn.response.mode = mode;
                                    send_attach_info(&conn.response, &output_tx)?;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                return Err(format!("decode daemon control message: {e}"));
                            }
                        }
                    }
                    // RAW_ACK は `send_raw_bytes` 内で同期消費されるため、通常ここには来ない。
                }
                Err(e) => return Err(format!("recv_frame: {e}")),
            }
        }

        if wake_ready {
            // wake pipe を 1 回だけ drain (= 残っても次 iteration で拾える)。
            let mut buf = [0u8; 128];
            let _ = nix::unistd::read(wake_r.as_fd(), &mut buf);
            // input queue を全部 drain し 1 frame に結合して送る (= fast typing 時の
            // frame 数削減、DR-0021 raw_ack 待ちも 1 回で済む)。Shutdown は途中で
            // 見つかれば残 bytes 送信後に return する。
            let mut batch = Vec::new();
            let mut controls = Vec::new();
            let mut shutdown = false;
            loop {
                match input_rx.try_recv() {
                    Ok(BridgeCmd::Bytes(b)) => batch.extend_from_slice(&b),
                    Ok(BridgeCmd::Resize {
                        request_id,
                        cols,
                        rows,
                    }) => controls.push(PendingControl::Resize {
                        request_id,
                        cols,
                        rows,
                    }),
                    Ok(BridgeCmd::LeaderRequest { request_id }) => {
                        controls.push(PendingControl::LeaderRequest { request_id });
                    }
                    Ok(BridgeCmd::Shutdown) => {
                        shutdown = true;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        shutdown = true;
                        break;
                    }
                }
            }
            if !batch.is_empty() {
                match conn.send_raw_bytes(&batch) {
                    Ok(()) => {}
                    Err(hyoui::Error::Remote(msg)) => {
                        // ro-rejected / lock-not-held 等の semantic error。
                        // fatal ではないので log のみ (= 将来 WS text message で
                        // client に error 表示する余地あり)。
                        eprintln!("hyoui-web: WS bridge send rejected: {msg}");
                    }
                    Err(e) => return Err(format!("send_raw_bytes: {e}")),
                }
            }
            for control in controls {
                let frame = match control {
                    PendingControl::Resize {
                        request_id,
                        cols,
                        rows,
                    } => {
                        if cols > 0 && rows > 0 {
                            latest_grid = Some((cols, rows));
                        }
                        let result = resize_on_connection(&mut conn, cols, rows, &output_tx);
                        ServerFrame::ResizeResult {
                            request_id,
                            ok: result.is_ok(),
                            error: result.err(),
                        }
                    }
                    PendingControl::LeaderRequest { request_id } => {
                        let result = leader_on_connection(&mut conn, latest_grid, &output_tx);
                        ServerFrame::LeaderResult {
                            request_id,
                            ok: result.is_ok(),
                            error: result.err(),
                        }
                    }
                };
                let text = encode_server_frame(&frame)?;
                if output_tx.send(BridgeOutput::Control(text)).is_err() {
                    return Ok(());
                }
            }
            if shutdown {
                return Ok(());
            }
        }
    }
}

/// WS bridge の daemon connection から leader 奪取を要求し、`leader.notify` または
/// `error` まで同期して結果を返す。成功後は最後に browser が提案した grid があれば
/// 同じ connection から resize し、resize 責務者の移動を即座に PTY サイズへ反映する。
fn leader_on_connection(
    conn: &mut hyoui::client::ClientConnection,
    latest_grid: Option<(u16, u16)>,
    output_tx: &tokio::sync::mpsc::UnboundedSender<BridgeOutput>,
) -> Result<(), ErrorInfo> {
    use hyoui::protocol::ControlMessage;
    use hyoui::protocol::messages::LeaderRequest;

    require_cap(&conn.response.caps, "leader-request-v1")?;

    conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| ErrorInfo::internal(format!("set leader request timeout: {e}")))?;
    let result = (|| {
        conn.send_control(&ControlMessage::LeaderRequest(LeaderRequest::default()))
            .map_err(|e| ErrorInfo::internal(format!("send leader.request: {e}")))?;

        let mut raw = Vec::new();
        loop {
            let message = conn.recv_control(Some(&mut raw)).map_err(|e| {
                ErrorInfo::internal(format!("await leader.request completion: {e}"))
            })?;
            if !raw.is_empty() {
                output_tx
                    .send(BridgeOutput::Bytes(std::mem::take(&mut raw)))
                    .map_err(|_| ErrorInfo::internal("WS writer closed"))?;
            }
            match message {
                ControlMessage::ModeChange(change) => {
                    if let Some(mode) = change.client_mode {
                        conn.response.mode = mode;
                        send_attach_info(&conn.response, output_tx).map_err(ErrorInfo::internal)?;
                    }
                }
                ControlMessage::LeaderNotify(notify) => {
                    conn.response.leader = notify.client_id == Some(conn.response.client_id);
                    send_attach_info(&conn.response, output_tx).map_err(ErrorInfo::internal)?;
                    if conn.response.leader {
                        return Ok(());
                    }
                }
                // daemon 由来の失敗は daemon の code をそのまま通す (DR-0035 決定 2)。
                ControlMessage::Error(error) => {
                    return Err(ErrorInfo::new(error.code.as_str(), error.message));
                }
                _ => {}
            }
        }
    })();
    conn.set_read_timeout(None)
        .map_err(|e| ErrorInfo::internal(format!("clear leader request timeout: {e}")))?;
    result?;

    if let Some((cols, rows)) = latest_grid {
        resize_on_connection(conn, cols, rows, output_tx).map_err(|e| {
            ErrorInfo::new(
                e.code,
                format!("leader acquired but resize failed: {}", e.message),
            )
        })?;
    }
    Ok(())
}

/// WS bridge が保持する leader connection から resize を送り、StatusResponse を
/// FIFO barrier として処理完了を確認する。途中の raw output は browser へ転送する。
fn resize_on_connection(
    conn: &mut hyoui::client::ClientConnection,
    cols: u16,
    rows: u16,
    output_tx: &tokio::sync::mpsc::UnboundedSender<BridgeOutput>,
) -> Result<(), ErrorInfo> {
    use hyoui::protocol::ControlMessage;
    use hyoui::protocol::messages::{ErrorCode, Resize, StatusQuery};

    if cols == 0 || rows == 0 {
        return Err(ErrorInfo::new(
            code::INVALID_REQUEST,
            format!("cols/rows must be > 0 (got cols={cols}, rows={rows})"),
        ));
    }
    if !conn.response.leader {
        return Err(ErrorInfo::new(
            ErrorCode::ModeNotLeader.as_str(),
            "WS attach connection is not the resize leader",
        ));
    }

    conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| ErrorInfo::internal(format!("set resize timeout: {e}")))?;
    let result = (|| {
        conn.send_control(&ControlMessage::Resize(Resize { cols, rows }))
            .map_err(|e| ErrorInfo::internal(format!("send resize: {e}")))?;
        conn.send_control(&ControlMessage::StatusQuery(StatusQuery {}))
            .map_err(|e| ErrorInfo::internal(format!("send status query: {e}")))?;

        let mut raw = Vec::new();
        loop {
            let message = conn
                .recv_control(Some(&mut raw))
                .map_err(|e| ErrorInfo::internal(format!("await resize completion: {e}")))?;
            if !raw.is_empty() {
                let bytes = std::mem::take(&mut raw);
                output_tx
                    .send(BridgeOutput::Bytes(bytes))
                    .map_err(|_| ErrorInfo::internal("WS writer closed"))?;
            }
            match message {
                ControlMessage::StatusResponse(_) => return Ok(()),
                // daemon 由来の失敗は daemon の code をそのまま通す (DR-0035 決定 2)。
                ControlMessage::Error(error) => {
                    return Err(ErrorInfo::new(error.code.as_str(), error.message));
                }
                _ => {}
            }
        }
    })();
    conn.set_read_timeout(None)
        .map_err(|e| ErrorInfo::internal(format!("clear resize timeout: {e}")))?;
    result
}

/// axum handler の型ヘルパ。`WebSocketUpgrade::on_upgrade` から `run_bridge` を呼ぶ。
pub fn on_upgrade(ws: WebSocketUpgrade, sock_path: PathBuf) -> axum::response::Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = run_bridge(socket, sock_path).await {
            eprintln!("hyoui-web: WS attach ended: {e}");
        }
    })
}

#[cfg(test)]
mod tests {
    //! frame の JSON 表現そのものは `contract.rs` の golden test が固定する。
    //! ここでは bridge が frame を組み立てる側の振る舞いを見る。

    use super::*;

    /// leader.request の cap 判定は共有ヘルパを通り、daemon の code をそのまま返す。
    #[test]
    fn leader_request_requires_the_negotiated_capability() {
        let poor: Vec<String> = vec!["data".to_string(), "lock".to_string()];
        let err = require_cap(&poor, "leader-request-v1").expect_err("cap が無いので Err");
        assert_eq!(err.code, "unsupported-capability");
        assert!(err.message.contains("leader-request-v1"), "{}", err.message);

        let rich: Vec<String> = vec!["leader-request-v1".to_string()];
        assert!(require_cap(&rich, "leader-request-v1").is_ok());
    }

    #[test]
    fn attach_info_carries_daemon_mode_and_leader_flag() {
        let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<BridgeOutput>();
        let response = hyoui::protocol::messages::HandshakeResponse {
            caps: vec!["data".to_string()],
            session_id: "s".to_string(),
            client_id: 1,
            leader: false,
            mode: hyoui::protocol::Mode::Ro,
            child_stopped: false,
        };
        send_attach_info(&response, &output_tx).expect("send attach.info");
        let BridgeOutput::Control(text) = output_rx.try_recv().expect("frame queued") else {
            panic!("attach.info must be a text frame");
        };
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(json["kind"], "attach.info");
        assert_eq!(json["mode"], "ro");
        assert_eq!(json["leader"], serde_json::Value::Bool(false));
    }
}
