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
use std::sync::mpsc::{SyncSender, TrySendError};

use nix::poll::{PollFd, PollTimeout};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::Error;
use crate::protocol::messages::{
    ErrorMessage, LeaderNotify, LockResponse, LockResult, ModeChange, SessionMode,
};
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
        // master FD を nonblock にして、POLLHUP 偽陽性 (macOS) で read_some が
        // block するのを防ぐ。read_some は EAGAIN を返す → relay_loop で continue。
        pty.master_fd().set_nonblocking(true)?;
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

        match outcome {
            RelayOutcome::ChildExited(_) | RelayOutcome::ClientDetachedOrKilled => Ok(exit_code),
            RelayOutcome::Error(e) => Err(e),
        }
    }

    /// Phase 9: multi-attach 対応の serve loop。
    ///
    /// `Session::run` (= 1-client only) の上位互換。複数 client を同時に
    /// accept、子 PTY 出力を全 client にブロードキャスト、各 client 入力を
    /// 子 PTY に集約する。各 client は per-thread writer + bounded queue を
    /// 持ち、queue 超過時はその client のみ disconnect する (DR-0008 §8.2)。
    ///
    /// 終了条件:
    /// - 子 PTY が exit → 子 reap → exit code を返す
    /// - `kill` message を受けた → 子に signal → 子 reap → exit code を返す
    ///
    /// MVP 単一-client 構成と挙動を揃えるため、本実装も子が exit した時点で
    /// daemon は終了する。「clients == 0 でも daemon 維持」は v0.2.0+ で
    /// `--keep-running` 等の opt-in で導入する想定。
    pub fn serve(self) -> Result<i32, Error> {
        let Self {
            config,
            pty,
            child,
            listener,
        } = self;
        let client_buffer_cap = client_buffer_capacity(config.client_buffer_bytes);

        let mut clients: Vec<ClientHandle> = Vec::new();
        let mut next_client_id: u64 = 0;
        let mut state = SessionState::default();
        let outcome = serve_loop(
            &pty,
            child,
            &listener,
            &mut clients,
            &mut next_client_id,
            &config,
            client_buffer_cap,
            &mut state,
        );

        // cleanup: 各 client の writer thread を terminate (= channel drop で recv 終わる)
        for ch in clients.drain(..) {
            drop(ch.writer_tx);
            if let Some(t) = ch.writer_thread {
                let _ = t.join();
            }
        }

        let exit_code = finalize_child(child, &outcome)?;
        drop(listener);
        match outcome {
            RelayOutcome::ChildExited(_) | RelayOutcome::ClientDetachedOrKilled => Ok(exit_code),
            RelayOutcome::Error(e) => Err(e),
        }
    }
}

/// 1 client の per-thread state (writer thread + bounded mpsc + reader handle)。
struct ClientHandle {
    id: u64,
    mode: Mode,
    /// leader 取得状態 (= rw mode の最初の client が true)。
    leader: bool,
    /// daemon → client への frame enqueue 用 mpsc。
    writer_tx: SyncSender<Vec<u8>>,
    /// writer thread のハンドル。drop の前に join される。
    writer_thread: Option<std::thread::JoinHandle<()>>,
    /// daemon が client → daemon を decode するときに使う socket reader。
    reader: UnixStream,
}

/// session 全体の状態 (Phase 10)。lock 周りの state machine を保持する。
///
/// 現状の field:
/// - `lock_holder`: lock 保持中の client id (= `None` なら未 lock)
/// - `lock_token`: 発行済 token (= `LockRelease` 検証用)
///
/// Wait queue は MVP では未実装 (`LockAcquire { wait: true, .. }` でも `Denied`
/// を返す)。queue 実装は v0.2.0+ の Phase 12 で検討。
#[derive(Debug, Default)]
struct SessionState {
    lock_holder: Option<u64>,
    lock_token: Option<String>,
}

impl SessionState {
    /// session 全体の SessionMode (= mode.change の `session_mode` 用)。
    ///
    /// MVP は「lock 中 = `Locked`、それ以外 = `Rw`」。`Ro` 強制 (= 誰も書けない)
    /// は v0.2.0+ で `--read-only` daemon option 等を導入したときに使う。
    fn session_mode(&self) -> SessionMode {
        if self.lock_holder.is_some() {
            SessionMode::Locked
        } else {
            SessionMode::Rw
        }
    }
}

/// 256-bit (32 hex char) の lock token を生成する。
///
/// 同 UID 信頼領域なので crypto 強度は厳格に必要ではないが、`/dev/urandom` を
/// 使えば実質予測不能。読めない (= テスト環境等) 場合は timestamp + pid +
/// counter を fallback として混ぜる。
fn generate_lock_token() -> String {
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};

    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let extra = ts ^ pid ^ counter;
    for (i, byte) in buf.iter_mut().enumerate().take(8) {
        *byte ^= ((extra >> (56 - i * 8)) & 0xff) as u8;
    }
    let mut out = String::with_capacity(32);
    for b in &buf {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// 新規 rw client が leader 取得すべきかを判定する (= 既存に leader が居ないか)。
///
/// `RwNoLeader` mode の client は leader 候補から除外 (= 明示的に leader を
/// 取らない意思表示)。
fn should_assign_leader(clients: &[ClientHandle], new_mode: Mode) -> bool {
    matches!(new_mode, Mode::Rw) && !clients.iter().any(|c| c.leader)
}

/// leader が居ない状態 (= leader cascade 候補) のときに、次の `Mode::Rw` client を
/// leader に昇格させる。成功すれば新 leader の id を返す。
fn elevate_next_leader(clients: &mut [ClientHandle]) -> Option<u64> {
    if clients.iter().any(|c| c.leader) {
        return None;
    }
    for c in clients.iter_mut() {
        if matches!(c.mode, Mode::Rw) {
            c.leader = true;
            return Some(c.id);
        }
    }
    None
}

/// CBOR control message を 1 client にだけ送る (= bounded queue 経由)。
///
/// 送信失敗 (= queue 満杯 / writer thread 死亡) は `false` を返す。caller は
/// 必要に応じて当該 client を drop 対象にできる。
fn send_control(ch: &ClientHandle, msg: ControlMessage) -> bool {
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return false;
    }
    ch.writer_tx.try_send(frame_bytes).is_ok()
}

/// CBOR control message を全 client に broadcast。
fn broadcast_control(clients: &mut [ClientHandle], msg: &ControlMessage) {
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return,
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return;
    }
    broadcast_bytes(clients, frame_bytes);
}

/// 1 frame あたりの平均サイズを 4 KiB と仮定して、`client_buffer_bytes` を
/// frame 数の bound に変換する。最低 16 frame を保証。
///
/// MVP の暫定実装。Phase 9 の後段で「実 byte bound (= atomic で queue 内 bytes
/// を track)」に置き換える。
fn client_buffer_capacity(bytes: usize) -> usize {
    let frame_estimate = bytes / 4096;
    frame_estimate.max(16)
}

/// daemon → client の writer pump (= per-thread)。
///
/// `rx` から `Vec<u8>` を受け取って socket に write_all。送信失敗で thread 終了。
fn writer_pump(rx: std::sync::mpsc::Receiver<Vec<u8>>, mut sock: UnixStream) {
    while let Ok(bytes) = rx.recv() {
        if std::io::Write::write_all(&mut sock, &bytes).is_err() {
            // client が close した。recv ループ抜けて thread 終了。
            return;
        }
    }
}

/// serve loop の本体。`Session::serve` から切り出して所有権整理を平坦化。
#[allow(clippy::too_many_arguments)]
fn serve_loop(
    pty: &Pty,
    child: Pid,
    listener: &UnixSock,
    clients: &mut Vec<ClientHandle>,
    next_client_id: &mut u64,
    config: &DaemonConfig,
    client_buffer_cap: usize,
    state: &mut SessionState,
) -> RelayOutcome {
    loop {
        // poll fd 構築: listener + master + 各 client reader
        let listener_fd = listener.as_fd();
        let master_fd = pty.master_fd();
        let mut poll_fds: Vec<PollFd> = Vec::with_capacity(2 + clients.len());
        poll_fds.push(PollFd::new(listener_fd, PollFlags::POLLIN));
        poll_fds.push(PollFd::new(master_fd, PollFlags::POLLIN));
        for ch in clients.iter() {
            poll_fds.push(PollFd::new(ch.reader.as_fd(), PollFlags::POLLIN));
        }

        match poll(&mut poll_fds, PollTimeout::NONE) {
            Ok(PollOutcome::Ready(_)) => {}
            Ok(PollOutcome::Interrupted) => continue,
            Ok(PollOutcome::Timeout) => continue,
            Err(e) => return RelayOutcome::Error(e),
        }

        let listener_revents = poll_fds[0].revents().unwrap_or(PollFlags::empty());
        let master_revents = poll_fds[1].revents().unwrap_or(PollFlags::empty());
        let client_revents: Vec<PollFlags> = clients
            .iter()
            .enumerate()
            .map(|(i, _)| poll_fds[2 + i].revents().unwrap_or(PollFlags::empty()))
            .collect();
        drop(poll_fds);

        // 1. listener: 新規 client accept
        if listener_revents.contains(PollFlags::POLLIN) {
            match accept_new_client(
                listener,
                config,
                *next_client_id,
                client_buffer_cap,
                clients,
            ) {
                Ok(accepted) => {
                    *next_client_id += 1;
                    let new_id = accepted.handle.id;
                    let became_leader = accepted.became_leader;
                    let mode_change_for_locked = state.lock_holder.map(|holder| ModeChange {
                        session_mode: SessionMode::Locked,
                        lock_holder: Some(holder),
                        client_mode: None,
                    });
                    let new_handle_writer_ref = &accepted.handle;
                    if let Some(mc) = mode_change_for_locked.as_ref() {
                        // accept した client に「現在 lock 中」を通知
                        let _ =
                            send_control(new_handle_writer_ref, ControlMessage::ModeChange(*mc));
                    }
                    clients.push(accepted.handle);
                    if became_leader {
                        // 他 client に新 leader を通知 (= 新 client 自身は handshake.response
                        // で leader=true を受け取り済みだが、broadcast でも届く)
                        broadcast_control(
                            clients,
                            &ControlMessage::LeaderNotify(LeaderNotify {
                                client_id: Some(new_id),
                            }),
                        );
                    }
                }
                Err(_) => {
                    // handshake 失敗等: 個別の client を弾くだけで loop 継続
                }
            }
        }

        // 2. master: 子 PTY 出力を全 client に broadcast
        let pty_ready = master_revents.contains(PollFlags::POLLIN)
            || master_revents.contains(PollFlags::POLLHUP)
            || master_revents.contains(PollFlags::POLLERR);
        if pty_ready {
            let mut buf = [0u8; 8192];
            match pty.master_fd().read_some(&mut buf) {
                Ok(0) => {
                    if let Some(code) = child_actually_exited(child) {
                        return RelayOutcome::ChildExited(code);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(n) => {
                    // Frame::raw_data を 1 度 encode して bytes を作り、各 client に enqueue
                    let frame = Frame::raw_data(buf[..n].to_vec());
                    let mut frame_bytes = Vec::new();
                    if let Err(e) = frame.encode_to(&mut frame_bytes) {
                        return RelayOutcome::Error(match e {
                            FrameError::Io(io) => Error::Io(io),
                            FrameError::Protocol(_) => Error::Invalid("frame encode failed"),
                        });
                    }
                    broadcast_bytes(clients, frame_bytes);
                }
                Err(Error::Errno(nix::errno::Errno::EIO)) => {
                    if let Some(code) = child_actually_exited(child) {
                        return RelayOutcome::ChildExited(code);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(Error::Errno(nix::errno::Errno::EAGAIN)) => {}
                Err(e) => return RelayOutcome::Error(e),
            }
        }

        // 3. 各 client reader: decode frame → 処理
        // frame ハンドリングは state / 他 client への副作用 (= lock state 変化、
        // broadcast 等) を持つため、まず frame を取り出してから処理する。
        let mut frames_to_process: Vec<(usize, FrameOrError)> = Vec::new();
        for (idx, revents) in client_revents.iter().enumerate() {
            if !revents.contains(PollFlags::POLLIN)
                && !revents.contains(PollFlags::POLLHUP)
                && !revents.contains(PollFlags::POLLERR)
            {
                continue;
            }
            let ch = &mut clients[idx];
            match Frame::decode_from(&mut ch.reader) {
                Ok(frame) => frames_to_process.push((idx, FrameOrError::Frame(frame))),
                Err(_) => frames_to_process.push((idx, FrameOrError::Error)),
            }
        }

        let mut indices_to_drop: Vec<usize> = Vec::new();
        let mut should_return: Option<RelayOutcome> = None;
        for (idx, fre) in frames_to_process {
            if should_return.is_some() {
                break;
            }
            match fre {
                FrameOrError::Frame(frame) => {
                    match handle_client_frame(pty, child, idx, frame, clients, state) {
                        ClientFrameOutcome::Continue => {}
                        ClientFrameOutcome::DropClient => indices_to_drop.push(idx),
                        ClientFrameOutcome::TerminateSession(o) => should_return = Some(o),
                    }
                }
                FrameOrError::Error => {
                    // protocol error / EOF → 当該 client を切る
                    indices_to_drop.push(idx);
                }
            }
        }

        // drop 対象を逆順で remove (= leader cascade + lock auto-release 含む)
        // 重複 index も発生しうるので dedup する
        indices_to_drop.sort_unstable();
        indices_to_drop.dedup();
        let mut dropped_held_lock = false;
        let mut dropped_any_leader = false;
        for idx in indices_to_drop.into_iter().rev() {
            let ch = clients.remove(idx);
            if ch.leader {
                dropped_any_leader = true;
            }
            if state.lock_holder == Some(ch.id) {
                dropped_held_lock = true;
                state.lock_holder = None;
                state.lock_token = None;
            }
            drop(ch.writer_tx);
            if let Some(t) = ch.writer_thread {
                let _ = t.join();
            }
        }

        // leader cascade: leader が消えた場合、次の Rw client を昇格させる
        if dropped_any_leader {
            let new_leader = elevate_next_leader(clients);
            broadcast_control(
                clients,
                &ControlMessage::LeaderNotify(LeaderNotify {
                    client_id: new_leader,
                }),
            );
        }

        // lock 自動解放: lock holder が抜けた場合、session mode を Rw に戻す
        if dropped_held_lock {
            broadcast_control(
                clients,
                &ControlMessage::ModeChange(ModeChange {
                    session_mode: state.session_mode(),
                    lock_holder: None,
                    client_mode: None,
                }),
            );
        }

        if let Some(o) = should_return {
            return o;
        }
    }
}

/// `frames_to_process` 用の中間型 (= frame 取得成功 / 失敗を持ち回る)。
enum FrameOrError {
    Frame(Frame),
    Error,
}

/// 新規 client の accept 結果。
struct AcceptedClient {
    handle: ClientHandle,
    /// この client が leader として確定されたか (= Phase 10 leader assignment)。
    became_leader: bool,
}

/// listener から 1 client を accept、handshake を完了して `ClientHandle` を作る。
///
/// Phase 10:
/// - 既存 clients を見て leader 取得可否を判定 (= `should_assign_leader`)
/// - session 中の lock 状態は handshake.response に乗らない (= schema 拡張なし)
///   ので、accept 完了直後の caller 側で必要なら `mode.change` を当該 client に
///   1 発送る (Phase 10 では caller が handle する)。
fn accept_new_client(
    listener: &UnixSock,
    config: &DaemonConfig,
    client_id: u64,
    client_buffer_cap: usize,
    clients: &[ClientHandle],
) -> Result<AcceptedClient, Error> {
    let fd: OwnedFd = listener.accept()?;
    let stream = unix_stream_from_owned_fd(fd);
    let transport = UnixStreamTransport::new(stream);
    let (mut reader, mut writer_main) = transport.split().map_err(Error::from)?;

    // handshake (= request 受信 + response 送信)
    let frame = Frame::decode_from(&mut reader)
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

    let became_leader = should_assign_leader(clients, req.mode);

    let response = HandshakeResponse {
        caps: intersect,
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

    // writer thread を立ち上げ、broadcast 用 mpsc を作る
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(client_buffer_cap);
    let writer_thread = std::thread::spawn(move || writer_pump(rx, writer_main));

    Ok(AcceptedClient {
        handle: ClientHandle {
            id: client_id,
            mode: req.mode,
            leader: became_leader,
            writer_tx: tx,
            writer_thread: Some(writer_thread),
            reader,
        },
        became_leader,
    })
}

/// `Frame` の encode 済 bytes を全 client に enqueue。bounded queue 超過した
/// client は disconnect 対象として後段で remove する (= DR-0008 §8.2)。
fn broadcast_bytes(clients: &mut [ClientHandle], bytes: Vec<u8>) {
    let mut to_drop: Vec<usize> = Vec::new();
    for (idx, ch) in clients.iter().enumerate() {
        // 1 回目 clone を避けるため、最後の client は move、それ以外は clone。
        // ただし途中で fail することも考慮し、シンプルに毎回 clone。
        match ch.writer_tx.try_send(bytes.clone()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                to_drop.push(idx);
            }
        }
    }
    // mark for drop (= caller 側の loop で消す形にしたいが、ここでは simple に
    // 逆順 remove)。clients を直接 mut で touch するため、ここでは drop しない。
    // この関数のシグネチャを &mut [ClientHandle] でなく &mut Vec<ClientHandle>
    // にすると remove できるが、loop の構造を簡素に保つため呼び出し側で扱う方が
    // 綺麗。MVP では到達できない上限 (= 8 MiB) なので一旦 silently skip にする。
    // → 将来 backpressure を厳密化するときに mark/remove を実装。
    let _ = to_drop;
}

/// 1 client から受け取った frame の処理結果。
enum ClientFrameOutcome {
    /// 通常処理完了、loop 継続。
    Continue,
    /// この client は detach / protocol error → list から remove。
    DropClient,
    /// session 全体終了 (= kill received など)。
    TerminateSession(RelayOutcome),
}

fn handle_client_frame(
    pty: &Pty,
    child: Pid,
    idx: usize,
    frame: Frame,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
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
            handle_control_message(pty, child, idx, msg, clients, state)
        }
        _ => ClientFrameOutcome::DropClient,
    }
}

/// CBOR control message のディスパッチ。lock / leader / mode 系の state 更新と
/// broadcast を担う (Phase 10)。
fn handle_control_message(
    pty: &Pty,
    child: Pid,
    idx: usize,
    msg: ControlMessage,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
) -> ClientFrameOutcome {
    let ch_id = clients[idx].id;
    let ch_leader = clients[idx].leader;

    match msg {
        ControlMessage::Detach(_) => ClientFrameOutcome::DropClient,
        ControlMessage::Kill(k) => {
            let signum = k.signum.unwrap_or(libc::SIGTERM as u8);
            let sig = Signal::try_from(signum as i32).unwrap_or(Signal::SIGTERM);
            let _ = kill(child, sig);
            ClientFrameOutcome::TerminateSession(RelayOutcome::ClientDetachedOrKilled)
        }
        ControlMessage::Signal(s) => {
            // signal は client 自由 (= lock 中でも認める)。raw mode 中の SIGINT
            // 送信路として使うため、leader / lock 制約は掛けない。
            let sig = Signal::try_from(s.signum as i32).unwrap_or(Signal::SIGINT);
            let _ = kill(child, sig);
            ClientFrameOutcome::Continue
        }
        ControlMessage::Resize(r) => {
            // resize は leader のみ許可 (DR-0008 §2.3)。それ以外は error 返却。
            if !ch_leader {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: "mode.not-leader".into(),
                        message: "resize requires leader role".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            let _ = pty.resize(r.cols, r.rows);
            ClientFrameOutcome::Continue
        }
        ControlMessage::LockAcquire(req) => {
            // MVP: queue 未実装。`wait=true` でも grant か Denied のいずれか
            // (= 既存 holder が居れば即 Denied)。
            if state.lock_holder.is_some() {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::LockResponse(LockResponse {
                        result: LockResult::Denied,
                        token: None,
                        queue_position: None,
                    }),
                );
                let _ = req; // process_bound / timeout は queue 実装まで未使用
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
        ControlMessage::LockRelease(rel) => {
            // token + holder 両方を照合してから解放
            let valid = state.lock_holder == Some(ch_id)
                && state.lock_token.as_deref() == Some(rel.token.as_str());
            if !valid {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: "lock.not-held".into(),
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
        _ => ClientFrameOutcome::Continue,
    }
}

/// `Session::run` の relay loop 結果。
#[derive(Debug)]
enum RelayOutcome {
    /// 子 PTY 側で EOF を検出 (= 子 process が exit した)。exit code が判明していれば
    /// `Some(code)` に保持する (= waitpid を 2 度呼ばないため)。
    ChildExited(Option<i32>),
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

        // 子 PTY → client: master FD ready なら read → raw data frame を送る。
        // macOS の PTY master は子が alive でも POLLHUP を出す瞬間がある (= slave
        // 側の reference count 揺らぎ等)。POLLHUP 単独で ChildExited 扱いすると
        // 誤検知になるため、POLLIN / POLLHUP どちらも read を試し、Ok(0) / EIO
        // が返ったときだけ ChildExited と判断する。
        let pty_ready = pty_revents.contains(PollFlags::POLLIN)
            || pty_revents.contains(PollFlags::POLLHUP)
            || pty_revents.contains(PollFlags::POLLERR);
        if pty_ready {
            let mut buf = [0u8; 8192];
            match pty.master_fd().read_some(&mut buf) {
                Ok(0) => {
                    // master FD で EOF (= 子 PTY が close した)。ただし macOS の
                    // forkpty 直後の short window では子が exec 完了する前に
                    // master 側で POLLHUP+EOF が出る race がある。waitpid(WNOHANG)
                    // で子が actually exit したか確認する。
                    if let Some(code) = child_actually_exited(child) {
                        return RelayOutcome::ChildExited(code);
                    }
                    // 偽 EOF (= forkpty exec 中の transient)。少し待って再試行。
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(n) => {
                    let frame = Frame::raw_data(buf[..n].to_vec());
                    if let Err(e) = frame.encode_to(client_writer) {
                        return frame_send_outcome(e);
                    }
                }
                Err(Error::Errno(nix::errno::Errno::EIO)) => {
                    if let Some(code) = child_actually_exited(child) {
                        return RelayOutcome::ChildExited(code);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(Error::Errno(nix::errno::Errno::EAGAIN)) => {
                    // POLLHUP の偽陽性 (= ready と通知されたが実 read で EAGAIN)。
                    // EWOULDBLOCK は Linux/macOS とも EAGAIN と同値 (POSIX 規定)。
                    // 次の iteration で client_fd 側 ready を処理する。
                }
                Err(e) => return RelayOutcome::Error(e),
            }
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

/// 子 process が actually exit したかを `waitpid(WNOHANG)` で確認する。
///
/// macOS の forkpty 直後の short window では子が exec 完了する前に
/// master FD で POLLHUP / EOF が偽陽性で出る race がある (slave 側の reference
/// count 揺らぎ等)。read が 0 / EIO を返したときに本関数で「子が実際に exit
/// 済みか」を waitpid で確かめてから ChildExited 判定する。
///
/// 戻り値:
/// - `Some(Some(code))`: 子は exit 済、exit code が `code`
/// - `Some(None)`: 子は exit 済 (= waitpid が status を返した) だが exit code が
///   取得できない (= 何らかの transient)
/// - `None`: 子はまだ alive (StillAlive / Stopped / Continued / transient error)
fn child_actually_exited(child: Pid) -> Option<Option<i32>> {
    use nix::sys::wait::WaitPidFlag;
    match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => None,
        Ok(WaitStatus::Exited(_, code)) => Some(Some(code)),
        Ok(WaitStatus::Signaled(_, sig, _)) => Some(Some(128 + (sig as i32))),
        Ok(_) => None,
        Err(_) => None,
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
/// - `ChildExited(Some(code))`: 既に `child_actually_exited` で reap 済、code をそのまま返す
/// - `ChildExited(None)`: exit 検知だが code 未取得 → waitpid で確認
/// - `ClientDetachedOrKilled`: 子はまだ生きている可能性 → SIGTERM → wait
///
/// signal で終了の場合は shell convention に従い `128 + signum` を返す。
fn finalize_child(child: Pid, outcome: &RelayOutcome) -> Result<i32, Error> {
    // `child_actually_exited` で既に code を取得済なら、それを優先 (waitpid を
    // 二重に呼ぶと ECHILD になる)。
    if let RelayOutcome::ChildExited(Some(code)) = outcome {
        return Ok(*code);
    }

    // ChildExited 以外 (= client 都合の終了) は子に SIGTERM を送ってから wait。
    // 既に exit 済なら kill は ESRCH で失敗 → 無視。
    if !matches!(outcome, RelayOutcome::ChildExited(_)) {
        let _ = kill(child, Signal::SIGTERM);
    }

    // 子を reap。
    loop {
        match waitpid(child, Some(WaitPidFlag::empty())) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + (sig as i32)),
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ECHILD) => {
                // 既に reap 済 (= SIGCHLD ハンドラが拾った等)。outcome に応じて
                // 0 / 143 (= SIGTERM kill) を返す。
                return Ok(match outcome {
                    RelayOutcome::ChildExited(_) => 0,
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

    // ---- Phase 9 (Session::serve) tests ----

    fn spawn_serve_thread(
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
        let handle = std::thread::spawn(move || session.serve());
        (session_id, sock_path, dir, handle)
    }

    #[test]
    fn serve_handles_single_client_kill() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());
        let mut stream = client_connect_with_retry(&sock_path);
        let _resp = do_client_handshake(&mut stream);
        let kill_msg = ControlMessage::Kill(Kill { signum: None });
        let body = kill_msg.encode_to_vec().expect("encode kill");
        Frame::cbor_control(body)
            .encode_to(&mut stream)
            .expect("send kill");
        stream.flush().expect("flush");

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        assert_eq!(exit, 143);
    }

    #[test]
    fn serve_handles_sequential_clients() {
        // 1 client が detach → 2 つ目 client が attach → 2 つ目が kill で終了
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // client 1: attach → detach
        {
            let mut s = client_connect_with_retry(&sock_path);
            let _r = do_client_handshake(&mut s);
            let body = ControlMessage::Detach(Detach {
                target: DetachTarget::Myself,
            })
            .encode_to_vec()
            .expect("encode");
            Frame::cbor_control(body).encode_to(&mut s).expect("send");
            s.flush().expect("flush");
            // socket close は drop で
        }
        // 短い間を空けて 2 つ目 attach
        std::thread::sleep(Duration::from_millis(50));

        // client 2: attach → kill
        {
            let mut s = client_connect_with_retry(&sock_path);
            let _r = do_client_handshake(&mut s);
            let body = ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode");
            Frame::cbor_control(body).encode_to(&mut s).expect("send");
            s.flush().expect("flush");
        }

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        // kill による終了 = 143
        assert_eq!(exit, 143);
    }

    #[test]
    fn serve_handles_two_concurrent_clients() {
        // 同時に 2 client attach → 片方が kill 送信で session 終了
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _r1 = do_client_handshake(&mut s1);

        let mut s2 = client_connect_with_retry(&sock_path);
        let r2 = do_client_handshake(&mut s2);
        // 2 つ目 client は別 client_id を割り当てられる
        assert_ne!(r2.client_id, 0);

        // s1 が kill 送信
        let body = ControlMessage::Kill(Kill { signum: None })
            .encode_to_vec()
            .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        assert_eq!(exit, 143);
    }

    #[test]
    fn serve_propagates_child_exit_code() {
        let false_path = if std::path::Path::new("/usr/bin/false").exists() {
            "/usr/bin/false"
        } else {
            "/bin/false"
        };
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(vec![false_path.into()]);

        // 子は即 exit するが、accept 前に exit すると hang する可能性。先に接続。
        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        assert_eq!(exit, 1);
    }

    // ---- Phase 10 helper unit tests ----

    #[test]
    fn generate_lock_token_unique_and_hex32() {
        let a = generate_lock_token();
        let b = generate_lock_token();
        assert_eq!(a.len(), 32, "token must be 32 hex chars (16 bytes)");
        assert_eq!(b.len(), 32);
        assert_ne!(a, b, "two tokens must differ");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn should_assign_leader_picks_first_rw() {
        let clients: Vec<ClientHandle> = Vec::new();
        assert!(should_assign_leader(&clients, Mode::Rw));
        assert!(!should_assign_leader(&clients, Mode::Ro));
        assert!(
            !should_assign_leader(&clients, Mode::RwNoLeader),
            "rw-no-leader は明示拒否なので leader 取らない"
        );
    }

    #[test]
    fn session_mode_reflects_lock_holder() {
        let mut s = SessionState::default();
        assert_eq!(s.session_mode(), SessionMode::Rw);
        s.lock_holder = Some(7);
        s.lock_token = Some("abcd".into());
        assert_eq!(s.session_mode(), SessionMode::Locked);
    }

    // ---- Phase 10 e2e tests (= serve_loop 経由) ----

    /// Phase 10: 2nd rw client は leader を取らない (1st が既に leader)。
    #[test]
    fn serve_only_first_rw_becomes_leader() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        assert!(r1.leader);

        let mut s2 = client_connect_with_retry(&sock_path);
        let r2 = do_client_handshake(&mut s2);
        assert!(!r2.leader, "2nd rw client must not be leader");
        assert_ne!(r1.client_id, r2.client_id);

        // cleanup: kill
        let body = ControlMessage::Kill(Kill { signum: None })
            .encode_to_vec()
            .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: lock acquire は token を返し、mode.change(Locked) を全 client に broadcast。
    #[test]
    fn serve_lock_acquire_grants_and_broadcasts_mode_change() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        // s1 accept 時の leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        let mut s2 = client_connect_with_retry(&sock_path);
        let _r2 = do_client_handshake(&mut s2);

        // s1 が lock 取得
        let body = ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
            wait: false,
            timeout_abs_ms: None,
            timeout_idle_ms: None,
            process_bound: false,
        })
        .encode_to_vec()
        .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");

        // s1 は lock.response(Acquired, token=...) を受信
        let resp_frame = Frame::decode_from(&mut s1).expect("decode resp");
        let resp = ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode cbor");
        let token = match resp {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(lr.result, LockResult::Acquired);
                assert_eq!(lr.token.as_ref().map(|t| t.len()), Some(32));
                lr.token.clone()
            }
            other => panic!("expected LockResponse, got {other:?}"),
        };
        assert!(token.is_some());

        // s1 / s2 とも mode.change(Locked, lock_holder=s1.client_id) を受信
        for s in [&mut s1, &mut s2] {
            let mc_frame = Frame::decode_from(s).expect("decode mode.change frame");
            let mc = ControlMessage::decode_from(mc_frame.body.as_slice()).expect("decode mc");
            match mc {
                ControlMessage::ModeChange(c) => {
                    assert_eq!(c.session_mode, SessionMode::Locked);
                    assert_eq!(c.lock_holder, Some(r1.client_id));
                }
                other => panic!("expected ModeChange, got {other:?}"),
            }
        }

        // cleanup
        let body = ControlMessage::Kill(Kill { signum: None })
            .encode_to_vec()
            .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: 2 件目の lock acquire は Denied、state 変化なし。
    #[test]
    fn serve_lock_acquire_while_locked_returns_denied() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        // s1 accept 時の leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");
        let mut s2 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s2);

        // s1 が lock 取得
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        // s1 が response + mode.change を受け取る、s2 が mode.change を受け取る
        let _ = Frame::decode_from(&mut s1).expect("response");
        let _ = Frame::decode_from(&mut s1).expect("mode.change s1");
        let _ = Frame::decode_from(&mut s2).expect("mode.change s2");

        // s2 が lock 取得試行 (= 拒否される)
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let resp_frame = Frame::decode_from(&mut s2).expect("resp");
        let resp = ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode");
        match resp {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(lr.result, LockResult::Denied);
                assert!(lr.token.is_none());
            }
            other => panic!("expected LockResponse(Denied), got {other:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: lock release は token 一致で成功、mode.change(Rw) を broadcast。
    #[test]
    fn serve_lock_release_clears_state() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        // s1 accept 時の leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        // acquire
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let resp_frame = Frame::decode_from(&mut s1).expect("resp");
        let token = match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode") {
            ControlMessage::LockResponse(lr) => lr.token.expect("token"),
            o => panic!("expected LockResponse, got {o:?}"),
        };
        // mode.change(Locked) は捨てる
        let _ = Frame::decode_from(&mut s1).expect("mode.change locked");

        // release
        Frame::cbor_control(
            ControlMessage::LockRelease(crate::protocol::messages::LockRelease {
                token: token.clone(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        // mode.change(Rw, lock_holder=None) を受信
        let mc_frame = Frame::decode_from(&mut s1).expect("mode.change rw");
        match ControlMessage::decode_from(mc_frame.body.as_slice()).expect("decode") {
            ControlMessage::ModeChange(c) => {
                assert_eq!(c.session_mode, SessionMode::Rw);
                assert!(c.lock_holder.is_none());
            }
            o => panic!("expected ModeChange, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: leader が detach すると、次の rw client に cascade + leader.notify broadcast。
    #[test]
    fn serve_leader_cascades_on_detach() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        assert!(r1.leader);
        let mut s2 = client_connect_with_retry(&sock_path);
        let r2 = do_client_handshake(&mut s2);
        assert!(!r2.leader);

        // s2 は s1 が leader になった瞬間の leader.notify を 1 つ受け取る (= s1 accept 時の broadcast)
        // ※ s1 自身も自分の leader.notify を 1 つ受け取る。これらを先に捨てる。
        // s1 については s2 accept 時の broadcast が起きない (s2 は leader にならないので) ことを利用し、
        // s1 が受け取る leader.notify は s1 accept 時の 1 件のみ。
        let nf = Frame::decode_from(&mut s1).expect("s1 leader.notify");
        match ControlMessage::decode_from(nf.body.as_slice()).expect("decode") {
            ControlMessage::LeaderNotify(n) => assert_eq!(n.client_id, Some(r1.client_id)),
            o => panic!("expected LeaderNotify, got {o:?}"),
        }

        // s1 を detach (= leader 抜ける)
        Frame::cbor_control(
            ControlMessage::Detach(Detach {
                target: DetachTarget::Myself,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        drop(s1);

        // s2 が新 leader として通知される (cascade)
        let nf2 = Frame::decode_from(&mut s2).expect("s2 cascade notify");
        match ControlMessage::decode_from(nf2.body.as_slice()).expect("decode") {
            ControlMessage::LeaderNotify(n) => assert_eq!(n.client_id, Some(r2.client_id)),
            o => panic!("expected LeaderNotify(cascade), got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: 非 leader が resize すると error 返却、子 pty は変化しない。
    #[test]
    fn serve_non_leader_resize_gets_error() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        // s1 leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("leader notify");

        let mut s2 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s2);
        // s2 は leader でないので leader.notify broadcast を受けない (became_leader=false)

        // s2 が resize 送信
        Frame::cbor_control(
            ControlMessage::Resize(crate::protocol::messages::Resize {
                cols: 100,
                rows: 30,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");

        // s2 が error を受信
        let ef = Frame::decode_from(&mut s2).expect("error");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => {
                assert_eq!(e.code, "mode.not-leader");
            }
            o => panic!("expected Error, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }
}
