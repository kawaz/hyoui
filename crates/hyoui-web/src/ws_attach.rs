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

/// bridge thread への入力コマンド。
enum BridgeCmd {
    /// WS から受け取った raw bytes (= xterm.js の key input)。daemon の PTY input へ。
    Bytes(Vec<u8>),
    /// WS close 相当。bridge を Ok で終わらせる。
    Shutdown,
}

/// WS handler の中身 (= `on_upgrade` 内で呼ぶ本体)。socket_path は事前解決済み。
///
/// # Errors
/// 内部の pipe 作成 / bridge join / connect 失敗を文字列で返す (= caller は log 出力のみ、
/// レスポンス body には反映されない = WS 既に upgrade 済み)。
pub async fn run_bridge(socket: WebSocket, sock_path: PathBuf) -> Result<(), String> {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (input_tx, input_rx) = std::sync::mpsc::channel::<BridgeCmd>();
    let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let (wake_r, wake_w) = nix::unistd::pipe().map_err(|e| format!("pipe: {e}"))?;
    let wake_w = Arc::new(wake_w);

    // Task A: WS → input queue + wake pipe。
    let wake_w_a = wake_w.clone();
    let input_tx_a = input_tx;
    let reader_task = tokio::spawn(async move {
        while let Some(msg) = ws_rx.next().await {
            let bytes = match msg {
                Ok(Message::Binary(b)) => b.to_vec(),
                Ok(Message::Text(s)) => s.as_str().as_bytes().to_vec(),
                Ok(Message::Ping(_) | Message::Pong(_)) => continue, // axum が自動応答
                Ok(Message::Close(_)) => break,
                Err(_) => break,
            };
            if input_tx_a.send(BridgeCmd::Bytes(bytes)).is_err() {
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
        while let Some(bytes) = output_rx.recv().await {
            if ws_tx.send(Message::Binary(bytes.into())).await.is_err() {
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

/// blocking bridge の本体。`ClientConnection` を Rw で connect し、
/// `reader_fd` + `wake_fd` を poll しながら双方向転送する。
fn bridge_loop(
    sock_path: &Path,
    input_rx: std::sync::mpsc::Receiver<BridgeCmd>,
    wake_r: std::os::fd::OwnedFd,
    output_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) -> Result<(), String> {
    use hyoui::client::{AttachOptions, ClientConnection};
    use hyoui::protocol::{MVP_CAPS, Mode, TYPE_RAW_DATA};
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
                    if frame.ty == TYPE_RAW_DATA && output_tx.send(frame.body).is_err() {
                        // WS 側 writer が閉じた = client 離脱、正常終了。
                        return Ok(());
                    }
                    // CBOR control (LeaderNotify / ModeChange 等) や RAW_ACK は現状
                    // WS に転写しない (= xterm.js は raw stream しか消費しない)。将来
                    // UI に leader バッジ等を出したくなったら CBOR → JSON text message で
                    // 転送する余地あり。
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
            let mut shutdown = false;
            loop {
                match input_rx.try_recv() {
                    Ok(BridgeCmd::Bytes(b)) => batch.extend_from_slice(&b),
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
            if shutdown {
                return Ok(());
            }
        }
    }
}

/// axum handler の型ヘルパ。`WebSocketUpgrade::on_upgrade` から `run_bridge` を呼ぶ。
pub fn on_upgrade(ws: WebSocketUpgrade, sock_path: PathBuf) -> axum::response::Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = run_bridge(socket, sock_path).await {
            eprintln!("hyoui-web: WS attach ended: {e}");
        }
    })
}
