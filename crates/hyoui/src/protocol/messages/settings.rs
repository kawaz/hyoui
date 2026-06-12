//! `set.request` / `set.ack` payload (DR-0019 Update 2026-06-12)。
//!
//! `hyoui set <session> <key>=<value>` の runtime 設定変更用。汎用 key=value 形式で、
//! 将来の runtime 設定もこの 1 対の message に載せる (= key を増やすだけ)。
//!
//! cap flag は `set-v1`。新 client → 旧 daemon (= `set-v1` を advertise しない) は
//! handshake の cap intersect で `set-v1` が落ちるため、CLI 側で「daemon が set 未対応」
//! と判定してエラーにする (= 既存 `ensure_cap` 流儀、DR-0008 §3)。
//!
//! key / value の妥当性 (= 未知 key / 不正値) は daemon 側で検証し、不正なら
//! `error` (kind=`mode.not-allowed` / `signal.invalid` ではなく、設定不正用の
//! `set.invalid-key` / `set.invalid-value`) を返す。

use serde::{Deserialize, Serialize};

/// `set.request` payload (DR-0019 Update、cap `set-v1`)。
///
/// client → daemon。rw 接続なら誰でも送れる (= leader 限定にしない。Ro のみ拒否、
/// Rw / RwNoLeader は OK)。daemon は key を解釈して runtime state を更新し、
/// `set.ack` を返す。
///
/// **反映タイミング**: `set.ack` が「適用完了」の境界。daemon の serve loop は
/// child transition (= stop 観測) を client frame より先に処理する意図的な順序のため、
/// ack より前に daemon が観測済みの child stop は **旧 policy で処理され得る**
/// (= 自然な happened-before)。新 policy が効くのは ack 以降に観測される stop から。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SetRequest {
    /// 設定 key (= `on-child-suspend` 等)。未知 key は daemon が `set.invalid-key` で reject。
    pub key: String,
    /// 設定 value (= `notify` / `auto-resume` 等)。不正値は daemon が `set.invalid-value` で reject。
    pub value: String,
}

/// `set.ack` payload (DR-0019 Update、cap `set-v1`)。
///
/// daemon → client。`set.request` を受理して runtime state を更新した後に返す。
/// client は ack を受けて即 return する。payload には適用後の key/value を載せて、
/// client が「何が適用されたか」を表示できるようにする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SetAck {
    /// 適用された設定 key。
    pub key: String,
    /// 適用された設定 value (= daemon が normalize した後の正規値)。
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_request_roundtrip() {
        let msg = SetRequest {
            key: "on-child-suspend".into(),
            value: "auto-resume".into(),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SetRequest = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn set_ack_roundtrip() {
        let msg = SetAck {
            key: "on-child-suspend".into(),
            value: "notify".into(),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SetAck = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
    }
}
