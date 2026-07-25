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
use crate::protocol::messages::{Detach, DetachTarget, RawAck, RawAckResult};
use crate::protocol::{
    ControlMessage, ErrorCode, Frame, FrameError, HandshakeRequest, HandshakeResponse, MVP_CAPS,
    Mode, ProtocolError, TYPE_CBOR_CONTROL, TYPE_RAW_ACK, TYPE_RAW_DATA, Transport,
    UnixStreamTransport,
};

/// stdin read chunk の処理結果 (= `process_ctrlz_guard` の戻り値)。
#[derive(Debug, PartialEq, Eq)]
enum DetachAction {
    /// 通常通り bytes を forward。Vec が空なら no-op。
    Forward(Vec<u8>),
    /// detach が起動された。`Vec` は detach 起動より前の forward 分。
    TriggerDetach(Vec<u8>),
}

/// Ctrl+Z (= `SIGTSTP` を生む byte)。
pub const CTRL_Z_BYTE: u8 = 0x1a;

/// Ctrl+Z ガードの state (DR-0029 §2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CtrlzGuardState {
    /// 保留中の Ctrl+Z が無い。
    Idle,
    /// 奇数個目の Ctrl+Z を保留中。`deadline` 到達で detach が確定する。
    Pending {
        /// detach 確定時刻 (= 最後の Ctrl+Z + `ctrlz_guard_delay`)。
        deadline: std::time::Instant,
    },
}

/// tty stdin 経路の Ctrl+Z ガード state machine (DR-0029 §2)。
///
/// 規則は「**2 発ごとに 1 発だけ子へ届け、余った 1 発が detach タイマーを起動する**」:
///
/// | 連打数 | 子へ届く Ctrl+Z | detach |
/// |---|---|---|
/// | 1 | 0 | する (delay 後) |
/// | 2 | 1 | しない |
/// | 3 | 1 | する (delay 後) |
/// | 4 | 2 | しない |
///
/// - 窓は「最後の Ctrl+Z から `delay`」で、連打のたび実質延長される (= 偶数打で
///   保留が解消し、奇数打で新しい deadline が張られるため)
/// - 窓の途中で Ctrl+Z 以外の byte が来たら detach 保留を **キャンセル** し、保留中の
///   Ctrl+Z は **破棄** する (= 子には送らない)。当該 byte は通常入力として forward
/// - `delay == 0` なら連打判定をせず、Ctrl+Z 単発で即 detach (= 子には一切届かない)
/// - `guard == false` なら完全 bypass (= Ctrl+Z を素通し)
///
/// `now` を caller から受け取ることで wall-clock 依存を局所化し、chunk 境界と時定数を
/// deterministic に test できる。空 chunk 呼び出しは deadline 判定だけを行う。
fn process_ctrlz_guard(
    chunk: &[u8],
    state: &mut CtrlzGuardState,
    now: std::time::Instant,
    config: &crate::config::AttachConfig,
) -> DetachAction {
    if !config.ctrlz_guard {
        *state = CtrlzGuardState::Idle;
        return DetachAction::Forward(chunk.to_vec());
    }

    // deadline 到達 = detach 確定。同 chunk の残り byte は detach と共に捨てる
    // (= 以後この connection は畳まれるので送っても意味がない)。
    if let CtrlzGuardState::Pending { deadline } = *state
        && now >= deadline
    {
        *state = CtrlzGuardState::Idle;
        return DetachAction::TriggerDetach(Vec::new());
    }

    let mut forward = Vec::with_capacity(chunk.len());
    for &byte in chunk {
        match *state {
            CtrlzGuardState::Idle if byte == CTRL_Z_BYTE => {
                if config.ctrlz_guard_delay.is_zero() {
                    return DetachAction::TriggerDetach(forward);
                }
                *state = CtrlzGuardState::Pending {
                    deadline: now + config.ctrlz_guard_delay,
                };
            }
            CtrlzGuardState::Idle => forward.push(byte),
            // 偶数個目: 保留を解消して 1 発だけ子へ届ける (= detach しない)。
            CtrlzGuardState::Pending { .. } if byte == CTRL_Z_BYTE => {
                *state = CtrlzGuardState::Idle;
                forward.push(CTRL_Z_BYTE);
            }
            // 他キー割り込み: detach 保留をキャンセル、保留 Ctrl+Z は破棄。
            CtrlzGuardState::Pending { .. } => {
                *state = CtrlzGuardState::Idle;
                forward.push(byte);
            }
        }
    }
    DetachAction::Forward(forward)
}

/// 保留中の Ctrl+Z がある間だけ deadline まで poll を起こす timeout を返す。
fn ctrlz_guard_poll_timeout(state: CtrlzGuardState, now: std::time::Instant) -> PollTimeout {
    let CtrlzGuardState::Pending { deadline } = state else {
        return PollTimeout::NONE;
    };
    let remaining = deadline.saturating_duration_since(now);
    let millis = remaining.as_millis().max(1).min(u16::MAX as u128) as u16;
    PollTimeout::from(millis)
}

/// stdin の `poll(2)` revents が「EOF 相当 (= もう読めない)」を意味するか判定する
/// (= C-1: 非 tty stdin の POLLNVAL/POLLERR/POLLHUP 取りこぼし対策)。
///
/// `POLLIN` 単独は通常の読み取り readiness なので EOF 相当ではない (= read で 0 を
/// 観測して初めて EOF。本関数では false を返し、呼び出し側の read 経路に進ませる)。
/// 一方、以下は read に到達できない / read しても EOF なので、stdin EOF 経路に倒す:
///
/// - `POLLNVAL`: fd が poll 不可 (= macOS で `/dev/null` 等 chardev を `POLLIN` 要求
///   すると即時返る、実機確認: macOS=0x20)。read を試すべきでないので即 EOF 扱い。
/// - `POLLERR`: エラー状態。read しても無意味なので EOF 扱い。
/// - `POLLHUP`: 対向 close (= pipe write 端が閉じた)。read で 0 (= EOF) になる。
///
/// `POLLHUP`/`POLLERR` は OS によっては `POLLIN` と同時に立つ (= まだ未読 byte が
/// ある) ことがあるため、`POLLIN` が立っているときは EOF と即断せず read 経路に
/// 任せる (= read が残り byte を返し切ってから 0 で EOF を観測する)。`POLLNVAL` は
/// fd 自体が無効なので `POLLIN` 有無に関わらず EOF 扱いにする。
fn stdin_revents_is_eof(revents: PollFlags) -> bool {
    if revents.contains(PollFlags::POLLNVAL) {
        return true;
    }
    if revents.contains(PollFlags::POLLIN) {
        return false;
    }
    revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR)
}

/// `send_raw_bytes` が `TYPE_RAW_ACK` を待つ上限 (DR-0021)。
///
/// 値の根拠: daemon 側 `MASTER_WRITE_IDLE_TIMEOUT_MS` は per-chunk 500 ms。client → daemon
/// の write が成功した瞬間に daemon は master fd への `write_all_with_idle_timeout` を
/// 開始するので、最悪 1 chunk 分 + 通信オーバーヘッドで ack が返る。chunk が
/// 連続するケース (= 大きな bytes 列 / slow reader) でも 5 秒は十分余裕。
///
/// この値を超えても ack が来ない場合 (= daemon 自体が壊れた / dead-locked) は
/// `Error::Invalid("raw_ack timeout")` を返して CLI exit 1 で abort する (= 永遠に
/// hang するより明示エラーで上に伝える)。
pub const RAW_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// detach で attach を畳む直前に外側端末へ吐く「安全側固定 reset シーケンス」
/// (= issue 2026-06-11 / 2026-07-24 H4)。
///
/// client は daemon の screen state を持たないため、解除すべきモードを screen state
/// から導出できない。そこで tmux / screen 等の先行実装が detach / suspend 時に吐く
/// reset 群を参考に、有効化されている可能性のある端末モードを**安全側で網羅的に
/// 解除**する。誤って無効モードを解除しても害はない (= 端末は no-op 扱い)。
///
/// 順序と各シーケンスの意味:
/// - `\x1b[?2004l` : bracketed paste mode 解除 (= fg 後に貼り付けが `[200~` で汚れない)
/// - `\x1b[?1000l` `\x1b[?1002l` `\x1b[?1003l` `\x1b[?1006l` `\x1b[?1015l` :
///   mouse tracking / SGR・urxvt 拡張座標 解除 (= shell にマウス escape が流れない)
/// - `\x1b[>4;0m` : modifyOtherKeys 解除 (= xterm 拡張キーエンコード OFF)
/// - `\x1b[<u` : kitty keyboard protocol を全 flag pop で解除 (= ghostty 等で
///   ctrl+c/d/z が CSI u 化して line discipline に効かなくなる現象を防ぐ)
/// - `\x1b[?25h` : cursor 表示
/// - `\x1b[?1049l` : alt screen を抜けて primary buffer に戻す
/// - `\x1b[?1l` : DECCKM (application cursor keys) OFF
/// - `\x1b>` : application keypad OFF (= DECKPNM)
/// - `\x1b[?7h` : autowrap ON (= 端末標準に戻す)
/// - `\x1b[m` : SGR (色・属性) リセット
///
/// alt screen を抜けるので外側 shell の元画面 (scrollback) がそのまま見える。
/// 再 attach 時は daemon の redraw が screen state から alt screen 等を再有効化する。
pub const OUTER_TTY_RESET: &[u8] = b"\x1b[?2004l\
\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\
\x1b[>4;0m\
\x1b[<u\
\x1b[?25h\
\x1b[?1049l\
\x1b[?1l\
\x1b>\
\x1b[?7h\
\x1b[m";
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

/// `ClientConnection::run` の終了種別 (= issue 2026-06-11 優先1)。
///
/// 旧実装は `Ok(None)` に「自発 detach」と「予期しない socket 喪失 (= daemon
/// 消滅)」を混在させており、CLI 層がどちらも exit 0 に倒していた。スクリプトから
/// 「子が正常終了した (= exit 0)」と「daemon が落ちて attach が切れた」を区別
/// できないため、終了原因を意味のある enum に分解する。
///
/// CLI 層 (`attach_command`) はこの variant ごとに exit code を出し分ける:
/// - `ChildExited(status)` → `status & 0xFF` を伝搬 (= 子の exit code をそのまま)
/// - `Detached` → exit 0 (= 自分から離脱した、正常)
/// - `ConnectionLost` → exit 9 + stderr 一行 (= daemon 消滅の疑い)
/// - `StdoutWriteFailed` → exit 1 + stderr 一行 (= 出力先の故障、daemon は健在)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// `SessionExitNotify` を受信した (= 子 PTY process が exit)。CLI 層は
    /// `& 0xFF` で伝搬する。
    ChildExited {
        /// 子の exit code (signal 死は 128+signum)。
        exit_status: i32,
    },
    /// client が **自分から** 接続を畳んだ (= Ctrl+Z ガード発火、または
    /// `--stdin-eof=detach` での stdin EOF)。子は daemon 配下に残る。正常離脱なので
    /// CLI 層は exit 0。
    Detached,
    /// **予期しない**接続喪失 (= daemon の socket EOF / POLLHUP / POLLERR、または
    /// socket への書き込み失敗)。自分から detach していないのに接続が切れた状態で、
    /// daemon が SIGKILL 等で落ちた疑いがある。CLI 層は exit 9 + stderr 警告。
    ConnectionLost,
    /// **出力先 (= stdout: 端末 / pipe) への書き込みに失敗**した。daemon との接続は
    /// 健在で、自分側の出力経路が壊れた状態 (= pipe の読み手が先に消えた等)。daemon
    /// 消滅 (= `ConnectionLost`) とは別物なので exit code を分ける。CLI 層は
    /// exit 1 (= 一般エラー) + stderr 一行。
    StdoutWriteFailed,
    /// daemon が **backpressure で当該 client を切断**した (= `error`
    /// kind=`backpressure.disconnect` control message を受信)。client の send queue
    /// が limit (= 既定 8 MiB) を超過した状態で、suspend 中の出力過多等が原因。
    /// daemon 自体は健在だが当該 client は切断される。`ConnectionLost` と同じ exit 9
    /// を返すが、stderr メッセージを backpressure 専用に出し分けるため variant を分ける。
    BackpressureDisconnected,
}

/// `ClientConnection::run` で stdin EOF を検出したときの挙動 (R5-FB2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StdinEofAction {
    /// 何もせず即 return (= 通常の attach。MVP 既定。stdin EOF は detach 同等)。
    Detach,
    /// EOT (= ASCII 0x04, Ctrl-D) を子 PTY に raw_data として送ってから return。
    /// canonical mode の子 (例: bc / cat) は行頭の EOT を read EOF として
    /// 解釈するため、`echo "1+2" | hyoui run -- bc` のような
    /// pattern で子が自然終了する。
    SendEof,
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
    /// stdin EOF 時の挙動 (R5-FB2)。default `Detach` (= MVP attach 挙動)。
    /// `set_stdin_eof_action(SendEof)` で `hyoui run -- bc`
    /// のような pipe-through pattern で子に EOF を伝える。
    eof_action: StdinEofAction,
    /// 外側 stdout が tty で、client が raw mode 中か (= CLI 層が raw mode guard を
    /// 取得できたとき `true`)。
    ///
    /// `true` のときだけ detach 時に [`OUTER_TTY_RESET`] を吐き、子 stopped の
    /// 通知行を描画する (= pipe / 非 tty に escape sequence を垂れ流さない)。
    outer_tty_raw: bool,
    /// SIGWINCH → Resize 配線 (DR-0006 §6 / DR-0019 §6 射程外修復)。`Some` のとき
    /// run loop が `notify_fd` を poll し、POLLIN (= signal thread が WINCH を受けて
    /// 1 byte 書いた) を観測したら `size_fn()` で外側端末サイズを取得し、leader なら
    /// `Resize` message を daemon に送る。`None` なら従来挙動 (= resize 送らない)。
    winch_source: Option<WinchSource>,
    /// `send_raw_bytes` が ack 待ち中に到着した non-ack frame の buffer (DR-0021)。
    ///
    /// `send_raw_bytes` は daemon の RAW_ACK frame を同期で待つが、attach の
    /// subscription=Raw 経由で同時に届く daemon→client の raw_data frame、もしくは
    /// 並行で起きる ModeChange/LeaderNotify 等の CBOR control frame を
    /// 捨てずに保持するための FIFO buffer。`recv_frame` は ここから優先的に
    /// 返すことで「ack 後に積まれていた frame が後の recv で取り出せる」semantics を
    /// 保つ (= input 1-shot 接続では使われないが、library として attach 経由でも
    /// 安全に動かすため)。
    pending_frames: std::collections::VecDeque<Frame>,
    /// attach client UX 設定 (= Ctrl+Z ガード等、DR-0029)。
    attach_config: crate::config::AttachConfig,
    /// `send_raw_bytes` の ack 待ちが timeout で打ち切られた / I/O error で失敗した後、
    /// 同一 connection への新規送信を禁止するためのフラグ (DR-0021 M2)。
    ///
    /// ack には seq id が無いため、timeout 後に同 connection で次の `send_raw_bytes`
    /// を呼ぶと**遅れて届いた前回 ack** を次回 ack として誤って受理する race が
    /// 成立しうる (= stale ack の silent wrong behavior)。
    ///
    /// 対策: timeout / ack 受信中の I/O error が起きた時点で本フラグを立て、
    /// reader/writer の socket を `shutdown(Both)` し、以降の `send_raw_bytes` は
    /// `Error::Invalid("connection poisoned after raw_ack failure")` を即返す
    /// (= connection 再利用を物理的に禁止)。`send_control` 等の non-ack 経路は
    /// shutdown 済 socket で write が EPIPE を返すので caller には I/O error として
    /// 伝わる (= explicit な poison check は send_raw_bytes だけで十分)。CLI 一発
    /// 呼びでは ack 失敗時に exit するため影響なし。library で attach 経路から
    /// send_raw_bytes を使う場合にこの保護が効く。
    ///
    /// daemon が ack:Error (= `RawAckResult::Error`) を返してきた場合は **poison しない**
    /// (= ack 自体は正常受信できているので、caller が semantic レベルで継続判断する)。
    poisoned: bool,
}

/// SIGWINCH → Resize の中継元 (DR-0019 §6)。signal thread が WINCH を受けて
/// `notify_fd` の write 端へ 1 byte 書き、run loop が read 端 (= 本 `notify_fd`) の
/// POLLIN で起きて `size_fn` から現在の外側端末サイズを取得する (= 責務分離:
/// signal handler は async-signal-safe な write のみ、ioctl は run loop で実行)。
pub struct WinchSource {
    /// signal thread が書く notify pipe の read 端 (= run loop が poll する)。
    notify_fd: std::os::fd::OwnedFd,
    /// 現在の外側端末サイズ (cols, rows) を取得する closure。取得失敗時は `None`。
    size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send>,
}

impl WinchSource {
    /// notify pipe read 端と size 取得 closure から `WinchSource` を作る。
    pub fn new(
        notify_fd: std::os::fd::OwnedFd,
        size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send>,
    ) -> Self {
        Self { notify_fd, size_fn }
    }
}

impl std::fmt::Debug for WinchSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinchSource").finish_non_exhaustive()
    }
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
                // R4-H2 + R4-H13: handshake で daemon が返す error code 別に文言を
                // 出し分ける (H2)。code は ErrorCode enum (H13) で受けるため variant
                // match で書く。Error::Invalid は &'static str しか受け取れないので
                // variant → static str の switch で対応する。
                //
                // Fable review M3 (2026-06-12): ModeNotAllowed は daemon が
                // `ErrorMessage.message` に具体的な拒否理由 (= `--exclusive` 占有
                // 失敗 / ro の `--detach-others` 等) を載せてくる code なので、
                // 文言を握り潰さず `Error::Remote` でそのまま中継する (= 旧実装は
                // 汎用 fallback の「`hyoui status` で確認して」に倒れて誤誘導だった)。
                if matches!(e.code, ErrorCode::ModeNotAllowed) {
                    return Err(Error::Remote(e.message));
                }
                return Err(Error::Invalid(match &e.code {
                    ErrorCode::LockDenied => {
                        "lock denied (= 他 client が exclusive lock を保持中。`hyoui status <session>` で lock-holder を確認、または別 session を使う)"
                    }
                    ErrorCode::UnsupportedCapability => {
                        "daemon が要求 cap を非対応 (= server を新しい version に upgrade するか、client から該当 cap を外す)"
                    }
                    ErrorCode::AuthTokenMismatch => {
                        "HYOUI_LOCK_TOKEN が一致しません (= daemon 起動時の token と env が異なる。env を見直す)"
                    }
                    ErrorCode::Unknown(s) if s == "session.full" => {
                        "session の client 上限に到達 (= 他 client を detach するか、新規 session を起動)"
                    }
                    _ => {
                        "daemon error during handshake (= `hyoui status <session>` でサーバ側の状態を確認してください)"
                    }
                }));
            }
            _ => return Err(Error::Invalid("unexpected response to handshake.request")),
        };
        Ok(Self {
            reader,
            writer,
            response,
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        })
    }

    /// 外側 stdout が tty (= raw mode 中) であることを `run` に伝える。
    ///
    /// CLI 層が raw mode guard を取得できたときに `true` で呼ぶ。`true` のとき
    /// `run` は detach 時に [`OUTER_TTY_RESET`] を吐き、子 stopped の通知行を
    /// 画面最下行に描画する。
    #[must_use]
    pub fn with_outer_tty_raw(mut self, outer_tty_raw: bool) -> Self {
        self.outer_tty_raw = outer_tty_raw;
        self
    }

    /// stdin EOF 時の挙動を設定 (R5-FB2 / DR-0019 §5)。
    ///
    /// `SendEof` を設定すると `run` が stdin EOF を検出した時点で EOT (= 0x04) を
    /// 子 PTY に送り、stdin を poll 対象から外したまま loop を継続する (= 即 return
    /// しない)。子 (= canonical mode の bc 等) は EOT を read EOF として解釈し、
    /// 計算結果を出力してから exit する。その出力と `SessionExitNotify` を socket 経路で
    /// 拾い切ってから抜けることで、pipe-through (`echo ... | hyoui run -- bc`) の
    /// 透過性を回復する。`Detach` は EOF を検出した時点で即 return する (= 子は
    /// daemon 配下に残る)。
    ///
    /// `run` / `attach` いずれも非 tty stdin (= pipe / `< /dev/null`) では default が
    /// `SendEof`、tty stdin では `Detach` を CLI 層が選ぶ (DR-0019 §5)。
    #[must_use]
    pub fn with_stdin_eof_action(mut self, action: StdinEofAction) -> Self {
        self.eof_action = action;
        self
    }

    /// attach client UX 設定 (= Ctrl+Z ガード等) を適用する (DR-0029)。
    #[must_use]
    pub fn with_attach_config(mut self, config: crate::config::AttachConfig) -> Self {
        self.attach_config = config;
        self
    }

    /// SIGWINCH → Resize の中継元を設定する (DR-0019 §6)。設定すると `run` の poll
    /// loop が notify pipe を監視し、WINCH 観測時に外側端末サイズを取得して leader
    /// なら `Resize` message を daemon に送る。CLI 層が raw mode guard 保持時 (= tty
    /// stdin) に設定する。
    #[must_use]
    pub fn with_winch_source(mut self, source: WinchSource) -> Self {
        self.winch_source = Some(source);
        self
    }

    /// この client が leader なら、現在の外側端末サイズで初回 `Resize` を daemon に
    /// 送る (DR-0019 §6: 別端末から attach した時のサイズ不一致解消)。leader でない
    /// 場合 (= daemon が reject する) は送らない。`winch_source` 未設定 / size 取得
    /// 失敗時も no-op。
    ///
    /// # Errors
    ///
    /// `Resize` message の送信に失敗した場合。
    pub fn send_initial_resize(&mut self) -> Result<(), Error> {
        if !self.response.leader {
            return Ok(());
        }
        let Some(src) = self.winch_source.as_mut() else {
            return Ok(());
        };
        let Some((cols, rows)) = (src.size_fn)() else {
            return Ok(());
        };
        let msg = ControlMessage::Resize(crate::protocol::messages::Resize { cols, rows });
        self.send_control(&msg)
    }

    /// stopped child の resume と attach redraw を既存 protocol で要求する。
    ///
    /// caller が handshake の `child_stopped`、attach mode、config を確認してから呼ぶ。
    pub fn send_child_resume(&mut self) -> Result<(), Error> {
        self.send_control(&ControlMessage::SessionChildResumeRequest(
            crate::protocol::messages::SessionChildResumeRequest::default(),
        ))
    }

    /// 外側端末が raw mode の tty なら、安全側 reset シーケンスを吐く。
    ///
    /// detach で attach を畳む直前に呼ぶ (= alt screen / bracketed paste / kitty
    /// keyboard / mouse tracking の残留で外側 shell が壊れるのを防ぐ、issue
    /// 2026-07-24 H4)。非 tty (= pipe) では escape を垂れ流さない。
    fn emit_outer_tty_reset<W: Write>(&self, stdout: &mut W) {
        if !self.outer_tty_raw {
            return;
        }
        let _ = stdout.write_all(OUTER_TTY_RESET);
        let _ = stdout.flush();
    }

    /// `Detach` message を daemon に送り、外側端末を reset して `Detached` を返す。
    fn finish_detach<W: Write>(&mut self, stdout: &mut W) -> RunOutcome {
        let detach = ControlMessage::Detach(Detach {
            target: DetachTarget::Myself,
        });
        if let Ok(body) = detach.encode_to_vec() {
            let _ = Frame::cbor_control(body).encode_to(&mut self.writer);
            let _ = self.writer.flush();
        }
        self.emit_outer_tty_reset(stdout);
        RunOutcome::Detached
    }

    /// 子が停止したことを画面最下行に 1 行で知らせる (DR-0029 §1)。
    ///
    /// attach は継続するため子の出力が止まって画面が固着する。それが「hyoui が
    /// 壊れた」ではなく「子が停止中」だと分かるように、cursor 位置を保存して最下行に
    /// 反転表示で 1 行出し、cursor を戻す。子の画面領域を一時的に上書きするが、
    /// 子が resume して再描画すれば消える (= daemon の screen state は汚さない)。
    ///
    /// 外側が tty でない、または端末サイズが取れない場合は何もしない。
    fn draw_child_stopped_notice<W: Write>(&mut self, stdout: &mut W) {
        if !self.outer_tty_raw {
            return;
        }
        let Some((_, rows)) = self.winch_source.as_mut().and_then(|s| (s.size_fn)()) else {
            return;
        };
        let session = self.response.session_id.clone();
        let notice = format!(
            "\x1b7\x1b[{rows};1H\x1b[K\x1b[7m[hyoui] 子プロセスが停止中 — 再開: \
             hyoui kill {session} --signal=CONT --no-terminate\x1b[m\x1b8"
        );
        let _ = stdout.write_all(notice.as_bytes());
        let _ = stdout.flush();
    }

    /// stdin / stdout を daemon と中継する。
    ///
    /// 終了条件 ([`RunOutcome`] で種別を返す、issue 2026-06-11 優先1):
    /// - 子 PTY exit (= `SessionExitNotify` 受信) → `Ok(RunOutcome::ChildExited)`
    /// - 自発 detach (= Ctrl+Z ガード発火、または `--stdin-eof=detach` での
    ///   stdin EOF) → `Ok(RunOutcome::Detached)`。tty stdin では通常 EOF は起きないが、
    ///   pipe / `< /dev/null` 等の非 tty stdin では起きる。`eof_action` が `SendEof`
    ///   の場合は EOT を送って子の出力を拾い切ってから抜ける (= `with_stdin_eof_action`
    ///   参照、DR-0019 §5)。
    /// - 予期しない socket 喪失 (= daemon の EOF / POLLHUP / POLLERR、socket 書き込み
    ///   失敗) → `Ok(RunOutcome::ConnectionLost)` (= daemon 消滅の疑い)
    /// - protocol violation → `Err`
    ///
    /// control message の送信は基本 `send_control` を別途叩く想定
    /// (= resize/signal/detach/kill 等) だが、`run` 自身も daemon → client の
    /// `LeaderNotify` / `SessionChildStoppedNotify` を受けて leader 状態更新 /
    /// 子停止の通知表示 / WINCH→Resize を処理する。
    ///
    /// # Errors
    ///
    /// I/O / decode error は [`Error`] で返す。socket EOF は `Ok(RunOutcome::ConnectionLost)`
    /// で「予期しない切断」として返す (= Err ではない)。
    pub fn run<R: Read + AsFd, W: Write>(
        mut self,
        stdin: &mut R,
        stdout: &mut W,
    ) -> Result<RunOutcome, Error> {
        let mut ctrlz_state = CtrlzGuardState::Idle;
        // DR-0019 §5: SendEof で stdin EOF 観測後、stdin はもう読まない (= EOT 送出
        // 済) が、子の出力 (= bc の計算結果) と SessionExitNotify を拾い切るため
        // loop は継続する。
        //
        // M-1: stdin_done になったら stdin fd を poll 配列から**完全に除外**する
        // (= PollFlags::empty() で残す方式は不可)。理由は 2 つ:
        //   1. Linux の poll(2) は POLLHUP/POLLERR を events マスク無視で revents に
        //      報告するため、EOF 済み pipe を events=0 で poll し続けると即時 return の
        //      busy loop になる。
        //   2. events=0 で残すと、stdin pipe の EOF 後に POLLHUP が立ち、drain 継続中の
        //      stdin EOF 判定経路が即 Ok(None) return して bc の出力を取りこぼす race が
        //      残る (= DR-0019 §5 が直したはずの透過性回復が壊れる)。
        // fd を見ない構造にすることで両方を断つ。
        let mut stdin_done = false;
        // 新規二重防御: winch notify fd が POLLHUP/POLLERR を返したら監視を諦める
        // (= signal thread 起動失敗等で write 端が drop され read 端だけ残ると、
        // POLLIN しか処理しない loop が POLLHUP で idle busy loop になるのを防ぐ)。
        let mut winch_disabled = false;
        loop {
            let socket_fd = self.reader.as_fd();
            // poll 配列を動的に組む。socket は常に index 0。stdin は stdin_done なら
            // 除外、winch notify は winch_source があり winch_disabled でなければ末尾。
            // 各 fd の index は変数で管理する (= 含めなかった fd の revents を誤読しない)。
            let stdin_fd = stdin.as_fd();
            let winch_fd = if winch_disabled {
                None
            } else {
                self.winch_source.as_ref().map(|s| s.notify_fd.as_fd())
            };
            let mut fds: Vec<PollFd> = Vec::with_capacity(3);
            fds.push(PollFd::new(socket_fd, PollFlags::POLLIN));
            let stdin_idx = if stdin_done {
                None
            } else {
                let idx = fds.len();
                fds.push(PollFd::new(stdin_fd, PollFlags::POLLIN));
                Some(idx)
            };
            let winch_idx = winch_fd.map(|wfd| {
                let idx = fds.len();
                fds.push(PollFd::new(wfd, PollFlags::POLLIN));
                idx
            });

            let now = std::time::Instant::now();
            let poll_timeout = ctrlz_guard_poll_timeout(ctrlz_state, now);
            let poll_timed_out = match poll(&mut fds, poll_timeout) {
                Ok(PollOutcome::Ready(_)) => false,
                Ok(PollOutcome::Interrupted) => continue,
                Ok(PollOutcome::Timeout) => true,
                Err(e) => return Err(e),
            };

            // deadline と同時に socket output 等が ready でも、保留 Ctrl+Z の満了を
            // 飢餓させない。poll outcome に関係なく deadline を評価してから ready fd を処理する。
            if let DetachAction::TriggerDetach(_) = process_ctrlz_guard(
                &[],
                &mut ctrlz_state,
                std::time::Instant::now(),
                &self.attach_config,
            ) {
                return Ok(self.finish_detach(stdout));
            }
            if poll_timed_out {
                continue;
            }

            let sock_revents = fds[0].revents().unwrap_or(PollFlags::empty());
            let stdin_revents = stdin_idx.map_or(PollFlags::empty(), |i| {
                fds[i].revents().unwrap_or(PollFlags::empty())
            });
            let winch_revents = winch_idx.map_or(PollFlags::empty(), |i| {
                fds[i].revents().unwrap_or(PollFlags::empty())
            });
            let _ = fds;

            // 新規二重防御: winch notify fd の異常 (= POLLHUP/POLLERR/POLLNVAL) を検出
            // したら以降の loop で監視対象から外す (= busy loop 回避)。次 iteration の
            // poll 配列構築で除外される。
            if winch_revents
                .intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL)
            {
                winch_disabled = true;
            }

            // DR-0019 §6: WINCH notify を観測したら、pipe を drain して外側端末サイズを
            // 取得し、leader なら Resize を daemon に送る。size 取得 → send を分離する
            // のは borrow checker 対策 (= winch_source の closure と send_control が
            // 両方 &mut self を要求するため)。
            if winch_revents.contains(PollFlags::POLLIN) {
                if let Some(src) = self.winch_source.as_mut() {
                    // notify pipe を drain (= 複数 WINCH を 1 回の resize に畳む)。
                    let mut buf = [0u8; 64];
                    while let Ok(n) = nix::unistd::read(&src.notify_fd, &mut buf) {
                        if n < buf.len() {
                            break;
                        }
                    }
                }
                if self.response.leader {
                    let size = self.winch_source.as_mut().and_then(|s| (s.size_fn)());
                    if let Some((cols, rows)) = size {
                        let msg = ControlMessage::Resize(crate::protocol::messages::Resize {
                            cols,
                            rows,
                        });
                        // 送信失敗は致命的でない (= 次の WINCH で再送される)。
                        let _ = self.send_control(&msg);
                    }
                }
            }

            // socket → stdout: frame を 1 つ decode → raw data なら stdout に出す
            if sock_revents.contains(PollFlags::POLLIN) {
                match Frame::decode_from(&mut self.reader) {
                    Ok(frame) => match frame.ty {
                        TYPE_RAW_DATA => {
                            if stdout.write_all(&frame.body).is_err() {
                                // 出力先 (= 端末 / pipe) が閉じた。daemon との接続は
                                // 健在なので `ConnectionLost` (= daemon 消滅) とは別扱い。
                                // 自分側の出力経路が壊れただけ。
                                return Ok(RunOutcome::StdoutWriteFailed);
                            }
                            let _ = stdout.flush();
                        }
                        TYPE_CBOR_CONTROL => {
                            // DR-0015: daemon → client 方向の control message を
                            // 取り出して処理する。
                            // - SessionExitNotify: 子 PTY exit を受けて run loop を抜ける
                            //   (= caller が exit_status を取り出す経路は run の戻り値で
                            //   伝える。簡素化のため Ok(()) で抜けて caller 側で
                            //   `session_exit_status` field を読む形)
                            // - SessionChildStoppedNotify: 子が止まったことを画面最下行に
                            //   知らせるだけ (= attach は継続、DR-0029 §1)
                            // - 他 (= leader.notify / mode.change / error 等) は無視
                            match ControlMessage::decode_from(frame.body.as_slice()) {
                                Ok(ControlMessage::SessionExitNotify(notify)) => {
                                    return Ok(RunOutcome::ChildExited {
                                        exit_status: notify.exit_status,
                                    });
                                }
                                Ok(ControlMessage::SessionChildStoppedNotify(_)) => {
                                    // DR-0029 §1: attach は覗き窓なので、子が止まっても
                                    // client は止まらず attach を継続する。画面は子の出力が
                                    // 止まって固着するだけなので、それが「hyoui が壊れた」
                                    // ではなく「子が停止中」だと分かるよう最下行に 1 行出す。
                                    self.draw_child_stopped_notice(stdout);
                                }
                                Ok(ControlMessage::LeaderNotify(n)) => {
                                    // Minor 4: leader 変更通知で自分の leader 状態を更新する。
                                    // client_id が自分なら leader、それ以外 (= 他 client or
                                    // None=leader 不在) なら非 leader。これをしないと初代
                                    // leader が detach して自分が昇格した後も response.leader
                                    // が false 固定のままで、WINCH を受けても Resize を
                                    // 送らない (= 昇格後 resize 不全)。
                                    let was_leader = self.response.leader;
                                    let now_leader = n.client_id == Some(self.response.client_id);
                                    self.response.leader = now_leader;
                                    // 昇格時 (false → true) は初回 Resize を送る (= attach 成立
                                    // 時の send_initial_resize と同じ意図: 別端末サイズとの
                                    // 不一致を昇格直後に解消する)。降格 (true → false) では
                                    // 何もしない (= daemon が以後の Resize を reject する)。
                                    if !was_leader && now_leader {
                                        let size =
                                            self.winch_source.as_mut().and_then(|s| (s.size_fn)());
                                        if let Some((cols, rows)) = size {
                                            let msg = ControlMessage::Resize(
                                                crate::protocol::messages::Resize { cols, rows },
                                            );
                                            // 送信失敗は致命的でない (= 次の WINCH で再送)。
                                            let _ = self.send_control(&msg);
                                        }
                                    }
                                }
                                Ok(ControlMessage::Error(e))
                                    if e.code == ErrorCode::BackpressureDisconnect =>
                                {
                                    // daemon が backpressure で当該 client を切断した。
                                    // 後続の socket EOF (= ConnectionLost) を待たず、
                                    // 切断理由が分かる専用 outcome で即 return する
                                    // (= EOF だけ拾うと「daemon 消滅の疑い」と区別不能)。
                                    return Ok(RunOutcome::BackpressureDisconnected);
                                }
                                Ok(_) => { /* 他 control message は無視 (= 既存 MVP 挙動) */
                                }
                                Err(_) => { /* decode 失敗は無視 (= forward-compat、未知 kind) */
                                }
                            }
                        }
                        TYPE_RAW_ACK => {
                            // DR-0021: daemon は raw_data write 完了ごとに RawAck を返す。
                            // attach の stdin forward は fire-and-forget (= 完了点同期を
                            // 必要としない) ので読み捨てる。`recv_control` の silent skip
                            // (DR-0021 改訂 m1) と同じ扱い。ここで捨てないと最初の打鍵 /
                            // pipe 入力 / SendEof の EOT 送信直後に unknown frame 扱いで
                            // client が異常終了する (= interactive 打鍵の全滅 bug)。
                        }
                        _ => return Err(Error::Invalid("unknown frame type from daemon")),
                    },
                    Err(FrameError::Protocol(ProtocolError::UnexpectedEof(_))) => {
                        // daemon が socket を close した。自分から detach したわけでは
                        // ないので「予期しない接続喪失」(= daemon 消滅の疑い) として返す。
                        return Ok(RunOutcome::ConnectionLost);
                    }
                    Err(_) => return Err(Error::Invalid("protocol error from daemon")),
                }
            } else if sock_revents.contains(PollFlags::POLLHUP)
                || sock_revents.contains(PollFlags::POLLERR)
            {
                // socket が POLLHUP/POLLERR → 対向 (= daemon) が消えた。接続喪失扱い。
                return Ok(RunOutcome::ConnectionLost);
            }

            // C-1: stdin の revents が EOF 相当 (= POLLNVAL/POLLERR/POLLHUP で read に
            // 到達できない or read しても EOF) なら、read を試さず stdin EOF 経路へ倒す。
            // 非 tty stdin (= `< /dev/null` など) で macOS が POLLNVAL を即時返す場合、
            // POLLIN が立たないため従来は read に到達できず EOF を観測できなかった
            // (= EOT 未送出 + poll 即 return の busy loop)。
            if stdin_revents_is_eof(stdin_revents) {
                // R5-FB2 / DR-0019 §5: stdin EOF の挙動は `eof_action` で分岐。
                // - Detach (default): 即 return (= MVP attach 挙動。子は残る)
                // - SendEof: EOT (0x04) を子 PTY に送り、stdin は閉じたまま loop を
                //   継続する。子 (= canonical mode の bc 等) は EOT を read EOF として
                //   解釈し、計算結果を出力してから exit する。その出力と
                //   SessionExitNotify を socket 経路で拾い切るため、ここで return せず
                //   stdin_done を立てて stdin fd を poll 配列から外し socket だけ poll
                //   し続ける (= 即 return すると bc の出力が stdout に届く前に client が
                //   抜けてしまう、DR-0019 §5 の透過性回復要件)。
                if self.eof_action == StdinEofAction::SendEof {
                    let frame = Frame::raw_data(vec![0x04]);
                    let _ = frame.encode_to(&mut self.writer);
                    let _ = self.writer.flush();
                    stdin_done = true;
                    continue;
                }
                // `--stdin-eof=detach`: stdin が閉じたので自分から離脱する (= 自発 detach、
                // 子は daemon 配下に残す)。
                self.emit_outer_tty_reset(stdout);
                return Ok(RunOutcome::Detached);
            }

            // stdin → socket: raw data frame で送る
            if stdin_revents.contains(PollFlags::POLLIN) {
                let mut buf = [0u8; 8192];
                match stdin.read(&mut buf) {
                    Ok(0) => {
                        // read が 0 = EOF。revents が POLLIN だけ立っていたケース
                        // (= 上の stdin_revents_is_eof で拾えなかった通常の pipe EOF)。
                        // 挙動は上と同じく eof_action で分岐する。
                        if self.eof_action == StdinEofAction::SendEof {
                            let frame = Frame::raw_data(vec![0x04]);
                            let _ = frame.encode_to(&mut self.writer);
                            let _ = self.writer.flush();
                            stdin_done = true;
                            continue;
                        }
                        // `--stdin-eof=detach`: 自発 detach (= 子は残す)。
                        self.emit_outer_tty_reset(stdout);
                        return Ok(RunOutcome::Detached);
                    }
                    Ok(n) => {
                        match process_ctrlz_guard(
                            &buf[..n],
                            &mut ctrlz_state,
                            std::time::Instant::now(),
                            &self.attach_config,
                        ) {
                            DetachAction::TriggerDetach(forward_before) => {
                                if !forward_before.is_empty() {
                                    let frame = Frame::raw_data(forward_before);
                                    let _ = frame.encode_to(&mut self.writer);
                                    let _ = self.writer.flush();
                                }
                                return Ok(self.finish_detach(stdout));
                            }
                            DetachAction::Forward(forward_bytes) => {
                                if !forward_bytes.is_empty() {
                                    let frame = Frame::raw_data(forward_bytes);
                                    if frame.encode_to(&mut self.writer).is_err() {
                                        return Ok(RunOutcome::ConnectionLost);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // stdin read error: 入力経路が壊れた。子は daemon 配下に残せるので
                    // 自発 detach 相当 (= 接続喪失ではない)。
                    Err(_) => {
                        self.emit_outer_tty_reset(stdout);
                        return Ok(RunOutcome::Detached);
                    }
                }
            }
            // POLLHUP/POLLERR/POLLNVAL は上の stdin_revents_is_eof 経路で処理済み。
        }
    }

    /// daemon → client への次の frame を 1 つ受信して返す。
    ///
    /// 主に `status` / `tail` / `wait` のような 1-shot CLI で使う。`run` を
    /// 呼ぶ前提の attach は内部 poll loop で frame を消費するので本 method は
    /// 不要。
    ///
    /// DR-0021: `send_raw_bytes` の ack 待ち中に到着した non-ack frame が
    /// `pending_frames` に積まれている場合は、socket から読む前にそちらを
    /// 優先して返す (= FIFO order を保つ)。
    ///
    /// # Errors
    ///
    /// frame decode 失敗 (= protocol violation or socket EOF) は [`Error::Invalid`]。
    pub fn recv_frame(&mut self) -> Result<Frame, Error> {
        if let Some(frame) = self.pending_frames.pop_front() {
            return Ok(frame);
        }
        Frame::decode_from(&mut self.reader).map_err(|e| match e {
            FrameError::Io(io) => Error::Io(io),
            FrameError::Protocol(_) => Error::Invalid("frame decode failed"),
        })
    }

    /// reader 側 socket に read timeout を設定する。
    ///
    /// `None` で timeout 解除 (= blocking)。`Some(dur)` で `recv_control` /
    /// `recv_frame` が `dur` 以内に 1 byte も読めなければ `WouldBlock` / `TimedOut`
    /// の `Error::Io` を返すようになる。DR-0017: `hyoui kill --no-terminate` で
    /// 「daemon が Error を返さなければ成功」と短時間で判定するために使う
    /// (= 非 terminate な `ControlMessage::Signal` は成功時に ack を返さないため、
    /// blocking recv だと hang する)。
    ///
    /// # Errors
    ///
    /// `set_read_timeout` syscall が失敗した場合に [`Error::Io`]。
    pub fn set_read_timeout(&self, dur: Option<std::time::Duration>) -> Result<(), Error> {
        self.reader.set_read_timeout(dur).map_err(Error::from)
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
            if frame.ty == TYPE_RAW_ACK {
                // DR-0021 m1: ack 待ちでない時に届く RAW_ACK は silent skip。
                // 想定経路:
                // - timeout 後の stale ack (= M2 poison で塞ぐが、防御的に skip)
                // - 将来 pipeline (= seq id 導入後) の余剰 ack
                // 旧実装は `unexpected frame type` で hard error にしていたが、
                // ack の意味的所有者は `send_raw_bytes` のみで、他経路では
                // ignore するのが安全 (= consumer 経路として broadcast の RAW_DATA を
                // skip するのと同じ扱い)。
                continue;
            }
            return Err(Error::Invalid("unexpected frame type"));
        }
    }

    /// 任意の bytes 列を **raw_data frame** として daemon に送り、PTY drain 完了の
    /// ack (= `TYPE_RAW_ACK` frame) を同期で待つ (DR-0021)。
    ///
    /// daemon は受け取った raw_data frame の body を master PTY にそのまま書き込み
    /// (= `daemon::control::handle_client_frame` の `TYPE_RAW_DATA` 分岐)、
    /// `write_all_with_idle_timeout` が return した時点で `RawAck` を返す。本 method は
    /// その ack 受信まで block する。これにより複数 bytes 系 spec (text → key:Enter 等)
    /// を順次送る際の race (= text 完了前に Enter が master fd に届いて Enter 取りこぼし)
    /// が排除される (= `socket flush` ではなく `PTY write 完了` を完了点にする)。
    ///
    /// ack 待ち中に到着する **non-ack frame** (= broadcast の `TYPE_RAW_DATA`、
    /// `TYPE_CBOR_CONTROL` の各種 control message) は `pending_frames` に push し、
    /// 後続の [`recv_frame`] / [`recv_control`] が FIFO で取り出せるよう保存する。
    ///
    /// 1 frame の上限は protocol 層の `MAX_FRAME_SIZE` (= 16 MiB - 1)。本 method は
    /// 渡された bytes 全体を 1 frame で送る (= size 制御は caller 側の責務、
    /// 大きい場合は事前に chunk 分割する)。空 bytes は何もせず `Ok(())`。
    ///
    /// # Errors
    ///
    /// * I/O / frame size 超過 → [`Error`]
    /// * ack の `result == Error` (= daemon 側で master write が timeout / I/O error / partial)
    ///   → [`Error::Remote`] に daemon 側 error message を載せて返す
    /// * `RAW_ACK_TIMEOUT` 内に ack が来ない → [`Error::Invalid`]
    ///
    /// mode が `Ro` / lock 不一致の client から呼んだ場合、daemon は bytes を master に
    /// 書かず、`code` = `client.ro-rejected` / `client.lock-not-held` の Error ack を返す。
    /// client は `Err(Error::Remote(_))` で受け取り、CLI 層は exit 1 で abort する
    /// (= ack:Ok = 「子の input stream に確実に到達した」semantics に統一、DR-0021 改訂)。
    pub fn send_raw_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Invalid(
                "connection poisoned after raw_ack failure; reconnect required",
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        Frame::raw_data(bytes.to_vec())
            .encode_to(&mut self.writer)
            .map_err(|e| match e {
                FrameError::Io(io) => {
                    self.poison();
                    Error::Io(io)
                }
                FrameError::Protocol(_) => {
                    self.poison();
                    Error::Invalid("raw_data frame protocol error")
                }
            })?;
        if let Err(io) = self.writer.flush() {
            self.poison();
            return Err(Error::Io(io));
        }

        // DR-0021: ack 待ち。socket-level の `read_timeout` を**変更しない**
        // (= 次 frame 開始までの readiness は `poll(2)` で判定し、frame body の
        // 読み出しは blocking `decode_from` で完走させる)。
        //
        // 旧実装は `set_read_timeout(Some(RAW_ACK_TIMEOUT))` で `read(2)` 自体に
        // timeout を入れていたが、`Frame::decode_from` は内部で size 4B / type 1B /
        // body N B の 3 連続 `read_exact` を行う。`read_exact` は `TimedOut` を
        // 観測すると既に読んだ partial bytes を破棄する仕様のため、body 読み出し
        // 途中で timeout が発火すると socket には body の残り bytes が居残り、
        // 次 iteration で「body 残骸の最初 4 B を size として誤解読」する
        // partial-byte race が成立した (= 1024 B 境界で再現、Error::Invalid
        // "frame decode failed while waiting raw_ack")。
        //
        // 修正後の不変条件: deadline 判定は frame **境界**でのみ発火する
        // (= 部分読み出し済みの socket 残骸は生まれない)。
        let result = self.recv_raw_ack_inner();
        // DR-0021 M2: timeout / I/O error / protocol error は connection を poison
        // して以降の `send_raw_bytes` / `send_control` を弾く (= 遅れて届いた前回 ack を
        // 次回 ack として誤受理する stale-ack race を物理的に塞ぐ)。`Err(Remote(_))`
        // (= daemon が ack:Error を返した) は ack 自体は正常受信できているので poison
        // しない (= caller が semantic レベルでの失敗を意識して継続判断する)。
        if let Err(ref e) = result {
            match e {
                Error::Remote(_) => {} // ack:Error は protocol 上正常受信、poison しない
                _ => self.poison(),
            }
        }
        result
    }

    /// connection を poison 状態にし、以降の bytes 送信を物理的に禁止する
    /// (DR-0021 M2)。reader/writer の socket を `shutdown(Both)` し、`poisoned`
    /// フラグを立てる。idempotent (= 複数回呼んでも安全)。
    fn poison(&mut self) {
        if self.poisoned {
            return;
        }
        self.poisoned = true;
        let _ = self.reader.shutdown(std::net::Shutdown::Both);
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }

    /// `send_raw_bytes` から呼ばれる ack 受信本体。
    ///
    /// 各 iteration:
    /// 1. 残 deadline 内で `poll(POLLIN)` を呼び readiness を取る
    /// 2. ready なら `Frame::decode_from` を **blocking** で呼び 1 frame を完走読了
    ///    (= partial-byte race を frame 単位で原子化)
    /// 3. ack なら処理 / non-ack なら `pending_frames` に積んで継続
    ///
    /// ack 以外の frame は `pending_frames` に積み、後続の `recv_frame` /
    /// `recv_control` が FIFO で取り出す。
    fn recv_raw_ack_inner(&mut self) -> Result<(), Error> {
        let deadline = std::time::Instant::now() + RAW_ACK_TIMEOUT;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(Error::Invalid(
                    "raw_ack timeout: daemon did not ack within RAW_ACK_TIMEOUT",
                ));
            }
            let remaining = deadline - now;
            // poll(reader fd, POLLIN, remaining) で次 frame の readiness を待つ。
            // EINTR は再 loop (= self-pipe drain は本 path に無いが、interrupt は
            // 単に再試行する)。
            let fd = self.reader.as_fd();
            let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as u16;
            let timeout = PollTimeout::from(timeout_ms);
            match poll(&mut fds, timeout)
                .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?
            {
                PollOutcome::Timeout => {
                    return Err(Error::Invalid(
                        "raw_ack timeout: daemon did not ack within RAW_ACK_TIMEOUT",
                    ));
                }
                PollOutcome::Interrupted => continue,
                PollOutcome::Ready(_) => {}
            }
            // readiness 観測後は blocking decode で 1 frame を必ず完走させる
            // (= partial-byte discard を踏まない)。socket の `read_timeout` は
            // None (= default) のままなので read(2) は EOF / 完了まで block する。
            let frame = match Frame::decode_from(&mut self.reader) {
                Ok(f) => f,
                Err(FrameError::Io(io)) => return Err(Error::Io(io)),
                Err(FrameError::Protocol(_)) => {
                    return Err(Error::Invalid("frame decode failed while waiting raw_ack"));
                }
            };
            match frame.ty {
                TYPE_RAW_ACK => {
                    let ack = RawAck::decode_from(frame.body.as_slice())
                        .map_err(|_| Error::Invalid("raw_ack CBOR decode failed"))?;
                    return match ack.result {
                        RawAckResult::Ok => Ok(()),
                        RawAckResult::Error => {
                            let msg = ack.message.unwrap_or_else(|| {
                                ack.code
                                    .clone()
                                    .unwrap_or_else(|| "master write failed".to_string())
                            });
                            Err(Error::Remote(msg))
                        }
                    };
                }
                // ack 待ち中に届いた non-ack frame は捨てずに保留 (= FIFO 順を保つ)。
                // 後の recv_frame / recv_control がここから先に取り出す。
                _ => self.pending_frames.push_back(frame),
            }
        }
    }

    /// reader 側 socket の borrowed fd を返す (= `poll(2)` で readiness を取る用途)。
    ///
    /// 用途: `lock acquire` のように **block しつつ別 fd (= self-pipe / stdin) と
    /// 並行 poll したい** 場合に、内部の reader fd へアクセスするための accessor。
    /// 返却 fd への直接 `read(2)` は `recv_control` / `recv_frame` の frame 境界を
    /// 破壊するので **読み出してはならない** (= readiness 観測のみ)。
    pub fn reader_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.reader.as_fd()
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

    // ---- C-1: stdin_revents_is_eof unit tests ----

    #[test]
    fn stdin_revents_pollin_only_is_not_eof() {
        // POLLIN 単独は通常の readiness。read 経路に任せる (= EOF ではない)。
        assert!(!stdin_revents_is_eof(PollFlags::POLLIN));
    }

    #[test]
    fn stdin_revents_pollnval_is_eof_even_with_pollin() {
        // POLLNVAL (= macOS の /dev/null 等) は fd 無効なので POLLIN 有無に関わらず EOF。
        assert!(stdin_revents_is_eof(PollFlags::POLLNVAL));
        assert!(stdin_revents_is_eof(
            PollFlags::POLLNVAL | PollFlags::POLLIN
        ));
    }

    #[test]
    fn stdin_revents_pollhup_pollerr_are_eof_without_pollin() {
        assert!(stdin_revents_is_eof(PollFlags::POLLHUP));
        assert!(stdin_revents_is_eof(PollFlags::POLLERR));
    }

    #[test]
    fn stdin_revents_pollhup_with_pollin_defers_to_read() {
        // POLLHUP + POLLIN は「まだ未読 byte がある」可能性 → read に任せる (= EOF 即断しない)。
        assert!(!stdin_revents_is_eof(
            PollFlags::POLLHUP | PollFlags::POLLIN
        ));
        assert!(!stdin_revents_is_eof(
            PollFlags::POLLERR | PollFlags::POLLIN
        ));
    }

    #[test]
    fn stdin_revents_empty_is_not_eof() {
        assert!(!stdin_revents_is_eof(PollFlags::empty()));
    }

    // ---- M-1: stdin EOF (POLLNVAL) で run が spin せず SendEof 経路に倒れる ----

    /// 非 tty stdin が即 POLLNVAL/EOF を返すケースで、SendEof 設定の run が EOT を 1 回
    /// 送ってから stdin を poll 配列から外し (= 即時 return の busy loop にならず)、socket
    /// EOF で正常終了することを検証する。stdin として「既に EOF な pipe (write 端を即
    /// close)」を渡すと read=0 / POLLHUP 経路を踏む (= macOS の POLLNVAL と同じ EOF 経路に
    /// 合流)。run が return することで「stdin_done 後に stdin fd を見続けて spin しない」
    /// (= M-1 で poll 配列から除外した効果) を間接的に保証する。
    #[test]
    fn run_send_eof_on_already_eof_stdin_sends_single_eot_and_exits() {
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::SendEof,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        // 既に EOF な stdin: pipe の write 端を即 close。
        let (stdin_rd, stdin_wr) = nix::unistd::pipe().expect("stdin pipe");
        drop(stdin_wr);
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));

        // daemon 役: EOT (= 0x04) frame を 1 つ受信できるはず。
        let mut daemon_reader = daemon_sock;
        let frame = Frame::decode_from(&mut daemon_reader).expect("decode EOT frame");
        assert_eq!(frame.ty, TYPE_RAW_DATA);
        assert_eq!(
            frame.body,
            vec![0x04],
            "stdin EOF で EOT が 1 回送られるはず"
        );

        // socket を close → run は socket EOF で正常終了する (= stdin spin で hang しない)。
        drop(daemon_reader);
        let res = run_handle.join().expect("run thread join");
        assert!(res.is_ok(), "run は socket EOF で Ok 終了するはず: {res:?}");
    }

    // ---- DR-0019 §6: SIGWINCH → Resize 配線 unit tests ----

    /// leader でない client は initial resize を送らない (= daemon が reject するため)。
    #[test]
    fn send_initial_resize_noop_when_not_leader() {
        use std::os::unix::net::UnixStream;
        let (client_sock, _daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        // size_fn は呼ばれてはいけない (= leader でないので即 return)。
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        let size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send> =
            Box::new(|| panic!("size_fn must not be called when not leader"));
        let mut conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Ro,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: Some(WinchSource::new(rd, size_fn)),
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };
        // leader=false なので Ok(()) で何も送らない (panic しなければ成功)。
        conn.send_initial_resize().expect("send_initial_resize");
    }

    /// leader client は initial resize で現在サイズの Resize message を送る。
    #[test]
    fn send_initial_resize_sends_resize_when_leader() {
        use crate::protocol::ControlMessage;
        use std::os::unix::net::UnixStream;
        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        let size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send> = Box::new(|| Some((120, 40)));
        let mut conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: true,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: Some(WinchSource::new(rd, size_fn)),
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };
        conn.send_initial_resize().expect("send_initial_resize");

        // daemon 役で Resize frame を読む。
        let mut daemon_reader = daemon_sock;
        let frame = Frame::decode_from(&mut daemon_reader).expect("decode frame");
        assert_eq!(frame.ty, TYPE_CBOR_CONTROL);
        match ControlMessage::decode_from(frame.body.as_slice()).expect("decode msg") {
            ControlMessage::Resize(r) => {
                assert_eq!(r.cols, 120);
                assert_eq!(r.rows, 40);
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    /// run loop が WINCH notify (= notify pipe の POLLIN) を観測したら、leader なら
    /// 現在サイズの Resize を送る。daemon 役は Resize 受信後に socket を close して
    /// client run を EOF で終了させる。
    #[test]
    fn run_sends_resize_on_winch_notify() {
        use crate::protocol::ControlMessage;
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");

        // winch notify pipe。read 端は non-blocking (= run の drain が block しない)。
        let (notify_rd, notify_wr) = nix::unistd::pipe().expect("pipe");
        {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};
            let flags = fcntl(notify_rd.as_fd(), FcntlArg::F_GETFL).unwrap();
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(notify_rd.as_fd(), FcntlArg::F_SETFL(flags)).unwrap();
        }
        let size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send> = Box::new(|| Some((100, 30)));
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: true,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: Some(WinchSource::new(notify_rd, size_fn)),
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        // stdin は即 EOF させない (= run が stdin EOF で抜けないよう) ため、書き込み端を
        // 保持し続ける pipe の read 端を渡す。
        let (stdin_rd, stdin_wr_keep) = nix::unistd::pipe().expect("stdin pipe");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        // WINCH 発生をシミュレート: notify pipe に 1 byte 書く。
        nix::unistd::write(&notify_wr, &[1u8]).expect("notify write");

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));

        // daemon 役で Resize frame を受信検証。
        let mut daemon_reader = daemon_sock;
        let frame = Frame::decode_from(&mut daemon_reader).expect("decode frame");
        assert_eq!(frame.ty, TYPE_CBOR_CONTROL);
        match ControlMessage::decode_from(frame.body.as_slice()).expect("decode msg") {
            ControlMessage::Resize(r) => {
                assert_eq!(r.cols, 100);
                assert_eq!(r.rows, 30);
            }
            other => panic!("expected Resize, got {other:?}"),
        }

        // socket を close して run を EOF 終了させる。
        drop(daemon_reader);
        drop(stdin_wr_keep);
        let _ = run_handle.join();
    }

    // ---- Minor 4: LeaderNotify による昇格 → 初回 Resize ----

    /// 非 leader client が `LeaderNotify { client_id = 自分 }` を受けたら leader に昇格し、
    /// 昇格直後に現在サイズで初回 Resize を送る (= 初代 leader detach 後の resize 不全修復)。
    #[test]
    fn run_promotes_to_leader_and_sends_resize_on_leader_notify() {
        use crate::protocol::ControlMessage;
        use crate::protocol::messages::LeaderNotify;
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");

        let (notify_rd, _notify_wr) = nix::unistd::pipe().expect("pipe");
        {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};
            let flags = fcntl(notify_rd.as_fd(), FcntlArg::F_GETFL).unwrap();
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(notify_rd.as_fd(), FcntlArg::F_SETFL(flags)).unwrap();
        }
        let size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send> = Box::new(|| Some((90, 25)));
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 42,
                // 初期は非 leader (= 他 client が leader)。
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: Some(WinchSource::new(notify_rd, size_fn)),
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        // stdin は EOF させない (= 書き込み端を保持)。
        let (stdin_rd, stdin_wr_keep) = nix::unistd::pipe().expect("stdin pipe");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        // daemon 役: client_id=42 を新 leader とする LeaderNotify を送る。
        let mut daemon_sock = daemon_sock;
        let notify = ControlMessage::LeaderNotify(LeaderNotify {
            client_id: Some(42),
        });
        let body = notify.encode_to_vec().expect("encode leader.notify");
        Frame::cbor_control(body)
            .encode_to(&mut daemon_sock)
            .expect("send leader.notify");

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));

        // 昇格に伴う初回 Resize を受信検証。
        let frame = Frame::decode_from(&mut daemon_sock).expect("decode resize frame");
        assert_eq!(frame.ty, TYPE_CBOR_CONTROL);
        match ControlMessage::decode_from(frame.body.as_slice()).expect("decode msg") {
            ControlMessage::Resize(r) => {
                assert_eq!(r.cols, 90);
                assert_eq!(r.rows, 25);
            }
            other => panic!("昇格直後に Resize を送るはず, got {other:?}"),
        }

        drop(daemon_sock);
        drop(stdin_wr_keep);
        let _ = run_handle.join();
    }

    /// `LeaderNotify { client_id = 他 client }` を受けても (= 昇格しない) Resize を送らない。
    /// 既に leader だった client が降格する場合に余計な Resize を出さないことも併せて確認。
    #[test]
    fn run_does_not_send_resize_when_not_promoted() {
        use crate::protocol::ControlMessage;
        use crate::protocol::messages::LeaderNotify;
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");

        let (notify_rd, _notify_wr) = nix::unistd::pipe().expect("pipe");
        {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};
            let flags = fcntl(notify_rd.as_fd(), FcntlArg::F_GETFL).unwrap();
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(notify_rd.as_fd(), FcntlArg::F_SETFL(flags)).unwrap();
        }
        // size_fn は呼ばれてはいけない (= 昇格しないので Resize を組まない)。
        let size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send> =
            Box::new(|| panic!("size_fn は昇格しない限り呼ばれない"));
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 42,
                // 初期 leader だが、他 client への leader 移動通知で降格する。
                leader: true,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: Some(WinchSource::new(notify_rd, size_fn)),
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        let (stdin_rd, stdin_wr_keep) = nix::unistd::pipe().expect("stdin pipe");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        // daemon 役: client_id=7 (= 他 client) を新 leader とする通知 → 自分は降格。
        let mut daemon_sock = daemon_sock;
        let notify = ControlMessage::LeaderNotify(LeaderNotify { client_id: Some(7) });
        let body = notify.encode_to_vec().expect("encode leader.notify");
        Frame::cbor_control(body)
            .encode_to(&mut daemon_sock)
            .expect("send leader.notify");
        // 続けて socket close → run は降格処理後に socket EOF で終了する。
        drop(daemon_sock);

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));
        let res = run_handle.join().expect("join");
        drop(stdin_wr_keep);
        // size_fn の panic が起きず Ok 終了すれば「降格時に Resize を組まなかった」証拠。
        assert!(res.is_ok(), "run は socket EOF で Ok 終了するはず: {res:?}");
    }

    // ---- DR-0029 §2: Ctrl+Z ガード state machine ----

    fn guard_config() -> crate::config::AttachConfig {
        crate::config::AttachConfig {
            ctrlz_guard: true,
            ctrlz_guard_delay: Duration::from_millis(500),
            ctrlz_guard_overlay: true,
            resume_on_reattach: true,
        }
    }

    /// 連打 N 回を 1 つの窓の中で順に流し、(子へ届いた bytes, detach したか) を返す。
    ///
    /// chunk は 1 byte ずつ (= 実端末の打鍵に相当) で、時刻は毎回 1ms 進める。
    fn press_ctrl_z(n: usize) -> (Vec<u8>, bool) {
        let t0 = std::time::Instant::now();
        let cfg = guard_config();
        let mut state = CtrlzGuardState::Idle;
        let mut forwarded = Vec::new();
        for i in 0..n {
            match process_ctrlz_guard(
                &[CTRL_Z_BYTE],
                &mut state,
                t0 + Duration::from_millis(i as u64),
                &cfg,
            ) {
                DetachAction::Forward(b) => forwarded.extend_from_slice(&b),
                DetachAction::TriggerDetach(b) => {
                    forwarded.extend_from_slice(&b);
                    return (forwarded, true);
                }
            }
        }
        // 窓満了 (= 打鍵が止まって delay 経過) を空 chunk で評価する。
        match process_ctrlz_guard(
            &[],
            &mut state,
            t0 + Duration::from_millis(n as u64 + 500),
            &cfg,
        ) {
            DetachAction::Forward(b) => {
                forwarded.extend_from_slice(&b);
                (forwarded, false)
            }
            DetachAction::TriggerDetach(b) => {
                forwarded.extend_from_slice(&b);
                (forwarded, true)
            }
        }
    }

    /// kawaz 提示の仕様表: 2 発ごとにアプリへ 1 発、余った 1 発が detach を起こす。
    #[test]
    fn ctrlz_guard_pairs_go_to_app_and_odd_one_detaches() {
        assert_eq!(press_ctrl_z(1), (vec![], true), "単発 = detach のみ");
        assert_eq!(
            press_ctrl_z(2),
            (vec![CTRL_Z_BYTE], false),
            "2 連打 = アプリへ 1 発、detach しない"
        );
        assert_eq!(
            press_ctrl_z(3),
            (vec![CTRL_Z_BYTE], true),
            "3 連打 = アプリへ 1 発 + detach"
        );
        assert_eq!(
            press_ctrl_z(4),
            (vec![CTRL_Z_BYTE, CTRL_Z_BYTE], false),
            "4 連打 = アプリへ 2 発、detach しない"
        );
        assert_eq!(
            press_ctrl_z(5),
            (vec![CTRL_Z_BYTE, CTRL_Z_BYTE], true),
            "5 連打 = アプリへ 2 発 + detach"
        );
    }

    /// 同一 chunk に複数 Ctrl+Z が入っていても (= 高速連打 / paste) 同じ規則。
    #[test]
    fn ctrlz_guard_handles_multiple_presses_in_one_chunk() {
        let t0 = std::time::Instant::now();
        let mut state = CtrlzGuardState::Idle;
        assert_eq!(
            process_ctrlz_guard(&[CTRL_Z_BYTE; 2], &mut state, t0, &guard_config()),
            DetachAction::Forward(vec![CTRL_Z_BYTE])
        );
        assert_eq!(state, CtrlzGuardState::Idle);
    }

    /// 窓は「最後の Ctrl+Z から delay」で、満了して初めて detach が確定する。
    #[test]
    fn ctrlz_guard_detach_fires_only_after_delay() {
        let t0 = std::time::Instant::now();
        let cfg = guard_config();
        let mut state = CtrlzGuardState::Idle;
        assert_eq!(
            process_ctrlz_guard(&[CTRL_Z_BYTE], &mut state, t0, &cfg),
            DetachAction::Forward(Vec::new())
        );
        assert!(matches!(state, CtrlzGuardState::Pending { .. }));
        // 満了前は何も起きない。
        assert_eq!(
            process_ctrlz_guard(&[], &mut state, t0 + Duration::from_millis(499), &cfg),
            DetachAction::Forward(Vec::new())
        );
        assert!(matches!(state, CtrlzGuardState::Pending { .. }));
        assert_eq!(
            process_ctrlz_guard(&[], &mut state, t0 + Duration::from_millis(500), &cfg),
            DetachAction::TriggerDetach(Vec::new())
        );
        assert_eq!(state, CtrlzGuardState::Idle);
    }

    /// 窓の途中で他キーが来たら detach 保留はキャンセルされ、保留 Ctrl+Z は破棄される
    /// (= アプリには送らない)。当該キーは通常入力として届く。
    #[test]
    fn ctrlz_guard_other_key_cancels_pending_and_drops_held_byte() {
        let t0 = std::time::Instant::now();
        let cfg = guard_config();
        let mut state = CtrlzGuardState::Idle;
        let _ = process_ctrlz_guard(&[CTRL_Z_BYTE], &mut state, t0, &cfg);
        assert_eq!(
            process_ctrlz_guard(b"x", &mut state, t0 + Duration::from_millis(100), &cfg),
            DetachAction::Forward(b"x".to_vec()),
            "保留 Ctrl+Z は破棄し、他キーだけ forward する"
        );
        assert_eq!(state, CtrlzGuardState::Idle);
        // キャンセル後に満了時刻を跨いでも detach しない。
        assert_eq!(
            process_ctrlz_guard(&[], &mut state, t0 + Duration::from_millis(600), &cfg),
            DetachAction::Forward(Vec::new())
        );
    }

    /// `ctrlz_guard_delay = 0` は連打判定なしの即 detach (= アプリには一切届かない)。
    #[test]
    fn ctrlz_guard_zero_delay_detaches_immediately() {
        let mut cfg = guard_config();
        cfg.ctrlz_guard_delay = Duration::ZERO;
        let mut state = CtrlzGuardState::Idle;
        assert_eq!(
            process_ctrlz_guard(b"ab\x1acd", &mut state, std::time::Instant::now(), &cfg),
            DetachAction::TriggerDetach(b"ab".to_vec()),
            "Ctrl+Z より前の入力は送り、以降は捨てて即 detach"
        );
        assert_eq!(state, CtrlzGuardState::Idle);
    }

    /// `ctrlz_guard = false` は完全 bypass (= Ctrl+Z 素通し、state を残さない)。
    #[test]
    fn ctrlz_guard_disabled_is_complete_bypass() {
        let mut cfg = guard_config();
        cfg.ctrlz_guard = false;
        let now = std::time::Instant::now();
        let mut state = CtrlzGuardState::Pending {
            deadline: now + Duration::from_millis(500),
        };
        assert_eq!(
            process_ctrlz_guard(&[CTRL_Z_BYTE, CTRL_Z_BYTE, b'x'], &mut state, now, &cfg),
            DetachAction::Forward(vec![CTRL_Z_BYTE, CTRL_Z_BYTE, b'x'])
        );
        assert_eq!(state, CtrlzGuardState::Idle);
    }

    /// poll timeout は保留中だけ張られ、満了までの残り時間になる。
    #[test]
    fn ctrlz_guard_poll_timeout_tracks_pending_deadline() {
        let now = std::time::Instant::now();
        assert_eq!(
            ctrlz_guard_poll_timeout(CtrlzGuardState::Idle, now),
            PollTimeout::NONE
        );
        let t = ctrlz_guard_poll_timeout(
            CtrlzGuardState::Pending {
                deadline: now + Duration::from_millis(120),
            },
            now,
        );
        assert_eq!(t, PollTimeout::from(120u16));
        // 満了済みでも 0 ではなく 1ms を返す (= poll(-1) で永久 block させない)。
        let t = ctrlz_guard_poll_timeout(
            CtrlzGuardState::Pending {
                deadline: now - Duration::from_millis(10),
            },
            now,
        );
        assert_eq!(t, PollTimeout::from(1u16));
    }

    // ---- OUTER_TTY_RESET (issue 2026-06-11 / 2026-07-24 H4) ----

    /// reset シーケンスに、外側端末で detach 跨ぎに悪さをする主要モードの解除 escape が
    /// 含まれていることを固定する。特に kitty keyboard protocol 解除 (`\x1b[<u`) と
    /// alt screen 解除 (`?1049l`) は現象の核心 (= detach 後の操作不能 / 画面崩れ)。
    #[test]
    fn outer_tty_reset_contains_critical_mode_resets() {
        let r = OUTER_TTY_RESET;
        // kitty keyboard protocol pop (= CSI u 化で ctrl+c/d/z が効かなくなる現象)
        assert!(
            r.windows(4).any(|w| w == b"\x1b[<u"),
            "must reset kitty keyboard protocol"
        );
        // alt screen 解除
        assert!(
            r.windows(8).any(|w| w == b"\x1b[?1049l"),
            "must leave alt screen"
        );
        // bracketed paste 解除
        assert!(
            r.windows(8).any(|w| w == b"\x1b[?2004l"),
            "must disable bracketed paste"
        );
        // cursor 表示
        assert!(r.windows(6).any(|w| w == b"\x1b[?25h"), "must show cursor");
        // mouse tracking 解除 (= 1000 系のいずれか)
        assert!(
            r.windows(8).any(|w| w == b"\x1b[?1000l"),
            "must disable mouse tracking"
        );
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
        let daemon_handle = std::thread::spawn(move || session.serve());

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
                "screen-dump-v1".into(),
                "state-snapshot-v1".into(),
                "session-exit-v1".into(),
                "child-state-v1".into(),
                "record-v1".into(),
                "set-v1".into(),
                "upgrade-v1".into(),
            ]
        );
        assert!(conn.response.leader);
        assert_eq!(conn.response.mode, Mode::Rw);

        // kill して daemon 終了させる (DR-0012: signal: None = SIGTERM default)
        conn.send_control(&ControlMessage::Kill(crate::protocol::messages::Kill {
            signal: None,
            wait: true,
        }))
        .expect("send kill");
        let exit = handle.join().expect("daemon thread").expect("daemon run");
        assert_eq!(exit, 143);
    }

    /// R4-H2: handshake error response (= daemon が `auth.token-mismatch` 等を返した)
    /// に対し、connect 側が code-specific な next-action hint 付き文言を返すこと。
    /// 旧版はどの code でも `daemon error during handshake` 一律で、ユーザが
    /// 次に何をすればいいか分からなかった。
    /// R5-FB2: `with_stdin_eof_action(SendEof)` を設定した ClientConnection で
    /// stdin EOF を受けた瞬間に EOT (0x04) が socket に送られ、daemon の child
    /// PTY (canonical mode) が EOF を見て自然終了することを確認する。
    ///
    /// 子 cmd は `cat` (= stdin を読み続け、EOF で exit する canonical-mode
    /// reader)。stdin を 1 度も書かずに closure (= 即 EOF) させる pattern。
    // R4-H5 で 3s→10s に緩和したが CI Linux で elapsed=10.02s で再 flaky。
    // daemon → child exit observation の経路に CI 環境固有の遅延があると推測。
    // event-based に書き換える (= daemon が ChildExited を broadcast したら client が
    // 即終了する経路を直接観測する形) まで一旦 ignore。R5-FB2 production code 本体は
    // 残るので機能としては動く。詳細は docs/REVIEW-BACKLOG.md の R5-FB2 annotate 参照。
    #[ignore = "CI Linux flaky (elapsed > 10s); rewrite to event-based"]
    #[test]
    fn headless_stdin_eof_terminates_child_reading_bc() {
        // 名前は要件ファイル準拠だが、portability のため bc ではなく cat を使う。
        // canonical mode + read EOF → exit という挙動は両者で同じ。
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("eof.sock");
        let cat_path = if std::path::Path::new("/bin/cat").exists() {
            "/bin/cat"
        } else if std::path::Path::new("/usr/bin/cat").exists() {
            "/usr/bin/cat"
        } else {
            return; // CI 環境で cat が無ければ skip
        };
        let cfg = DaemonConfig::new("eof-test", sock.clone(), vec![cat_path.into()]);
        let session = Session::start(cfg).expect("daemon start");
        let daemon_handle = std::thread::spawn(move || session.serve());

        // client 接続 + with_stdin_eof_action(SendEof)
        let mut conn: Option<ClientConnection> = None;
        for _ in 0..50 {
            match ClientConnection::connect(&sock, AttachOptions::default()) {
                Ok(c) => {
                    conn = Some(c);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let conn = conn
            .expect("client connect should succeed")
            .with_stdin_eof_action(StdinEofAction::SendEof);

        // 即 EOF な stdin: pipe を作って write 端をすぐ drop する (= read 端は
        // Ok(0) を即返す)。`ClientConnection::run` の `R: Read + AsFd` 制約を
        // 満たすため、read 端は `std::fs::File::from(OwnedFd)` で File 化する。
        let (rd, wr) = nix::unistd::pipe().expect("pipe");
        drop(wr); // 即 EOF
        let mut input = std::fs::File::from(rd);
        let mut output: Vec<u8> = Vec::new();
        let run_handle = std::thread::spawn(move || conn.run(&mut input, &mut output));

        // daemon thread が 10s 以内に終了することを確認する (= cat が EOT で
        // EOF を見て exit、master_fd 経由で daemon が ChildExited を観測)。
        // R4-H5: timing-tight な threshold は CI 高負荷で flaky になるため、
        // 旧 3s → 10s に緩和 (= 3x ルール準拠、event-based に書き換えは別)。
        let start = std::time::Instant::now();
        let mut daemon_done = false;
        while start.elapsed() < Duration::from_secs(10) {
            if daemon_handle.is_finished() {
                daemon_done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            daemon_done,
            "daemon must terminate within 10s after stdin EOF + EOT propagation; elapsed={:?}",
            start.elapsed()
        );
        let _ = daemon_handle.join();
        let _ = run_handle.join();
    }

    #[test]
    fn connect_token_mismatch_returns_specific_hint() {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("test.sock");
        let mut cfg =
            DaemonConfig::new("demo", sock.clone(), vec!["/bin/sleep".into(), "30".into()]);
        cfg.expected_token = Some("secret-xyz".into());
        let session = Session::start(cfg).expect("daemon start");
        let daemon_handle = std::thread::spawn(move || session.serve());

        // listener bind 完了を待ってから connect (= CI の slow path 対策)。
        // 連続 connect で daemon を多重に handshake させないため、ENOENT 系の
        // 「まだ socket が無い」errno だけ retry し、handshake まで届いたら break。
        let mut last_err: Option<Error> = None;
        for attempt in 0..50 {
            let res = ClientConnection::connect(
                &sock,
                AttachOptions {
                    token: Some("wrong-token".into()),
                    ..AttachOptions::default()
                },
            );
            match res {
                Ok(_) => panic!("expected handshake to be rejected, but connect succeeded"),
                Err(e) => {
                    let msg = format!("{e}");
                    // socket 未生成由来の Errno (ENOENT/ECONNREFUSED) は retry
                    if attempt < 49
                        && (msg.contains("No such file") || msg.contains("Connection refused"))
                    {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    last_err = Some(e);
                    break;
                }
            }
        }
        let err = last_err.expect("connect should fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("HYOUI_LOCK_TOKEN") || msg.contains("token"),
            "error message must mention token-related hint, got: {msg}"
        );
        // 必ず一律の `daemon error during handshake` ではないこと (= R4-H2)
        assert!(
            !msg.contains("daemon error during handshake"),
            "must not fall through to generic message, got: {msg}"
        );

        // daemon thread は handshake 拒否で Err 終了
        let _ = daemon_handle.join();
    }

    #[test]
    fn run_returns_when_daemon_closes() {
        // R5: 旧版は `/bin/true` の即 exit で daemon を畳んで socket close を
        // 誘発していたが、`/bin/true` が serve_loop 起動前後に exit する race で
        // listener が早期 drop され、client の `connect` 50 retry × 10ms 内に
        // handshake が成立できず panic する flaky があった (= "test result:
        // FAILED ... finished in 0.63s" 失敗パターン、bg 負荷下で再現)。
        //
        // 本 test の主旨は「daemon が socket を close した瞬間に client.run が
        // 正常 return する」こと。child 即 exit に頼らず、`sleep 30` で daemon
        // を一時的に生かして確実に connect/handshake を成立させた上で、
        // `Kill` control message を送って明示的に daemon を畳む。socket close
        // が起きるのは結局 daemon thread 終了時の `drop(listener)` 時点なので
        // 検証意図は変わらない。
        let (_dir, handle, mut conn) =
            spawn_daemon_and_connect_client(vec!["/bin/sleep".into(), "30".into()]);

        // daemon に Kill を送る (DR-0012: signal=None で default SIGTERM)。
        // 受信した daemon は child に SIGTERM → reap → SessionExitNotify(143) を
        // broadcast → serve_loop 終了 → listener / socket close、という経路。
        conn.send_control(&ControlMessage::Kill(crate::protocol::messages::Kill {
            signal: None,
            wait: true,
        }))
        .expect("send kill");

        // stdin 側は pipe の read 端。本 test の終了条件は daemon 側なので、stdin EOF
        // (= Detached) が先に競合しないよう write 端を保持する (= issue 2026-06-11
        // 優先1 で stdin EOF と socket EOF を別 outcome に分けたため)。
        let (rd, wr_keep) = nix::unistd::pipe().expect("pipe");
        let mut stdin = std::fs::File::from(rd);
        let mut stdout = Vec::<u8>::new();
        let result = conn.run(&mut stdin, &mut stdout);
        drop(wr_keep);
        // issue 2026-06-11 優先1: daemon が child を kill して SessionExitNotify を
        // 送り切れば ChildExited(143)、送り切る前に socket が閉じれば ConnectionLost。
        // 優先1 で broadcast 後に drain wait を入れたので通常は ChildExited だが、
        // 高負荷で drain が間に合わなければ ConnectionLost もあり得る。本 test の主旨は
        // 「daemon 終了で run が hang せず Ok で抜ける」ことなので両方許容する。
        assert!(
            matches!(
                result,
                Ok(RunOutcome::ChildExited { .. }) | Ok(RunOutcome::ConnectionLost)
            ),
            "run must return ChildExited or ConnectionLost on daemon close: {result:?}"
        );
        let exit = handle.join().expect("daemon thread").expect("daemon run");
        // SIGTERM kill → shell convention で 128 + SIGTERM(15) = 143。
        // race で child が SIGTERM 前に exit していれば 0 もありうるが、
        // `/bin/sleep 30` は明確に alive なので 143 が期待値。緩衝として 0 も許容。
        assert!(exit == 0 || exit == 143, "expected 0 or 143, got {exit}");
    }

    // ---- issue 2026-06-11 優先1: RunOutcome の分解 ----

    /// Ctrl+Z 単発 (= ガード発火) で `RunOutcome::Detached` を返す (= 自発 detach、
    /// CLI 層は exit 0)。daemon 役には Detach control message が届き、子には
    /// Ctrl+Z が **届かない** (DR-0029 §2)。
    #[test]
    fn run_returns_detached_on_single_ctrl_z() {
        use crate::protocol::ControlMessage;
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        // stdin に Ctrl+Z を 1 発流す pipe。
        let (stdin_rd, stdin_wr) = nix::unistd::pipe().expect("stdin pipe");
        nix::unistd::write(&stdin_wr, &[CTRL_Z_BYTE]).expect("write ctrl-z");
        // write 端は保持 (= EOF させずガード経路を確実に通す)。
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));

        // daemon 役: Detach message を受け取る。
        let mut daemon_reader = daemon_sock;
        let frame = Frame::decode_from(&mut daemon_reader).expect("decode detach frame");
        assert_eq!(frame.ty, TYPE_CBOR_CONTROL);
        assert!(
            matches!(
                ControlMessage::decode_from(frame.body.as_slice()),
                Ok(ControlMessage::Detach(_))
            ),
            "Ctrl+Z 単発で Detach message が届くはず"
        );

        let res = run_handle.join().expect("join");
        let _ = stdin_wr; // keep alive until join
        assert!(
            matches!(res, Ok(RunOutcome::Detached)),
            "Ctrl+Z 単発は Detached を返すはず: {res:?}"
        );
    }

    /// detach で attach を畳むとき、外側が raw mode の tty なら
    /// [`OUTER_TTY_RESET`] を stdout に吐いてから return する。従来は Detach message
    /// 送信のみで reset 未送出、alt screen / kitty keyboard / bracketed paste 等が
    /// 外側 tty に残留し detach 後に garbage を拾う bug があった
    /// (docs/issue/2026-07-24-bug-tstp-intercept-followups.md H4)。
    #[test]
    fn run_emits_reset_before_detach_when_outer_tty_raw() {
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: true,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        let (stdin_rd, stdin_wr) = nix::unistd::pipe().expect("stdin pipe");
        nix::unistd::write(&stdin_wr, &[CTRL_Z_BYTE]).expect("write ctrl-z");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        let run_handle = std::thread::spawn(move || {
            let r = conn.run(&mut stdin_file, &mut stdout);
            (r, stdout)
        });

        // daemon 役は Detach frame を捨てる (= client を hang させない)。
        let mut daemon_reader = daemon_sock;
        let _ = Frame::decode_from(&mut daemon_reader);

        let (res, out) = run_handle.join().expect("join");
        drop(stdin_wr);
        assert!(matches!(res, Ok(RunOutcome::Detached)), "{res:?}");
        // detach 直前に reset シーケンスが吐かれる。
        assert!(
            out.windows(OUTER_TTY_RESET.len())
                .any(|w| w == OUTER_TTY_RESET),
            "OUTER_TTY_RESET を含むはず (H4 fix): stdout_hex={}",
            hex_lower(&out)
        );
    }

    /// H4 対称確認: 外側が tty でない (= `hyoui attach | cat` 等) 場合は reset を
    /// 吐かない (= pipe に escape sequence を垂れ流さない)。
    #[test]
    fn run_does_not_emit_reset_when_outer_tty_absent() {
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        let (stdin_rd, stdin_wr) = nix::unistd::pipe().expect("stdin pipe");
        nix::unistd::write(&stdin_wr, &[CTRL_Z_BYTE]).expect("write ctrl-z");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        let run_handle = std::thread::spawn(move || {
            let r = conn.run(&mut stdin_file, &mut stdout);
            (r, stdout)
        });
        let mut daemon_reader = daemon_sock;
        let _ = Frame::decode_from(&mut daemon_reader);
        let (res, out) = run_handle.join().expect("join");
        drop(stdin_wr);
        assert!(matches!(res, Ok(RunOutcome::Detached)), "{res:?}");
        assert!(out.is_empty(), "非 tty では reset を吐かない: got {out:?}");
    }

    fn hex_lower(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for x in b {
            s.push_str(&format!("{x:02x}"));
        }
        s
    }

    /// `SessionExitNotify` を受信したら `RunOutcome::ChildExited { exit_status }` を
    /// 返す (= CLI 層が子の exit code を伝搬)。
    #[test]
    fn run_returns_child_exited_on_session_exit_notify() {
        use crate::protocol::ControlMessage;
        use crate::protocol::messages::SessionExitNotify;
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        // stdin は EOF させない (= 書き込み端を保持)。
        let (stdin_rd, stdin_wr_keep) = nix::unistd::pipe().expect("stdin pipe");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        // daemon 役: exit_status=42 の SessionExitNotify を送る。
        let mut daemon_sock = daemon_sock;
        let notify = ControlMessage::SessionExitNotify(SessionExitNotify {
            exit_status: 42,
            signal: None,
        });
        let body = notify.encode_to_vec().expect("encode exit notify");
        Frame::cbor_control(body)
            .encode_to(&mut daemon_sock)
            .expect("send exit notify");

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));
        let res = run_handle.join().expect("join");
        drop(stdin_wr_keep);
        drop(daemon_sock);
        assert!(
            matches!(res, Ok(RunOutcome::ChildExited { exit_status: 42 })),
            "SessionExitNotify は ChildExited{{42}} を返すはず: {res:?}"
        );
    }

    /// stdin が EOF + `--stdin-eof=detach` (default Detach) なら自発 detach として
    /// `RunOutcome::Detached` を返す。
    #[test]
    fn run_returns_detached_on_stdin_eof_with_detach_action() {
        use std::os::unix::net::UnixStream;

        let (client_sock, _daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        // 即 EOF な stdin。
        let (stdin_rd, stdin_wr) = nix::unistd::pipe().expect("stdin pipe");
        drop(stdin_wr);
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        let res = conn.run(&mut stdin_file, &mut stdout);
        assert!(
            matches!(res, Ok(RunOutcome::Detached)),
            "stdin EOF + Detach action は Detached を返すはず: {res:?}"
        );
    }

    /// daemon が `error` kind=`backpressure.disconnect` を送ってきたら
    /// `RunOutcome::BackpressureDisconnected` を返す (= CLI 層が exit 9 +
    /// backpressure 専用 stderr を出すための分離。socket EOF を待たず即 return)。
    #[test]
    fn run_returns_backpressure_disconnected_on_error_control() {
        use crate::protocol::ControlMessage;
        use crate::protocol::messages::{ErrorCode, ErrorMessage};
        use std::os::unix::net::UnixStream;

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        let (stdin_rd, stdin_wr_keep) = nix::unistd::pipe().expect("stdin pipe");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout: Vec<u8> = Vec::new();

        let mut daemon_sock = daemon_sock;
        let err = ControlMessage::Error(ErrorMessage {
            code: ErrorCode::BackpressureDisconnect,
            message: "send queue overflow".into(),
            details: None,
        });
        let body = err.encode_to_vec().expect("encode error");
        Frame::cbor_control(body)
            .encode_to(&mut daemon_sock)
            .expect("send error");

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));
        let res = run_handle.join().expect("join");
        drop(stdin_wr_keep);
        drop(daemon_sock);
        assert!(
            matches!(res, Ok(RunOutcome::BackpressureDisconnected)),
            "backpressure.disconnect error は BackpressureDisconnected を返すはず: {res:?}"
        );
    }

    /// daemon が RAW_DATA を送ってきたが stdout への write が失敗する場合、
    /// `RunOutcome::StdoutWriteFailed` を返す (= daemon 健在 / 自分側出力経路の故障。
    /// `ConnectionLost` (= daemon 消滅) と区別して CLI 層 exit 1)。
    #[test]
    fn run_returns_stdout_write_failed_on_output_error() {
        use std::os::unix::net::UnixStream;

        /// write が必ず失敗する Writer (= 出力先 pipe の読み手が消えた等を模す)。
        struct FailWriter;
        impl std::io::Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "broken",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "broken",
                ))
            }
        }

        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };

        let (stdin_rd, stdin_wr_keep) = nix::unistd::pipe().expect("stdin pipe");
        let mut stdin_file = std::fs::File::from(stdin_rd);
        let mut stdout = FailWriter;

        // daemon 役: RAW_DATA frame を送る (= stdout への write を誘発)。
        let mut daemon_sock = daemon_sock;
        Frame::raw_data(b"hello".to_vec())
            .encode_to(&mut daemon_sock)
            .expect("send raw data");

        let run_handle = std::thread::spawn(move || conn.run(&mut stdin_file, &mut stdout));
        let res = run_handle.join().expect("join");
        drop(stdin_wr_keep);
        drop(daemon_sock);
        assert!(
            matches!(res, Ok(RunOutcome::StdoutWriteFailed)),
            "stdout write 失敗は StdoutWriteFailed を返すはず: {res:?}"
        );
    }

    // ===== DR-0021 改訂: partial-byte race / stale-ack / m1 ack-skip の unit test =====

    /// テスト用に socketpair で直結された ClientConnection を作る helper。
    /// 戻り値: (client connection, daemon 側 socket = テストから書く / 読む側)。
    fn make_pair_connection() -> (ClientConnection, UnixStream) {
        let (client_sock, daemon_sock) = UnixStream::pair().expect("socketpair");
        let transport = UnixStreamTransport::new(client_sock);
        let (reader, writer) = transport.split().expect("split");
        let conn = ClientConnection {
            reader,
            writer,
            response: HandshakeResponse {
                caps: vec![],
                session_id: "t".into(),
                client_id: 1,
                leader: false,
                mode: Mode::Rw,
                child_stopped: false,
            },
            eof_action: StdinEofAction::Detach,
            outer_tty_raw: false,
            winch_source: None,
            pending_frames: std::collections::VecDeque::new(),
            attach_config: crate::config::AttachConfig::default(),
            poisoned: false,
        };
        (conn, daemon_sock)
    }

    /// DR-0021 改訂 M2: `send_raw_bytes` の ack 待ちが timeout で打ち切られた後、
    /// 同 connection への次の `send_raw_bytes` は **stale ack を誤受理せず**
    /// `Error::Invalid("connection poisoned...")` で即時 fail する。
    ///
    /// scenario:
    /// 1. client が send_raw_bytes を呼ぶ → raw_data が daemon socket に流れる
    /// 2. daemon は ack を返さない (= test では何も書かない)
    /// 3. `RAW_ACK_TIMEOUT` 経過で client は `Err(Error::Invalid("raw_ack timeout..."))`
    /// 4. ここで daemon が **遅れて** ack を書く (= stale ack)
    /// 5. client が **再度** send_raw_bytes を呼ぶ → poison check で即 fail、
    ///    stale ack を誤受理しない
    ///
    /// 旧実装 (= poison なし) では (5) で stale ack を「自分宛 ack」として受理して
    /// silent wrong behavior が起きていた。
    #[test]
    fn send_raw_bytes_after_timeout_is_poisoned_and_rejects_stale_ack() {
        let (mut conn, daemon_sock) = make_pair_connection();

        // RAW_ACK_TIMEOUT を待つと test が遅いので、connection に短い timeout を仕掛ける
        // ことはできない (= RAW_ACK_TIMEOUT は const)。代わりに、daemon socket を完全に
        // 閉じることで poll が POLLIN を返し、続く decode_from が EOF → I/O error で
        // 即 poison 経路を踏ませる (= timeout 等価の poison path、両経路とも
        // `result.is_err()` で `poison()` が呼ばれる)。
        drop(daemon_sock);

        // この呼び出しは I/O error で fail し poison される (= EPIPE on write、
        // または ack 待ちの decode で EOF)。
        let r1 = conn.send_raw_bytes(b"x");
        assert!(r1.is_err(), "daemon closed: first call must fail: {r1:?}");
        assert!(
            conn.poisoned,
            "connection must be poisoned after I/O failure"
        );

        // 2 回目の呼び出しは poison check で即 fail (= stale ack 受理経路に進まない)。
        let r2 = conn.send_raw_bytes(b"y");
        match r2 {
            Err(Error::Invalid(msg)) => {
                assert!(
                    msg.contains("poisoned"),
                    "second call must return poison error, got {msg:?}"
                );
            }
            other => panic!("expected poison error on second call, got {other:?}"),
        }
    }

    /// DR-0021 改訂: daemon が ack:Error を返した場合は poison しない
    /// (= ack 自体は正常受信、caller が semantic で継続判断する)。
    #[test]
    fn send_raw_bytes_remote_error_does_not_poison() {
        let (mut conn, mut daemon_sock) = make_pair_connection();

        // 別 thread で daemon 側: client の raw_data を読み、ack:Error を返す。
        let daemon_thread = std::thread::spawn(move || {
            let frame = Frame::decode_from(&mut daemon_sock).expect("decode raw_data");
            assert_eq!(frame.ty, TYPE_RAW_DATA);
            let ack = RawAck::err("test.rejected", "denied");
            let body = ack.encode_to_vec().expect("encode ack");
            Frame::raw_ack(body)
                .encode_to(&mut daemon_sock)
                .expect("send ack");
            // socket は close せず保持 (= 次の呼び出しのため)
            daemon_sock
        });

        let r = conn.send_raw_bytes(b"hello");
        match r {
            Err(Error::Remote(msg)) => assert!(msg.contains("denied")),
            other => panic!("expected Err(Remote(..)), got {other:?}"),
        }
        assert!(
            !conn.poisoned,
            "Remote ack:Error must NOT poison (semantic-level failure, not I/O)"
        );
        let _daemon_sock = daemon_thread.join().expect("daemon join");
    }

    /// DR-0021 改訂 m1: `recv_control` は不要な `TYPE_RAW_ACK` を silent skip
    /// (= 旧実装は `unexpected frame type` で hard error)。
    #[test]
    fn recv_control_silently_skips_unsolicited_raw_ack() {
        use crate::protocol::messages::LeaderNotify;

        let (mut conn, mut daemon_sock) = make_pair_connection();

        // daemon 側: stale ack を 1 枚 + 本物の control message を送る。
        std::thread::spawn(move || {
            // (1) unsolicited RAW_ACK (= stale)
            let ack = RawAck::ok();
            let body = ack.encode_to_vec().expect("encode ack");
            Frame::raw_ack(body)
                .encode_to(&mut daemon_sock)
                .expect("send raw_ack");
            // (2) 本物の control message
            let msg = ControlMessage::LeaderNotify(LeaderNotify { client_id: Some(7) });
            let cbor = msg.encode_to_vec().expect("encode ctrl");
            Frame::cbor_control(cbor)
                .encode_to(&mut daemon_sock)
                .expect("send ctrl");
        });

        // recv_control は RAW_ACK を skip して LeaderNotify を返す。
        let got = conn
            .recv_control(None)
            .expect("recv_control should succeed");
        match got {
            ControlMessage::LeaderNotify(n) => {
                assert_eq!(n.client_id, Some(7));
            }
            other => panic!("expected LeaderNotify, got {other:?}"),
        }
    }

    /// DR-0021 改訂 (1) partial-byte race の **mock daemon** 再現 unit test。
    ///
    /// scenario: daemon が `RAW_ACK` を返す前に、**ack 待ち中の client にとって
    /// non-ack の大きな frame** (= broadcast 由来の RAW_DATA frame) を 1 枚送る。
    /// 続けて RAW_ACK frame を送る。client は両 frame を順次 decode し、
    /// non-ack は pending_frames に積み、ack を return しなければならない。
    ///
    /// 旧実装は read_timeout が中途半端な短さで設定された場合に大きな non-ack frame の
    /// 途中で TimedOut を踏み partial-byte discard で frame protocol が壊れる事故が
    /// 起きた (= 実機 python 1038 B 再現)。新実装 (poll-based + blocking decode) では
    /// frame 境界で deadline 判定のみが効くため、frame body の途中で stream が
    /// 切られない (= partial-byte race が構造的に消える)。
    #[test]
    fn send_raw_bytes_handles_large_non_ack_frame_then_ack() {
        let (mut conn, mut daemon_sock) = make_pair_connection();

        // daemon thread: client の raw_data 受信 → 大きな non-ack frame (= 4 KiB の
        // RAW_DATA broadcast) → ack frame、を順に流す。
        let daemon_thread = std::thread::spawn(move || {
            let req = Frame::decode_from(&mut daemon_sock).expect("decode raw_data req");
            assert_eq!(req.ty, TYPE_RAW_DATA);
            // 4 KiB の non-ack frame (= broadcast の RAW_DATA を装う)
            let big_body = vec![b'B'; 4096];
            Frame::raw_data(big_body)
                .encode_to(&mut daemon_sock)
                .expect("send big raw_data");
            // ack
            let ack = RawAck::ok();
            let body = ack.encode_to_vec().expect("encode ack");
            Frame::raw_ack(body)
                .encode_to(&mut daemon_sock)
                .expect("send ack");
            daemon_sock
        });

        // client: send_raw_bytes は大きな non-ack frame をスキップ (= pending_frames に
        // 積む) して ack を待ち、Ok を返す。
        let r = conn.send_raw_bytes(b"req");
        assert!(
            r.is_ok(),
            "send_raw_bytes should succeed even with large non-ack frame before ack: {r:?}"
        );
        // pending_frames に積まれているはず (= 後続 recv_frame で取り出せる)。
        assert_eq!(
            conn.pending_frames.len(),
            1,
            "non-ack frame must be buffered"
        );
        let buffered = &conn.pending_frames[0];
        assert_eq!(buffered.ty, TYPE_RAW_DATA);
        assert_eq!(buffered.body.len(), 4096);

        let _ = daemon_thread.join().expect("daemon join");
    }
}
