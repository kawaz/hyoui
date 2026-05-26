//! `tail.request` / `tail.data` / `tail.end` payload (DR-0008 §2.3、basic)。
//!
//! detailed schema (= cursor, line vs byte モード等) は実装フェーズで詰める。

use serde::{Deserialize, Serialize};

/// `tail.request` payload (basic)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TailRequest {
    /// 過去 ms 以内の chunk を ring buffer から流す (= null なら ring buffer 全体)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// since 範囲が ring buffer から押し出されていたら error にする (= `--since-strict`)。
    pub since_strict: bool,
    /// true = follow (live stream 継続)、false = 現在の buffer を流して end。
    pub follow: bool,
    /// ANSI escape を strip して raw text にする。
    pub strip_ansi: bool,
    /// 末尾 N bytes に絞る (= `--last`)、null なら無制限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_bytes: Option<u64>,
}

/// `tail.data` payload (= daemon → client、tail stream の 1 chunk)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TailData {
    /// chunk bytes (CBOR byte string)。
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    /// chunk の生成時刻 (= Unix epoch from millis)、scrollback ring buffer から取得した値。
    pub timestamp_ms: i64,
}

/// `tail.end` の終了理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TailEndReason {
    /// follow=false で buffer flush 完了。
    Eof,
    /// since 範囲が ring buffer から押し出されていた (= `--since-strict`)。
    BufferTruncated,
    /// client が detach / 接続切断。
    ClientCancel,
    /// 子 PTY が exit した。
    ChildExited,
}

/// `tail.end` payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TailEnd {
    /// 終了理由。
    pub reason: TailEndReason,
}

/// `serde_bytes` の代替 (= ciborium 用に CBOR byte string で encode)。
///
/// `Vec<u8>` を serde の default で扱うと array<u8> (CBOR major 4) になるが、
/// terminal stream は byte string (CBOR major 2) として扱う方が自然。
mod serde_bytes {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    struct BytesVisitor;

    impl<'de> serde::de::Visitor<'de> for BytesVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("byte string or byte array")
        }

        fn visit_bytes<E>(self, v: &[u8]) -> Result<Vec<u8>, E> {
            Ok(v.to_vec())
        }

        fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
            Ok(v)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
            let mut out = Vec::new();
            while let Some(b) = seq.next_element::<u8>()? {
                out.push(b);
            }
            Ok(out)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        // CBOR byte string と array<u8> どちらも受理。
        d.deserialize_byte_buf(BytesVisitor)
    }
}
