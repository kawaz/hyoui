//! `resize` / `signal` payload (DR-0008 §2.3, DR-0012)。
//!
//! `resize` は leader が送る TIOCSWINSZ 相当。leader 以外が送ると daemon が
//! `error` (code=`mode.not-leader`) を返す。
//!
//! `signal` は raw mode 中など、line discipline 経由で signal が飛ばない
//! ケース用の明示送信路。
//!
//! DR-0012: wire 上の signal は **signal name string** ("SIGTERM", "SIGINT" 等)
//! を送る。POSIX が signal 数値を規定していないため、cross-OS で意味が一意に
//! 決まる name で表現し、daemon 側で OS native 値に解決する。

use serde::{Deserialize, Serialize};

/// `resize` payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Resize {
    /// 列数。
    pub cols: u16,
    /// 行数。
    pub rows: u16,
}

/// `signal` payload (DR-0012)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Signal {
    /// 送る signal 名 (= "SIGTERM" / "SIGINT" / "SIGKILL" 等、正規表記は SIG-prefix
    /// 大文字)。未知 name は daemon 側で `signal.invalid` で reject される。
    pub signal: String,
}
