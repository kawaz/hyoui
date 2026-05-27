//! `error` payload (DR-0008 §2.3)。
//!
//! 回復可能 (= 当該 message を受理できなかったが connection は維持) と
//! 致命的 (= disconnect 必至) の両方に使う。区別は code prefix で表現する
//! 慣習 (例: `protocol.*` は致命、`lock.*` は回復可能 等)。
//!
//! `code` field は構造化された [`ErrorCode`] enum で扱う。wire 上は引き続き
//! dotted text string として encode され、未知 code は [`ErrorCode::Unknown`]
//! で保持される (前方互換)。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `error` payload。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ErrorMessage {
    /// error 種別を表す構造化 code。wire 上は dotted text string。
    pub code: ErrorCode,
    /// 人間可読の説明。
    pub message: String,
    /// 追加情報 (任意)。schema は code ごとに自由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<ciborium::Value>,
}

/// `error.code` の構造化 discriminator。
///
/// wire 互換のため Serialize / Deserialize は手書きで dotted text string と
/// 1:1 対応する。未知 code は [`ErrorCode::Unknown`] arm で受け、
/// round trip 可能にしておく (前方互換)。
///
/// 新 code を追加するときは:
/// 1. 新 variant を追加し
/// 2. [`ErrorCode::as_str`] / [`ErrorCode::from_str`] に枝を追加し
/// 3. DR-0008 §2.3 の error code 一覧にも追記する
///
/// `#[non_exhaustive]` のため、library user は必ず `_` arm を用意する。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// `protocol.malformed` — frame / CBOR が解釈不能。致命的、disconnect。
    ProtocolMalformed,
    /// `protocol.unexpected-kind` — 受信した kind がこの方向 (client↔daemon)
    /// で受理されない、または cap negotiation 後に不正。
    ProtocolUnexpectedKind,
    /// `unsupported-capability` — cap negotiation 後に未対応 feature が
    /// 呼ばれた。
    UnsupportedCapability,
    /// `handshake.timeout` — handshake が制限時間内に完了しなかった。
    HandshakeTimeout,
    /// `auth.token-mismatch` — handshake の auth_token が一致しない。
    AuthTokenMismatch,
    /// `backpressure.disconnect` — client の send queue が limit 超過、
    /// daemon が disconnect する。
    BackpressureDisconnect,
    /// `mode.not-allowed` — 現在の mode (interactive / readonly / 等)
    /// では当該操作が許可されていない。
    ModeNotAllowed,
    /// `mode.not-leader` — leader 専用操作 (resize 等) を non-leader が送った。
    ModeNotLeader,
    /// `lock.denied` — lock acquire が既存 holder により拒否された。
    LockDenied,
    /// `lock.not-held` — lock 操作 (release 等) を、当該 lock を持たない
    /// client が送った。
    LockNotHeld,
    /// `signal.invalid` — signal 番号 / 名前が不正。
    SignalInvalid,
    /// `detach.target-partial` — detach 対象指定の一部が見つからない / 失敗。
    DetachTargetPartial,
    /// `master.write-timeout` — client → master PTY への raw_data write が、
    /// 子の slow-reader (line discipline buffer 満杯) で idle timeout を超えた。
    /// daemon は当該 client を disconnect する (= 子に届かなかった bytes は
    /// drop)。R5-C3 で導入。
    MasterWriteTimeout,
    /// `internal.error` — daemon 側の予期しない内部 error (= 通常は OS resource
    /// 不足等)。client が retry すれば成功する余地がある回復可能 error として
    /// 通知し、daemon 自身は panic せず session を継続する。R5-H11 で導入。
    InternalError,
    /// 未知の error code。wire 上の dotted text を保持する。
    ///
    /// 前方互換のため、新しい daemon / client が将来追加した code を
    /// 旧 binary が受信したときに drop せず保持できる。`_` arm が必要な
    /// match では `ErrorCode::Unknown(s) if s == "..."` 形式で文字列照合も可能。
    Unknown(String),
}

impl ErrorCode {
    /// wire 上の dotted text 表現を返す。
    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::ProtocolMalformed => "protocol.malformed",
            ErrorCode::ProtocolUnexpectedKind => "protocol.unexpected-kind",
            ErrorCode::UnsupportedCapability => "unsupported-capability",
            ErrorCode::HandshakeTimeout => "handshake.timeout",
            ErrorCode::AuthTokenMismatch => "auth.token-mismatch",
            ErrorCode::BackpressureDisconnect => "backpressure.disconnect",
            ErrorCode::ModeNotAllowed => "mode.not-allowed",
            ErrorCode::ModeNotLeader => "mode.not-leader",
            ErrorCode::LockDenied => "lock.denied",
            ErrorCode::LockNotHeld => "lock.not-held",
            ErrorCode::SignalInvalid => "signal.invalid",
            ErrorCode::DetachTargetPartial => "detach.target-partial",
            ErrorCode::MasterWriteTimeout => "master.write-timeout",
            ErrorCode::InternalError => "internal.error",
            ErrorCode::Unknown(s) => s.as_str(),
        }
    }

    /// dotted text を [`ErrorCode`] に変換する。未知 code は
    /// [`ErrorCode::Unknown`] で wrap される (Err にはならない)。
    pub fn from_wire(s: &str) -> Self {
        match s {
            "protocol.malformed" => ErrorCode::ProtocolMalformed,
            "protocol.unexpected-kind" => ErrorCode::ProtocolUnexpectedKind,
            "unsupported-capability" => ErrorCode::UnsupportedCapability,
            "handshake.timeout" => ErrorCode::HandshakeTimeout,
            "auth.token-mismatch" => ErrorCode::AuthTokenMismatch,
            "backpressure.disconnect" => ErrorCode::BackpressureDisconnect,
            "mode.not-allowed" => ErrorCode::ModeNotAllowed,
            "mode.not-leader" => ErrorCode::ModeNotLeader,
            "lock.denied" => ErrorCode::LockDenied,
            "lock.not-held" => ErrorCode::LockNotHeld,
            "signal.invalid" => ErrorCode::SignalInvalid,
            "detach.target-partial" => ErrorCode::DetachTargetPartial,
            "master.write-timeout" => ErrorCode::MasterWriteTimeout,
            "internal.error" => ErrorCode::InternalError,
            other => ErrorCode::Unknown(other.to_string()),
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for ErrorCode {
    fn from(s: &str) -> Self {
        Self::from_wire(s)
    }
}

impl From<String> for ErrorCode {
    fn from(s: String) -> Self {
        // 既知 code に該当しなければ String をそのまま Unknown に持たせる
        // (= 確実に move して clone 回避)。
        match Self::from_wire(&s) {
            ErrorCode::Unknown(_) => ErrorCode::Unknown(s),
            known => known,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 所有権ある String を受け取り Unknown(String) で move 保持できるようにする。
        let raw = String::deserialize(deserializer)?;
        Ok(ErrorCode::from(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 既知 code が enum 経由で string と round trip する。
    #[test]
    fn error_code_round_trip_via_cbor() {
        let known: &[(ErrorCode, &str)] = &[
            (ErrorCode::ProtocolMalformed, "protocol.malformed"),
            (
                ErrorCode::ProtocolUnexpectedKind,
                "protocol.unexpected-kind",
            ),
            (ErrorCode::UnsupportedCapability, "unsupported-capability"),
            (ErrorCode::HandshakeTimeout, "handshake.timeout"),
            (ErrorCode::AuthTokenMismatch, "auth.token-mismatch"),
            (ErrorCode::BackpressureDisconnect, "backpressure.disconnect"),
            (ErrorCode::ModeNotAllowed, "mode.not-allowed"),
            (ErrorCode::ModeNotLeader, "mode.not-leader"),
            (ErrorCode::LockDenied, "lock.denied"),
            (ErrorCode::LockNotHeld, "lock.not-held"),
            (ErrorCode::SignalInvalid, "signal.invalid"),
            (ErrorCode::DetachTargetPartial, "detach.target-partial"),
            (ErrorCode::MasterWriteTimeout, "master.write-timeout"),
            (ErrorCode::InternalError, "internal.error"),
        ];

        for (variant, wire) in known {
            // as_str() が正しい dotted text を返す
            assert_eq!(variant.as_str(), *wire, "as_str for {variant:?}");

            // CBOR encode → decode で同じ variant
            let mut buf = Vec::new();
            ciborium::into_writer(variant, &mut buf).expect("encode");
            let decoded: ErrorCode = ciborium::from_reader(buf.as_slice()).expect("decode");
            assert_eq!(decoded, *variant, "round trip for {variant:?}");

            // wire string から戻しても等価
            assert_eq!(ErrorCode::from_wire(wire), *variant);
        }
    }

    /// 未知 string が Unknown で保持される (前方互換)。
    #[test]
    fn error_code_unknown_string_preserved() {
        let raw = "future.brand-new-code";

        // from_wire で Unknown variant に
        let code = ErrorCode::from_wire(raw);
        assert_eq!(code, ErrorCode::Unknown(raw.to_string()));
        assert_eq!(code.as_str(), raw);

        // CBOR round trip でも保持される
        let mut buf = Vec::new();
        ciborium::into_writer(&code, &mut buf).expect("encode");
        let decoded: ErrorCode = ciborium::from_reader(buf.as_slice()).expect("decode");
        assert_eq!(decoded, code);
        assert_eq!(decoded.as_str(), raw);
    }

    /// ErrorMessage 全体の CBOR round trip。
    #[test]
    fn error_message_round_trip_with_code_enum() {
        let msg = ErrorMessage {
            code: ErrorCode::LockDenied,
            message: "held by client 7".into(),
            details: None,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: ErrorMessage = ciborium::from_reader(buf.as_slice()).expect("decode");
        assert_eq!(decoded, msg);
    }
}
