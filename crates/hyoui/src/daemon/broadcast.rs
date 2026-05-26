//! Per-client writer thread + byte-bound queue + broadcast helpers
//! (DR-0009 Phase C で `session.rs` から分離)。
//!
//! ## 構成
//!
//! - [`ClientHandle`]: 1 client の per-thread state (writer thread + queued bytes counter + reader)
//! - [`Subscription`]: client の出力 subscription (Raw / TailFollow)
//! - [`EnqueueOutcome`]: 1 frame の enqueue 結果 (Sent / Overflow / WriterDead)
//! - [`writer_pump`]: per-client thread の本体 (mpsc rx → socket write、queued_bytes fetch_sub)
//! - [`enqueue_for_client`]: 1 client への frame enqueue (= byte-level cap check)
//! - [`send_backpressure_error`]: backpressure.disconnect error の best-effort 送信
//! - [`send_control`]: 1 client への control message 送信
//! - [`broadcast_master_bytes`]: 子 PTY 出力を subscription 別 frame で全 client に
//! - [`broadcast_control`]: CBOR control message を全 client に
//! - [`broadcast_bytes`]: 既に encode 済 bytes を全 client に enqueue
//! - [`instant_to_epoch_ms`]: tail.data の timestamp_ms 用近似変換
//!
//! ## session.rs / 他 module との接続
//!
//! - [`ClientHandle`] は `pub(super)` の private フィールドを持つ。
//!   `daemon` module 内 (= session.rs の `finalize_accepted_client`, serve_loop,
//!   Drop drain 経路) から直接構築 / 解体する。
//! - control.rs / wait.rs / tail.rs / lock.rs は `send_control` / `broadcast_control`
//!   を呼ぶ片方向依存 (= DR-0009 §module DAG)。

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::protocol::messages::{ErrorCode, ErrorMessage, TailData};
use crate::protocol::{ControlMessage, Frame, Mode};

/// daemon が同時 attach を許す client 数上限 (= D6 集合 backpressure DoS 対策)。
/// 超過した accept は即 socket close で reject。`client_buffer_bytes` が 8 MiB の
/// 場合、64 clients × 8 MiB = 最大 512 MiB の queue 占有が理論上限。
pub(super) const MAX_CLIENTS_PER_DAEMON: usize = 64;

/// 1 client の per-thread state (writer thread + 自前 byte bound queue + reader handle)。
///
/// Phase 12: queue capacity は **byte 単位の厳密 cap** (DR-0008 §8.2)。
/// `writer_tx` は unbounded mpsc、enqueue の可否は `queued_bytes` を atomic で
/// check + add し、`buffer_limit` 超過なら enqueue を拒否して当該 client を
/// disconnect する (= `error` kind=`backpressure.disconnect` を best-effort 送信)。
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
    /// daemon → client への frame enqueue 用 unbounded mpsc。
    pub(super) writer_tx: Sender<Vec<u8>>,
    /// 現在 queue 内に積まれている bytes 数 (= writer_pump が送信完了で減らす)。
    pub(super) queued_bytes: Arc<AtomicUsize>,
    /// queue の byte 上限 (= `DaemonConfig::client_buffer_bytes`)。
    pub(super) buffer_limit: usize,
    /// writer thread のハンドル。drop の前に join される。
    pub(super) writer_thread: Option<std::thread::JoinHandle<()>>,
    /// daemon が client → daemon を decode するときに使う socket reader。
    pub(super) reader: UnixStream,
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
    /// queue に追加成功、writer thread が socket に書き出す。
    Sent,
    /// `buffer_limit` 超過 (= 当該 client を disconnect すべき)。
    Overflow,
    /// writer thread が既に死亡 (= socket close 検知済み、再 enqueue 不能)。
    WriterDead,
}

/// 1 frame の bytes を 1 client の queue に積む。
///
/// **race semantics (= L4 review メモ)**: `load` → `fetch_add` の間に他の writer が
/// `fetch_add` していると、`queued_bytes` が `buffer_limit` を一時的に **超過**
/// する。serve_loop は **single-threaded** main thread のみが broadcast / enqueue を
/// 呼ぶため実 daemon では race しないが、unit test で別 thread から enqueue 呼ぶと
/// 厳密 cap は崩れる。`compare_exchange_weak` loop で書き直せば厳密化できるが、
/// 「ms 単位 throughput を最優先」と「将来 multi-writer になる必然性が低い」を
/// 天秤にかけて relax で許容。実用上は writer_pump が `fetch_sub` するので大局
/// 収束する。
pub(super) fn enqueue_for_client(ch: &ClientHandle, bytes: Vec<u8>) -> EnqueueOutcome {
    let size = bytes.len();
    let cur = ch.queued_bytes.load(Ordering::Acquire);
    if cur.saturating_add(size) > ch.buffer_limit {
        return EnqueueOutcome::Overflow;
    }
    ch.queued_bytes.fetch_add(size, Ordering::AcqRel);
    if ch.writer_tx.send(bytes).is_err() {
        // writer thread 死亡 → queued_bytes を戻して終了
        ch.queued_bytes.fetch_sub(size, Ordering::AcqRel);
        return EnqueueOutcome::WriterDead;
    }
    EnqueueOutcome::Sent
}

/// `backpressure.disconnect` error message を best-effort で投げる。
///
/// L5: 旧実装は `writer_tx.send` を直接呼んで `queued_bytes` をバイパスしていた。
/// すると writer_pump の `fetch_sub` で「送ったぶんを引く」想定が破れ、
/// `queued_bytes` が unsigned wrap (= 巨大値) を返す可能性があった。本実装では
/// `queued_bytes` を明示加算してから send することで writer_pump の `fetch_sub`
/// と整合させる。`buffer_limit` は意図的に超えて送る (= disconnect 直前の最後の
/// 1 メッセージ、defensible)。writer_tx が closed なら加算分を戻して諦める。
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
    let size = frame_bytes.len();
    ch.queued_bytes.fetch_add(size, Ordering::AcqRel);
    if ch.writer_tx.send(frame_bytes).is_err() {
        ch.queued_bytes.fetch_sub(size, Ordering::AcqRel);
    }
}

/// CBOR control message を 1 client にだけ送る。
///
/// `true` = enqueue 成功、`false` = overflow / writer dead (= caller は当該
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
    matches!(enqueue_for_client(ch, frame_bytes), EnqueueOutcome::Sent)
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
/// 戻り値: backpressure overflow / writer dead で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
pub(super) fn broadcast_master_bytes(
    clients: &mut [ClientHandle],
    bytes: &[u8],
    ts: Instant,
) -> Vec<u64> {
    let raw_frame_bytes: Option<Vec<u8>> = if clients
        .iter()
        .any(|c| matches!(c.subscription, Subscription::Raw))
    {
        let mut buf = Vec::new();
        if Frame::raw_data(bytes.to_vec()).encode_to(&mut buf).is_ok() {
            Some(buf)
        } else {
            None
        }
    } else {
        None
    };

    let ts_ms = instant_to_epoch_ms(ts);
    let mut tail_cache: [Option<Vec<u8>>; 2] = [None, None];
    let encode_tail = |strip: bool, cache: &mut [Option<Vec<u8>>; 2]| -> Option<Vec<u8>> {
        let key = if strip { 1 } else { 0 };
        if let Some(ref cached) = cache[key] {
            return Some(cached.clone());
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
        cache[key] = Some(frame_bytes.clone());
        Some(frame_bytes)
    };

    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        let fb = match ch.subscription {
            Subscription::Raw => raw_frame_bytes.clone(),
            Subscription::TailFollow { strip_ansi } => encode_tail(strip_ansi, &mut tail_cache),
        };
        if let Some(fb) = fb {
            match enqueue_for_client(ch, fb) {
                EnqueueOutcome::Sent => {}
                EnqueueOutcome::Overflow => {
                    send_backpressure_error(ch, ch.queued_bytes.load(Ordering::Acquire));
                    overflow_ids.push(ch.id);
                }
                EnqueueOutcome::WriterDead => {
                    overflow_ids.push(ch.id);
                }
            }
        }
    }
    overflow_ids
}

/// CBOR control message を全 client に broadcast。
///
/// 戻り値: backpressure overflow / writer dead で disconnect すべき client の
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
    broadcast_bytes(clients, frame_bytes)
}

/// daemon → client の writer pump (= per-thread)。
///
/// `rx` から `Vec<u8>` を受け取って socket に write_all、送信完了で
/// `queued_bytes` から減算する (= Phase 12 byte bound 厳密化)。送信失敗で thread 終了。
pub(super) fn writer_pump(
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    mut sock: UnixStream,
    queued_bytes: Arc<AtomicUsize>,
) {
    while let Ok(bytes) = rx.recv() {
        let size = bytes.len();
        if std::io::Write::write_all(&mut sock, &bytes).is_err() {
            // client が close した。recv ループ抜けて thread 終了。
            return;
        }
        queued_bytes.fetch_sub(size, Ordering::AcqRel);
    }
}

/// `Frame` の encode 済 bytes を全 client に enqueue。
///
/// 戻り値: backpressure overflow / writer dead で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
pub(super) fn broadcast_bytes(clients: &mut [ClientHandle], bytes: Vec<u8>) -> Vec<u64> {
    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        match enqueue_for_client(ch, bytes.clone()) {
            EnqueueOutcome::Sent => {}
            EnqueueOutcome::Overflow => {
                send_backpressure_error(ch, ch.queued_bytes.load(Ordering::Acquire));
                overflow_ids.push(ch.id);
            }
            EnqueueOutcome::WriterDead => {
                overflow_ids.push(ch.id);
            }
        }
    }
    overflow_ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_for_client_respects_buffer_limit() {
        // 単体 unit test: queued_bytes が buffer_limit を超えるなら Overflow
        let (tx, _rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // ダミー UnixStream 作って ClientHandle を構築
        let (a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let _keep = a; // close 防止用
        let ch = ClientHandle {
            id: 0,
            mode: Mode::Rw,
            leader: true,
            subscription: Subscription::Raw,
            negotiated_caps: vec![],
            writer_tx: tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            buffer_limit: 100,
            writer_thread: None,
            reader: b,
        };

        // 50 byte → OK、累計 50
        assert_eq!(enqueue_for_client(&ch, vec![0u8; 50]), EnqueueOutcome::Sent);
        assert_eq!(ch.queued_bytes.load(Ordering::Acquire), 50);
        // 50 byte → 累計 100、まだ OK (= 100 <= 100)
        assert_eq!(enqueue_for_client(&ch, vec![0u8; 50]), EnqueueOutcome::Sent);
        assert_eq!(ch.queued_bytes.load(Ordering::Acquire), 100);
        // 1 byte → 累計 101 > 100、Overflow
        assert_eq!(
            enqueue_for_client(&ch, vec![0u8; 1]),
            EnqueueOutcome::Overflow
        );
        // queued_bytes は変化なし (= Overflow 時は加算前に reject)
        assert_eq!(ch.queued_bytes.load(Ordering::Acquire), 100);
    }
}
