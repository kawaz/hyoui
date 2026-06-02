//! `record.*` payload (DR-0016 §7、tty I/O timeline 永続録画)。
//!
//! cap `record-v1` (optional) を negotiate した client/daemon 間のみで使われる。
//! 未 cap で送ると daemon は `error` kind=`unsupported-capability` を返す。
//!
//! ## kind 一覧
//!
//! - `record.start.request` — client → daemon、録画開始
//! - `record.start.response` — daemon → client、`record_id` 採番
//! - `record.stop.request` — client → daemon、`record_id` 指定停止
//! - `record.stop.all.request` — client → daemon、同 session の全 record 停止
//! - `record.stop.response` — daemon → client、停止数 ACK (= 成功も応答する)
//! - `record.list.request` — client → daemon、active record 一覧要求
//! - `record.list.response` — daemon → client、active record 一覧
//!
//! ## 設計メモ
//!
//! - 全 struct は **empty braces `{}`** で定義する (= unit struct `;` は CBOR `null`
//!   になり `#[serde(tag = "kind")]` の dispatch が破綻する。既存 `StatusQuery {}` /
//!   `SessionChildResumeRequest {}` と同様)。
//! - field naming は `#[serde(rename_all = "kebab-case")]` で kebab-case
//!   (例: `record_id` → `record-id`、既存 message と整合)。
//! - enum は `#[non_exhaustive]` で variant 追加に強くする。

use serde::{Deserialize, Serialize};

/// 録画対象の direction (DR-0016 §2、§7)。
///
/// - `Stdin`: client → daemon → 子 PTY (= 認可済 write 成功 bytes のみ)
/// - `Stdout`: 子 PTY → daemon (= 加工前生 bytes)
/// - `Both`: 両方 (= jsonl format default)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RecordDirection {
    /// 子 PTY への入力のみ記録 (= 認可済 write 成功 bytes)。
    Stdin,
    /// 子 PTY からの出力のみ記録 (= screen 加工前の生 bytes)。
    Stdout,
    /// stdin + stdout 双方を記録 (= jsonl 限定、raw では invalid)。
    Both,
}

/// 録画 file format (DR-0016 §3, §5)。
///
/// - `Jsonl`: header + body 構造化 (= timestamp / seq / lifecycle event 込み、診断用)
/// - `Raw`: 単一 direction の raw bytes (= stream export 専用、`Both` 不可)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RecordFormat {
    /// 単一 direction の raw bytes をそのまま書き出す (`cat` 互換、診断 timeline 不可)。
    Raw,
    /// 1 行目 header + 2 行目以降 bytes / lifecycle event (= 診断 timeline 用)。
    Jsonl,
}

/// stdin の secret redaction policy (DR-0016 §6)。
///
/// - `RedactAfterPrompt`: default、password/OTP prompt 後の stdin を `in-secret-redacted` 化
/// - `RecordAll`: redaction なし、全 stdin を hex 記録 (loud warning 必須)
/// - `NeverRecordStdin`: 全 stdin を `in-redacted` 化 (= 内容は捨てる、byte_count のみ)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum InputSecrecy {
    /// password/OTP prompt 検知後の stdin を redaction (= default)。
    RedactAfterPrompt,
    /// 全 stdin を raw bytes で記録 (= opt-in、`record start` 時に loud warning)。
    RecordAll,
    /// 全 stdin を redaction (= 内容捨て、byte_count のみ保持)。
    NeverRecordStdin,
}

/// `record.start.request` payload (DR-0016 §7、client → daemon)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordStartRequest {
    /// 録画する direction (= stdin / stdout / both)。
    pub direction: RecordDirection,
    /// 出力 format (= raw / jsonl)。`Raw` は単一 direction 限定。
    pub format: RecordFormat,
    /// 出力 file path (= **絶対 path 必須**、daemon 側で再検証)。
    pub output_path: String,
    /// 録画 byte 数上限 (= 到達で自動 stop、`None` で disable)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// 録画 duration 上限 ms (= 到達で自動 stop、`None` で disable)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<u64>,
    /// stdin redaction policy (= default は `RedactAfterPrompt`)。
    pub input_secrecy: InputSecrecy,
    /// custom prompt detection regex (= `None` で default の英 password/OTP pattern)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_pattern: Option<String>,
}

/// `record.start.response` payload (DR-0016 §7、daemon → client)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordStartResponse {
    /// daemon が採番した session-scope monotonic な録画識別子 (= 同 session 内一意)。
    pub record_id: u32,
}

/// `record.stop.request` payload (DR-0016 §7、client → daemon)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordStopRequest {
    /// 停止対象の `record_id`。
    pub record_id: u32,
}

/// `record.stop.response` payload (DR-0016 §7、daemon → client)。
///
/// `record.stop.request` / `record.stop.all.request` 双方の成功 ACK。client は
/// 成功でも response を待つ (= request/response の同期完結 + 失敗時の `record-not-found`
/// error と同じ recv 経路で受ける)。成功を無音にすると client が永久 hang する
/// (= 旧設計の bug)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordStopResponse {
    /// 実際に停止した record 数 (= single stop なら 1、`--all` なら N)。
    pub stopped: u32,
}

/// `record.stop.all.request` payload (DR-0016 §7、client → daemon、`--all` 用)。
///
/// 同 session の全 active record を一括停止する。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordStopAllRequest {}

/// `record.list.request` payload (DR-0016 §7、client → daemon)。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordListRequest {}

/// `record.list.response` payload (DR-0016 §7、daemon → client)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordListResponse {
    /// active record の一覧 (= 空ベクタで「録画なし」を表現)。
    pub records: Vec<RecordInfo>,
}

/// 1 active record の情報 (`record.list.response.records` 要素)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecordInfo {
    /// daemon 採番の session-scope monotonic 録画識別子。
    pub record_id: u32,
    /// 録画 direction。
    pub direction: RecordDirection,
    /// 録画 file format。
    pub format: RecordFormat,
    /// 出力 file path (= 絶対 path)。
    pub output_path: String,
    /// 録画開始時刻 (= unix ms、jsonl header `started_unix_ms` と同値)。
    pub started_unix_ms: u64,
    /// 録画を開始した client の `client_id` (= 認可境界の根拠)。
    pub started_by_client_id: u64,
    /// 録画開始からの生 bytes 累計 (= 子 PTY I/O 量、`max_bytes` 判定の正本)。
    pub raw_bytes_recorded: u64,
    /// 録画 file に書き込まれた bytes 数 (= jsonl overhead 含む実 file size 相当)。
    pub file_bytes_written: u64,
    /// 最終 flush 時刻 (= unix ms、observability 用)。
    pub last_flushed_unix_ms: u64,
}
