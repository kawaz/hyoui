//! `TYPE_RAW_ACK` frame body (DR-0021)。
//!
//! daemon が `TYPE_RAW_DATA` を受け取って master PTY への `write_all_with_idle_timeout`
//! を return した時点で client に返す ack payload。client は ack を受信してから
//! 次の raw_data を送ることで、複数 spec を連続送信したときの順序を保証する
//! (= socket flush では line discipline buffer の drain 完了が保証されず、後続 frame
//! が「先に」master fd に書き込まれる race が起きるため)。
//!
//! 明示 seq id は持たない (= connection-level の同期 / `client → daemon` の raw_data 1 個に
//! 対し `daemon → client` の ack 1 個が 1:1 で対応する)。

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// `raw_ack` の result discriminator。wire 上は `kebab-case` text string。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawAckResult {
    /// PTY drain 完了 (= 全 bytes が master fd に write_all 成功)。
    Ok,
    /// 部分書き込み / IdleTimeout / IoError 等の失敗。詳細は同 ack の
    /// `code` / `message` field を見る。daemon は同時に当該 client を disconnect する
    /// (= 後続 ack は来ない、client は当該 spec 失敗として CLI exit 1)。
    Error,
}

/// `TYPE_RAW_ACK` の body schema。
///
/// CBOR map で encode される。`result` / `code` / `message` のシンプルな構造で、
/// 拡張は forward-compatible field 追加で行う (= 未知 field は ignore、§DR-0008 §3.2)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RawAck {
    /// 成功 / 失敗の discriminator。
    pub result: RawAckResult,
    /// 失敗時のみ意味を持つ機械可読 error code (= `master.write-timeout` /
    /// `master.write-error` / `master.write-partial`)。`Ok` 時は `None` を送る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// 失敗時のみ意味を持つ人間可読 description。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl RawAck {
    /// 成功 ack (= 全 bytes drain 完了)。
    pub fn ok() -> Self {
        RawAck {
            result: RawAckResult::Ok,
            code: None,
            message: None,
        }
    }

    /// 失敗 ack を組み立てる。`code` は dotted text、`message` は人間可読 description。
    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        RawAck {
            result: RawAckResult::Error,
            code: Some(code.into()),
            message: Some(message.into()),
        }
    }

    /// CBOR encode して `w` に書き込む。
    ///
    /// # Errors
    ///
    /// * [`RawAckError::Encode`] — CBOR serialization が失敗。
    pub fn encode_to<W: Write>(&self, w: &mut W) -> Result<(), RawAckError> {
        ciborium::ser::into_writer(self, w).map_err(RawAckError::Encode)
    }

    /// `r` から 1 個の CBOR item を読んで `RawAck` として decode。
    ///
    /// 再帰深度は protocol crate の `MAX_CBOR_RECURSION_DEPTH` (= 16) で制限される
    /// (= 認証前 stack overflow DoS 対策と同じ理由、ack は daemon → client なので
    /// 攻撃面は小さいが対称性のため同じ制限を入れる)。
    ///
    /// # Errors
    ///
    /// * [`RawAckError::Decode`] — CBOR parse 失敗 / schema mismatch / 再帰深度超過。
    pub fn decode_from<R: Read>(r: R) -> Result<Self, RawAckError> {
        ciborium::de::from_reader_with_recursion_limit(r, super::MAX_CBOR_RECURSION_DEPTH)
            .map_err(RawAckError::Decode)
    }

    /// CBOR encode して `Vec<u8>` を返す (= frame body に詰める用)。
    ///
    /// # Errors
    ///
    /// `encode_to` と同じ。
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, RawAckError> {
        let mut buf = Vec::new();
        self.encode_to(&mut buf)?;
        Ok(buf)
    }
}

/// `RawAck` の encode / decode error。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RawAckError {
    /// CBOR encode 失敗。
    #[error("cbor encode: {0}")]
    Encode(ciborium::ser::Error<std::io::Error>),

    /// CBOR decode 失敗 (truncated / type mismatch 等)。
    #[error("cbor decode: {0}")]
    Decode(ciborium::de::Error<std::io::Error>),
}

// ---- Stable wire code constants ----

/// `code` = master PTY への write が forward progress を立てられず timeout 経過
/// (= `MASTER_WRITE_IDLE_TIMEOUT_MS`、子が SIGSTOP 中 / dead 等)。
pub const CODE_MASTER_WRITE_TIMEOUT: &str = "master.write-timeout";

/// `code` = master fd への write が EIO / EPIPE 等の I/O error で失敗。
pub const CODE_MASTER_WRITE_ERROR: &str = "master.write-error";

/// `code` = idle timeout 以外の理由で `write_all_with_idle_timeout` が partial write
/// で return した (= `outcome.is_complete() == false` だが `IdleTimeout` でも `Io` でもない
/// 想定外の組合せ、defense-in-depth)。
pub const CODE_MASTER_WRITE_PARTIAL: &str = "master.write-partial";

/// `code` = client が Ro mode のため bytes 系 input を daemon が reject (= master fd に
/// 書かれなかったことを ack で明示する)。
///
/// 旧実装は `Ok` を返して silent drop と整合させていたが、ack の意味論を「bytes が
/// 子の input stream に確実に到達した」に統一するため、reject 経路は Error ack に変更
/// (DR-0021 の改訂)。client は CLI exit 1 で abort できる。
pub const CODE_CLIENT_RO_REJECTED: &str = "client.ro-rejected";

/// `code` = lock holder と異なる client から bytes 系 input が送られ、daemon が reject
/// (= 同上)。`CODE_CLIENT_RO_REJECTED` と同じ理由で Error ack。
pub const CODE_CLIENT_LOCK_NOT_HELD: &str = "client.lock-not-held";

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(ack: &RawAck) -> RawAck {
        let bytes = ack.encode_to_vec().expect("encode");
        RawAck::decode_from(bytes.as_slice()).expect("decode")
    }

    #[test]
    fn ok_roundtrip() {
        let ack = RawAck::ok();
        assert_eq!(roundtrip(&ack), ack);
    }

    #[test]
    fn err_timeout_roundtrip() {
        let ack = RawAck::err(CODE_MASTER_WRITE_TIMEOUT, "no forward progress for 500 ms");
        assert_eq!(roundtrip(&ack), ack);
    }

    #[test]
    fn err_io_roundtrip() {
        let ack = RawAck::err(CODE_MASTER_WRITE_ERROR, "EPIPE: child closed PTY");
        assert_eq!(roundtrip(&ack), ack);
    }

    #[test]
    fn err_ro_rejected_roundtrip() {
        let ack = RawAck::err(
            CODE_CLIENT_RO_REJECTED,
            "client is read-only; input rejected by daemon",
        );
        assert_eq!(roundtrip(&ack), ack);
        assert_eq!(ack.code.as_deref(), Some("client.ro-rejected"));
    }

    #[test]
    fn err_lock_not_held_roundtrip() {
        let ack = RawAck::err(
            CODE_CLIENT_LOCK_NOT_HELD,
            "lock is held by another client; input rejected",
        );
        assert_eq!(roundtrip(&ack), ack);
        assert_eq!(ack.code.as_deref(), Some("client.lock-not-held"));
    }

    #[test]
    fn ok_wire_omits_optional_fields() {
        // `Ok` 時は code / message を skip_serializing_if で省略する (= debug 出力時
        // のノイズ削減 + wire size 最適化)。
        let ack = RawAck::ok();
        let bytes = ack.encode_to_vec().expect("encode");
        // CBOR map の field 数を確認するために decode し直して field 数を測る。
        let value: ciborium::Value =
            ciborium::de::from_reader(bytes.as_slice()).expect("decode value");
        if let ciborium::Value::Map(entries) = value {
            assert_eq!(entries.len(), 1, "Ok ack must have only `result` field");
        } else {
            panic!("RawAck wire must be CBOR map");
        }
    }

    #[test]
    fn err_wire_includes_code_and_message() {
        let ack = RawAck::err("x.y", "boom");
        let bytes = ack.encode_to_vec().expect("encode");
        let value: ciborium::Value =
            ciborium::de::from_reader(bytes.as_slice()).expect("decode value");
        if let ciborium::Value::Map(entries) = value {
            // result + code + message = 3 fields
            assert_eq!(entries.len(), 3);
        } else {
            panic!("RawAck wire must be CBOR map");
        }
    }
}
