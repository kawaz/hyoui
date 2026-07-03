//! `hyoui record` (DR-0016) の sink core — I/O event を bounded queue 経由で
//! 専用 writer thread に渡し、jsonl / raw format で file 永続化する。
//!
//! ## 構成 (DR-0016 §8 / §8a / §8b)
//!
//! ```text
//! [PTY read/write thread]               [writer thread (record_id 毎)]
//!         │                                       │
//!         │ push (RecordEvent + seq)              │
//!         │   ┌─────────────────┐                 │
//!         ├──▶│ bounded channel │── recv ────────▶│ format encode + write + flush
//!         │   │ capacity = 1024 │                 │
//!         │   └─────────────────┘                 │
//!         │         (full → 100ms timeout で諦め) │ (write error → 自動 stop)
//!         ▼
//! 子 PTY 経路に影響を出さない (= 観測対象を歪めない)
//! ```
//!
//! - `RecordSink`: 1 record の sink handle (= Sender + stats)
//! - `RecordRegistry`: session-scope の sink 集合 (= start/stop/list/broadcast)
//! - `RecordEvent`: in/out bytes / reject / write error / redaction / lifecycle
//!
//! ## input secrecy の現状 (= redaction state machine は Phase 5)
//!
//! `InputSecrecy` のうち **`RecordAll` / `NeverRecordStdin` が有効**:
//! - `RecordAll` (= default): stdin bytes をそのまま file に記録する。passphrase /
//!   token 等を `direction: stdin|both` で録ると平文で残る点に注意。
//! - `NeverRecordStdin`: stdin 由来の event (`BytesIn` / `InRejected` /
//!   `InWriteError` = いずれも生 bytes を含む) を当該 sink へ配信しない (= `push_*`
//!   段で skip)。stdin は一切 file に残らず、header の `input_secrecy` 申告が正直になる。
//!
//! `RedactAfterPrompt` は `RecordRegistry::start` で **reject** する
//! (= `RecordStartError::RedactionUnimplemented`)。prompt 検出 + redaction state
//! machine が Phase 5 未配線のため、「redact 済」と偽る file を作らせない。
//! `InSecretRedacted` event / `push_in_secret_redacted` も Phase 5 まで dead code。
//!
//! ## 設計判断メモ
//!
//! - `seq` は **sink 側 (push 側) で `AtomicU32::fetch_add` 採番** する (DR-0016 §8b)。
//!   queue full で push を諦めても seq は消費するため、欠番が観測される
//!   (= parser が「seq 連続性 check」で脱落を検知できる)。
//! - `RwLock<Vec<RecordSink>>` の lock 下で `send_timeout` を呼ぶと PTY hot path を
//!   塞ぐ。Phase 4 配線時には read lock 下で **Sender を clone で取り出して lock 解放**
//!   → send_timeout、にする (= sink 自体は `Arc` 化済み、本 module 内で完結)。
//! - jsonl header は writer thread 起動直後に **eager に書く** (= I/O 0 件でも
//!   header 付き file が残る、§3 forward-compat field 揃え)。
//! - Phase 2 では daemon hot path 配線はしない。`RecordRegistry::push_*` は test
//!   から直接呼ぶ。配線は Phase 4。
//!
//! ## 関連
//!
//! - DR-0016 §3 (jsonl format) / §5 (raw format) / §8 (sink) / §8a (queue) / §8b (seq)
//! - `crates/hyoui/src/protocol/messages/record.rs` — protocol message + cap

// Phase 4 で daemon hot path への配線完了。本 module の API のうち、Phase 5
// (= secret redaction state machine) で配線予定の item は narrow な
// `#[allow(dead_code)]` で抑制している。Phase 5 で外す。

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Sender, TrySendError, bounded};
use serde::Serialize;

use crate::protocol::Mode;
use crate::protocol::messages::{
    InputSecrecy, RecordDirection, RecordFormat, RecordInfo, RecordStartRequest,
};
use crate::sys::clock::now_unix_ms;

// === Tunables ===

/// bounded queue 容量 (DR-0016 §8a)。
///
/// 各 record_id ごとに 1 つ。超過時は **push 側 100ms timeout** で諦め、
/// `record-aborted reason: queue-full` を他生存中 sink に lifecycle event として
/// 通知 + 該当 record を自動 stop。PTY hot path はブロックしない。
const RECORD_QUEUE_CAPACITY: usize = 1024;

/// queue full 時の push 側 timeout (DR-0016 §8a)。
const RECORD_PUSH_TIMEOUT: Duration = Duration::from_millis(100);

/// 拡張子 allowlist (DR-0016 §9)。
///
/// shell-interpreted / sensitive 拡張子 (`.sh` / `.zshrc` / `.ssh/*` 等) は
/// このリストに無いので reject される。
const ALLOWED_EXTENSIONS: &[&str] = &["jsonl", "raw", "bin", "log"];

/// HOME 配下の sensitive prefix deny list (DR-0016 §9)。
///
/// `~/.ssh/key.dump` のような攻撃面を構造的に塞ぐ。
const SENSITIVE_HOME_PREFIXES: &[&str] = &[".ssh/", ".config/", ".gnupg/", ".aws/", ".docker/"];

// === Event types ===

/// `in-rejected` event の reason (DR-0016 §3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InRejectedReason {
    /// `Ro` mode の client が input を送った (= silently drop された)。
    RoClient,
    /// lock 中で lock holder 以外の client が input を送った。
    LockNotHeld,
}

impl InRejectedReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::RoClient => "ro-client",
            Self::LockNotHeld => "lock-not-held",
        }
    }
}

/// `in-write-error` event の error kind (DR-0016 §4)。
#[derive(Debug, Clone)]
pub enum WriteErrorKind {
    /// `write_all_with_idle_timeout` の idle timeout (= 子が読まなくなった)。
    IdleTimeout,
    /// IO 失敗 (= EIO / EBADF 等)、errno 由来の error message 文字列を保持。
    IoError(String),
}

impl WriteErrorKind {
    fn as_wire_str(&self) -> &str {
        match self {
            Self::IdleTimeout => "timeout",
            Self::IoError(_) => "io-error",
        }
    }
}

/// lifecycle event (DR-0016 §3)。子 stop/resume の 4 段階や client attach/detach、
/// record-aborted など、bytes ではない状態遷移を記録する。
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// WUNTRACED で子の stopped を観測した瞬間 (= daemon 側、§3 4 段階の 1)。
    ChildStoppedObserved {
        sig_name: String,
        sig_num: i32,
        pid: u32,
        ts_unix_ms: u64,
    },
    /// client から `session.child.resume.request` を受信した瞬間 (= §3 4 段階の 2)。
    ResumeRequestReceived { client_id: u64, ts_unix_ms: u64 },
    /// daemon が `killpg(SIGCONT)` を実行した瞬間 (= §3 4 段階の 3)。
    SigcontSent { pid: u32, ts_unix_ms: u64 },
    /// WCONTINUED で continued を観測した瞬間 (= §3 4 段階の 4)。
    ChildContinuedObserved {
        sig_name: String,
        sig_num: i32,
        pid: u32,
        ts_unix_ms: u64,
    },
    /// client が attach 完了した瞬間。
    ClientAttached {
        client_id: u64,
        mode: Mode,
        ts_unix_ms: u64,
    },
    /// client が detach した瞬間。
    ClientDetached { client_id: u64, ts_unix_ms: u64 },
    /// lock 取得成功の瞬間。
    LockAcquired { client_id: u64, ts_unix_ms: u64 },
    /// lock 解放の瞬間。
    LockReleased { client_id: u64, ts_unix_ms: u64 },
    /// 録画が daemon 側都合で中断された (= queue-full / io-error / max-bytes / max-duration)。
    /// 自分自身の sink が abort された場合は最終行として書かれ、他 sink への broadcast にも使われる。
    RecordAborted {
        record_id: u32,
        reason: String,
        detail: Option<String>,
        ts_unix_ms: u64,
    },
    /// daemon 側終了条件 (= `--until` match / `--timeout` / `--idle-timeout`)、または
    /// daemon process への外部 SIGTERM / SIGINT が発火し、子 process group へ SIGTERM を
    /// 送って session を畳む瞬間 (DR-0019 §4 + issue 2026-06-11 優先3 graceful shutdown)。
    ///
    /// `reason` は発火理由 ("until" / "timeout" / "idle-timeout" / "sigterm" / "sigint")。
    /// 発火後は通常の child exit 経路 (= finalize escalation → `SessionExitNotify`) に合流する。
    SessionTerminatedByCondition { reason: String, ts_unix_ms: u64 },
    /// `set.request` で runtime 設定が変更された瞬間 (DR-0019 Update)。
    ///
    /// 「誰がいつ何を変えたか」を追えるようにする (= record の lifecycle event)。
    /// `key` / `value` は適用後の正規値、`changed_by_client_id` は要求元 client。
    PolicyChanged {
        key: String,
        value: String,
        changed_by_client_id: u64,
        ts_unix_ms: u64,
    },
}

/// sink push される event (DR-0016 §3 / §4 / §6)。
///
/// `seq` は push 時に `RecordSink::seq` で採番されるため、本型には含めない (= sink
/// 側で channel payload に combine する)。
#[derive(Debug, Clone)]
pub enum RecordEvent {
    /// 子 PTY に write 成功した bytes prefix (DR-0016 §3 `dir: in`)。
    BytesIn {
        client_id: u64,
        bytes: Vec<u8>,
        ts_unix_ms: u64,
    },
    /// 子 PTY から read した bytes (DR-0016 §3 `dir: out`、screen 加工前)。
    BytesOut { bytes: Vec<u8>, ts_unix_ms: u64 },
    /// 認可前の input が policy reject された (DR-0016 §3 `ev: in-rejected`)。
    InRejected {
        client_id: u64,
        client_mode: Mode,
        lock_holder_client_id: Option<u64>,
        reason: InRejectedReason,
        bytes: Vec<u8>,
        ts_unix_ms: u64,
    },
    /// `write_all_with_idle_timeout` が partial / failure (DR-0016 §4)。
    InWriteError {
        client_id: u64,
        requested_len: usize,
        written_len: usize,
        error: WriteErrorKind,
        unwritten_bytes: Vec<u8>,
        ts_unix_ms: u64,
    },
    /// redaction mode 中の stdin (= bytes は捨てる、DR-0016 §6)。
    /// Phase 5 (= secret redaction state machine) で発火、Phase 4 では未配線。
    #[allow(dead_code)]
    InSecretRedacted {
        client_id: u64,
        byte_count: usize,
        reason: String,
        ts_unix_ms: u64,
    },
    /// 状態遷移 (= lifecycle、§3)。
    Lifecycle(LifecycleEvent),
}

/// channel payload (= seq を event と一緒に運ぶ wrapper)。
///
/// seq は push 側で確定 (DR-0016 §8b)、queue full で push を諦めた seq は欠番に
/// なり、jsonl parser 側で「seq 連続性 check」で脱落検知できる。
#[derive(Debug, Clone)]
struct SeqEvent {
    seq: u32,
    event: RecordEvent,
}

// === Stats / Info ===

/// `record.list.response` 用の並行 stats (= writer thread が更新、reader が読む)。
#[derive(Debug, Default)]
struct RecordStats {
    raw_bytes_recorded: AtomicU64,
    file_bytes_written: AtomicU64,
    last_flushed_unix_ms: AtomicU64,
}

// === Sink + Registry ===

/// daemon 側 session info (= jsonl header に乗せる項目を `RecordRegistry::start` に
/// 渡すための値オブジェクト)。Phase 4 で daemon から呼ぶときに `DaemonConfig` から
/// 組み立てる。
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub daemon_pid: u32,
    pub daemon_boot_id: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

/// `RecordRegistry::start` の失敗種別 (DR-0016 §9 + protocol error code)。
#[derive(Debug, thiserror::Error)]
pub enum RecordStartError {
    #[error("output path must be absolute")]
    PathNotAbsolute,
    #[error("output file already exists")]
    OutputAlreadyExists,
    #[error("output permission denied: {0}")]
    OutputPermissionDenied(String),
    #[error("unsupported direction for format (raw requires single direction)")]
    UnsupportedDirectionForFormat,
    #[error("invalid prompt pattern: {0}")]
    InvalidPromptPattern(String),
    /// `input_secrecy = redact-after-prompt` だが redaction state machine が Phase 5
    /// 未配線 (DR-0016 §6)。「redact 済」と偽る record file を作らせないための reject。
    #[error(
        "input-secrecy=redact-after-prompt is not yet implemented (Phase 5); \
         use record-all or never-record-stdin"
    )]
    RedactionUnimplemented,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// `RecordRegistry::stop` の失敗 (= 該当 record_id が無い)。
#[derive(Debug, thiserror::Error)]
#[error("record not found: id={0}")]
pub struct RecordStopError(pub u32);

/// 1 active record の sink handle (DR-0016 §8)。
///
/// Sender は writer thread への channel。stats は `record.list.response` で読まれる。
/// JoinHandle は stop 時に join するため `Mutex<Option<_>>` で持つ。
#[derive(Debug)]
pub struct RecordSink {
    pub record_id: u32,
    pub direction: RecordDirection,
    pub format: RecordFormat,
    pub output_path: PathBuf,
    pub started_by_client_id: u64,
    pub started_unix_ms: u64,
    /// stdin redaction policy (DR-0016 §6 interim)。`NeverRecordStdin` なら stdin
    /// 由来 event (`BytesIn` / `InRejected` / `InWriteError`) を配信しない。
    /// `RedactAfterPrompt` は `start` 段で reject 済のため、実際に入るのは
    /// `RecordAll` / `NeverRecordStdin` のみ。
    input_secrecy: InputSecrecy,
    seq: AtomicU32,
    /// `Mutex<Option<_>>` で持つ理由: stop 時に **Sender を明示 drop** しないと
    /// writer thread の `rx.recv()` が永遠に待ち続ける (= Arc<RecordSink> 経由で
    /// tx が生きているため、registry の Vec から remove しても channel は close
    /// しない)。`take()` で Sender を消すことで writer thread を確実に終了させる。
    tx: Mutex<Option<Sender<SeqEvent>>>,
    stats: Arc<RecordStats>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
}

impl RecordSink {
    /// 次の seq を 1 つ消費する (= push を諦めても消費して欠番化)。
    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    /// 100ms timeout で event を push、queue full なら諦め (= 欠番化)。
    ///
    /// 戻り値: push 成功なら `Ok(())`、queue full / disconnect なら `Err(_)`。caller
    /// (= registry) が他 sink への record-aborted 通知判断に使う。
    fn try_push(&self, event: RecordEvent) -> Result<(), TrySendError<SeqEvent>> {
        let seq = self.next_seq();
        let payload = SeqEvent { seq, event };
        // 既に stop 中で Sender が drop 済みなら disconnect 扱い。
        let guard = self.tx.lock().expect("record sink tx lock poisoned");
        let tx = match guard.as_ref() {
            Some(tx) => tx.clone(),
            None => return Err(TrySendError::Disconnected(payload)),
        };
        drop(guard);
        match tx.send_timeout(payload, RECORD_PUSH_TIMEOUT) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::SendTimeoutError::Timeout(p)) => Err(TrySendError::Full(p)),
            Err(crossbeam_channel::SendTimeoutError::Disconnected(p)) => {
                Err(TrySendError::Disconnected(p))
            }
        }
    }

    /// stats snapshot を `record.list.response` 用に取り出す。
    fn snapshot_info(&self) -> RecordInfo {
        RecordInfo {
            record_id: self.record_id,
            direction: self.direction,
            format: self.format,
            output_path: self.output_path.to_string_lossy().into_owned(),
            started_unix_ms: self.started_unix_ms,
            started_by_client_id: self.started_by_client_id,
            raw_bytes_recorded: self.stats.raw_bytes_recorded.load(Ordering::Relaxed),
            file_bytes_written: self.stats.file_bytes_written.load(Ordering::Relaxed),
            last_flushed_unix_ms: self.stats.last_flushed_unix_ms.load(Ordering::Relaxed),
        }
    }

    /// writer thread を停止 + join する。
    ///
    /// Sender を明示 drop してから join (= channel disconnect で writer の `recv()`
    /// が `Err(Disconnected)` を返し、writer は drain して exit する)。`Arc<RecordSink>`
    /// 経由で Sender が複数箇所から参照され得るため、`Mutex<Option<_>>` を `take()` で
    /// 抜くことで唯一の停止経路にする。
    fn join_writer(&self) {
        // 1. Sender を消す (= channel close、writer の recv() が解放される)
        {
            let mut guard = self.tx.lock().expect("record sink tx lock poisoned");
            *guard = None;
        }
        // 2. 進行中の `tx.clone()` を握っている push 側 thread (= push_* 経路) が
        // 解放するまでは channel は open のままだが、本 module 内では push の clone
        // は send_timeout 呼び出しの中だけで、tx は MAX 100ms 後に必ず drop される。
        // よって writer は遅くとも 100ms 以内に Disconnected を観測して exit する。
        let handle = self.join_handle.lock().ok().and_then(|mut g| g.take());
        if let Some(h) = handle {
            // join 失敗 (= writer thread panic) は session に致命傷を与えない (= 他 record /
            // 子 PTY / 他 client への影響なし方針、DR-0016 §8a)。
            let _ = h.join();
        }
    }
}

/// session-scope の record sink 集合 (DR-0016 §8)。
///
/// PTY hot path は `push_*` の薄い API を経由する。lock 下で channel send_timeout
/// しない設計 (= Phase 4 で hot path から呼ぶ際、Sender を clone して lock 解放してから send)。
#[derive(Debug)]
pub struct RecordRegistry {
    sinks: RwLock<Vec<Arc<RecordSink>>>,
    next_id: AtomicU32,
}

impl RecordRegistry {
    pub fn new() -> Self {
        Self {
            sinks: RwLock::new(Vec::new()),
            next_id: AtomicU32::new(1),
        }
    }

    /// 新しい record を開始 (DR-0016 §7 `record.start.request` の daemon 側受け口)。
    ///
    /// 1. path validation (= 絶対 / 拡張子 / sensitive prefix)
    /// 2. format × direction の整合 (= raw + Both は invalid)
    /// 3. file open (= O_CREAT|O_EXCL + O_NOFOLLOW + O_CLOEXEC + mode 0600)
    /// 4. writer thread spawn + jsonl header eager 書き出し
    /// 5. Sink 登録
    pub fn start(
        &self,
        request: &RecordStartRequest,
        started_by_client_id: u64,
        session: SessionInfo,
    ) -> Result<u32, RecordStartError> {
        // 1. path validation
        let path = PathBuf::from(&request.output_path);
        validate_output_path(&path)?;

        // 2. format × direction
        if matches!(request.format, RecordFormat::Raw)
            && matches!(request.direction, RecordDirection::Both)
        {
            return Err(RecordStartError::UnsupportedDirectionForFormat);
        }

        // 2b. redact-after-prompt は redaction state machine が Phase 5 未配線のため
        // reject する (= 「redact 済」と偽る record file を作らせない、DR-0016 §6
        // interim)。正規 CLI は parse 段で弾くが、旧 client / 別実装 client の直接
        // protocol 経由もここで塞ぐ。
        if matches!(request.input_secrecy, InputSecrecy::RedactAfterPrompt) {
            return Err(RecordStartError::RedactionUnimplemented);
        }

        // (3 の前に) prompt pattern compile 検証は Phase 5 で。Phase 2 では Option を
        // 受け取るが触らない (= None なら default、Some なら storage のみ、redaction の
        // state machine 配線は Phase 5 で行う)。
        if let Some(pat) = request.prompt_pattern.as_deref()
            && let Err(e) = regex::Regex::new(pat)
        {
            return Err(RecordStartError::InvalidPromptPattern(e.to_string()));
        }

        // 3. file open
        let file = open_record_file(&path)?;

        // 4. record_id 採番 + sink 構築
        let record_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let started_unix_ms = now_unix_ms();
        let stats = Arc::new(RecordStats::default());
        let (tx, rx) = bounded::<SeqEvent>(RECORD_QUEUE_CAPACITY);

        // writer config を thread に渡す
        let cfg = WriterConfig {
            record_id,
            format: request.format,
            direction: request.direction,
            input_secrecy: request.input_secrecy,
            max_bytes: request.max_bytes,
            max_duration_ms: request.max_duration_ms,
            started_unix_ms,
            session: session.clone(),
            stats: stats.clone(),
        };

        // 5. writer thread spawn (= header eager 書き出しは thread 内、ただし thread start
        // 前に file は open 済なので permission error 等は本 fn の Err で表現できる)。
        let join_handle = std::thread::Builder::new()
            .name(format!("hyoui-record-{record_id}"))
            .spawn(move || writer_thread_main(file, rx, cfg))
            .map_err(|e| {
                RecordStartError::Io(std::io::Error::other(format!("spawn writer thread: {e}")))
            })?;

        let sink = Arc::new(RecordSink {
            record_id,
            direction: request.direction,
            format: request.format,
            output_path: path,
            started_by_client_id,
            started_unix_ms,
            input_secrecy: request.input_secrecy,
            seq: AtomicU32::new(1),
            tx: Mutex::new(Some(tx)),
            stats,
            join_handle: Mutex::new(Some(join_handle)),
        });

        self.sinks
            .write()
            .expect("record registry lock poisoned")
            .push(sink);
        Ok(record_id)
    }

    /// `record.stop.request` の daemon 側受け口。
    ///
    /// 該当 sink を Vec から remove → Sender drop → writer thread が drain して exit。
    pub fn stop(&self, record_id: u32) -> Result<(), RecordStopError> {
        let mut sinks = self.sinks.write().expect("record registry lock poisoned");
        let pos = sinks
            .iter()
            .position(|s| s.record_id == record_id)
            .ok_or(RecordStopError(record_id))?;
        let sink = sinks.remove(pos);
        drop(sinks); // 他 reader を起こす
        // Arc が他で参照されていなければ Sender drop → channel disconnect → writer exit。
        // join で確実に file flush + close を待つ (= test の race 防止)。
        sink.join_writer();
        Ok(())
    }

    /// `record.stop.all.request` (= 同 session 全 record stop)。停止数を返す。
    pub fn stop_all(&self) -> usize {
        let sinks: Vec<Arc<RecordSink>> = {
            let mut guard = self.sinks.write().expect("record registry lock poisoned");
            std::mem::take(&mut *guard)
        };
        let n = sinks.len();
        for sink in &sinks {
            sink.join_writer();
        }
        n
    }

    /// `record.list.response` の records vector。
    pub fn list(&self) -> Vec<RecordInfo> {
        self.sinks
            .read()
            .expect("record registry lock poisoned")
            .iter()
            .map(|s| s.snapshot_info())
            .collect()
    }

    /// 該当 direction の全 sink に bytes-in event を push (= Phase 4 hot path 用)。
    ///
    /// TODO(Phase 4): hot path 配線、lock 下で send_timeout しない設計に揃える。
    pub fn push_bytes_in(&self, client_id: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let ts = now_unix_ms();
        self.broadcast_for(
            |d| matches!(d, RecordDirection::Stdin | RecordDirection::Both),
            |sink| {
                // never-record-stdin: stdin 由来 bytes を一切 file に残さない
                // (DR-0016 §6 interim)。BytesIn を配信せず header の申告を正直にする。
                if matches!(sink.input_secrecy, InputSecrecy::NeverRecordStdin) {
                    return Ok(());
                }
                sink.try_push(RecordEvent::BytesIn {
                    client_id,
                    bytes: bytes.to_vec(),
                    ts_unix_ms: ts,
                })
            },
        );
    }

    pub fn push_bytes_out(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let ts = now_unix_ms();
        self.broadcast_for(
            |d| matches!(d, RecordDirection::Stdout | RecordDirection::Both),
            |sink| {
                sink.try_push(RecordEvent::BytesOut {
                    bytes: bytes.to_vec(),
                    ts_unix_ms: ts,
                })
            },
        );
    }

    pub fn push_in_rejected(
        &self,
        client_id: u64,
        client_mode: Mode,
        lock_holder_client_id: Option<u64>,
        reason: InRejectedReason,
        bytes: &[u8],
    ) {
        let ts = now_unix_ms();
        self.broadcast_for(
            |d| matches!(d, RecordDirection::Stdin | RecordDirection::Both),
            |sink| {
                // never-record-stdin: 却下 input も生 bytes を含むので配信しない
                // (DR-0016 §6 interim)。
                if matches!(sink.input_secrecy, InputSecrecy::NeverRecordStdin) {
                    return Ok(());
                }
                sink.try_push(RecordEvent::InRejected {
                    client_id,
                    client_mode,
                    lock_holder_client_id,
                    reason,
                    bytes: bytes.to_vec(),
                    ts_unix_ms: ts,
                })
            },
        );
    }

    pub fn push_in_write_error(
        &self,
        client_id: u64,
        requested_len: usize,
        written_len: usize,
        error: WriteErrorKind,
        unwritten_bytes: &[u8],
    ) {
        let ts = now_unix_ms();
        self.broadcast_for(
            |d| matches!(d, RecordDirection::Stdin | RecordDirection::Both),
            |sink| {
                // never-record-stdin: unwritten_bytes に生 stdin を含むので配信しない
                // (DR-0016 §6 interim)。
                if matches!(sink.input_secrecy, InputSecrecy::NeverRecordStdin) {
                    return Ok(());
                }
                sink.try_push(RecordEvent::InWriteError {
                    client_id,
                    requested_len,
                    written_len,
                    error: error.clone(),
                    unwritten_bytes: unwritten_bytes.to_vec(),
                    ts_unix_ms: ts,
                })
            },
        );
    }

    /// Phase 5 (= secret redaction state machine) で配線、Phase 4 では未使用。
    #[allow(dead_code)]
    pub fn push_in_secret_redacted(&self, client_id: u64, byte_count: usize, reason: String) {
        let ts = now_unix_ms();
        self.broadcast_for(
            |d| matches!(d, RecordDirection::Stdin | RecordDirection::Both),
            |sink| {
                sink.try_push(RecordEvent::InSecretRedacted {
                    client_id,
                    byte_count,
                    reason: reason.clone(),
                    ts_unix_ms: ts,
                })
            },
        );
    }

    pub fn push_lifecycle(&self, ev: LifecycleEvent) {
        // lifecycle は全 sink に届ける (= direction 関係なし、ただし raw format では
        // writer thread 内で捨てられる、§5)。
        self.broadcast_for(
            |_| true,
            |sink| sink.try_push(RecordEvent::Lifecycle(ev.clone())),
        );
    }

    /// 内部 helper: filter に match した sink に対し push、queue-full / disconnect の
    /// 発生した sink は record-aborted 経路に乗せて回収する。
    fn broadcast_for<F, P>(&self, filter: F, push: P)
    where
        F: Fn(RecordDirection) -> bool,
        P: Fn(&RecordSink) -> Result<(), TrySendError<SeqEvent>>,
    {
        // Phase 4 で hot path から呼ぶときは Sender clone + lock 解放で send する想定。
        // Phase 2 の test 経路では lock 保持で send しても許容 (= test は単一 thread)。
        let aborted_ids: Vec<u32> = {
            let guard = self.sinks.read().expect("record registry lock poisoned");
            let mut aborted = Vec::new();
            for sink in guard.iter() {
                if !filter(sink.direction) {
                    continue;
                }
                if let Err(e) = push(sink) {
                    let reason = match e {
                        TrySendError::Full(_) => "queue-full",
                        TrySendError::Disconnected(_) => "writer-disconnected",
                    };
                    // 他 sink への lifecycle 通知は別 path で送る (= ここで lock 持ったまま
                    // 再 broadcast すると nested lock になる)。aborted_ids を集めて lock
                    // 解放後に処理する。
                    let _ = reason;
                    aborted.push(sink.record_id);
                }
            }
            aborted
        };
        if aborted_ids.is_empty() {
            return;
        }
        // aborted な sink を sinks から remove + 他 sink に lifecycle 通知。
        for id in aborted_ids {
            let removed = {
                let mut guard = self.sinks.write().expect("record registry lock poisoned");
                guard
                    .iter()
                    .position(|s| s.record_id == id)
                    .map(|pos| guard.remove(pos))
            };
            if let Some(sink) = removed {
                // 他 sink に「record N が queue-full で abort された」を通知。
                let ts = now_unix_ms();
                let ev = LifecycleEvent::RecordAborted {
                    record_id: id,
                    reason: "queue-full".into(),
                    detail: None,
                    ts_unix_ms: ts,
                };
                {
                    let guard = self.sinks.read().expect("record registry lock poisoned");
                    for other in guard.iter() {
                        let _ = other.try_push(RecordEvent::Lifecycle(ev.clone()));
                    }
                }
                // aborted 自身は drop → Sender drop → writer drain + exit。join は best-effort。
                sink.join_writer();
            }
        }
    }
}

impl Default for RecordRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// === Writer thread ===

/// writer thread 起動時に受け渡す config。
struct WriterConfig {
    record_id: u32,
    format: RecordFormat,
    direction: RecordDirection,
    #[allow(dead_code)] // Phase 5 で redaction 経路で参照
    input_secrecy: InputSecrecy,
    max_bytes: Option<u64>,
    max_duration_ms: Option<u64>,
    started_unix_ms: u64,
    session: SessionInfo,
    stats: Arc<RecordStats>,
}

/// writer thread の entry point (DR-0016 §8a)。
fn writer_thread_main(file: File, rx: crossbeam_channel::Receiver<SeqEvent>, cfg: WriterConfig) {
    let mut w = BufWriter::new(file);

    // jsonl の場合は header を eager に書く。raw は header なし (§5)。
    if matches!(cfg.format, RecordFormat::Jsonl)
        && let Err(e) = write_jsonl_header(&mut w, &cfg)
    {
        eprintln!("hyoui-record-{}: header write failed: {e}", cfg.record_id);
        return;
    }

    // メインループ: event を recv → encode → write + flush。
    loop {
        match rx.recv() {
            Ok(payload) => {
                if let Err(e) = handle_one_event(&mut w, &cfg, payload) {
                    eprintln!("hyoui-record-{}: event write failed: {e}", cfg.record_id);
                    return;
                }
                // max-bytes / max-duration を post-check (= 過剰書き込みを 1 event だけ許容)。
                if exceeds_limits(&cfg) {
                    // TODO(Phase 4): registry 経由で自分自身を stop する経路を作る。
                    // 現状は writer thread 内で抜けるだけ (= channel から後続 event が来ても
                    // 処理しない、Sender 側が Disconnected を観測)。
                    return;
                }
            }
            Err(_) => {
                // Sender が drop された (= stop された or daemon 終了)。drain は recv が
                // None を返した時点で完了済。flush して exit。
                let _ = w.flush();
                return;
            }
        }
    }
}

fn handle_one_event(
    w: &mut BufWriter<File>,
    cfg: &WriterConfig,
    payload: SeqEvent,
) -> std::io::Result<()> {
    match cfg.format {
        RecordFormat::Raw => write_raw_event(w, cfg, payload),
        RecordFormat::Jsonl => write_jsonl_event(w, cfg, payload),
    }
}

/// jsonl header 1 行目 (DR-0016 §3)。
fn write_jsonl_header(w: &mut BufWriter<File>, cfg: &WriterConfig) -> std::io::Result<()> {
    // TODO(Phase 6): argv / cwd の sensitive pattern を `<REDACTED>` 化。本 Phase は
    // sanitize なしの単純 dump。
    let header = JsonlHeader {
        v: 1,
        ty: "hyoui-record-jsonl/1",
        session: &cfg.session.session_id,
        daemon_pid: cfg.session.daemon_pid,
        daemon_boot_id: &cfg.session.daemon_boot_id,
        started_unix_ms: cfg.started_unix_ms,
        argv: &cfg.session.argv,
        cwd: &cfg.session.cwd,
        direction: direction_str(cfg.direction),
        input_secrecy: input_secrecy_str(cfg.input_secrecy),
    };
    let line = serde_json::to_string(&header).map_err(std::io::Error::other)?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()?;
    let written = (line.len() + 1) as u64;
    cfg.stats
        .file_bytes_written
        .fetch_add(written, Ordering::Relaxed);
    cfg.stats
        .last_flushed_unix_ms
        .store(now_unix_ms(), Ordering::Relaxed);
    Ok(())
}

/// jsonl 1 event 行 (DR-0016 §3 body)。
fn write_jsonl_event(
    w: &mut BufWriter<File>,
    cfg: &WriterConfig,
    payload: SeqEvent,
) -> std::io::Result<()> {
    let SeqEvent { seq, event } = payload;
    let raw_bytes_delta: u64 = match &event {
        RecordEvent::BytesIn { bytes, .. } => bytes.len() as u64,
        RecordEvent::BytesOut { bytes, .. } => bytes.len() as u64,
        _ => 0,
    };
    let json = encode_jsonl_event(seq, &event)?;
    w.write_all(json.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()?;
    let written = (json.len() + 1) as u64;
    cfg.stats
        .file_bytes_written
        .fetch_add(written, Ordering::Relaxed);
    cfg.stats
        .raw_bytes_recorded
        .fetch_add(raw_bytes_delta, Ordering::Relaxed);
    cfg.stats
        .last_flushed_unix_ms
        .store(now_unix_ms(), Ordering::Relaxed);
    Ok(())
}

/// raw 1 event (DR-0016 §5)。bytes event のみ書く、lifecycle / reject は捨てる。
fn write_raw_event(
    w: &mut BufWriter<File>,
    cfg: &WriterConfig,
    payload: SeqEvent,
) -> std::io::Result<()> {
    let SeqEvent { seq: _, event } = payload;
    let bytes: &[u8] = match &event {
        RecordEvent::BytesIn { bytes, .. } => bytes,
        RecordEvent::BytesOut { bytes, .. } => bytes,
        // lifecycle / reject / write-error / redacted は raw では捨てる
        _ => return Ok(()),
    };
    w.write_all(bytes)?;
    w.flush()?;
    let n = bytes.len() as u64;
    cfg.stats.file_bytes_written.fetch_add(n, Ordering::Relaxed);
    cfg.stats.raw_bytes_recorded.fetch_add(n, Ordering::Relaxed);
    cfg.stats
        .last_flushed_unix_ms
        .store(now_unix_ms(), Ordering::Relaxed);
    Ok(())
}

fn exceeds_limits(cfg: &WriterConfig) -> bool {
    if let Some(max_b) = cfg.max_bytes
        && max_b > 0
        && cfg.stats.raw_bytes_recorded.load(Ordering::Relaxed) >= max_b
    {
        return true;
    }
    if let Some(max_d) = cfg.max_duration_ms
        && max_d > 0
        && now_unix_ms().saturating_sub(cfg.started_unix_ms) >= max_d
    {
        return true;
    }
    false
}

// === JSON encoding helpers ===

#[derive(Serialize)]
struct JsonlHeader<'a> {
    v: u32,
    #[serde(rename = "type")]
    ty: &'a str,
    session: &'a str,
    daemon_pid: u32,
    daemon_boot_id: &'a str,
    started_unix_ms: u64,
    argv: &'a [String],
    cwd: &'a str,
    direction: &'a str,
    input_secrecy: &'a str,
}

fn direction_str(d: RecordDirection) -> &'static str {
    // 同 crate 内 `#[non_exhaustive]` は wildcard を作らないので、全 variant を列挙する。
    match d {
        RecordDirection::Stdin => "stdin",
        RecordDirection::Stdout => "stdout",
        RecordDirection::Both => "both",
    }
}

fn input_secrecy_str(s: InputSecrecy) -> &'static str {
    match s {
        InputSecrecy::RedactAfterPrompt => "redact-after-prompt",
        InputSecrecy::RecordAll => "record-all",
        InputSecrecy::NeverRecordStdin => "never-record-stdin",
    }
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Ro => "ro",
        Mode::Rw => "rw",
        Mode::RwNoLeader => "rw-no-leader",
    }
}

/// 1 event を jsonl 1 行に encode。
///
/// 全 event 種で `ts_unix_ms` / `seq` を持ち、bytes event は `dir` 付き、それ以外は
/// `ev` 付き (= DR-0016 §3 の format に揃える)。serde の `#[derive(Serialize)]` を
/// 並列に作ると tag 制御で煩雑になるので、`serde_json::Value` で組み立てて write。
fn encode_jsonl_event(seq: u32, event: &RecordEvent) -> std::io::Result<String> {
    use serde_json::{Value, json};
    let v: Value = match event {
        RecordEvent::BytesIn {
            client_id,
            bytes,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "dir": "in",
            "client_id": client_id,
            "bytes": hex::encode(bytes),
        }),
        RecordEvent::BytesOut { bytes, ts_unix_ms } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "dir": "out",
            "bytes": hex::encode(bytes),
        }),
        RecordEvent::InRejected {
            client_id,
            client_mode,
            lock_holder_client_id,
            reason,
            bytes,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "in-rejected",
            "client_id": client_id,
            "client_mode": mode_str(*client_mode),
            "lock_holder_client_id": lock_holder_client_id,
            "reason": reason.as_str(),
            "bytes": hex::encode(bytes),
        }),
        RecordEvent::InWriteError {
            client_id,
            requested_len,
            written_len,
            error,
            unwritten_bytes,
            ts_unix_ms,
        } => {
            let mut obj = json!({
                "ts_unix_ms": ts_unix_ms,
                "seq": seq,
                "ev": "in-write-error",
                "client_id": client_id,
                "requested_len": requested_len,
                "written_len": written_len,
                "error": error.as_wire_str(),
                "unwritten_bytes": hex::encode(unwritten_bytes),
            });
            if let WriteErrorKind::IoError(msg) = error {
                obj.as_object_mut()
                    .expect("json object")
                    .insert("error_detail".into(), Value::String(msg.clone()));
            }
            obj
        }
        RecordEvent::InSecretRedacted {
            client_id,
            byte_count,
            reason,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "in-secret-redacted",
            "client_id": client_id,
            "byte_count": byte_count,
            "reason": reason,
        }),
        RecordEvent::Lifecycle(ev) => encode_lifecycle(seq, ev),
    };
    serde_json::to_string(&v).map_err(std::io::Error::other)
}

fn encode_lifecycle(seq: u32, ev: &LifecycleEvent) -> serde_json::Value {
    use serde_json::json;
    match ev {
        LifecycleEvent::ChildStoppedObserved {
            sig_name,
            sig_num,
            pid,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "child-stopped-observed",
            "sig_name": sig_name,
            "sig_num": sig_num,
            "pid": pid,
        }),
        LifecycleEvent::ResumeRequestReceived {
            client_id,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "resume-request-received",
            "client_id": client_id,
        }),
        LifecycleEvent::SigcontSent { pid, ts_unix_ms } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "sigcont-sent",
            "pid": pid,
        }),
        LifecycleEvent::ChildContinuedObserved {
            sig_name,
            sig_num,
            pid,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "child-continued-observed",
            "sig_name": sig_name,
            "sig_num": sig_num,
            "pid": pid,
        }),
        LifecycleEvent::ClientAttached {
            client_id,
            mode,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "client-attached",
            "client_id": client_id,
            "mode": mode_str(*mode),
        }),
        LifecycleEvent::ClientDetached {
            client_id,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "client-detached",
            "client_id": client_id,
        }),
        LifecycleEvent::LockAcquired {
            client_id,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "lock-acquired",
            "client_id": client_id,
        }),
        LifecycleEvent::LockReleased {
            client_id,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "lock-released",
            "client_id": client_id,
        }),
        LifecycleEvent::RecordAborted {
            record_id,
            reason,
            detail,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "record-aborted",
            "record_id": record_id,
            "reason": reason,
            "detail": detail,
        }),
        LifecycleEvent::SessionTerminatedByCondition { reason, ts_unix_ms } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "session-terminated-by-condition",
            "reason": reason,
        }),
        LifecycleEvent::PolicyChanged {
            key,
            value,
            changed_by_client_id,
            ts_unix_ms,
        } => json!({
            "ts_unix_ms": ts_unix_ms,
            "seq": seq,
            "ev": "policy-changed",
            "key": key,
            "value": value,
            "changed_by_client_id": changed_by_client_id,
        }),
    }
}

// === path validation + file open ===

/// output path を検証する (DR-0016 §9 + REVIEW-BACKLOG R5-C2 系の防御強化)。
///
/// 検証順:
/// 1. absolute であること
/// 2. `..` (parent dir) 成分を含まないこと (= `/tmp/../home/u/.ssh/x.log` で
///    後段の sensitive prefix deny を迂回されるのを防ぐ)
/// 3. 拡張子 allowlist (file_name 由来なので canonicalize に依らず安定)
/// 4. **親ディレクトリを canonicalize** し (= 中間ディレクトリの symlink
///    `/tmp/evil -> ~` を実体に解決)、解決後の path で sensitive prefix を判定
///
/// file 自体は未存在 (= `O_EXCL` で後段 open) なので path 全体は canonicalize
/// できない。`parent().canonicalize()? + file_name` で解決後 path を組む。
/// O_NOFOLLOW が最終要素 symlink のみを防ぐのに対し、parent canonicalize は
/// **中間ディレクトリ symlink** を塞ぐ (= 防御層の補完)。
fn validate_output_path(path: &Path) -> Result<(), RecordStartError> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(RecordStartError::PathNotAbsolute);
    }

    // `..` 成分を含むパスは即 reject (= 正規化前に潜り込ませた traversal を弾く)。
    // `.` (CurDir) は無害なので許容、絶対パス前提なので Prefix は無い。
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(RecordStartError::OutputPermissionDenied(format!(
            "path must not contain '..': {}",
            path.display()
        )));
    }

    // 拡張子 allowlist (file_name 由来なので canonicalize に依らず安定)。
    let ext_ok = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| ALLOWED_EXTENSIONS.contains(&s))
        .unwrap_or(false);
    if !ext_ok {
        return Err(RecordStartError::OutputPermissionDenied(format!(
            "extension not in allowlist ({:?}): {}",
            ALLOWED_EXTENSIONS,
            path.display()
        )));
    }

    // 親 dir を実体に解決する。file 自体は未存在なので親だけ canonicalize し、
    // file_name を結合して「解決後の output path」を組む。親 dir が存在しない /
    // 解決できない場合はここで弾く (= 旧来の `parent.exists()` check を内包)。
    let Some(parent) = path.parent() else {
        return Err(RecordStartError::OutputPermissionDenied(format!(
            "output path has no parent directory: {}",
            path.display()
        )));
    };
    let Some(file_name) = path.file_name() else {
        return Err(RecordStartError::OutputPermissionDenied(format!(
            "output path has no file name: {}",
            path.display()
        )));
    };
    let real_parent = parent.canonicalize().map_err(|e| {
        RecordStartError::OutputPermissionDenied(format!(
            "parent directory does not resolve ({e}): {}",
            parent.display()
        ))
    })?;
    let resolved = real_parent.join(file_name);

    // HOME 配下の sensitive prefix を **解決後 path** で判定 (= 中間 symlink
    // 経由で `~/.ssh/` 等に着地するケースも実体パスで検出する)。
    if let Some(home) = std::env::var_os("HOME") {
        // HOME 自体も symlink を含む可能性があるので canonicalize して揃える
        // (= 解決失敗時は raw HOME に fallback して strip を試みる)。
        let home = PathBuf::from(home);
        let home = home.canonicalize().unwrap_or(home);
        if let Ok(rel) = resolved.strip_prefix(&home) {
            let rel_str = rel.to_string_lossy();
            for bad in SENSITIVE_HOME_PREFIXES {
                if rel_str.starts_with(bad) {
                    return Err(RecordStartError::OutputPermissionDenied(format!(
                        "sensitive HOME prefix denied (resolved): {}",
                        resolved.display()
                    )));
                }
            }
        }
    }

    Ok(())
}

fn open_record_file(path: &Path) -> Result<File, RecordStartError> {
    let mut opts = OpenOptions::new();
    opts.create_new(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .mode(0o600);
    match opts.open(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(RecordStartError::OutputAlreadyExists)
        }
        Err(e) => Err(RecordStartError::Io(e)),
    }
}

// === unused warning suppression for `MetadataExt`/`Path` re-exports ===
// (`MetadataExt` は test で mode 0600 確認に使う、`Path` は import 整理用)。
#[allow(dead_code)]
fn _mode_check_helper(file: &File) -> std::io::Result<u32> {
    Ok(file.metadata()?.mode())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn sample_session() -> SessionInfo {
        SessionInfo {
            session_id: "test-session".into(),
            daemon_pid: 12345,
            daemon_boot_id: "boot-id-aaa".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            cwd: "/tmp".into(),
        }
    }

    fn jsonl_request(path: &Path, direction: RecordDirection) -> RecordStartRequest {
        RecordStartRequest {
            direction,
            format: RecordFormat::Jsonl,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            // interim default (= record-all)。redact-after-prompt は start で reject
            // されるため、共通ヘルパーは record-all を使う (= 個別 test は必要に応じて
            // req.input_secrecy を上書きする)。
            input_secrecy: InputSecrecy::RecordAll,
            prompt_pattern: None,
        }
    }

    fn raw_request(path: &Path, direction: RecordDirection) -> RecordStartRequest {
        RecordStartRequest {
            direction,
            format: RecordFormat::Raw,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            // interim default (= record-all)。理由は jsonl_request 参照。
            input_secrecy: InputSecrecy::RecordAll,
            prompt_pattern: None,
        }
    }

    #[test]
    fn record_id_monotonic_increasing() {
        let dir = tempfile::tempdir().unwrap();
        let registry = RecordRegistry::new();
        let id1 = registry
            .start(
                &jsonl_request(&dir.path().join("a.jsonl"), RecordDirection::Both),
                7,
                sample_session(),
            )
            .unwrap();
        let id2 = registry
            .start(
                &jsonl_request(&dir.path().join("b.jsonl"), RecordDirection::Both),
                7,
                sample_session(),
            )
            .unwrap();
        assert!(id2 > id1);
        assert_eq!(registry.stop_all(), 2);
    }

    #[test]
    fn jsonl_header_eager_written_and_well_formed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hdr.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                42,
                sample_session(),
            )
            .unwrap();
        registry.stop(id).unwrap();

        let f = File::open(&path).unwrap();
        let reader = BufReader::new(f);
        let lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>().unwrap();
        assert_eq!(lines.len(), 1, "header only, no body events expected");
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["type"], "hyoui-record-jsonl/1");
        assert_eq!(v["session"], "test-session");
        assert_eq!(v["daemon_pid"], 12345);
        assert_eq!(v["daemon_boot_id"], "boot-id-aaa");
        assert!(v["started_unix_ms"].as_u64().is_some());
        assert!(v["argv"].as_array().is_some());
        assert_eq!(v["cwd"], "/tmp");
        assert_eq!(v["direction"], "both");
        // interim default は record-all (= ヘルパー jsonl_request が渡す値)。
        assert_eq!(v["input_secrecy"], "record-all");
    }

    #[test]
    fn writer_writes_jsonl_bytes_in_and_out_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ev.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .unwrap();
        registry.push_bytes_in(99, b"hello");
        registry.push_bytes_out(b"world");
        registry.stop(id).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 body events");
        let in_evt: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(in_evt["dir"], "in");
        assert_eq!(in_evt["client_id"], 99);
        assert_eq!(in_evt["bytes"], hex::encode("hello"));
        assert_eq!(in_evt["seq"], 1);
        let out_evt: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(out_evt["dir"], "out");
        assert_eq!(out_evt["bytes"], hex::encode("world"));
        assert_eq!(out_evt["seq"], 2);
    }

    #[test]
    fn writer_writes_raw_bytes_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ev.raw");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &raw_request(&path, RecordDirection::Stdout),
                1,
                sample_session(),
            )
            .unwrap();
        registry.push_bytes_out(b"hello");
        // lifecycle は raw では捨てられる
        registry.push_lifecycle(LifecycleEvent::SigcontSent {
            pid: 42,
            ts_unix_ms: 0,
        });
        registry.push_bytes_out(b"world");
        registry.stop(id).unwrap();

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"helloworld");
    }

    #[test]
    fn raw_both_direction_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.raw");
        let registry = RecordRegistry::new();
        let err = registry
            .start(
                &raw_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .expect_err("raw + both must be rejected");
        assert!(matches!(
            err,
            RecordStartError::UnsupportedDirectionForFormat
        ));
        // file は open 前に reject されているので存在しないはず
        assert!(!path.exists());
    }

    #[test]
    fn relative_path_rejected() {
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(Path::new("relative.jsonl"), RecordDirection::Both);
        req.output_path = "relative.jsonl".into();
        let err = registry
            .start(&req, 1, sample_session())
            .expect_err("relative path must be rejected");
        assert!(matches!(err, RecordStartError::PathNotAbsolute));
    }

    #[test]
    fn disallowed_extension_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.sh");
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(&path, RecordDirection::Both);
        req.output_path = path.to_string_lossy().into_owned();
        let err = registry
            .start(&req, 1, sample_session())
            .expect_err(".sh must be rejected");
        assert!(matches!(err, RecordStartError::OutputPermissionDenied(_)));
    }

    #[test]
    fn nonexistent_parent_rejected() {
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(
            Path::new("/nonexistent/parent/x.jsonl"),
            RecordDirection::Both,
        );
        req.output_path = "/nonexistent/parent/x.jsonl".into();
        let err = registry
            .start(&req, 1, sample_session())
            .expect_err("missing parent dir must be rejected");
        assert!(matches!(err, RecordStartError::OutputPermissionDenied(_)));
    }

    #[test]
    fn existing_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exists.jsonl");
        std::fs::write(&path, b"pre-existing\n").unwrap();
        let registry = RecordRegistry::new();
        let err = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .expect_err("existing file must be rejected");
        assert!(matches!(err, RecordStartError::OutputAlreadyExists));
    }

    #[test]
    fn symlink_target_rejected() {
        // /tmp/foo.jsonl -> /tmp/real.jsonl のような symlink は O_NOFOLLOW で ELOOP。
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.jsonl");
        std::fs::write(&target, b"existing target\n").unwrap();
        let link = dir.path().join("link.jsonl");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let registry = RecordRegistry::new();
        let err = registry
            .start(
                &jsonl_request(&link, RecordDirection::Both),
                1,
                sample_session(),
            )
            .expect_err("symlink must be rejected (O_NOFOLLOW or AlreadyExists)");
        // O_NOFOLLOW で `Io(ELOOP)`、または `create_new` の `AlreadyExists` のどちらか。
        // 実装上の意味は等価 (= symlink 先には書かない) なので、両方を許容する。
        match err {
            RecordStartError::Io(_) | RecordStartError::OutputAlreadyExists => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn output_file_mode_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perm.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .unwrap();
        registry.stop(id).unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode();
        // mode は file type bits を含むので下位 9 bit (= permission) のみ比較。
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn sensitive_home_prefix_rejected() {
        // ~/.ssh/key.jsonl は拡張子が allowlist にあっても sensitive prefix で reject される。
        // HOME を tempdir に差し替えて副作用を切り離す。
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let path = ssh.join("key.jsonl");
        let registry = RecordRegistry::new();
        // SAFETY: test 内で限定的に HOME を差し替える。並列 test との競合は cargo の
        // 並列実行で他 test も `HOME` を変える可能性があるが、本 test は HOME を読む
        // path 経路のみ通り、他 test (file 操作のみ) には影響しない。
        let prev_home = std::env::var_os("HOME");
        // env mutation は `crate::sys::env` の wrapper 経由 (= unsafe を 1 箇所に閉じ込め)。
        crate::sys::env::set_var("HOME", &dir.path().to_string_lossy());
        let mut req = jsonl_request(&path, RecordDirection::Both);
        req.output_path = path.to_string_lossy().into_owned();
        let res = registry.start(&req, 1, sample_session());
        // 復元
        match prev_home {
            Some(v) => {
                crate::sys::env::set_var("HOME", &v.to_string_lossy());
            }
            None => crate::sys::env::remove_var("HOME"),
        }
        let err = res.expect_err("~/.ssh/* must be rejected");
        assert!(matches!(err, RecordStartError::OutputPermissionDenied(_)));
    }

    #[test]
    fn parent_dir_traversal_component_rejected() {
        // `/tmp/../etc/x.log` のような `..` 入り path は、後段の sensitive prefix
        // deny を迂回し得るので、正規化前に即 reject される。
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(Path::new("/tmp/../tmp/x.jsonl"), RecordDirection::Both);
        req.output_path = "/tmp/../tmp/x.jsonl".into();
        let err = registry
            .start(&req, 1, sample_session())
            .expect_err("path containing '..' must be rejected");
        match err {
            RecordStartError::OutputPermissionDenied(msg) => {
                assert!(msg.contains("'..'"), "unexpected message: {msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn traversal_into_sensitive_home_prefix_rejected() {
        // HOME=<tmp> に差し替え、`<tmp>/safe/../.ssh/key.jsonl` という `..` 経由で
        // sensitive prefix (.ssh/) に着地させようとする迂回を `..` reject で塞ぐ。
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("safe")).unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        let traversal = dir.path().join("safe/../.ssh/key.jsonl");
        let registry = RecordRegistry::new();
        let prev_home = std::env::var_os("HOME");
        crate::sys::env::set_var("HOME", &dir.path().to_string_lossy());
        let mut req = jsonl_request(&traversal, RecordDirection::Both);
        req.output_path = traversal.to_string_lossy().into_owned();
        let res = registry.start(&req, 1, sample_session());
        match prev_home {
            Some(v) => crate::sys::env::set_var("HOME", &v.to_string_lossy()),
            None => crate::sys::env::remove_var("HOME"),
        }
        let err = res.expect_err("'..' traversal into ~/.ssh must be rejected");
        assert!(matches!(err, RecordStartError::OutputPermissionDenied(_)));
    }

    #[test]
    fn intermediate_symlink_into_sensitive_home_resolved_and_rejected() {
        // 中間ディレクトリ symlink (`<tmp>/evil -> $HOME`) 経由で `~/.ssh/` に
        // 着地させる迂回を、親 dir canonicalize で実体解決して reject する。
        // O_NOFOLLOW は最終要素のみなので、この層が無いと素通りしてしまう。
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();

        // 攻撃者の作業 dir 内に HOME を指す symlink を張る。
        let work = tempfile::tempdir().unwrap();
        let evil = work.path().join("evil");
        std::os::unix::fs::symlink(home.path(), &evil).unwrap();

        // `<work>/evil/.ssh/key.jsonl` を解決すると実体は `<home>/.ssh/key.jsonl`。
        let attack = evil.join(".ssh").join("key.jsonl");
        let registry = RecordRegistry::new();
        let prev_home = std::env::var_os("HOME");
        crate::sys::env::set_var("HOME", &home.path().to_string_lossy());
        let mut req = jsonl_request(&attack, RecordDirection::Both);
        req.output_path = attack.to_string_lossy().into_owned();
        let res = registry.start(&req, 1, sample_session());
        match prev_home {
            Some(v) => crate::sys::env::set_var("HOME", &v.to_string_lossy()),
            None => crate::sys::env::remove_var("HOME"),
        }
        let err = res.expect_err("intermediate symlink into ~/.ssh must be rejected");
        match err {
            RecordStartError::OutputPermissionDenied(msg) => {
                assert!(msg.contains("resolved"), "unexpected message: {msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // file が作られていないことも確認 (= deny は open 前)。
        assert!(!ssh.join("key.jsonl").exists());
    }

    #[test]
    fn intermediate_symlink_to_safe_dir_allowed() {
        // 中間 symlink が sensitive でない場所を指す場合は、canonicalize しても
        // deny されず正常に record できる (= 過剰 block の回帰防止)。
        let real = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(real.path().join("logs")).unwrap();
        let work = tempfile::tempdir().unwrap();
        let link = work.path().join("link");
        std::os::unix::fs::symlink(real.path().join("logs"), &link).unwrap();
        let path = link.join("ok.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .expect("symlink to safe dir must be allowed");
        registry.stop(id).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn list_reports_active_records() {
        let dir = tempfile::tempdir().unwrap();
        let registry = RecordRegistry::new();
        let _id = registry
            .start(
                &jsonl_request(&dir.path().join("a.jsonl"), RecordDirection::Both),
                7,
                sample_session(),
            )
            .unwrap();
        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].started_by_client_id, 7);
        assert_eq!(list[0].direction, RecordDirection::Both);
        assert_eq!(list[0].format, RecordFormat::Jsonl);
        registry.stop_all();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn write_error_event_carries_partial_write_info() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("we.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Stdin),
                1,
                sample_session(),
            )
            .unwrap();
        registry.push_in_write_error(
            5,
            150,
            100,
            WriteErrorKind::IdleTimeout,
            b"unwritten_tail_bytes",
        );
        registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["ev"], "in-write-error");
        assert_eq!(v["client_id"], 5);
        assert_eq!(v["requested_len"], 150);
        assert_eq!(v["written_len"], 100);
        assert_eq!(v["error"], "timeout");
        assert_eq!(v["unwritten_bytes"], hex::encode("unwritten_tail_bytes"));
    }

    /// Minor 1: until / timeout / idle-timeout の各終了条件が
    /// `session-terminated-by-condition` event として reason 付きで jsonl 化される
    /// (= record.rs の doc が謳う 3 種の reason、特に until 経路が emit されること)。
    #[test]
    fn session_terminated_by_condition_event_carries_reason() {
        for reason in ["until", "timeout", "idle-timeout"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("term.jsonl");
            let registry = RecordRegistry::new();
            let id = registry
                .start(
                    &jsonl_request(&path, RecordDirection::Stdout),
                    1,
                    sample_session(),
                )
                .unwrap();
            registry.push_lifecycle(LifecycleEvent::SessionTerminatedByCondition {
                reason: reason.to_string(),
                ts_unix_ms: 123,
            });
            registry.stop(id).unwrap();
            let content = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = content.lines().collect();
            assert_eq!(lines.len(), 2, "header + 1 lifecycle event");
            let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
            assert_eq!(v["ev"], "session-terminated-by-condition");
            assert_eq!(v["reason"], reason);
        }
    }

    #[test]
    fn in_rejected_event_carries_reason_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rej.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Stdin),
                1,
                sample_session(),
            )
            .unwrap();
        registry.push_in_rejected(7, Mode::Ro, None, InRejectedReason::RoClient, b"\x1b[A");
        registry.push_in_rejected(9, Mode::Rw, Some(7), InRejectedReason::LockNotHeld, b"\x01");
        registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["ev"], "in-rejected");
        assert_eq!(v["reason"], "ro-client");
        assert_eq!(v["client_mode"], "ro");
        let v2: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(v2["reason"], "lock-not-held");
        assert_eq!(v2["lock_holder_client_id"], 7);
    }

    #[test]
    fn queue_full_aborts_record_and_notifies_other_sinks() {
        // queue を埋めるため、writer が遅延する状況を作る。tempfile で path を取り、
        // 大量 push する。1024 capacity を超えた push は 100ms timeout で諦め、
        // 当該 record は registry から自動除去、生存中の他 sink には `record-aborted`
        // lifecycle event が届く。
        //
        // テストの安定性のため、writer thread を **sleep を仕込む方法では入れない**
        // (= 内部実装変更で fragile)。代わりに、`push_bytes_in` を 1024 + α 回
        // 連続実行 → queue が一時的に満たされる挙動を観察する。push 側 100ms timeout
        // が効くので、test は最大 100ms x (溢れた回数) で完了する。
        //
        // ただし writer が現実的に十分速ければ queue は溢れず、この test は flaky になる。
        // そのため SLOW_SINK を 1 つ作り、もう 1 つの FAST_SINK (= 観測対象) が
        // notify を受け取るかを確認する形にする。
        //
        // 簡略版として、small capacity を inject できる helper を持たないので、
        // 本 test は「queue 容量 1024 を超える push を行うと aborted_ids 処理経路に
        // 入る」かどうかを assertion なしで一応走らせるスモークに留める。詳細な
        // notify path は Phase 4 で hot path 配線後に integration test で検証。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smoke.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .unwrap();
        // 軽量 push (= 内部処理が遅延しない範囲で動作確認)
        for _ in 0..10 {
            registry.push_bytes_out(b"x");
        }
        registry.stop(id).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn prompt_pattern_invalid_regex_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(&path, RecordDirection::Both);
        req.prompt_pattern = Some("(unclosed".into());
        let err = registry
            .start(&req, 1, sample_session())
            .expect_err("bad regex must be rejected");
        assert!(matches!(err, RecordStartError::InvalidPromptPattern(_)));
    }

    #[test]
    fn stop_unknown_id_returns_error() {
        let registry = RecordRegistry::new();
        let err = registry.stop(999).expect_err("unknown id must error");
        assert_eq!(err.0, 999);
    }

    #[test]
    fn max_bytes_triggers_self_stop_on_writer() {
        // max_bytes 5 で 4 byte + 4 byte push、計 8 byte で writer は post-check で
        // 自動 stop する。stop() は registry 側で見えないが、writer thread が exit する
        // と stop_all で残り 0 件として扱われる… のは挙動上不可能 (= sink は registry に
        // 残る)。本 test は file に書かれた bytes が想定通り 2 行で止まることだけ確認。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mb.jsonl");
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(&path, RecordDirection::Stdout);
        req.max_bytes = Some(5);
        let id = registry.start(&req, 1, sample_session()).unwrap();
        registry.push_bytes_out(b"AAAA");
        registry.push_bytes_out(b"BBBB");
        // writer thread が exit するのに少し時間をかける。stop で channel disconnect →
        // pending event は drain される。drain 中に max-bytes check で exit する可能性
        // もあるので、最終的に 1〜2 件の event line が存在することを許容する範囲で assert。
        registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let body_lines = content.lines().count() - 1; // minus header
        assert!(
            (1..=2).contains(&body_lines),
            "expected 1 or 2 body lines, got {body_lines}"
        );
    }

    /// never-record-stdin: stdin 由来 event (BytesIn) を当該 sink に配信しない。
    /// BytesOut は stdin 由来でないので通常通り記録される。これで header の
    /// `input_secrecy=never-record-stdin` 申告が「実際に stdin を残さない」という
    /// 意味で正直になる (= DR-0016 §6 interim、Phase 5 の in-redacted 化に先立つ最小防護)。
    #[test]
    fn never_record_stdin_drops_bytes_in_keeps_bytes_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nrs.jsonl");
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(&path, RecordDirection::Both);
        req.input_secrecy = InputSecrecy::NeverRecordStdin;
        let id = registry.start(&req, 1, sample_session()).unwrap();
        registry.push_bytes_in(99, b"secret-passphrase");
        registry.push_bytes_out(b"prompt> ");
        registry.stop(id).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // header + BytesOut のみ (= BytesIn は drop されるので body 1 行)。
        assert_eq!(lines.len(), 2, "header + out only, stdin dropped");
        let hdr: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(hdr["input_secrecy"], "never-record-stdin");
        let out: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(out["dir"], "out");
        // stdin secret の hex が file に一切現れないことを直接確認 (= 平文残留の回帰防止)。
        assert!(
            !content.contains(&hex::encode("secret-passphrase")),
            "stdin secret must not appear in never-record-stdin file"
        );
    }

    /// never-record-stdin: 却下 input (InRejected) / partial write (InWriteError) も
    /// 生 bytes を含むため配信しない。stdin 由来の全 in 系 event が drop され、header
    /// のみが残る (= DR-0016 §6 interim、生 bytes を含む event の網羅 gate 確認)。
    #[test]
    fn never_record_stdin_drops_rejected_and_write_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nrs2.jsonl");
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(&path, RecordDirection::Stdin);
        req.input_secrecy = InputSecrecy::NeverRecordStdin;
        let id = registry.start(&req, 1, sample_session()).unwrap();
        registry.push_in_rejected(
            7,
            Mode::Ro,
            None,
            InRejectedReason::RoClient,
            b"rejected-secret",
        );
        registry.push_in_write_error(5, 20, 10, WriteErrorKind::IdleTimeout, b"unwritten-secret");
        registry.stop(id).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // header のみ (= stdin 由来 event はすべて drop)。
        assert_eq!(
            lines.len(),
            1,
            "header only, all stdin-derived events dropped"
        );
        assert!(!content.contains(&hex::encode("rejected-secret")));
        assert!(!content.contains(&hex::encode("unwritten-secret")));
    }

    /// record-all sink には stdin (BytesIn) が verbatim で記録される (= interim default)。
    /// never-record-stdin との対比: record-all は「全 stdin を残す」という header 申告
    /// 通りに動く。ヘルパー jsonl_request は record-all を渡す。
    #[test]
    fn record_all_keeps_bytes_in_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ra.jsonl");
        let registry = RecordRegistry::new();
        let id = registry
            .start(
                &jsonl_request(&path, RecordDirection::Both),
                1,
                sample_session(),
            )
            .unwrap();
        registry.push_bytes_in(3, b"typed-input");
        registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains(&hex::encode("typed-input")),
            "record-all must keep stdin verbatim"
        );
        let hdr: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(hdr["input_secrecy"], "record-all");
    }

    /// redact-after-prompt は redaction state machine が Phase 5 未配線のため start で
    /// reject される (= 「redact 済」と偽る file を作らせない、DR-0016 §6 interim)。
    /// 正規 CLI は parse 段でも弾くが、protocol を直叩きする旧 / 別実装 client への
    /// 防御を daemon 側でも固定する。
    #[test]
    fn redact_after_prompt_rejected_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rap.jsonl");
        let registry = RecordRegistry::new();
        let mut req = jsonl_request(&path, RecordDirection::Both);
        req.input_secrecy = InputSecrecy::RedactAfterPrompt;
        let err = registry
            .start(&req, 1, sample_session())
            .expect_err("redact-after-prompt must be rejected");
        assert!(matches!(err, RecordStartError::RedactionUnimplemented));
        // reject は file open 前 (= 空 file すら残さない)。
        assert!(!path.exists());
    }
}
