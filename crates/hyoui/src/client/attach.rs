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
use crate::protocol::messages::{Detach, DetachTarget};
use crate::protocol::{
    ControlMessage, Frame, FrameError, HandshakeRequest, HandshakeResponse, MVP_CAPS, Mode,
    ProtocolError, TYPE_CBOR_CONTROL, TYPE_RAW_DATA, Transport, UnixStreamTransport,
};

/// stdin read chunk の処理結果 (= `process_detach_prefix` の戻り値)。
#[derive(Debug, PartialEq, Eq)]
enum DetachAction {
    /// 通常通り bytes を forward。Vec が空なら no-op。
    Forward(Vec<u8>),
    /// detach が起動された (= prefix + 'd' 検知)。`Vec` は detach 起動より前の
    /// forward 分 (= 同 chunk 内で prefix 前にあった bytes)。
    TriggerDetach(Vec<u8>),
}

/// chunk 内の bytes を走査し、prefix state machine を更新しつつ forward bytes を
/// 抽出する。`prefix_armed` は呼び出し間で state を維持するため `&mut`。
///
/// 規則:
/// - `prefix_armed=false` で `prefix` byte 検知 → armed=true、forward しない
/// - `prefix_armed=true` で `DETACH_TRIGGER_BYTE` 検知 → `TriggerDetach`、以降は無視
/// - `prefix_armed=true` で `prefix` byte 検知 → literal `prefix` を forward、
///   armed=false (= escape)
/// - `prefix_armed=true` でその他 byte 検知 → prefix + 当該 byte 両方とも捨てる、
///   armed=false (= screen 慣例の "no matching command")
/// - その他: そのまま forward
fn process_detach_prefix(chunk: &[u8], prefix_armed: &mut bool, prefix: u8) -> DetachAction {
    let mut forward = Vec::with_capacity(chunk.len());
    for &b in chunk {
        if *prefix_armed {
            *prefix_armed = false;
            if b == DETACH_TRIGGER_BYTE {
                return DetachAction::TriggerDetach(forward);
            } else if b == prefix {
                forward.push(prefix); // escape
            } else {
                // unknown post-prefix key → 両 byte 捨てる
            }
        } else if b == prefix {
            *prefix_armed = true;
        } else {
            forward.push(b);
        }
    }
    DetachAction::Forward(forward)
}

/// env `HYOUI_DETACH_PREFIX` から detach prefix byte を解決する。
///
/// - 未設定 / 空 → default (= `DETACH_PREFIX_BYTE`)
/// - `"none"` / `"off"` / `"disable"` (大小文字無視) → `None` (= detach key 無効化、
///   stdin bytes はそのまま forward)
/// - `"0xNN"` (hex) → 当該 byte
/// - `"<integer>"` (decimal 0..=255) → 当該 byte
/// - `"ctrl-X"` / `"^X"` (X = a..z) → `(X - 'a' + 1)` (= ASCII C0 制御文字)
///
/// 解釈不能な値は **Err 化** して caller (= CLI の attach_command) が raw mode に
/// 入る **前に** 明示 error として stderr 出力 + exit する責務を持つ
/// (= レビュー指摘 H3: 旧版は silent fallback で raw mode 後の scrollback に
/// warning が流されて気付かれない罠だった)。
pub fn resolve_detach_prefix_from_env() -> Result<Option<u8>, String> {
    let raw = match std::env::var("HYOUI_DETACH_PREFIX") {
        Ok(v) => v,
        Err(_) => return Ok(Some(DETACH_PREFIX_BYTE)),
    };
    parse_detach_prefix(&raw).ok_or_else(|| {
        format!(
            "invalid HYOUI_DETACH_PREFIX={raw:?}: expected hex (0xNN) / decimal \
             (0..=255) / `Ctrl-X` / `^X` (X = a..z) / `none`/`off`/`disable`"
        )
    })
}

/// `HYOUI_DETACH_PREFIX` の文字列値を解釈する pure 関数 (= test 対象)。
///
/// 戻り値:
/// - `Some(Some(byte))`: 正常に prefix が決まった
/// - `Some(None)`: 明示的に detach key を無効化 (= "none"/"off"/"disable")
/// - `None`: 解釈不能 (= caller が default fallback)
fn parse_detach_prefix(raw: &str) -> Option<Option<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Some(Some(DETACH_PREFIX_BYTE));
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(lower.as_str(), "none" | "off" | "disable" | "disabled") {
        return Some(None);
    }
    // hex "0xNN"
    if let Some(hex) = lower.strip_prefix("0x") {
        if let Ok(v) = u8::from_str_radix(hex, 16) {
            return Some(Some(v));
        }
    }
    // ctrl-x / ^x
    let ctrl_letter = lower
        .strip_prefix("ctrl-")
        .or_else(|| lower.strip_prefix('^'));
    if let Some(rest) = ctrl_letter {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() {
                let byte = (c as u8) - b'a' + 1;
                return Some(Some(byte));
            }
        }
    }
    // decimal 0..=255
    if let Ok(v) = trimmed.parse::<u8>() {
        return Some(Some(v));
    }
    None
}

/// detach prefix の既定 byte (= `Ctrl-A`, 0x01)。screen 慣例。
///
/// stdin で prefix byte を 1 度押すと「prefix armed」状態になり、次の 1 byte で:
/// - `'d'` (0x64) → detach (= Detach message を送って attach 終了)
/// - prefix byte 自身 → literal prefix を forward (= escape)
/// - その他 → prefix + 当該 byte を共に **捨てる** (= screen/tmux 慣例の
///   "no matching command" 扱い)。literal forward が必要なら escape を使う
///
/// 環境変数 `HYOUI_DETACH_PREFIX` で別 byte / 無効化を選択可能 (=
/// `resolve_detach_prefix_from_env`)。
pub const DETACH_PREFIX_BYTE: u8 = 0x01;
/// detach prefix の後に来ると detach を起動する byte (= `'d'`, 0x64)。
pub const DETACH_TRIGGER_BYTE: u8 = b'd';
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
        // 万一 caller が事前 validate を忘れていた場合の defense-in-depth。
        // 通常は CLI が attach 開始前に明示 validate して exit する (H3)。
        let detach_prefix = resolve_detach_prefix_from_env()
            .map_err(|_| Error::Invalid("invalid HYOUI_DETACH_PREFIX env"))?;
        let mut detach_prefix_armed: bool = false;
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
                        // detach prefix が disabled (= env HYOUI_DETACH_PREFIX=none) なら
                        // state machine を通さず raw forward
                        if let Some(prefix) = detach_prefix {
                            let action =
                                process_detach_prefix(&buf[..n], &mut detach_prefix_armed, prefix);
                            if let DetachAction::TriggerDetach(forward_before) = &action {
                                if !forward_before.is_empty() {
                                    let frame = Frame::raw_data(forward_before.clone());
                                    let _ = frame.encode_to(&mut self.writer);
                                    let _ = self.writer.flush();
                                }
                                let detach = ControlMessage::Detach(Detach {
                                    target: DetachTarget::Myself,
                                });
                                if let Ok(body) = detach.encode_to_vec() {
                                    let _ = Frame::cbor_control(body).encode_to(&mut self.writer);
                                    let _ = self.writer.flush();
                                }
                                return Ok(());
                            }
                            if let DetachAction::Forward(forward_bytes) = action {
                                if !forward_bytes.is_empty() {
                                    let frame = Frame::raw_data(forward_bytes);
                                    if frame.encode_to(&mut self.writer).is_err() {
                                        return Ok(());
                                    }
                                }
                            }
                        } else {
                            // detach key 無効 → 全 bytes をそのまま forward
                            let frame = Frame::raw_data(buf[..n].to_vec());
                            if frame.encode_to(&mut self.writer).is_err() {
                                return Ok(());
                            }
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

    /// daemon → client への次の frame を 1 つ受信して返す。
    ///
    /// 主に `status` / `tail` / `wait` のような 1-shot CLI で使う。`run` を
    /// 呼ぶ前提の attach は内部 poll loop で frame を消費するので本 method は
    /// 不要。
    ///
    /// # Errors
    ///
    /// frame decode 失敗 (= protocol violation or socket EOF) は [`Error::Invalid`]。
    pub fn recv_frame(&mut self) -> Result<Frame, Error> {
        Frame::decode_from(&mut self.reader).map_err(|e| match e {
            FrameError::Io(io) => Error::Io(io),
            FrameError::Protocol(_) => Error::Invalid("frame decode failed"),
        })
    }

    /// daemon → client への次の **CBOR control** message を受信。raw_data frame
    /// は skip して次の CBOR frame を待つ (= attach 切替前の旧 raw_data を捨てる
    /// 用途で便利)。`buffer_raw_data` が Some なら skip した raw_data の body を
    /// そこに append (= tail follow で過渡的に raw_data を取りこぼさない)。
    ///
    /// # Errors
    ///
    /// frame decode 失敗 (= protocol violation or socket EOF) は [`Error::Invalid`]。
    /// CBOR control body の decode 失敗も同上。
    pub fn recv_control(
        &mut self,
        mut buffer_raw_data: Option<&mut Vec<u8>>,
    ) -> Result<ControlMessage, Error> {
        loop {
            let frame = self.recv_frame()?;
            if frame.ty == TYPE_CBOR_CONTROL {
                return ControlMessage::decode_from(frame.body.as_slice())
                    .map_err(|_| Error::Invalid("control message decode failed"));
            }
            if frame.ty == TYPE_RAW_DATA {
                if let Some(buf) = buffer_raw_data.as_mut() {
                    buf.extend_from_slice(&frame.body);
                }
                continue;
            }
            return Err(Error::Invalid("unexpected frame type"));
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
            .map_err(|e| match e {
                FrameError::Io(io) => Error::Io(io),
                FrameError::Protocol(_) => Error::Invalid("control message frame protocol error"),
            })?;
        self.writer.flush().map_err(Error::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{DaemonConfig, Session};
    use std::time::Duration;
    use tempfile::TempDir;

    // ---- process_detach_prefix unit tests ----

    #[test]
    fn detach_prefix_passes_through_normal_bytes() {
        let mut armed = false;
        let a = process_detach_prefix(b"hello", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a, DetachAction::Forward(b"hello".to_vec()));
        assert!(!armed);
    }

    #[test]
    fn detach_prefix_then_d_triggers_detach() {
        let mut armed = false;
        let a = process_detach_prefix(b"\x01d", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a, DetachAction::TriggerDetach(Vec::new()));
        assert!(!armed);
    }

    #[test]
    fn detach_prefix_keeps_state_across_chunks() {
        let mut armed = false;
        // chunk 1: 終端で prefix → armed=true、forward 空
        let a1 = process_detach_prefix(b"abc\x01", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a1, DetachAction::Forward(b"abc".to_vec()));
        assert!(armed);
        // chunk 2: 'd' で detach
        let a2 = process_detach_prefix(b"d", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a2, DetachAction::TriggerDetach(Vec::new()));
        assert!(!armed);
    }

    #[test]
    fn detach_prefix_escape_doubles_prefix() {
        let mut armed = false;
        let a = process_detach_prefix(b"\x01\x01", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a, DetachAction::Forward(b"\x01".to_vec()));
        assert!(!armed);
    }

    #[test]
    fn detach_prefix_unknown_key_swallows_both() {
        let mut armed = false;
        // Ctrl-A + 'x' → 両方とも捨てる
        let a = process_detach_prefix(b"\x01x", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a, DetachAction::Forward(Vec::new()));
        assert!(!armed);
    }

    #[test]
    fn detach_prefix_detach_with_preceding_bytes() {
        let mut armed = false;
        // "hello" の後に prefix+d → "hello" を forward した上で detach
        let a = process_detach_prefix(b"hello\x01d", &mut armed, DETACH_PREFIX_BYTE);
        assert_eq!(a, DetachAction::TriggerDetach(b"hello".to_vec()));
        assert!(!armed);
    }

    #[test]
    fn detach_prefix_custom_byte_works() {
        // Ctrl-B (0x02) を prefix にしても挙動同じ
        let mut armed = false;
        let a = process_detach_prefix(b"\x02d", &mut armed, 0x02);
        assert_eq!(a, DetachAction::TriggerDetach(Vec::new()));
        assert!(!armed);
        // Ctrl-A (0x01) は普通の文字として forward される
        let mut armed = false;
        let a = process_detach_prefix(b"\x01d", &mut armed, 0x02);
        assert_eq!(a, DetachAction::Forward(b"\x01d".to_vec()));
        assert!(!armed);
    }

    // ---- parse_detach_prefix ----

    #[test]
    fn parse_detach_prefix_default_when_empty() {
        assert_eq!(parse_detach_prefix(""), Some(Some(DETACH_PREFIX_BYTE)));
        assert_eq!(parse_detach_prefix("   "), Some(Some(DETACH_PREFIX_BYTE)));
    }

    #[test]
    fn parse_detach_prefix_disable_keywords() {
        for s in ["none", "NONE", "Off", "disable", "disabled"] {
            assert_eq!(parse_detach_prefix(s), Some(None));
        }
    }

    #[test]
    fn parse_detach_prefix_hex_format() {
        assert_eq!(parse_detach_prefix("0x01"), Some(Some(0x01)));
        assert_eq!(parse_detach_prefix("0x02"), Some(Some(0x02)));
        assert_eq!(parse_detach_prefix("0xFF"), Some(Some(0xFF)));
        assert_eq!(parse_detach_prefix("0xff"), Some(Some(0xFF)));
    }

    #[test]
    fn parse_detach_prefix_decimal() {
        assert_eq!(parse_detach_prefix("1"), Some(Some(1)));
        assert_eq!(parse_detach_prefix("28"), Some(Some(28)));
    }

    #[test]
    fn parse_detach_prefix_ctrl_letter() {
        assert_eq!(parse_detach_prefix("Ctrl-A"), Some(Some(0x01)));
        assert_eq!(parse_detach_prefix("ctrl-b"), Some(Some(0x02)));
        assert_eq!(parse_detach_prefix("^A"), Some(Some(0x01)));
        assert_eq!(parse_detach_prefix("^z"), Some(Some(0x1A)));
    }

    #[test]
    fn parse_detach_prefix_invalid_returns_none() {
        assert_eq!(parse_detach_prefix("garbage"), None);
        assert_eq!(parse_detach_prefix("ctrl-AB"), None);
        assert_eq!(parse_detach_prefix("0xZZ"), None);
        assert_eq!(parse_detach_prefix("999"), None); // > u8
    }

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
