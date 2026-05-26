//! Control message payload (= `Frame { ty = TYPE_CBOR_CONTROL }` の body)。
//!
//! 各 message は `#[serde(tag = "kind")]` enum で表現され、CBOR map の
//! 第一 field として `"kind": "<dotted.name>"` が必ず含まれる。kind に応じて
//! variant が dispatch される。
//!
//! kind 一覧は DR-0008 §2.2、payload schema は §2.3 を参照。
//!
//! ## Forward compatibility
//!
//! - 未知 **field** は ignore (serde の default 挙動)。送信側は CBOR map に
//!   新 field を後付け可能。
//! - 未知 **kind** は decode error になる (`ControlMessageError::UnknownKind`)。
//!   cap negotiation で「相手がその kind を話せる」ことを確認した上で送る。

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

mod handshake;

pub use handshake::{HandshakeRequest, HandshakeResponse, Mode};

/// CBOR control message の全 kind を包む tagged enum。
///
/// serde の `tag = "kind"` で CBOR map 上の `"kind"` field が variant
/// discriminator になる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ControlMessage {
    /// `kind = "handshake.request"`、client → daemon、cap negotiation + 認証。
    #[serde(rename = "handshake.request")]
    HandshakeRequest(HandshakeRequest),

    /// `kind = "handshake.response"`、daemon → client、cap 確定 + session 情報。
    #[serde(rename = "handshake.response")]
    HandshakeResponse(HandshakeResponse),
    // 残りの kind は順次追加 (error, resize, signal, lock.*, status.*, tail.*,
    // wait.*, detach, kill, leader.notify, mode.change ...)
}

/// CBOR control message の encode/decode error。
#[derive(Debug, thiserror::Error)]
pub enum ControlMessageError {
    /// CBOR encode 失敗 (ciborium が ser を拒否)。
    #[error("cbor encode: {0}")]
    Encode(ciborium::ser::Error<std::io::Error>),

    /// CBOR decode 失敗 (truncated / type mismatch / 未知 kind 等)。
    ///
    /// 未知 kind (`tag = "kind"` enum で variant が見つからない) はこのエラーで
    /// 表現される。serde の error 内容に kind 名が含まれる。
    #[error("cbor decode: {0}")]
    Decode(ciborium::de::Error<std::io::Error>),
}

impl ControlMessage {
    /// CBOR encode して `w` に書き込む。
    ///
    /// # Errors
    ///
    /// * [`ControlMessageError::Encode`] — CBOR serialization が失敗 (通常は I/O)。
    pub fn encode_to<W: Write>(&self, w: &mut W) -> Result<(), ControlMessageError> {
        ciborium::ser::into_writer(self, w).map_err(ControlMessageError::Encode)
    }

    /// `r` から 1 つの CBOR item を読んで control message として decode。
    ///
    /// # Errors
    ///
    /// * [`ControlMessageError::Decode`] — CBOR parse 失敗、未知 kind、型不一致。
    pub fn decode_from<R: Read>(r: R) -> Result<Self, ControlMessageError> {
        ciborium::de::from_reader(r).map_err(ControlMessageError::Decode)
    }

    /// CBOR encode して `Vec<u8>` を返す (frame の body にそのまま入れる用)。
    ///
    /// # Errors
    ///
    /// 上記 `encode_to` と同じ。
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, ControlMessageError> {
        let mut buf = Vec::new();
        self.encode_to(&mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: &ControlMessage) -> ControlMessage {
        let bytes = msg.encode_to_vec().expect("encode");
        ControlMessage::decode_from(bytes.as_slice()).expect("decode")
    }

    #[test]
    fn handshake_request_roundtrip_minimal() {
        let msg = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: vec!["data".to_string(), "lock".to_string()],
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn handshake_request_roundtrip_full() {
        let msg = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: vec!["data".into(), "lock".into(), "tail-v1".into()],
            mode: Mode::Ro,
            exclusive: true,
            detach_others: true,
            token: Some("tok-xyz".into()),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn handshake_response_roundtrip() {
        let msg = ControlMessage::HandshakeResponse(HandshakeResponse {
            caps: vec!["data".into(), "lock".into()],
            session_id: "demo".into(),
            client_id: 42,
            leader: true,
            mode: Mode::Rw,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn unknown_kind_is_decode_error() {
        // 未知 kind = "future.kind" の CBOR map を作って decode 試行
        // CBOR map (kind=text "future.kind") → "a1 64 6b 69 6e 64 6a 66 75 74 75 72 65 2e 6b 69 6e 64"
        // ↑ map(1 pair), text(4)="kind", text(10)="future.kind"
        let bytes: Vec<u8> = vec![
            0xa1, // map(1)
            0x64, b'k', b'i', b'n', b'd', // text(4) "kind"
            0x6a, b'f', b'u', b't', b'u', b'r', b'e', b'.', b'k', b'i', b'n',
            b'd', // text(10) "future.kind"
        ];
        let err = ControlMessage::decode_from(bytes.as_slice()).expect_err("must error");
        match err {
            ControlMessageError::Decode(_) => {} // OK: serde が unknown variant で error
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_field_is_ignored() {
        // handshake.request に "future-field" を後付け → 未知 field は ignore (= forward-compat)
        // CBOR map で以下を構築:
        //   kind="handshake.request"
        //   caps=["data"]
        //   mode="rw"
        //   exclusive=false
        //   detach-others=false
        //   token=null
        //   future-field=12345 (未知)
        // Construct using ciborium::Value to avoid hand-encoding.
        use ciborium::Value;
        let map = Value::Map(vec![
            (
                Value::Text("kind".into()),
                Value::Text("handshake.request".into()),
            ),
            (
                Value::Text("caps".into()),
                Value::Array(vec![Value::Text("data".into())]),
            ),
            (Value::Text("mode".into()), Value::Text("rw".into())),
            (Value::Text("exclusive".into()), Value::Bool(false)),
            (Value::Text("detach-others".into()), Value::Bool(false)),
            (Value::Text("token".into()), Value::Null),
            (
                Value::Text("future-field".into()),
                Value::Integer(12345.into()),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&map, &mut bytes).expect("encode value");

        let msg = ControlMessage::decode_from(bytes.as_slice()).expect("decode");
        let expected = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: vec!["data".into()],
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        assert_eq!(msg, expected);
    }
}
