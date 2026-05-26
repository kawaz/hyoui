//! `detach` / `kill` payload (DR-0008 §2.3)。

use serde::{Deserialize, Serialize};

/// detach 対象 (DR-0006 §3-4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetachTarget {
    /// 自分のみ切断 (default)。
    #[serde(rename = "self")]
    Myself,
    /// 自分以外の全 client を daemon が切断、自分は残る。
    Others,
    /// 自分含む全 client を切断 (= daemon は子 PTY 接続維持、新規 attach 待ち)。
    All,
}

/// `detach` payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Detach {
    /// 切断対象。
    pub target: DetachTarget,
}

/// `kill` payload。daemon は子 PTY に signal を送って自身も exit する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Kill {
    /// 子に送る signal (= null なら SIGTERM)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signum: Option<u8>,
}
