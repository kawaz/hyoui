//! `hyoui attach` の中核 (daemon の relay と対称)。
//!
//! `ClientConnection::connect` で socket connect + handshake、
//! `ClientConnection::run` で stdin/stdout を daemon と中継する。

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use nix::poll::{PollFd, PollTimeout};

use crate::Error;
use crate::protocol::{
    ControlMessage, Frame, FrameError, HandshakeRequest, HandshakeResponse, MVP_CAPS, Mode,
    ProtocolError, TYPE_CBOR_CONTROL, TYPE_RAW_DATA, Transport, UnixStreamTransport,
};
use crate::sys::{poll::PollFlags, poll::PollOutcome, poll::poll, socket as sys_socket};

/// `ClientConnection::connect` で渡す接続オプション (HandshakeRequest 相当)。
#[derive(Debug, Clone)]
pub struct AttachOptions {
    /// 動作 mode (DR-0006)。
    pub mode: Mode,
    /// 自分が話せる cap (= 既定は MVP_CAPS)。
    pub caps: Vec<String>,
    /// HYOUI_LOCK_TOKEN env 由来の token。
    pub token: Option<String>,
    /// 起動時 exclusive 要求。
    pub exclusive: bool,
    /// attach 時に他 client を奪取。
    pub detach_others: bool,
}

impl Default for AttachOptions {
    fn default() -> Self {
        Self {
            mode: Mode::Rw,
            caps: MVP_CAPS.iter().map(|s| (*s).to_string()).collect(),
            token: None,
            exclusive: false,
            detach_others: false,
        }
    }
}

/// daemon と確立した 1 接続。
///
/// `connect` で handshake 完了状態を持ち、`run` で stdin/stdout 中継に入る。
#[derive(Debug)]
pub struct ClientConnection {
    reader: UnixStream,
    writer: UnixStream,
    /// daemon が返した handshake response (= 確定した cap / mode / leader / session_id)。
    pub response: HandshakeResponse,
}

impl ClientConnection {
    /// socket connect + handshake を完了し `ClientConnection` を返す。
    ///
    /// # Errors
    ///
    /// * socket connect 失敗 → [`Error::Errno`]
    /// * handshake frame 送受信失敗 → [`Error::Invalid`]
    /// * daemon が `error` を返した → 後の Phase で `Error::Protocol` 等を新設予定、
    ///   現在は [`Error::Invalid`] にまとめる
    pub fn connect(socket_path: &Path, opts: AttachOptions) -> Result<Self, Error> {
        let fd = sys_socket::connect(socket_path)?;
        let stream = UnixStream::from(fd);
        let transport = UnixStreamTransport::new(stream);
        let (mut reader, mut writer) = transport.split().map_err(Error::from)?;

        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: opts.caps,
            mode: opts.mode,
            exclusive: opts.exclusive,
            detach_others: opts.detach_others,
            token: opts.token,
        });
        let body = req
            .encode_to_vec()
            .map_err(|_| Error::Invalid("handshake.request encode failed"))?;
        Frame::cbor_control(body)
            .encode_to(&mut writer)
            .map_err(|_| Error::Invalid("handshake.request frame send failed"))?;

        let resp_frame = Frame::decode_from(&mut reader)
            .map_err(|_| Error::Invalid("handshake.response decode failed"))?;
        if resp_frame.ty != TYPE_CBOR_CONTROL {
            return Err(Error::Invalid("handshake response must be CBOR control"));
        }
        let resp_msg = ControlMessage::decode_from(resp_frame.body.as_slice())
            .map_err(|_| Error::Invalid("handshake.response CBOR decode failed"))?;
        let response = match resp_msg {
            ControlMessage::HandshakeResponse(r) => r,
            ControlMessage::Error(e) => {
                return Err(Error::Invalid(if e.code == "lock.denied" {
                    "lock denied"
                } else {
                    "daemon error during handshake"
                }));
            }
            _ => return Err(Error::Invalid("unexpected response to handshake.request")),
        };
        Ok(Self {
            reader,
            writer,
            response,
        })
    }

    /// stdin / stdout を daemon と中継する。
    ///
    /// 終了条件:
    /// - socket EOF (= daemon が終了)
    /// - stdin EOF (= 呼び出し側が input stream を close、ただし通常 terminal では起きない)
    /// - protocol violation
    ///
    /// MVP: control message の送信は呼び出し側で `send_control` を別途叩く想定
    /// (= resize/signal/detach/kill 等)。`run` は raw data の中継に専念する。
    ///
    /// # Errors
    ///
    /// I/O / decode error は [`Error`] で返す。socket EOF は `Ok(())` で正常終了扱い。
    pub fn run<R: Read + AsFd, W: Write>(
        mut self,
        stdin: &mut R,
        stdout: &mut W,
    ) -> Result<(), Error> {
        loop {
            let socket_fd = self.reader.as_fd();
            let stdin_fd = stdin.as_fd();
            let mut fds = [
                PollFd::new(socket_fd, PollFlags::POLLIN),
                PollFd::new(stdin_fd, PollFlags::POLLIN),
            ];

            match poll(&mut fds, PollTimeout::NONE) {
                Ok(PollOutcome::Ready(_)) => {}
                Ok(PollOutcome::Interrupted) => continue,
                Ok(PollOutcome::Timeout) => continue,
                Err(e) => return Err(e),
            }

            let sock_revents = fds[0].revents().unwrap_or(PollFlags::empty());
            let stdin_revents = fds[1].revents().unwrap_or(PollFlags::empty());
            let _ = fds;

            // socket → stdout: frame を 1 つ decode → raw data なら stdout に出す
            if sock_revents.contains(PollFlags::POLLIN) {
                match Frame::decode_from(&mut self.reader) {
                    Ok(frame) => match frame.ty {
                        TYPE_RAW_DATA => {
                            if stdout.write_all(&frame.body).is_err() {
                                return Ok(());
                            }
                            let _ = stdout.flush();
                        }
                        TYPE_CBOR_CONTROL => {
                            // MVP: control message は warn 出さず無視 (= daemon → client
                            // は error / mode.change / leader.notify 等が来るが Phase A2
                            // では未処理)
                        }
                        _ => return Err(Error::Invalid("unknown frame type from daemon")),
                    },
                    Err(FrameError::Protocol(ProtocolError::UnexpectedEof(_))) => {
                        // daemon が close → 正常終了
                        return Ok(());
                    }
                    Err(_) => return Err(Error::Invalid("protocol error from daemon")),
                }
            } else if sock_revents.contains(PollFlags::POLLHUP)
                || sock_revents.contains(PollFlags::POLLERR)
            {
                return Ok(());
            }

            // stdin → socket: raw data frame で送る
            if stdin_revents.contains(PollFlags::POLLIN) {
                let mut buf = [0u8; 8192];
                match stdin.read(&mut buf) {
                    Ok(0) => {
                        // stdin EOF → MVP では「自分は detach」と解釈、return
                        // 本実装では detach frame は送らず単に socket close で終了
                        return Ok(());
                    }
                    Ok(n) => {
                        let frame = Frame::raw_data(buf[..n].to_vec());
                        if frame.encode_to(&mut self.writer).is_err() {
                            return Ok(());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return Ok(()),
                }
            } else if stdin_revents.contains(PollFlags::POLLHUP) {
                return Ok(());
            }
        }
    }

    /// 任意の `ControlMessage` を daemon に送る (= Resize / Signal / Kill / Detach 等)。
    ///
    /// `run` の外から (= signal handler や別 thread から) 呼ぶ用途。MVP では同期
    /// 単発送信のみ。
    ///
    /// # Errors
    ///
    /// CBOR encode / I/O 失敗時。
    pub fn send_control(&mut self, msg: &ControlMessage) -> Result<(), Error> {
        let body = msg
            .encode_to_vec()
            .map_err(|_| Error::Invalid("control message CBOR encode failed"))?;
        Frame::cbor_control(body)
            .encode_to(&mut self.writer)
            .map_err(|_| Error::Invalid("control message frame send failed"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{DaemonConfig, Session};
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_temp_socket_dir() -> TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .prefix("hyoui-client-test-")
            .tempdir()
            .expect("tempdir");
        let perm = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir.path(), perm).expect("chmod 0700");
        dir
    }

    /// daemon + client を組み合わせた e2e テスト用 fixture。
    fn spawn_daemon_and_connect_client(
        cmd: Vec<String>,
    ) -> (
        TempDir,
        std::thread::JoinHandle<Result<i32, Error>>,
        ClientConnection,
    ) {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("demo", sock.clone(), cmd);
        let session = Session::start(cfg).expect("daemon start");
        let daemon_handle = std::thread::spawn(move || session.run());

        // listener bind 完了後の socket connect は retry なしで通る想定だが、
        // CI の slow path に備えて短いリトライ
        let mut conn = None;
        for _ in 0..50 {
            match ClientConnection::connect(&sock, AttachOptions::default()) {
                Ok(c) => {
                    conn = Some(c);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let conn = conn.expect("client connect");
        (dir, daemon_handle, conn)
    }

    #[test]
    fn handshake_returns_intersect_caps() {
        let (_dir, handle, mut conn) =
            spawn_daemon_and_connect_client(vec!["/bin/sleep".into(), "30".into()]);

        assert_eq!(
            conn.response.caps,
            vec![
                "data".to_string(),
                "lock".into(),
                "tail-v1".into(),
                "wait-l0".into()
            ]
        );
        assert!(conn.response.leader);
        assert_eq!(conn.response.mode, Mode::Rw);

        // kill して daemon 終了させる
        conn.send_control(&ControlMessage::Kill(crate::protocol::messages::Kill {
            signum: None,
        }))
        .expect("send kill");
        let exit = handle.join().expect("daemon thread").expect("daemon run");
        assert_eq!(exit, 143);
    }

    #[test]
    fn run_returns_when_daemon_closes() {
        // /bin/true は即 exit → daemon の Session::run も即終了 → client run も EOF
        // で抜ける
        let true_path = if std::path::Path::new("/usr/bin/true").exists() {
            "/usr/bin/true"
        } else {
            "/bin/true"
        };
        let (_dir, handle, conn) = spawn_daemon_and_connect_client(vec![true_path.into()]);

        // stdin 側は pipe の read 端 (= write 端を即 close で EOF 状態)。
        // poll が成立し、stdin_revents=POLLHUP or stdin EOF で run が抜ける。
        let (rd, wr) = nix::unistd::pipe().expect("pipe");
        drop(wr);
        let mut stdin = std::fs::File::from(rd);
        let mut stdout = Vec::<u8>::new();
        let result = conn.run(&mut stdin, &mut stdout);
        assert!(result.is_ok());
        let exit = handle.join().expect("daemon thread").expect("daemon run");
        // race: 子 /bin/true 即 exit と client 側 stdin EOF → socket close の
        // どちらが先に daemon に検知されるかでこの値が変わる:
        // - 子 exit 検知が先 → ChildExited → exit 0
        // - client EOF が先 → ClientDetachedOrKilled → SIGTERM → exit 0 or 143
        //   (kill 時点で既に死んでれば 0、生きてれば 143)
        // 本 test は「client.run が socket EOF で正常 return」のみ重要なので
        // exit value は許容範囲を広く取る。
        assert!(exit == 0 || exit == 143, "expected 0 or 143, got {exit}");
    }
}
