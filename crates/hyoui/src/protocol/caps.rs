//! Capability flags (DR-0008 §3 schema evolution + §3.4 cap 命名規約)。
//!
//! handshake で交換した自分の caps と相手の caps の intersect が「有効
//! capability」になる。送信側は intersect に含まれない feature を呼ぼうと
//! しないこと。受信側は不足 cap で送られた message に対して `error` (kind=
//! `"unsupported-capability"`) を返す。

use std::collections::BTreeSet;

/// MVP (v0.1.0) で hyoui daemon / client が両方持つ capability の名前一覧。
///
/// DR-0007 / DR-0008 §8 に整合 (= MVP scope: data 中継 / lock / tail /
/// state-based wait)。state-based wait は cap を持たず、CLI 側で
/// `screen.snapshot.request` を polling する形に再実装された (DR-0006 §9 改訂)。
/// 将来 cap (leader CLI 等) は v0.2.0+ で追加。
///
/// 各 cap の意味:
/// - `"data"`: raw PTY data 中継 (= type=0x00 frame、子 stdout/stdin)
/// - `"lock"`: `lock.*` + `leader.notify` + `mode.change` messages
/// - `"tail-v1"`: `tail.*` messages (`-v1` は schema breaking change の余地)
/// - `"screen-dump-v1"`: `screen.dump.*` messages (DR-0013 §9、debug / 自動 test 用)
/// - `"state-snapshot-v1"`: `screen.snapshot.*` messages (DR-0013 §9、構造化 state)
/// - `"session-exit-v1"`: `session.exit.notify` (DR-0015 §2.1、子 PTY exit code 伝搬)
/// - `"child-state-v1"`: `session.child.stopped.notify` /
///   `session.child.resume.request` (DR-0015 §2.2、軸 1 follow / auto-resume)
/// - `"record-v1"`: `record.*` messages (DR-0016 §7、tty I/O timeline 録画。
///   **optional cap** — daemon は default で advertise、client は negotiate で
///   得たら使う。未 cap の client が他機能を使うのは無影響)
/// - `"set-v1"`: `set.request` / `set.ack` messages (DR-0019 Update、runtime 設定
///   変更。新 client → 旧 daemon は cap intersect でこの cap が落ちるため、CLI は
///   「daemon が set 未対応」と判定して明示エラーにする)
/// - `"upgrade-v1"`: `upgrade.request` / `upgrade.ack` messages (DR-0028 §2、
///   graceful self-exec upgrade)。旧 daemon (Phase 1/2 の SIGUSR1 隠し経路のみ) には
///   cap 無し → 新 client は「upgrade protocol 未対応」と判定して SIGUSR1 fallback
///   もしくは明示 error にする
pub const MVP_CAPS: &[&str] = &[
    "data",
    "lock",
    "tail-v1",
    "screen-dump-v1",
    "state-snapshot-v1",
    "session-exit-v1",
    "child-state-v1",
    "record-v1",
    "set-v1",
    "upgrade-v1",
];

/// 2 つの cap 集合の intersect を取って `Vec<String>` で返す。
///
/// 結果の順序は引数 `a` の出現順に合わせる (= debug 容易性)。重複は排除。
pub fn intersect_caps(a: &[String], b: &[String]) -> Vec<String> {
    let b_set: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for cap in a {
        if b_set.contains(cap.as_str()) && seen.insert(cap.as_str()) {
            out.push(cap.clone());
        }
    }
    out
}

/// caps に required が含まれていれば `Ok(())`、無ければ `Err(MissingCapability)`。
///
/// daemon / client がメッセージ送信前に「相手の cap」をチェックする用途。
pub fn require_cap(caps: &[String], required: &str) -> Result<(), MissingCapability> {
    if caps.iter().any(|c| c == required) {
        Ok(())
    } else {
        Err(MissingCapability {
            cap: required.to_string(),
        })
    }
}

/// 不足 capability を表す error。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("missing capability: {cap}")]
pub struct MissingCapability {
    /// 不足している cap 名。
    pub cap: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mvp_caps_contains_expected() {
        assert!(MVP_CAPS.contains(&"data"));
        assert!(MVP_CAPS.contains(&"lock"));
        assert!(MVP_CAPS.contains(&"tail-v1"));
        assert!(MVP_CAPS.contains(&"screen-dump-v1"));
        assert!(MVP_CAPS.contains(&"state-snapshot-v1"));
        assert!(MVP_CAPS.contains(&"session-exit-v1"));
        assert!(MVP_CAPS.contains(&"child-state-v1"));
        assert!(MVP_CAPS.contains(&"record-v1"));
        assert!(MVP_CAPS.contains(&"set-v1"));
        assert!(MVP_CAPS.contains(&"upgrade-v1"));
    }

    /// wait-l0 cap は DR-0006 §9 改訂 (state-based wait 移行) に伴い削除済。
    #[test]
    fn mvp_caps_does_not_contain_removed_wait_l0() {
        assert!(!MVP_CAPS.contains(&"wait-l0"));
    }

    #[test]
    fn intersect_preserves_order_of_first_arg() {
        let a = vec![
            "data".to_string(),
            "lock".to_string(),
            "tail-v1".to_string(),
        ];
        let b = vec!["tail-v1".to_string(), "data".to_string()];
        let got = intersect_caps(&a, &b);
        assert_eq!(got, vec!["data".to_string(), "tail-v1".to_string()]);
    }

    #[test]
    fn intersect_drops_unsupported() {
        let a = vec!["data".to_string(), "snapshot-v1".to_string()];
        let b = vec!["data".to_string()];
        let got = intersect_caps(&a, &b);
        assert_eq!(got, vec!["data".to_string()]);
    }

    #[test]
    fn intersect_dedups() {
        let a = vec!["data".to_string(), "data".to_string(), "lock".to_string()];
        let b = vec!["data".to_string(), "lock".to_string()];
        let got = intersect_caps(&a, &b);
        assert_eq!(got, vec!["data".to_string(), "lock".to_string()]);
    }

    #[test]
    fn intersect_empty_when_disjoint() {
        let a = vec!["data".to_string()];
        let b = vec!["snapshot-v1".to_string()];
        let got = intersect_caps(&a, &b);
        assert!(got.is_empty());
    }

    #[test]
    fn require_ok_for_present_cap() {
        let caps = vec!["data".to_string(), "lock".to_string()];
        assert!(require_cap(&caps, "data").is_ok());
        assert!(require_cap(&caps, "lock").is_ok());
    }

    #[test]
    fn require_err_for_missing_cap() {
        let caps = vec!["data".to_string()];
        let err = require_cap(&caps, "snapshot-v1").expect_err("must miss");
        assert_eq!(err.cap, "snapshot-v1");
    }

    #[test]
    fn intersect_of_mvp_with_itself_is_full_set() {
        let mvp: Vec<String> = MVP_CAPS.iter().map(|s| s.to_string()).collect();
        let got = intersect_caps(&mvp, &mvp);
        assert_eq!(got, mvp);
    }
}
