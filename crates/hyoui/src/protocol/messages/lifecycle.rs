//! `detach` / `kill` payload (DR-0008 §2.3)。

use serde::{Deserialize, Serialize};

/// detach 対象 (DR-0006 §3-4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
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

/// `kill` payload (DR-0012)。daemon は子 PTY に signal を送って自身も exit する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Kill {
    /// 子に送る signal 名 (= null なら SIGTERM、正規表記は SIG-prefix 大文字)。
    /// 未知 name は daemon 側で `signal.invalid` で reject される。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}
