//! client ごとの送信 queue + broadcast helpers (DR-0009 Phase C で `session.rs` から分離、
//! DR-0037 段階 4 で送信を serve loop 内の nonblocking write に移した)。
//!
//! ## 構成
//!
//! - [`ClientHandle`]: attach 中の 1 client (socket + 送信 queue + 受信 decoder)
//! - [`ClosingClient`]: 切断が決まった client。送信 queue が空になるか deadline で close する
//! - [`SendQueue`]: 1 client の送信 queue (byte 数で上限判定、nonblocking write で流す)
//! - [`Subscription`]: client の出力 subscription (Raw / TailFollow)
//! - [`EnqueueOutcome`]: 1 frame の enqueue 結果 (Sent / Overflow / SendFailed)
//! - [`enqueue_for_client`]: 1 client への frame enqueue (= byte-level cap check)
//! - [`send_backpressure_error`]: backpressure.disconnect error の best-effort 送信
//! - [`send_control`]: 1 client への control message 送信
//! - [`broadcast_master_bytes`]: 子 PTY 出力を subscription 別 frame で全 client に
//! - [`broadcast_control`]: CBOR control message を全 client に
//! - [`broadcast_bytes`]: 既に encode 済 bytes を全 client に enqueue
//! - [`flush_clients`] / [`advance_closing`]: serve loop が毎周回呼ぶ nonblocking write
//! - [`flush_until`] / [`drain_closing`]: serve loop の外 (終了処理・linger) で、送信 queue が
//!   空になるのを deadline まで `poll(POLLOUT)` で待つ
//! - [`instant_to_epoch_ms`]: tail.data の timestamp_ms 用近似変換
//!
//! ## 送信の流れ (DR-0037 段階 4)
//!
//! client socket は O_NONBLOCK。enqueue は送信 queue に積むだけで socket に触らない。serve loop
//! は周回の冒頭で全 client の queue を書けるだけ書き ([`flush_clients`])、書き残しがある client
//! は `POLLOUT` を付けて poll する。書けない (`EAGAIN`) 時は次の周回に回すだけで、相手が
//! 読まなくても loop は戻る。queue が上限を超えた client は切断する (backpressure)。
//!
//! 切断は socket の close だけで完結する。切断前に積んだ frame
//! (失敗 ack・detach ack・backpressure error) を届けるため、切断が決まった client は
//! [`ClosingClient`] として queue が空になるまで書き続け、空になった時点か deadline
//! ([`CLOSE_FLUSH_TIMEOUT`]) で close する。
//!
//! ## payload sharing (R5-H9)
//!
//! broadcast 系の payload は [`SharedBytes`] (= `Arc<Vec<u8>>`) で共有する。
//! 1 frame を encode した直後に `Arc::new` で wrap し、各 client への enqueue は
//! `Arc::clone` (= refcount 加算のみ、bytes copy なし) で渡す。これにより
//! `N clients × frame_size` 分の memcpy が消える (R5-PERF-H1 / R5-H9)。
//! payload は最後の Arc が drop された時点で解放される (= 全 client が socket 送信を
//! 完了した後)。
//!
//! ## session.rs / 他 module との接続
//!
//! - [`ClientHandle`] は `daemon` module 内 (= accept.rs の `finalize_accepted_client`、
//!   session.rs の serve_loop / 終了処理) で構築・切断する。
//! - control.rs / wait.rs / tail.rs / lock.rs は `send_control` / `broadcast_control`
//!   を呼ぶ片方向依存 (= DR-0009 §module DAG)。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write as _;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::protocol::messages::{ErrorCode, ErrorMessage, RawAck, TailData};
use crate::protocol::{ControlMessage, Frame, FrameDecoder, Mode};
use crate::sys::clock::now_unix_ms;
use crate::sys::poll::{PollFlags, poll};

/// daemon が同時 attach を許す client 数上限 (= D6 集合 backpressure DoS 対策)。
/// 超過した accept は即 socket close で reject。`client_buffer_bytes` が 8 MiB の
/// 場合、64 clients × 8 MiB = 最大 512 MiB の queue 占有が理論上限。
pub(super) const MAX_CLIENTS_PER_DAEMON: usize = 64;

/// 切断が決まった client ([`ClosingClient`]) の送信 queue を書き切るのを待つ上限。
///
/// client が socket を読まない / 死んでいる場合に、close までに待つ最大時間。short ack frame
/// (≤ ~200 bytes) は kernel socket buffer が空でない限り即書ける想定なので、500 ms は
/// 十分余裕。これを超えるなら client が backpressure に陥っており、どの道 client は
/// 既に応答できない状態にある。serve loop は待たず、この deadline を poll の timeout に
/// 畳み込むだけ (DR-0037 I-3)。
pub(super) const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

/// broadcast payload の共有所有型 (R5-H9 zero-copy 化)。
///
/// 1 frame の encode 済 bytes を `Arc::new` で wrap し、`Arc::clone` で
/// N clients に配布する。payload bytes 自体は最後の Arc が drop された時点で解放される。
pub(super) type SharedBytes = Arc<Vec<u8>>;

/// 1 回の [`SendQueue::write_to`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteProgress {
    /// queue を全部書き終えた (= queue は空)。
    Drained,
    /// socket の送信 buffer が埋まった (`EAGAIN`)。残りは次に書ける時に書く。
    Blocked,
    /// 書き込みが失敗した (相手が閉じた等)。queue は捨て、以後は積まない。
    Failed,
}

/// 1 client の送信 queue (DR-0037 段階 4)。
///
/// Phase 12 の byte 単位の厳密 cap (DR-0008 §8.2) をここで数える。`queued_bytes` は queue に
/// 残っていて socket にまだ書いていない bytes 数 (先頭 frame の書き終えた分は含まない)。
/// kernel の socket buffer に入った分は数えない。
#[derive(Debug, Default)]
pub(super) struct SendQueue {
    frames: VecDeque<SharedBytes>,
    /// 先頭 frame のうち socket に書き終えた bytes 数 (部分 write の続きの位置)。
    head_written: usize,
    queued_bytes: usize,
    /// 書き込みが失敗した。相手は既に閉じているので、以後の frame は積まずに捨てる。
    failed: bool,
}

impl SendQueue {
    /// まだ socket に書いていない bytes 数。
    pub(super) fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    /// これ以上書くものが無い (= queue が空、または書き込みに失敗して捨てた)。
    pub(super) fn is_settled(&self) -> bool {
        self.failed || self.frames.is_empty()
    }

    fn push(&mut self, payload: SharedBytes) {
        self.queued_bytes += payload.len();
        self.frames.push_back(payload);
    }

    fn fail(&mut self) {
        self.failed = true;
        self.frames.clear();
        self.head_written = 0;
        self.queued_bytes = 0;
    }

    /// queue の先頭から、`stream` に書けるだけ書く。`stream` は O_NONBLOCK の前提で、送信
    /// buffer が埋まれば `EAGAIN` で戻る (相手の読み取りを待たない、DR-0037 I-1)。
    pub(super) fn write_to(&mut self, mut stream: &UnixStream) -> WriteProgress {
        if self.failed {
            return WriteProgress::Failed;
        }
        while let Some(front) = self.frames.front() {
            let rest = &front[self.head_written..];
            if rest.is_empty() {
                self.frames.pop_front();
                self.head_written = 0;
                continue;
            }
            match stream.write(rest) {
                Ok(0) => {
                    self.fail();
                    return WriteProgress::Failed;
                }
                Ok(n) => {
                    self.head_written += n;
                    self.queued_bytes -= n;
                    if self.head_written == front.len() {
                        self.frames.pop_front();
                        self.head_written = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return WriteProgress::Blocked;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.fail();
                    return WriteProgress::Failed;
                }
            }
        }
        WriteProgress::Drained
    }
}

/// attach 中の 1 client (socket + 送信 queue + 受信 decoder)。
///
/// `stream` は O_NONBLOCK で、受信 (serve loop の read) と送信 ([`SendQueue::write_to`]) の
/// 両方に使う。どちらも `EAGAIN` で loop に戻り、相手の協力を待たない (DR-0037 I-1)。
pub(super) struct ClientHandle {
    pub(super) id: u64,
    pub(super) mode: Mode,
    /// leader 取得状態 (= rw mode の最初の client が true)。
    pub(super) leader: bool,
    /// 受信 subscription (= broadcast の encoding 種類を切り替える)。
    pub(super) subscription: Subscription,
    /// handshake 後の有効 capability 集合 (= MVP_CAPS と req.caps の intersect)。
    /// D7: 後続 message の処理で「cap が無いのに該当 message を送ってきた」を
    /// reject する。
    pub(super) negotiated_caps: Vec<String>,
    /// client との socket (O_NONBLOCK)。
    pub(super) stream: UnixStream,
    /// daemon → client の送信 queue。
    ///
    /// Design rationale: enqueue は `&ClientHandle` (共有参照) から行う。broadcast / control
    /// handler / reducer の execute は client 列を共有参照で走査しながら複数の client に積む
    /// 構造で、送信 queue への追加は client の識別情報 (mode / leader / caps) を変えない。
    /// そこで送信 queue だけを内部可変にする。serve loop は単一 thread なので `RefCell` で足り、borrow は enqueue / write の
    /// 関数内で閉じる (再入しない)。
    send: RefCell<SendQueue>,
    /// queue の byte 上限 (= `DaemonConfig::client_buffer_bytes`)。
    pub(super) buffer_limit: usize,
    /// `stream` から受信して、まだ frame として揃っていない bytes (DR-0037 段階 1)。
    /// serve loop は届いた分だけここに足し、揃った frame を取り出して処理する
    /// (= 相手が frame の途中で止まっても loop は待たない)。
    pub(super) decoder: FrameDecoder,
    /// この client が attach した時刻 (= unix epoch ミリ秒、DR-0020 §5)。
    /// `status` の client 一覧で「接続時刻」を表示する用途。
    pub(super) connected_at_unix_ms: u64,
}

impl ClientHandle {
    /// handshake を終えた client を組み立てる。`stream` を O_NONBLOCK にする (受信と送信の
    /// どちらも loop を止めないための前提で、ここで一括して付ける)。
    pub(super) fn new(
        id: u64,
        mode: Mode,
        leader: bool,
        negotiated_caps: Vec<String>,
        stream: UnixStream,
        buffer_limit: usize,
    ) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            id,
            mode,
            leader,
            subscription: Subscription::Raw,
            negotiated_caps,
            stream,
            send: RefCell::new(SendQueue::default()),
            buffer_limit,
            decoder: FrameDecoder::new(),
            connected_at_unix_ms: now_unix_ms(),
        })
    }

    /// まだ socket に書いていない bytes 数。
    pub(super) fn queued_bytes(&self) -> usize {
        self.send.borrow().queued_bytes()
    }

    /// 送信 queue にこれ以上書くものが無いか (空、または書き込み失敗で捨てた)。
    pub(super) fn send_settled(&self) -> bool {
        self.send.borrow().is_settled()
    }

    /// 送信 queue を書けるだけ書く (nonblocking)。
    pub(super) fn flush_send(&self) -> WriteProgress {
        self.send.borrow_mut().write_to(&self.stream)
    }

    /// 切断が決まった client を [`ClosingClient`] に移す。送信 queue に何も残っていなければ
    /// `None` を返し、socket はここで close される (= drop)。
    pub(super) fn into_closing(self, deadline: Instant) -> Option<ClosingClient> {
        let send = self.send.into_inner();
        if send.is_settled() {
            return None;
        }
        Some(ClosingClient {
            stream: self.stream,
            send,
            deadline,
        })
    }

    #[cfg(test)]
    pub(super) fn queued_frames(&self) -> Vec<SharedBytes> {
        self.send.borrow().frames.iter().cloned().collect()
    }

    /// test 用: 送信 queue の先頭 frame を取り出す (socket には書かない)。
    #[cfg(test)]
    pub(super) fn pop_queued_frame(&self) -> Option<SharedBytes> {
        let mut q = self.send.borrow_mut();
        let f = q.frames.pop_front()?;
        q.queued_bytes -= f.len() - q.head_written;
        q.head_written = 0;
        Some(f)
    }
}

/// 切断が決まった client。送信 queue に残った frame (失敗 ack・detach ack・backpressure error
/// 等) を書き続け、queue が空になった時点、書き込みが失敗した時点、`deadline` を過ぎた時点の
/// いずれかで close する (DR-0037「ack を送り切ってから切る」の deadline 付き state)。
///
/// 受信は処理しない (切断が決まった client の frame は捨てる)。close は drop (= socket の close)
/// だけで、join や blocking write は無い。
pub(super) struct ClosingClient {
    stream: UnixStream,
    send: SendQueue,
    deadline: Instant,
}

impl ClosingClient {
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

/// 切断する client を close へ進める。送信 queue に残りがあれば `closing` に移して
/// [`CLOSE_FLUSH_TIMEOUT`] まで書き続け、無ければその場で close する。
pub(super) fn retire_client(closing: &mut Vec<ClosingClient>, ch: ClientHandle) {
    if let Some(c) = ch.into_closing(Instant::now() + CLOSE_FLUSH_TIMEOUT) {
        closing.push(c);
    }
}

/// client の出力 subscription (Phase 11)。
///
/// - `Raw`: 通常 attach (= `hyoui run` / `hyoui attach`)、子 PTY 出力を
///   `TYPE_RAW_DATA` frame で受け取る。
/// - `TailFollow`: `tail.request { follow: true }` 後、子 PTY 出力を
///   `tail.data` CBOR frame で受け取る (strip_ansi 適用は per-chunk best-effort)。
#[derive(Debug, Clone, Copy)]
pub(super) enum Subscription {
    Raw,
    TailFollow { strip_ansi: bool },
}

/// 1 client への frame enqueue 結果 (Phase 12 backpressure)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnqueueOutcome {
    /// queue に追加成功、serve loop が socket に書き出す。
    Sent,
    /// `buffer_limit` 超過 (= 当該 client を disconnect すべき)。
    Overflow,
    /// 以前の書き込みが失敗している (= 相手が閉じた。再 enqueue 不能)。
    SendFailed,
}

/// 1 frame の bytes を 1 client の queue に積む。
pub(super) fn enqueue_for_client(ch: &ClientHandle, payload: SharedBytes) -> EnqueueOutcome {
    let size = payload.len();
    let mut q = ch.send.borrow_mut();
    if q.failed {
        return EnqueueOutcome::SendFailed;
    }
    let cur = q.queued_bytes();
    // **単一 frame が buffer_limit を超える場合は overflow にしない** (= queue が空
    // なら必ず 1 frame は受け入れる)。backpressure は「client が読み遅れて queue が
    // 溜まった」ことを検出する機構であって、frame 1 個の大きさを検査するものではない。
    //
    // 子 PTY の読み取り chunk は 8 KiB 固定なので、`cur + size > limit` だけを見ると
    // `client_buffer_bytes` を 8 KiB 未満に設定した時に queue が空の client すら attach
    // 直後に切られ、**誰も接続できない daemon** になる (= kill も届かず serve が永久に
    // 終わらない)。
    //
    // 「空 queue なら 1 frame は通す」ことで、大きい frame も必ず前進する
    // (= 書き切れば queued_bytes は 0 に戻る)。読み遅れている client は
    // cur > 0 のまま次の frame で overflow するので、backpressure の意図は保たれる。
    if cur > 0 && cur.saturating_add(size) > ch.buffer_limit {
        return EnqueueOutcome::Overflow;
    }
    q.push(payload);
    EnqueueOutcome::Sent
}

/// 1 client への enqueue 結果を評価し、overflow なら disconnect
/// 対象として `overflow_ids` に push する。overflow の場合は client に
/// `backpressure.disconnect` error を best-effort 送信して切断理由を通知する。
///
/// 3 つの broadcast helper (`broadcast_master_bytes` / `broadcast_bytes` /
/// `broadcast_control_with_cap`) の overflow 時挙動を統一するための共通ヘルパ
/// (= 旧 `broadcast_control_with_cap` は backpressure error 通知を欠いて無通知で
/// 切断していた)。
fn handle_enqueue_outcome(ch: &ClientHandle, outcome: EnqueueOutcome, overflow_ids: &mut Vec<u64>) {
    match outcome {
        EnqueueOutcome::Sent => {}
        EnqueueOutcome::Overflow => {
            send_backpressure_error(ch, ch.queued_bytes());
            overflow_ids.push(ch.id);
        }
        // 書き込み失敗は **disconnect の根拠にしない**。socket は全二重で、write 半分が
        // 死んでいることは「client が既に送ってきた frame」の有効性と無関係。ここで
        // 即 disconnect すると、client の受信済み frame を serve_loop が読む前に
        // ClientHandle ごと捨ててしまい、control message が無言で失われる
        // (= 「送信して即 close」する短命 client の Kill / Resize が効かない実バグ)。
        //
        // 正しい disconnect 点は受信側の EOF。EOF 経路は
        // `frames_to_process` で受信済み frame を全て処理した **後**に当該 client を
        // drop するため、順序が保たれる。書き込みが失敗する状況では peer は既に閉じている
        // ので受信側の EOF は直後に来る (= 居残りは 1 poll 周期程度)。
        EnqueueOutcome::SendFailed => {}
    }
}

/// `backpressure.disconnect` error message を best-effort で投げる。
///
/// `buffer_limit` は意図的に超えて積む (= disconnect 直前の最後の 1 メッセージ、
/// defensible)。queue の byte 数は通常の enqueue と同じく加算する (書いた分を引く勘定と
/// 整合させる)。書き込みが既に失敗している client には積まない。
pub(super) fn send_backpressure_error(ch: &ClientHandle, queued: usize) {
    let msg = ControlMessage::Error(ErrorMessage {
        code: ErrorCode::BackpressureDisconnect,
        message: "client buffer full".into(),
        details: Some(ciborium::Value::Map(vec![
            (
                ciborium::Value::Text("queued_bytes".into()),
                ciborium::Value::Integer((queued as u64).into()),
            ),
            (
                ciborium::Value::Text("limit".into()),
                ciborium::Value::Integer((ch.buffer_limit as u64).into()),
            ),
        ])),
    });
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
    let mut q = ch.send.borrow_mut();
    if !q.failed {
        q.push(Arc::new(frame_bytes));
    }
}

/// CBOR control message を 1 client にだけ送る。
///
/// `true` = enqueue 成功、`false` = overflow / 書き込み失敗 (= caller は当該
/// client を drop すべき)。
pub(super) fn send_control(ch: &ClientHandle, msg: ControlMessage) -> bool {
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
    let payload: SharedBytes = Arc::new(frame_bytes);
    matches!(enqueue_for_client(ch, payload), EnqueueOutcome::Sent)
}

/// `TYPE_RAW_ACK` frame を 1 client にだけ送る (DR-0021)。
///
/// daemon が `TYPE_RAW_DATA` を受け取って master PTY drain を完了 (= 成功 or 失敗) した時に
/// 当該 client に「次の raw_data を送って良い / 送れない」を通知する。
///
/// 戻り値: enqueue 成功なら `true`、overflow / 書き込み失敗なら `false`
/// (= caller は当該 client を drop すべき)。
pub(super) fn send_raw_ack(ch: &ClientHandle, ack: &RawAck) -> bool {
    let body = match ack.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut frame_bytes = Vec::new();
    if Frame::raw_ack(body).encode_to(&mut frame_bytes).is_err() {
        return false;
    }
    let payload: SharedBytes = Arc::new(frame_bytes);
    matches!(enqueue_for_client(ch, payload), EnqueueOutcome::Sent)
}

/// `Instant` (monotonic) を Unix epoch millis に近似変換する。
///
/// `now_inst - ts` で elapsed を求め、`SystemTime::now() - elapsed` を取る。
/// SystemTime と Instant が線形に対応していない場合 (= clock jump) に誤差は
/// 出るが、tail.data の timestamp_ms は debug / 表示用なので実用上問題ない。
pub(super) fn instant_to_epoch_ms(ts: Instant) -> i64 {
    let now_inst = Instant::now();
    let elapsed = now_inst.saturating_duration_since(ts);
    let now_sys = std::time::SystemTime::now();
    let then = now_sys.checked_sub(elapsed).unwrap_or(now_sys);
    then.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 子 PTY 出力 `bytes` を全 client に broadcast する。subscription 種類に応じて
/// raw_data frame (= Raw) or tail.data CBOR frame (= TailFollow) を送る。
///
/// 戻り値: backpressure overflow で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
pub(super) fn broadcast_master_bytes(
    clients: &mut [ClientHandle],
    bytes: &[u8],
    ts: Instant,
) -> Vec<u64> {
    // R5-H9: payload を 1 度だけ encode し、`Arc<Vec<u8>>` で wrap して
    // N clients に `Arc::clone` (= refcount 加算のみ、bytes copy なし) で配布する。
    let raw_frame_bytes: Option<SharedBytes> = if clients
        .iter()
        .any(|c| matches!(c.subscription, Subscription::Raw))
    {
        let mut buf = Vec::new();
        if Frame::raw_data(bytes.to_vec()).encode_to(&mut buf).is_ok() {
            Some(Arc::new(buf))
        } else {
            None
        }
    } else {
        None
    };

    let ts_ms = instant_to_epoch_ms(ts);
    // strip=false/true の 2 variant 分の payload を最大 1 度ずつ encode して cache。
    let mut tail_cache: [Option<SharedBytes>; 2] = [None, None];
    let encode_tail = |strip: bool, cache: &mut [Option<SharedBytes>; 2]| -> Option<SharedBytes> {
        let key = if strip { 1 } else { 0 };
        if let Some(ref cached) = cache[key] {
            return Some(Arc::clone(cached));
        }
        let payload = if strip {
            crate::strip::strip_ansi(bytes)
        } else {
            bytes.to_vec()
        };
        let msg = ControlMessage::TailData(TailData {
            bytes: payload,
            timestamp_ms: ts_ms,
        });
        let body = msg.encode_to_vec().ok()?;
        let mut frame_bytes = Vec::new();
        Frame::cbor_control(body).encode_to(&mut frame_bytes).ok()?;
        let shared: SharedBytes = Arc::new(frame_bytes);
        cache[key] = Some(Arc::clone(&shared));
        Some(shared)
    };

    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        let fb = match ch.subscription {
            Subscription::Raw => raw_frame_bytes.as_ref().map(Arc::clone),
            Subscription::TailFollow { strip_ansi } => encode_tail(strip_ansi, &mut tail_cache),
        };
        if let Some(fb) = fb {
            handle_enqueue_outcome(ch, enqueue_for_client(ch, fb), &mut overflow_ids);
        }
    }
    overflow_ids
}

/// CBOR control message を全 client に broadcast。
///
/// 戻り値: backpressure overflow で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
pub(super) fn broadcast_control(clients: &mut [ClientHandle], msg: &ControlMessage) -> Vec<u64> {
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return Vec::new();
    }
    // R5-H9: encode 結果を `Arc` で wrap し、N clients に zero-copy 配布。
    broadcast_bytes(clients, Arc::new(frame_bytes))
}

/// cap-aware 版 `broadcast_control` (DR-0015 §2.0)。
///
/// `negotiated_caps` に `required_cap` を含む client にだけ送信する。新 message を
/// 未対応 client に送ると serde decode error になる (= 未知 kind は
/// `ControlMessageError::Decode`) ため、cap-gated 配信が必須。
///
/// 戻り値: 送信先 client のうち overflow で disconnect すべき
/// `client_id` 一覧。cap 不足で skip した client は含まない。
///
/// overflow 時は他 2 helper (`broadcast_master_bytes` / `broadcast_bytes`) と
/// 同様に [`send_backpressure_error`] で切断理由を通知してから disconnect 対象に
/// 加える (= [`handle_enqueue_outcome`] で挙動を統一)。
pub(super) fn broadcast_control_with_cap(
    clients: &mut [ClientHandle],
    msg: &ControlMessage,
    required_cap: &str,
) -> Vec<u64> {
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return Vec::new();
    }
    let payload = Arc::new(frame_bytes);
    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        // cap 不足 client は skip (= 旧 client / 別実装が新 message を受けて
        // decode error にならないように)
        if !ch.negotiated_caps.iter().any(|c| c == required_cap) {
            continue;
        }
        handle_enqueue_outcome(
            ch,
            enqueue_for_client(ch, Arc::clone(&payload)),
            &mut overflow_ids,
        );
    }
    overflow_ids
}

/// `Frame` の encode 済 bytes を全 client に enqueue。
///
/// 戻り値: backpressure overflow で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
pub(super) fn broadcast_bytes(clients: &mut [ClientHandle], payload: SharedBytes) -> Vec<u64> {
    // R5-H9: caller が 1 度だけ `Arc::new` した payload を受け取り、各 client
    // へは `Arc::clone` (refcount 加算のみ) で配布する。旧実装は `Vec<u8>::clone`
    // (= memcpy O(payload.len())) を N clients 分繰り返していた。
    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        handle_enqueue_outcome(
            ch,
            enqueue_for_client(ch, Arc::clone(&payload)),
            &mut overflow_ids,
        );
    }
    overflow_ids
}

/// 全 client の送信 queue を書けるだけ書く (nonblocking)。serve loop が周回の冒頭で呼ぶ。
///
/// 書き込みに失敗した client は queue を捨てるだけで、ここでは切断しない (相手の close は
/// 受信側の EOF で検出して、受信済みの frame を処理してから切る。理由は
/// [`EnqueueOutcome::SendFailed`] の扱いを書いた `handle_enqueue_outcome` 参照)。
pub(super) fn flush_clients(clients: &[ClientHandle]) {
    for ch in clients {
        if !ch.send_settled() {
            let _ = ch.flush_send();
        }
    }
}

/// 切断が決まった client を 1 段進める。queue を書けるだけ書き、書き終えた・書き込みに
/// 失敗した・deadline を過ぎた client を close する (= `closing` から外して drop)。
pub(super) fn advance_closing(closing: &mut Vec<ClosingClient>, now: Instant) {
    closing.retain_mut(|c| {
        if c.send.write_to(&c.stream) != WriteProgress::Blocked {
            return false;
        }
        now < c.deadline
    });
}

/// `closing` の deadline のうち最も近いもの (= serve loop の poll timeout の上限)。
pub(super) fn closing_deadline(closing: &[ClosingClient]) -> Option<Instant> {
    closing.iter().map(ClosingClient::deadline).min()
}

/// `fds` のどれかが書ける (`POLLOUT`) か、`deadline` になるまで poll で待つ。
///
/// `EINTR` は戻るだけにする (呼び出し側が状態を見直して呼び直す)。
pub(super) fn poll_writable<'a>(fds: impl Iterator<Item = BorrowedFd<'a>>, deadline: Instant) {
    let mut poll_fds: Vec<nix::poll::PollFd<'a>> = fds
        .map(|fd| nix::poll::PollFd::new(fd, PollFlags::POLLOUT))
        .collect();
    let rem = deadline.saturating_duration_since(Instant::now());
    let _ = poll(&mut poll_fds, poll_timeout_until(rem));
}

/// 残り時間を poll の timeout (ms) に切り上げて変換する (`u16` を超える分は頭打ち)。
/// 切り上げるのは、deadline の直前に 0ms で起きて空回りしないため。
fn poll_timeout_until(rem: Duration) -> nix::poll::PollTimeout {
    let ms = rem.as_micros().div_ceil(1000);
    nix::poll::PollTimeout::from(u16::try_from(ms).unwrap_or(u16::MAX))
}

/// `clients` の送信 queue が全部書き終わる (または書き込みに失敗する) か、`deadline` に
/// なるまで書き続ける。待ちは `poll(POLLOUT)` で、間隔の固定された確認はしない。
///
/// serve loop を抜けた後の終了処理 (子の exit 通知を送り切ってから close する) と linger で
/// 使う。読まない client が居ても `deadline` で戻る。
pub(super) fn flush_until(clients: &[ClientHandle], deadline: Instant) {
    loop {
        flush_clients(clients);
        if clients.iter().all(ClientHandle::send_settled) || Instant::now() >= deadline {
            return;
        }
        poll_writable(
            clients
                .iter()
                .filter(|c| !c.send_settled())
                .map(|c| c.stream.as_fd()),
            deadline,
        );
    }
}

/// `closing` の全 client が close されるまで (= 書き終える・書き込みに失敗する・各自の
/// deadline を過ぎる) 書き続ける。serve loop を抜けた後の終了処理と linger で使う。
pub(super) fn drain_closing(closing: &mut Vec<ClosingClient>) {
    loop {
        advance_closing(closing, Instant::now());
        let Some(deadline) = closing_deadline(closing) else {
            return;
        };
        poll_writable(closing.iter().map(ClosingClient::fd), deadline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用に `ClientHandle` を組み立てるヘルパ。相手側 socket を一緒に返す
    /// (= drop で daemon 側の write が EPIPE にならないよう caller が保持する)。
    fn make_test_client(
        id: u64,
        buffer_limit: usize,
        caps: Vec<String>,
    ) -> (ClientHandle, UnixStream) {
        let (peer, stream) = UnixStream::pair().expect("pair");
        let ch = ClientHandle::new(id, Mode::Rw, true, caps, stream, buffer_limit)
            .expect("client handle");
        (ch, peer)
    }

    #[test]
    fn enqueue_for_client_respects_buffer_limit() {
        // 単体 unit test: queued_bytes が buffer_limit を超えるなら Overflow
        let (ch, _peer) = make_test_client(0, 100, vec![]);

        // 50 byte → OK、累計 50
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; 50])),
            EnqueueOutcome::Sent
        );
        assert_eq!(ch.queued_bytes(), 50);
        // 50 byte → 累計 100、まだ OK (= 100 <= 100)
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; 50])),
            EnqueueOutcome::Sent
        );
        assert_eq!(ch.queued_bytes(), 100);
        // 1 byte → 累計 101 > 100、Overflow
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; 1])),
            EnqueueOutcome::Overflow
        );
        // queued_bytes は変化なし (= Overflow 時は加算前に reject)
        assert_eq!(ch.queued_bytes(), 100);
    }

    /// overflow 時に backpressure error が client に通知されることを確認する
    /// 共通テスト本体。3 つの broadcast helper で挙動が統一されたことの回帰。
    ///
    /// `broadcast` クロージャに対象 helper を渡し、queue を limit まで埋めた
    /// client に 1 メッセージ broadcast する。overflow_id が返り、かつ
    /// queue 末尾に `backpressure.disconnect` error frame が積まれていることを
    /// 検証する (= `send_backpressure_error` が呼ばれた証跡)。
    fn assert_overflow_notifies_backpressure<F>(caps: Vec<String>, broadcast: F)
    where
        F: FnOnce(&mut [ClientHandle]) -> Vec<u64>,
    {
        // buffer_limit を小さくし、予め limit ちょうどまで queue を埋めておく。
        let (ch, _peer) = make_test_client(7, 64, caps);
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; 64])),
            EnqueueOutcome::Sent
        );

        let mut clients = vec![ch];
        let overflow_ids = broadcast(&mut clients);

        // overflow として disconnect 対象に挙がる
        assert_eq!(overflow_ids, vec![7]);

        // queue を浚って backpressure.disconnect error frame が積まれているか確認。
        // (= broadcast 本体の payload は overflow で reject されるが、
        //  send_backpressure_error が limit を超えて 1 frame 積む)
        let mut found_backpressure = false;
        for payload in clients[0].queued_frames() {
            let mut cur = std::io::Cursor::new(&payload[..]);
            if let Ok(frame) = Frame::decode_from(&mut cur)
                && let Ok(ControlMessage::Error(err)) =
                    ControlMessage::decode_from(frame.body.as_slice())
                && err.code == ErrorCode::BackpressureDisconnect
            {
                found_backpressure = true;
            }
        }
        assert!(
            found_backpressure,
            "overflow 時に backpressure.disconnect error が通知されるべき"
        );
    }

    /// queue が空なら、`buffer_limit` を超える単一 frame でも受け入れる。
    ///
    /// 回帰対象: `cur + size > limit` だけを見ると、`size > limit` の frame で
    /// **queue が空の client すら即 disconnect** される。子 PTY の読み取り
    /// chunk は 8 KiB 固定なので、`client_buffer_bytes` が 8 KiB 未満だと全 client が
    /// attach 直後に切られ、誰も接続できない daemon になる (= kill も届かない)。
    #[test]
    fn enqueue_accepts_oversized_frame_when_queue_is_empty() {
        let (ch, _peer) = make_test_client(1, 4096, vec![]);
        // queue は空 (= 0)。limit 4096 に対し 8192 byte の payload を積む。
        let outcome = enqueue_for_client(&ch, Arc::new(vec![0u8; 8192]));
        assert_eq!(
            outcome,
            EnqueueOutcome::Sent,
            "queue が空なら limit 超の単一 frame も通すべき (= 前進保証)"
        );
        assert_eq!(ch.queued_bytes(), 8192);
        assert_eq!(
            ch.queued_frames().len(),
            1,
            "payload が送信 queue に届くべき"
        );
    }

    /// 読み遅れている client (= queue に残がある) は従来どおり overflow で切る。
    /// 上の「空 queue なら通す」緩和が backpressure 自体を殺していないことの確認。
    #[test]
    fn enqueue_still_overflows_when_queue_is_non_empty() {
        let (ch, _peer) = make_test_client(2, 4096, vec![]);
        // 1 byte でも残っていれば、limit を超える追加は overflow。
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; 1])),
            EnqueueOutcome::Sent
        );
        let outcome = enqueue_for_client(&ch, Arc::new(vec![0u8; 4096]));
        assert_eq!(
            outcome,
            EnqueueOutcome::Overflow,
            "queue に残がある client の limit 超過は従来どおり overflow"
        );
    }

    /// `broadcast_bytes` の overflow 時 backpressure 通知 (既存挙動の回帰)。
    #[test]
    fn broadcast_bytes_notifies_backpressure_on_overflow() {
        assert_overflow_notifies_backpressure(vec![], |clients| {
            broadcast_bytes(clients, Arc::new(vec![0u8; 32]))
        });
    }

    /// `broadcast_master_bytes` の overflow 時 backpressure 通知 (既存挙動の回帰)。
    #[test]
    fn broadcast_master_bytes_notifies_backpressure_on_overflow() {
        assert_overflow_notifies_backpressure(vec![], |clients| {
            broadcast_master_bytes(clients, b"some master output bytes", Instant::now())
        });
    }

    /// `broadcast_control_with_cap` の overflow 時 backpressure 通知。
    ///
    /// overflow 時に `overflow_ids.push` のみで `send_backpressure_error` を呼ばないと、
    /// client は理由不明のまま切断される。共通ヘルパ化で他 2 helper と挙動が統一された
    /// ことを保証する。
    #[test]
    fn broadcast_control_with_cap_notifies_backpressure_on_overflow() {
        let cap = "session-exit-v1";
        let msg = ControlMessage::LeaderNotify(crate::protocol::messages::LeaderNotify {
            client_id: Some(1),
        });
        assert_overflow_notifies_backpressure(vec![cap.to_string()], move |clients| {
            broadcast_control_with_cap(clients, &msg, cap)
        });
    }

    /// socket buffer より十分大きい payload (= 相手が読み進めないと書き終わらない大きさ)。
    const FLUSH_TEST_PAYLOAD: usize = 8 * 1024 * 1024;

    /// `ClientHandle` の socket は O_NONBLOCK で、相手が読まなくても `flush_send` は
    /// 送信 buffer が埋まった所で戻る (= serve loop が相手の読み取りを待たない、DR-0037 I-1)。
    #[test]
    fn flush_send_returns_blocked_when_the_peer_does_not_read() {
        let (ch, _unread_peer) = make_test_client(3, 64 * 1024 * 1024, vec![]);
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        );
        assert_eq!(ch.flush_send(), WriteProgress::Blocked);
        let left = ch.queued_bytes();
        assert!(
            left > 0 && left < FLUSH_TEST_PAYLOAD,
            "送信 buffer に入った分だけ減り、残りは queue に残る: left={left}"
        );
    }

    /// 相手が閉じた client への書き込みは失敗として queue を捨て、以後の enqueue は
    /// `SendFailed` になる (= 切断の根拠にはしない。切断は受信側の EOF で行う)。
    #[test]
    fn flush_send_to_a_closed_peer_fails_and_later_enqueue_reports_send_failed() {
        let (ch, peer) = make_test_client(4, 1 << 20, vec![]);
        drop(peer);
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![1u8; 16])),
            EnqueueOutcome::Sent
        );
        assert_eq!(ch.flush_send(), WriteProgress::Failed);
        assert!(ch.send_settled());
        assert_eq!(ch.queued_bytes(), 0);
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![1u8; 16])),
            EnqueueOutcome::SendFailed
        );
    }

    /// `ClientHandle` の drop は close だけで、送信 queue が残っていて相手が読まなくても
    /// 待たずに戻る。相手は close を EOF として観測する。
    #[test]
    fn client_handle_drop_closes_without_waiting_for_the_queue() {
        let (ch, mut peer) = make_test_client(5, 64 * 1024 * 1024, vec![]);
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        );
        let start = Instant::now();
        drop(ch);
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "drop は close だけで待たない: {:?}",
            start.elapsed()
        );
        let mut buf = [0u8; 16];
        assert_eq!(
            std::io::Read::read(&mut peer, &mut buf).expect("read"),
            0,
            "queue に積んだだけの frame は書かれず、相手は EOF を見る"
        );
    }

    /// 切断が決まった client は、queue に積んだ frame (ack 等) を書き終えてから close される
    /// (= 相手は frame を全部読んでから EOF を見る)。
    #[test]
    fn retired_client_delivers_the_queued_frames_before_close() {
        let (ch, peer) = make_test_client(6, 64 * 1024 * 1024, vec![]);
        let reader = std::thread::spawn(move || {
            let mut peer = peer;
            let mut all = Vec::new();
            std::io::Read::read_to_end(&mut peer, &mut all).expect("read to EOF");
            all.len()
        });
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![7u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        );
        let mut closing = Vec::new();
        // test は deadline を長めに取り、書き終えたことで close されるのを見る。
        if let Some(c) = ch.into_closing(Instant::now() + Duration::from_secs(30)) {
            closing.push(c);
        }
        drain_closing(&mut closing);
        assert!(closing.is_empty());
        assert_eq!(reader.join().expect("reader"), FLUSH_TEST_PAYLOAD);
    }

    /// 切断が決まった client の相手が読まなければ、deadline で close する (= 読まない
    /// client の close を待ち続けない)。
    #[test]
    fn retired_client_is_closed_at_the_deadline_when_the_peer_does_not_read() {
        let (ch, mut peer) = make_test_client(7, 64 * 1024 * 1024, vec![]);
        assert_eq!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        );
        let budget = Duration::from_millis(200);
        let start = Instant::now();
        let mut closing = Vec::new();
        closing.extend(ch.into_closing(start + budget));
        drain_closing(&mut closing);
        assert!(
            start.elapsed() >= budget,
            "deadline の前に close した: {:?}",
            start.elapsed()
        );
        assert!(closing.is_empty(), "deadline を過ぎたら close される");
        // close されたので、相手は buffer に入った分を読み切ると EOF を見る。
        let mut all = Vec::new();
        std::io::Read::read_to_end(&mut peer, &mut all).expect("read to EOF");
        assert!(all.len() < FLUSH_TEST_PAYLOAD, "全部は届いていない");
    }

    /// 送信 queue が空の client は、切断時に closing に入らずその場で close される。
    #[test]
    fn retiring_a_client_with_an_empty_queue_closes_immediately() {
        let (ch, mut peer) = make_test_client(8, 1 << 20, vec![]);
        let mut closing = Vec::new();
        retire_client(&mut closing, ch);
        assert!(closing.is_empty());
        let mut buf = [0u8; 4];
        assert_eq!(std::io::Read::read(&mut peer, &mut buf).expect("read"), 0);
    }

    /// DR-0028 §4: 相手が読んでいれば、`flush_until` は queue を書き終える
    /// (`queued_bytes` が 0) まで戻らない。payload は相手が読み進めないと書き終わらない
    /// 大きさなので、待たずに戻ると `queued_bytes` が残る。
    #[test]
    fn flush_until_returns_after_the_queue_was_written() {
        let (ch, mut peer) = make_test_client(1, 64 * 1024 * 1024, vec![]);
        let drain = std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut total = 0usize;
            while total < FLUSH_TEST_PAYLOAD {
                match std::io::Read::read(&mut peer, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => total += n,
                }
            }
            total
        });
        assert!(matches!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        ));
        flush_until(
            std::slice::from_ref(&ch),
            Instant::now() + Duration::from_secs(30),
        );
        assert_eq!(ch.queued_bytes(), 0, "書き終える前に戻ってはいけない");
        assert_eq!(drain.join().expect("drain"), FLUSH_TEST_PAYLOAD);
    }

    /// 相手が読まなければ、`flush_until` は deadline で諦めて戻る (= 読まない client が
    /// upgrade / 終了処理を止めない)。
    #[test]
    fn flush_until_gives_up_at_the_deadline_when_the_peer_does_not_read() {
        let (ch, _unread_peer) = make_test_client(2, 64 * 1024 * 1024, vec![]);
        assert!(matches!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        ));
        let budget = Duration::from_millis(200);
        let start = Instant::now();
        flush_until(std::slice::from_ref(&ch), start + budget);
        assert!(
            start.elapsed() >= budget,
            "deadline の前に戻った: {:?}",
            start.elapsed()
        );
        assert!(ch.queued_bytes() > 0, "相手が読まないのに書き終わっている");
    }

    /// 書き込みに失敗した client (= 相手が閉じた) は、残りが書かれることは無いので
    /// 待たない (= deadline まで待たずに戻る)。
    #[test]
    fn flush_until_does_not_wait_for_a_failed_queue() {
        let (ch, peer) = make_test_client(3, 64 * 1024 * 1024, vec![]);
        drop(peer);
        assert!(matches!(
            enqueue_for_client(&ch, Arc::new(vec![0u8; FLUSH_TEST_PAYLOAD])),
            EnqueueOutcome::Sent
        ));
        let deadline = Instant::now() + Duration::from_secs(30);
        flush_until(std::slice::from_ref(&ch), deadline);
        assert!(
            Instant::now() < deadline,
            "失敗した queue を deadline まで待った"
        );
        assert!(ch.send_settled(), "書き込み失敗で queue は捨てられている");
    }
}
