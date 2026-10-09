//! 一発 CLI (`set` / `status` / `input` 等) の相手をする最小の偽 daemon。
//!
//! 本物の daemon では作れない frame の順序 (= 応答の前に通知を挟む、応答を返さずに接続を
//! 閉じる) を決まった形で作るために使う。handshake までを肩代わりし、その後の振る舞いは
//! test が `script` で書く。

#![allow(dead_code)] // 各 test は subset しか使わないため

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use hyoui::protocol::messages::HandshakeResponse;
use hyoui::protocol::{ControlMessage, Frame, FrameError, ProtocolError, TYPE_CBOR_CONTROL};

/// 偽 daemon 側の 1 回の読み出しの上限 (= client が来ない / 送らない時に test を止めない)。
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// listen 中の偽 daemon。socket は `dir` の下に置く。
pub struct MockDaemon {
    dir: tempfile::TempDir,
    socket: PathBuf,
    server: JoinHandle<()>,
}

impl MockDaemon {
    /// 1 つの接続を受け、handshake に `caps` を返してから `script` を走らせる。
    /// `script` が戻ると接続を閉じる (= client からは EOF に見える)。
    pub fn spawn(
        caps: &[&str],
        script: impl FnOnce(&mut UnixStream) + Send + 'static,
    ) -> MockDaemon {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("mock.sock");
        let listener = UnixListener::bind(&socket).expect("bind mock daemon");
        let caps: Vec<String> = caps.iter().map(|c| (*c).to_string()).collect();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream
                .set_read_timeout(Some(READ_TIMEOUT))
                .expect("set read timeout");
            let mode = match recv(&mut stream) {
                ControlMessage::HandshakeRequest(req) => req.mode,
                other => panic!("expected handshake.request, got {other:?}"),
            };
            send(
                &mut stream,
                &ControlMessage::HandshakeResponse(HandshakeResponse {
                    caps,
                    session_id: "mock-daemon".into(),
                    client_id: 1,
                    leader: false,
                    mode,
                    child_stopped: false,
                }),
            );
            script(&mut stream);
        });
        MockDaemon {
            dir,
            socket,
            server,
        }
    }

    /// client に渡す socket path。
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// 偽 daemon の thread の終了を待つ (= `script` 内の assert の失敗をここで拾う)。
    pub fn join(self) {
        self.server.join().expect("mock daemon thread");
        drop(self.dir);
    }
}

/// control message を 1 つ送る。
pub fn send(stream: &mut UnixStream, msg: &ControlMessage) {
    Frame::cbor_control(msg.encode_to_vec().expect("encode control"))
        .encode_to(stream)
        .expect("send control");
}

/// control message を 1 つ受ける。
pub fn recv(stream: &mut UnixStream) -> ControlMessage {
    let frame = Frame::decode_from(stream).expect("decode frame");
    assert_eq!(frame.ty, TYPE_CBOR_CONTROL, "expected a control frame");
    ControlMessage::decode_from(frame.body.as_slice()).expect("decode control")
}

/// client が接続を閉じるまで読み捨てる (= client の終了を待つ)。
pub fn drain_until_closed(stream: &mut UnixStream) {
    loop {
        match Frame::decode_from(stream) {
            Ok(_) => continue,
            Err(FrameError::Protocol(ProtocolError::UnexpectedEof(_))) => return,
            Err(FrameError::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                return;
            }
            Err(e) => panic!("unexpected read error while draining: {e:?}"),
        }
    }
}
