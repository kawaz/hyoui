//! `resize` / `signal` payload (DR-0008 §2.3)。
//!
//! `resize` は leader が送る TIOCSWINSZ 相当。leader 以外が送ると daemon が
//! `error` (code=`mode.not-leader`) を返す。
//!
//! `signal` は raw mode 中など、line discipline 経由で signal が飛ばない
//! ケース用の明示送信路。

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

/// `signal` payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Signal {
    /// POSIX signal number (SIGINT=2, SIGTERM=15, etc.)。
    pub signum: u8,
}
