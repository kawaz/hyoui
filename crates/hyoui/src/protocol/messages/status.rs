//! `status.query` / `status.response` payload (DR-0008 §2.3、basic schema)。
//!
//! detailed schema は実装フェーズで詰める。最小限 (= session 名、子 pid、
//! client 一覧、scrollback 情報、lock 状態) を最初に固める。

use serde::{Deserialize, Serialize};

use super::Mode;

/// `status.query` payload (引数なし)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct StatusQuery {}

/// 1 client の情報 (status.response の clients 配列要素)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ClientInfo {
    /// daemon が割り当てた client 番号。
    pub client_id: u64,
    /// 個別 mode。
    pub mode: Mode,
    /// leader かどうか。
    pub leader: bool,
}

/// `status.response` payload (basic)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StatusResponse {
    /// session 名。
    pub session_id: String,
    /// 子 PTY の PID (= null なら子が exit 済)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    /// 現在 attach 中の client 一覧。
    pub clients: Vec<ClientInfo>,
    /// scrollback ring buffer 内の総 byte 数。
    pub scrollback_bytes: u64,
    /// lock 保持者の client-id (= null なら未保持)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_holder: Option<u64>,
}
