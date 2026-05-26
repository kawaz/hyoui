//! `wait.request` / `wait.result` payload (DR-0008 §2.3、basic L0)。
//!
//! L0 = `text:` / `pattern:` / `idle:` (DR-0006 §5)。L1 (`--rect`/`--cursor`)
//! 以降は capability gate で後付け。

use serde::{Deserialize, Serialize};

/// 待ち条件 (DR-0006 §5 の predicate)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WaitPredicate {
    /// 部分文字列マッチ (`text:<string>`)。
    Text {
        /// マッチさせる substring。
        value: String,
    },
    /// regex マッチ (`pattern:<regex>`)。
    Pattern {
        /// regex (= ECMAScript 互換、実装フェーズで詰める)。
        regex: String,
    },
    /// 子 PTY 出力が `ms` 静かなら成立 (`wait-idle:<dur>` / `wait:<dur>`)。
    Idle {
        /// 静寂閾値 (ms)。
        ms: u64,
    },
}

/// 装飾除去 / 改行変換のオプション (DR-0006 §11)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct WaitMatchOptions {
    /// マッチ対象から ANSI escape を strip した text を見る (default = true)。
    pub strip_escapes: bool,
    /// CRLF → LF 正規化を施す。
    pub newline_convert_lf: bool,
}

/// `wait.request` payload (basic)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WaitRequest {
    /// 待ち条件。
    pub predicate: WaitPredicate,
    /// 絶対 timeout (ms)、null なら無限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// 適用するマッチ加工オプション。
    pub options: WaitMatchOptions,
}

/// wait の outcome。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WaitOutcome {
    /// 条件成立。
    Matched,
    /// timeout 到達。
    Timeout,
    /// client が cancel (detach / connection lost)。
    Cancelled,
    /// 子 PTY が exit した (条件成立せず)。
    ChildExited,
}

/// `wait.result` payload (basic)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WaitResult {
    /// 結果区分。
    pub outcome: WaitOutcome,
    /// マッチした位置 (= scrollback ring buffer 内の byte offset、`matched` のみ意味あり)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_offset: Option<u64>,
}
