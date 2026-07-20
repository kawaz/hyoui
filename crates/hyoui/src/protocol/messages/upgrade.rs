//! `upgrade.request` / `upgrade.ack` payload (DR-0028 §2、cap `upgrade-v1`)。
//!
//! self-exec 経由の graceful upgrade を **protocol message** として形式化する
//! (Phase 3、DR-0025 message 駆動原則との整合)。Phase 1/2 で隠し `SIGUSR1` 経路
//! として実装した trigger の正規化。
//!
//! # フロー
//!
//! ```text
//! client                 daemon
//!   │  upgrade.request     │
//!   ├─────────────────────>│   (1) cap check + binary_path 検証
//!   │                      │       (存在 / 実行 bit / 同一 UID)
//!   │  upgrade.ack         │
//!   │<─────────────────────┤   (2) 受理応答
//!   │  (subsequent          │
//!   │   raw_data rejected  │   (3) upgrade_pending flag set、raw_data は
//!   │   with error)        │       error(kind=upgrade.rejected) で reject
//!   │                      │
//!   │  socket EOF          │   (4) drain 完了 → serve_loop UpgradeRequested
//!   │<─────────────────────┤       → self-exec (fd 継承 + state file + execve)
//!   │  (attach client       │
//!   │   reconnect w/ retry)│   (5) 新プロセスが同 socket で accept 再開
//! ```
//!
//! # binary_path
//!
//! `Option<String>`。省略時は daemon 自身の `current_exe()` (DR-0028 §2)。
//! 明示指定時は daemon 側の [`crate::daemon::upgrade::precheck_upgrade_target`]
//! で事前検証してから execve に渡す。**テスト用 `HYOUI_UPGRADE_EXE_OVERRIDE`
//! env は本 field を経由する経路とは独立** (= daemon 内部 test の便宜、Phase 3 の
//! 正規経路は `binary_path` field を使う)。
//!
//! # ack の意味
//!
//! `upgrade.ack` は「daemon が upgrade.request を受理し、以降 raw_data を reject
//! して drain を待つ状態に入った」という **意思表示**。ack 送信後 daemon は
//! serve_loop の drain 完了を待って execve に飛ぶ (= 実際の exec 完了は
//! client からは socket EOF で観測する)。

use serde::{Deserialize, Serialize};

/// `upgrade.request` payload (DR-0028 §2、cap `upgrade-v1`)。
///
/// client → daemon。daemon は cap `upgrade-v1` を advertise していれば処理する。
/// `binary_path` が指定されていれば precheck target をそちらに切り替える。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UpgradeRequest {
    /// 新バイナリの絶対パス (省略時は daemon 自身の `current_exe()`)。
    ///
    /// 明示指定時は daemon 側で pre-check (存在 / 実行 bit / 同一 UID) を実施。
    /// 失敗時は `upgrade.ack` を返さず、`error` kind=`upgrade.precheck-failed`
    /// で reject する (= 旧 daemon はそのまま継続、DR-0028 §5.1)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
}

/// `upgrade.ack` payload (DR-0028 §2、cap `upgrade-v1`)。
///
/// daemon → client、`upgrade.request` の受理応答。ack 送信後 daemon は raw_data を
/// reject して drain を待ち、drain 完了で execve に飛ぶ。client は socket EOF を
/// 観測してから backoff で再接続する。
///
/// payload は現時点で空 (= 「受理した」ことだけを伝える)。将来 exec 実施の予定
/// 時刻や drain wait budget など補足情報を載せたくなったら field を追加する。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UpgradeAck {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_request_default_omits_binary_path() {
        let msg = UpgradeRequest::default();
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: UpgradeRequest = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
        assert_eq!(decoded.binary_path, None);
    }

    #[test]
    fn upgrade_request_with_binary_path_roundtrips() {
        let msg = UpgradeRequest {
            binary_path: Some("/opt/hyoui/bin/hyoui".to_string()),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: UpgradeRequest = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
        assert_eq!(decoded.binary_path.as_deref(), Some("/opt/hyoui/bin/hyoui"));
    }

    #[test]
    fn upgrade_ack_roundtrips_empty() {
        let msg = UpgradeAck::default();
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: UpgradeAck = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
    }
}
