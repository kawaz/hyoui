//! `lock.acquire` / `lock.response` / `lock.release` / `leader.notify` /
//! `mode.change` payload (DR-0008 §2.3)。

use serde::{Deserialize, Serialize};

use super::Mode;

/// `lock.acquire` payload (DR-0006 §6)。
///
/// 3 種類の timeout (絶対 / idle / process-bound) を組み合わせて指定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LockAcquire {
    /// true = queue 参加待機 (`--wait`)、false = fail mode (`--no-wait`)。
    pub wait: bool,
    /// 絶対 timeout (ms)、null なら無限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_abs_ms: Option<u64>,
    /// idle timeout (ms、子 PTY 出力が静かなら解放)、null なら無効。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_idle_ms: Option<u64>,
    /// process が消えたら自動解放。
    pub process_bound: bool,
}

/// `lock.response` の result discriminator。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockResult {
    /// lock を取得した (token が同梱)。
    Acquired,
    /// queue に入った (`queue_position` が同梱)、後で `lock.response` が再送される。
    Queued,
    /// 他者保持中で wait=false のため失敗。
    Denied,
    /// timeout 到達。
    Timeout,
}

/// `lock.response` payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LockResponse {
    /// 取得結果。
    pub result: LockResult,
    /// 取得成功時のみ存在する token (= HYOUI_LOCK_TOKEN env で渡す)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// queued 時の順位 (0 = 次に取れる)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
}

/// `lock.release` payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LockRelease {
    /// 保持していた token (daemon が照合)。
    pub token: String,
}

/// `leader.notify` payload (daemon → all clients broadcast)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LeaderNotify {
    /// 新 leader の client-id (= null なら leader 不在)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<u64>,
}

/// `mode.change` payload (daemon → all clients broadcast)。
///
/// 「session 全体の現在 mode」(rw 可能 / ro 強制 / lock 中) を通知する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionMode {
    /// rw clients が入力可能。
    Rw,
    /// 全 client が ro 強制 (= lock 持ちなしで誰も入力できない状態)。
    Ro,
    /// 誰かが lock 保持中 (= lock 持ち以外は ro 同等)。
    Locked,
}

/// `mode.change` payload (DR-0006 §6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ModeChange {
    /// 新しい session-wide mode。
    pub session_mode: SessionMode,
    /// lock 保持者の client-id (= session_mode == Locked のとき意味あり、それ以外は null)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_holder: Option<u64>,
    /// 各 client が見ている個別 mode (= rw/ro/rw-no-leader、handshake で確定)。
    /// 個別 mode の変更通知用 (任意 field、未設定なら個別 mode に変化なし)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_mode: Option<Mode>,
}
