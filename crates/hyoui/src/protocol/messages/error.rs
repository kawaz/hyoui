//! `error` payload (DR-0008 §2.3)。
//!
//! 回復可能 (= 当該 message を受理できなかったが connection は維持) と
//! 致命的 (= disconnect 必至) の両方に使う。区別は code prefix で表現する
//! 慣習を実装フェーズで詰める (例: `protocol.*` は致命、`lock.*` は回復可能 等)。

use serde::{Deserialize, Serialize};

/// `error` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ErrorMessage {
    /// error 種別を表す dotted text (例: `"protocol.malformed"`, `"lock.denied"`)。
    pub code: String,
    /// 人間可読の説明。
    pub message: String,
    /// 追加情報 (任意)。schema は code ごとに自由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<ciborium::Value>,
}
