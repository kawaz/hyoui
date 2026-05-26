//! daemon 1 つ分の session 状態 + 起動ロジック (Phase 7)。
//!
//! `Session::start` で:
//! 1. 子 PTY を `Pty::spawn` で起動 (= forkpty + login_tty + execvp)
//! 2. Unix socket を `UnixSock::listen` で bind (perm 0600 + 親 dir 0700)
//!
//! Phase 7 では `accept_handshake_once` で 1 client の handshake を処理する
//! 単発 API のみ提供 (= e2e test 可能な最小単位)。Phase 8 で `run()` (= data
//! 中継 + lifecycle 完結) に拡張する。

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use nix::unistd::Pid;

use crate::Error;
use crate::protocol::{
    ControlMessage, Frame, HandshakeResponse, MVP_CAPS, Mode, Transport, UnixStreamTransport,
    intersect_caps,
};
use crate::sys::{Pty, UnixSock, pty::Spawned};

use super::DaemonConfig;

/// daemon 1 つ分の起動済 session。
///
/// `Session` 自身は `Drop` で子 PTY を SIGKILL しない (= 呼び出し側が
/// graceful な lifecycle を制御する)。Phase 8 で `run()` を追加した時点で
/// 子の reap / `unlink(socket_path)` まで責任を持つ。
#[derive(Debug)]
pub struct Session {
    config: DaemonConfig,
    pty: Pty,
    child: Pid,
    listener: UnixSock,
}

impl Session {
    /// 子 PTY を spawn し、Unix socket を bind して session を立ち上げる。
    ///
    /// # Errors
    ///
    /// * `cmd` が空、または argv に NUL を含む → [`Error::Invalid`]
    /// * forkpty / execvp が失敗 → [`Error::Errno`]
    /// * socket parent dir が mode 0700 でない → [`Error::Precondition`]
    /// * bind / listen が失敗 → [`Error::Errno`]
    pub fn start(config: DaemonConfig) -> Result<Self, Error> {
        if config.cmd.is_empty() {
            return Err(Error::Invalid("DaemonConfig::cmd must not be empty"));
        }
        let argv: Vec<&str> = config.cmd.iter().map(String::as_str).collect();
        let Spawned { pty, child } = Pty::spawn(&argv, config.cols, config.rows)?;
        let listener = UnixSock::listen(&config.socket_path)?;
        Ok(Self {
            config,
            pty,
            child,
            listener,
        })
    }

    /// session 名 (handshake response 用 + status 表示用)。
    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    /// 子 PTY の PID。
    pub fn child_pid(&self) -> Pid {
        self.child
    }

    /// listener が bind している socket path。
    pub fn socket_path(&self) -> &std::path::Path {
        self.listener.path()
    }

    /// 子 PTY master fd (= 後の Phase で broadcast/multiplex に使用)。
    pub fn pty(&self) -> &Pty {
        &self.pty
    }

    /// 1 client の handshake を完了させる (Phase 7 用、Phase 8 で置き換え)。
    ///
    /// 流れ:
    /// 1. `listener.accept()` で client fd を取る
    /// 2. 最初の Frame を読み、`type=0x01` + `kind="handshake.request"` を期待
    /// 3. cap negotiation (MVP_CAPS と intersect)
    /// 4. `client_id = 0` 固定で `handshake.response` を返す
    /// 5. client_id 0 を leader として割り当てる
    ///
    /// 返り値は (client_id, handshake response の中身) のタプル。
    ///
    /// 返した時点で client connection は close されている (= 後続 Phase で
    /// connection 持続版に差し替え)。
    pub fn accept_handshake_once(&self) -> Result<(u64, HandshakeResponse), Error> {
        let client_fd: OwnedFd = self.listener.accept()?;
        let stream = unix_stream_from_owned_fd(client_fd);
        let transport = UnixStreamTransport::new(stream);
        let (mut reader, mut writer) = transport.split().map_err(Error::from)?;

        let frame = Frame::decode_from(&mut reader)
            .map_err(|_| Error::Invalid("failed to decode handshake frame"))?;
        if frame.ty != crate::protocol::TYPE_CBOR_CONTROL {
            return Err(Error::Invalid("handshake frame must be CBOR control"));
        }
        let msg = ControlMessage::decode_from(frame.body.as_slice())
            .map_err(|_| Error::Invalid("handshake CBOR decode failed"))?;
        let req = match msg {
            ControlMessage::HandshakeRequest(r) => r,
            _ => return Err(Error::Invalid("first message must be handshake.request")),
        };

        // cap negotiation
        let mvp: Vec<String> = MVP_CAPS.iter().map(|s| (*s).to_string()).collect();
        let intersect = intersect_caps(&req.caps, &mvp);

        // mode は request をそのまま採用 (= Phase 10 で lock/leader 制約に基づく
        // 上書きを実装)。client_id = 0、Phase 9 で multi-attach の採番に差し替え。
        let response = HandshakeResponse {
            caps: intersect,
            session_id: self.config.session_id.clone(),
            client_id: 0,
            leader: matches!(req.mode, Mode::Rw),
            mode: req.mode,
        };

        let body = ControlMessage::HandshakeResponse(response.clone())
            .encode_to_vec()
            .map_err(|_| Error::Invalid("handshake.response encode failed"))?;
        Frame::cbor_control(body)
            .encode_to(&mut writer)
            .map_err(|_| Error::Invalid("handshake.response frame encode failed"))?;

        // reader/writer drop で client connection close。
        Ok((response.client_id, response))
    }
}

/// `OwnedFd` を `std::os::unix::net::UnixStream` に変換する。
///
/// `UnixStream::from(OwnedFd)` は `From` impl が存在するが、明示的な
/// hyoui 内 helper を経由することで「ここで所有権が移る」点を可視化する。
fn unix_stream_from_owned_fd(fd: OwnedFd) -> UnixStream {
    UnixStream::from(fd)
}

impl Drop for Session {
    fn drop(&mut self) {
        // 子 PTY は SIGKILL しない (Phase 8 で graceful lifecycle を実装)。
        // listener (UnixSock) は自身の Drop で socket file を unlink する。
        // 子が orphan で残る可能性は MVP までは許容 (= test 終了後 process が
        // 確実に reap される責任は test 側にある)。
        let _ = self.child;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HandshakeRequest;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_temp_socket_dir() -> TempDir {
        let dir = tempfile::Builder::new()
            .prefix("hyoui-test-")
            .tempdir()
            .expect("tempdir");
        // parent dir を mode 0700 にする (UnixSock::listen の前提)
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir.path(), perm).expect("chmod 0700");
        dir
    }

    fn long_running_cmd() -> Vec<String> {
        // 30 秒 sleep。test 中に確実に alive。
        vec!["/bin/sleep".into(), "30".into()]
    }

    fn cleanup_child(pid: Pid) {
        // 子 process が orphan で残らないように SIGKILL → wait。
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        let _ = nix::sys::wait::waitpid(pid, None);
    }

    #[test]
    fn start_spawns_child_and_binds_socket() {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("test", sock.clone(), long_running_cmd());
        let session = Session::start(cfg).expect("start");

        assert_eq!(session.session_id(), "test");
        assert_eq!(session.socket_path(), sock.as_path());
        assert!(session.pty().master_fd().as_raw_fd() >= 0);

        let pid = session.child_pid();
        drop(session); // Drop で listener が unlink される
        cleanup_child(pid);
        assert!(!sock.exists(), "socket should be unlinked on Drop");
    }

    #[test]
    fn start_rejects_empty_cmd() {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("test", sock, Vec::<String>::new());
        let err = Session::start(cfg).expect_err("must error");
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn accept_handshake_once_completes() {
        let dir = make_temp_socket_dir();
        let sock_path = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("demo", sock_path.clone(), long_running_cmd());
        let session = Session::start(cfg).expect("start");
        let pid = session.child_pid();

        // client thread: connect + handshake.request 送信 + response 受信
        let client_thread = std::thread::spawn(move || -> HandshakeResponse {
            // race 回避: daemon が listen 開始しているか軽くリトライ
            let mut attempts = 0;
            let client_fd = loop {
                match crate::sys::socket::connect(&sock_path) {
                    Ok(fd) => break fd,
                    Err(_) if attempts < 50 => {
                        attempts += 1;
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("client connect failed: {e:?}"),
                }
            };

            let mut stream = UnixStream::from(client_fd);
            let req = ControlMessage::HandshakeRequest(HandshakeRequest {
                caps: vec!["data".into(), "lock".into(), "snapshot-v1".into()],
                mode: Mode::Rw,
                exclusive: false,
                detach_others: false,
                token: None,
            });
            let body = req.encode_to_vec().expect("cbor encode");
            Frame::cbor_control(body)
                .encode_to(&mut stream)
                .expect("write frame");
            stream.flush().expect("flush");

            let resp_frame = Frame::decode_from(&mut stream).expect("decode response");
            assert_eq!(resp_frame.ty, crate::protocol::TYPE_CBOR_CONTROL);
            let resp_msg = ControlMessage::decode_from(resp_frame.body.as_slice())
                .expect("decode response cbor");
            match resp_msg {
                ControlMessage::HandshakeResponse(r) => r,
                other => panic!("unexpected: {other:?}"),
            }
        });

        // daemon thread (= main): accept + handshake
        let (client_id, response) = session.accept_handshake_once().expect("handshake");

        let resp_via_client = client_thread.join().expect("client thread");

        // 検証
        assert_eq!(client_id, 0);
        assert_eq!(response.session_id, "demo");
        assert_eq!(response.client_id, 0);
        assert!(response.leader, "rw mode の最初の client は leader 取れる");
        // cap negotiation: client 側の caps と MVP_CAPS の intersect
        // (snapshot-v1 は MVP 外なので落ちる)
        assert_eq!(response.caps, vec!["data".to_string(), "lock".to_string()]);

        // server 送信内容 = client 受信内容
        assert_eq!(response, resp_via_client);

        drop(session);
        cleanup_child(pid);
    }
}
