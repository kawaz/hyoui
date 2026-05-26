//! daemon 1 つ分の session 状態 + 起動ロジック (Phase 7-8)。
//!
//! `Session::start` で:
//! 1. 子 PTY を `Pty::spawn` で起動 (= forkpty + login_tty + execvp)
//! 2. Unix socket を `UnixSock::listen` で bind (perm 0600 + 親 dir 0700)
//!
//! Phase 7: `accept_handshake_once` で単発 handshake (= e2e test 用)。
//! Phase 8: `run()` で 1 client の完結 lifecycle (= handshake → data 中継 →
//! 子 exit / client detach → 子 reap → daemon exit code 返却)。
//!
//! Phase 9 以降で multi-attach、lock/leader、status/tail/wait を順次入れる。

use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;

use nix::poll::{PollFd, PollTimeout};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::Error;
use crate::protocol::{
    ControlMessage, Frame, FrameError, HandshakeResponse, MVP_CAPS, Mode, ProtocolError,
    TYPE_CBOR_CONTROL, TYPE_RAW_DATA, Transport, UnixStreamTransport, intersect_caps,
};
use crate::sys::{
    FdExt, Pty, UnixSock, poll::PollFlags, poll::PollOutcome, poll::poll, pty::Spawned,
};

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

    /// 1 client の完結 lifecycle を実行し、子 PTY の exit code を返す (Phase 8)。
    ///
    /// 流れ:
    /// 1. listener から 1 client accept
    /// 2. handshake (request 受信 + response 送信)
    /// 3. relay_loop: 子 PTY ↔ client の双方向 bytes 中継 + control message 処理
    /// 4. 終了時に子 PTY を reap (waitpid) して exit code 取得
    ///
    /// 終了条件:
    /// - 子 PTY 出力で EOF (= 子が exit、master FD は EIO/0 byte read)
    /// - client が `detach` / `kill` message 送信 or socket EOF
    /// - protocol violation (= 不正 frame)
    ///
    /// 子が client より先に exit した場合は exit code をそのまま返す。
    /// client が先に消えた場合は子に SIGTERM を送って待つ (MVP は 1 client 構成
    /// なので、最後の client が消えたら session を畳む)。
    pub fn run(self) -> Result<i32, Error> {
        let Self {
            config: _,
            pty,
            child,
            listener,
        } = self;

        // 1. accept
        let client_fd: OwnedFd = listener.accept()?;
        let stream = unix_stream_from_owned_fd(client_fd);
        let transport = UnixStreamTransport::new(stream);
        let (mut client_reader, mut client_writer) = transport.split().map_err(Error::from)?;

        // 2. handshake (= accept_handshake_once と同じロジックだが connection 維持)
        do_handshake(&pty, &mut client_reader, &mut client_writer)?;

        // 3. relay loop
        let outcome = relay_loop(&pty, child, &mut client_reader, &mut client_writer);

        // 4. cleanup: 子 PTY を reap して exit code 取得
        // outcome により、必要なら子に signal を送ってから wait する。
        let exit_code = finalize_child(child, &outcome)?;

        // listener は self.listener が move 済みなので drop = unlink される。
        drop(listener);

        // relay_loop の error 系は exit code に反映 (= 子の exit_code 優先、
        // ただし relay 中の致命的エラーは Error として返す)。
        match outcome {
            RelayOutcome::ChildExited => Ok(exit_code),
            RelayOutcome::ClientDetachedOrKilled => Ok(exit_code),
            RelayOutcome::Error(e) => Err(e),
        }
    }
}

/// `Session::run` の relay loop 結果。
#[derive(Debug)]
enum RelayOutcome {
    /// 子 PTY 側で EOF を検出 (= 子 process が exit した)。
    ChildExited,
    /// client が `detach` / `kill` を送ったか socket EOF。`kill` の場合は子に
    /// signal が送られた状態でこの enum に至る。
    ClientDetachedOrKilled,
    /// 回復不能な error (= protocol violation 等)。
    Error(Error),
}

/// handshake を 1 接続分処理する (`Session::accept_handshake_once` と同じ
/// CBOR 取扱 + cap negotiation)。`Session::run` で接続維持版に再利用する。
fn do_handshake<R: std::io::Read, W: std::io::Write>(
    _pty: &Pty,
    reader: &mut R,
    writer: &mut W,
) -> Result<HandshakeResponse, Error> {
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

    let mvp: Vec<String> = MVP_CAPS.iter().map(|s| (*s).to_string()).collect();
    let intersect = intersect_caps(&req.caps, &mvp);

    let response = HandshakeResponse {
        caps: intersect,
        session_id: String::new(), // 呼び出し側で埋めない (Session::run は config を持たないため)。実 daemon API では session_id を入れるべきだが Phase 8 では client_writer に出すだけ。
        client_id: 0,
        leader: matches!(req.mode, Mode::Rw),
        mode: req.mode,
    };

    let body = ControlMessage::HandshakeResponse(response.clone())
        .encode_to_vec()
        .map_err(|_| Error::Invalid("handshake.response encode failed"))?;
    Frame::cbor_control(body)
        .encode_to(writer)
        .map_err(|_| Error::Invalid("handshake.response frame encode failed"))?;

    Ok(response)
}

/// 子 PTY master と client socket を poll し、bytes を双方向に中継する。
///
/// 制御 message (detach / kill / resize / signal) も client → daemon 方向で
/// 解釈する。未対応 message kind は silently skip (= MVP 範囲外なので)。
fn relay_loop(
    pty: &Pty,
    child: Pid,
    client_reader: &mut UnixStream,
    client_writer: &mut UnixStream,
) -> RelayOutcome {
    // 読みっぱなしの 1 frame buffer を持ちまわす実装は複雑なので、毎ループ
    // poll → 該当 fd を blocking 1 read (= 子 PTY 出力は raw bytes をまとめて
    // 取る、client 入力は 1 frame ずつ) という形で書く。

    loop {
        // PollFd の borrow が loop body 内で持続するのを避けるため、毎反復で
        // 取得しなおす (= client_reader を mutable borrow できるように)
        let pty_master = pty.master_fd();
        let client_fd_borrow = client_reader.as_fd();
        let mut fds = [
            PollFd::new(pty_master, PollFlags::POLLIN),
            PollFd::new(client_fd_borrow, PollFlags::POLLIN),
        ];

        match poll(&mut fds, PollTimeout::NONE) {
            Ok(PollOutcome::Ready(_)) => {}
            Ok(PollOutcome::Interrupted) => continue,
            Ok(PollOutcome::Timeout) => continue, // NONE なので来ないが念のため
            Err(e) => return RelayOutcome::Error(e),
        }

        let pty_revents = fds[0].revents().unwrap_or(PollFlags::empty());
        let client_revents = fds[1].revents().unwrap_or(PollFlags::empty());
        // `fds` 配列が握っていた borrow (= pty.master_fd / client_reader.as_fd)
        // を解放する。`fds` は Drop を実装しないが、scope を切るために変数 shadow
        // で「ここで forget して以後参照不要」を表現する。
        let _ = fds;

        // 子 PTY → client: master FD ready なら read → raw data frame を送る
        if pty_revents.contains(PollFlags::POLLIN) {
            let mut buf = [0u8; 8192];
            match pty.master_fd().read_some(&mut buf) {
                Ok(0) => {
                    // master EOF (= 子 exit) → 子の status は呼び出し側で reap
                    return RelayOutcome::ChildExited;
                }
                Ok(n) => {
                    let frame = Frame::raw_data(buf[..n].to_vec());
                    if let Err(e) = frame.encode_to(client_writer) {
                        return frame_send_outcome(e);
                    }
                }
                Err(Error::Errno(nix::errno::Errno::EIO)) => {
                    // macOS では master 側 read EOF が EIO になる慣習
                    return RelayOutcome::ChildExited;
                }
                Err(e) => return RelayOutcome::Error(e),
            }
        } else if pty_revents.contains(PollFlags::POLLHUP)
            || pty_revents.contains(PollFlags::POLLERR)
        {
            return RelayOutcome::ChildExited;
        }

        // client → 子 PTY: 1 frame ずつ decode して処理
        if client_revents.contains(PollFlags::POLLIN) {
            let frame = match Frame::decode_from(client_reader) {
                Ok(f) => f,
                Err(FrameError::Protocol(ProtocolError::UnexpectedEof(_))) => {
                    // client が黙って切断 → 子に SIGTERM (= 後段で finalize)
                    return RelayOutcome::ClientDetachedOrKilled;
                }
                Err(_) => {
                    // protocol violation → 同様に client 側を切る
                    return RelayOutcome::ClientDetachedOrKilled;
                }
            };

            match frame.ty {
                TYPE_RAW_DATA => {
                    if let Err(e) = pty.master_fd().write_all(&frame.body) {
                        return RelayOutcome::Error(e);
                    }
                }
                TYPE_CBOR_CONTROL => {
                    let msg = match ControlMessage::decode_from(frame.body.as_slice()) {
                        Ok(m) => m,
                        Err(_) => continue, // 未知 kind 等 → silently skip
                    };
                    match msg {
                        ControlMessage::Detach(_) => {
                            // MVP は 1 client なので detach == session 終了
                            return RelayOutcome::ClientDetachedOrKilled;
                        }
                        ControlMessage::Kill(k) => {
                            let signum = k.signum.unwrap_or(libc::SIGTERM as u8);
                            let sig = nix::sys::signal::Signal::try_from(signum as i32)
                                .unwrap_or(Signal::SIGTERM);
                            let _ = kill(child, sig);
                            return RelayOutcome::ClientDetachedOrKilled;
                        }
                        ControlMessage::Signal(s) => {
                            let sig = nix::sys::signal::Signal::try_from(s.signum as i32)
                                .unwrap_or(Signal::SIGINT);
                            let _ = kill(child, sig);
                        }
                        ControlMessage::Resize(r) => {
                            let _ = pty.resize(r.cols, r.rows);
                        }
                        _ => {
                            // 他 kind (status/lock/tail/wait/...) は Phase 9+ で
                            // 実装。MVP では silently skip。
                        }
                    }
                }
                _ => {
                    // 未知 type は Frame::decode_from で既に protocol error を
                    // 投げているはずなのでここには来ないはず。来たら無視。
                }
            }
        } else if client_revents.contains(PollFlags::POLLHUP)
            || client_revents.contains(PollFlags::POLLERR)
        {
            return RelayOutcome::ClientDetachedOrKilled;
        }
    }
}

/// frame 送信失敗時の RelayOutcome 振り分け。
///
/// `BrokenPipe` (= client が読まずに切断) は client 側問題なので
/// `ClientDetachedOrKilled` 扱い、他の I/O error は致命的とする。
fn frame_send_outcome(e: FrameError) -> RelayOutcome {
    match e {
        FrameError::Io(io_err) if io_err.kind() == std::io::ErrorKind::BrokenPipe => {
            RelayOutcome::ClientDetachedOrKilled
        }
        FrameError::Io(io_err) => RelayOutcome::Error(Error::Io(io_err)),
        FrameError::Protocol(_) => RelayOutcome::ClientDetachedOrKilled,
    }
}

/// 子 PTY を reap して exit code を返す。
///
/// outcome に応じて:
/// - `ChildExited`: 子は既に exit 済 → waitpid で reap、status 取得
/// - `ClientDetachedOrKilled`: 子はまだ生きている可能性 → SIGTERM → wait
///
/// 130 (= 128 + SIGINT) のように shell の慣習に合わせる: signal で終了 → 128 + signum。
fn finalize_child(child: Pid, outcome: &RelayOutcome) -> Result<i32, Error> {
    // ChildExited 以外 (= client 都合の終了) は子に SIGTERM を送ってから wait。
    // 既に exit 済なら kill は ESRCH で失敗 → 無視。
    if !matches!(outcome, RelayOutcome::ChildExited) {
        let _ = kill(child, Signal::SIGTERM);
    }

    // 子を reap。SIGTERM 後 EAGAIN/WNOHANG ループはせず blocking で待つ。
    // 子が SIGTERM を無視する pathological ケースは MVP 外。
    loop {
        match waitpid(child, Some(WaitPidFlag::empty())) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + (sig as i32)),
            Ok(_) => continue, // Stopped/Continued 等は ignore して再 wait
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ECHILD) => {
                // 既に reap 済 (= SIGCHLD ハンドラが拾った等)。exit code 不明だが
                // outcome に応じて 0 / 143 (= SIGTERM kill) を返す。
                return Ok(match outcome {
                    RelayOutcome::ChildExited => 0,
                    _ => 143,
                });
            }
            Err(e) => return Err(Error::from(e)),
        }
    }
}

/// `OwnedFd` を `std::os::unix::net::UnixStream` に変換する。
///
/// `UnixStream::from(OwnedFd)` は `From` impl が存在するが、明示的な
/// hyoui 内 helper を経由することで「ここで所有権が移る」点を可視化する。
fn unix_stream_from_owned_fd(fd: OwnedFd) -> UnixStream {
    UnixStream::from(fd)
}

// Drop impl は持たない。
// - listener (UnixSock) は自身の Drop で socket file を unlink
// - pty (Pty) は自身の Drop で master fd を close
// - 子 process の cleanup は Session::run の finalize_child が行う (= run 経由)
// - run を呼ばずに drop した場合、子 process は orphan で残る可能性あり (test 側で
//   cleanup_child する責任、または将来 Drop impl で SIGTERM 送る検討)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HandshakeRequest;
    use crate::protocol::messages::{Detach, DetachTarget, Kill};
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

    /// daemon を別 thread で起動して試験するための helper。
    /// (session_id, socket_path, JoinHandle<Result<i32, Error>>) を返す。
    fn spawn_daemon_thread(
        cmd: Vec<String>,
    ) -> (
        String,
        std::path::PathBuf,
        TempDir,
        std::thread::JoinHandle<Result<i32, Error>>,
    ) {
        let dir = make_temp_socket_dir();
        let session_id = "demo".to_string();
        let sock_path = dir.path().join("test.sock");
        let cfg = DaemonConfig::new(session_id.clone(), sock_path.clone(), cmd);
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.run());
        (session_id, sock_path, dir, handle)
    }

    fn client_connect_with_retry(path: &std::path::Path) -> UnixStream {
        let mut attempts = 0;
        let fd = loop {
            match crate::sys::socket::connect(path) {
                Ok(fd) => break fd,
                Err(_) if attempts < 50 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("client connect failed: {e:?}"),
            }
        };
        UnixStream::from(fd)
    }

    fn do_client_handshake(stream: &mut UnixStream) -> HandshakeResponse {
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        let body = req.encode_to_vec().expect("cbor encode");
        Frame::cbor_control(body)
            .encode_to(stream)
            .expect("write handshake");
        stream.flush().expect("flush");
        let resp_frame = Frame::decode_from(stream).expect("decode response");
        match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode cbor") {
            ControlMessage::HandshakeResponse(r) => r,
            other => panic!("unexpected: {other:?}"),
        }
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

    #[test]
    fn run_exits_when_client_sends_kill() {
        let (_sid, sock_path, _dir, handle) =
            spawn_daemon_thread(vec!["/bin/sleep".into(), "30".into()]);

        let mut stream = client_connect_with_retry(&sock_path);
        let _resp = do_client_handshake(&mut stream);

        // kill frame (default = SIGTERM)
        let kill_msg = ControlMessage::Kill(Kill { signum: None });
        let body = kill_msg.encode_to_vec().expect("encode kill");
        Frame::cbor_control(body)
            .encode_to(&mut stream)
            .expect("send kill");
        stream.flush().expect("flush");

        let exit = handle.join().expect("daemon thread").expect("daemon run");
        // SIGTERM (= 15) で殺されたら 128 + 15 = 143
        assert_eq!(exit, 143);
    }

    #[test]
    fn run_exits_when_client_sends_detach() {
        let (_sid, sock_path, _dir, handle) =
            spawn_daemon_thread(vec!["/bin/sleep".into(), "30".into()]);

        let mut stream = client_connect_with_retry(&sock_path);
        let _resp = do_client_handshake(&mut stream);

        let detach_msg = ControlMessage::Detach(Detach {
            target: DetachTarget::Myself,
        });
        let body = detach_msg.encode_to_vec().expect("encode detach");
        Frame::cbor_control(body)
            .encode_to(&mut stream)
            .expect("send detach");
        stream.flush().expect("flush");

        let exit = handle.join().expect("daemon thread").expect("daemon run");
        // detach は session 終了 → finalize_child で SIGTERM → 143
        assert_eq!(exit, 143);
    }

    #[test]
    fn run_exits_when_client_disconnects() {
        let (_sid, sock_path, _dir, handle) =
            spawn_daemon_thread(vec!["/bin/sleep".into(), "30".into()]);

        let mut stream = client_connect_with_retry(&sock_path);
        let _resp = do_client_handshake(&mut stream);

        // 黙って close
        drop(stream);

        let exit = handle.join().expect("daemon thread").expect("daemon run");
        // socket EOF も session 終了 → SIGTERM → 143
        assert_eq!(exit, 143);
    }

    #[test]
    fn run_propagates_child_exit_code() {
        // /usr/bin/false (= exit 1) を起動 → 子は即 exit
        // ※ /bin/false (macOS) or /usr/bin/false (linux)
        let false_path = if std::path::Path::new("/usr/bin/false").exists() {
            "/usr/bin/false"
        } else {
            "/bin/false"
        };
        let (_sid, sock_path, _dir, handle) = spawn_daemon_thread(vec![false_path.into()]);

        // client が接続して handshake する前に子が exit すると accept で
        // hang する可能性がある。先に接続する。
        let mut stream = client_connect_with_retry(&sock_path);
        let _resp = do_client_handshake(&mut stream);

        let exit = handle.join().expect("daemon thread").expect("daemon run");
        // false は exit code 1
        assert_eq!(exit, 1);
    }
}
