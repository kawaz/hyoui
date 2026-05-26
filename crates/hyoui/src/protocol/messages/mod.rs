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

mod control;
mod error;
mod handshake;
mod lifecycle;
mod lock;
mod status;
mod tail;
mod wait;

pub use control::{Resize, Signal};
pub use error::{ErrorCode, ErrorMessage};
pub use handshake::{HandshakeRequest, HandshakeResponse, Mode};
pub use lifecycle::{Detach, DetachTarget, Kill};
pub use lock::{
    LeaderNotify, LockAcquire, LockRelease, LockResponse, LockResult, ModeChange, SessionMode,
};
pub use status::{ClientInfo, StatusQuery, StatusResponse};
pub use tail::{TailData, TailEnd, TailEndReason, TailRequest};
pub use wait::{WaitMatchOptions, WaitOutcome, WaitPredicate, WaitRequest, WaitResult};

/// CBOR control message の全 kind を包む tagged enum。
///
/// serde の `tag = "kind"` で CBOR map 上の `"kind"` field が variant
/// discriminator になる。各 variant の rename は DR-0008 §2.2 の kind 一覧に対応。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
#[non_exhaustive]
pub enum ControlMessage {
    /// `kind = "handshake.request"` — client → daemon、cap negotiation + 認証。
    #[serde(rename = "handshake.request")]
    HandshakeRequest(HandshakeRequest),

    /// `kind = "handshake.response"` — daemon → client、cap 確定 + session 情報。
    #[serde(rename = "handshake.response")]
    HandshakeResponse(HandshakeResponse),

    /// `kind = "error"` — 双方向、回復可能 / 致命的 error 通知。
    #[serde(rename = "error")]
    Error(ErrorMessage),

    /// `kind = "resize"` — leader → daemon、TIOCSWINSZ。
    #[serde(rename = "resize")]
    Resize(Resize),

    /// `kind = "signal"` — client → daemon → 子、明示 signal 送信。
    #[serde(rename = "signal")]
    Signal(Signal),

    /// `kind = "lock.acquire"` — client → daemon、lock 取得要求。
    #[serde(rename = "lock.acquire")]
    LockAcquire(LockAcquire),

    /// `kind = "lock.response"` — daemon → client、lock 取得結果。
    #[serde(rename = "lock.response")]
    LockResponse(LockResponse),

    /// `kind = "lock.release"` — client → daemon、lock 解放。
    #[serde(rename = "lock.release")]
    LockRelease(LockRelease),

    /// `kind = "leader.notify"` — daemon → all clients、leader 変更通知。
    #[serde(rename = "leader.notify")]
    LeaderNotify(LeaderNotify),

    /// `kind = "mode.change"` — daemon → all clients、rw/ro/locked 変更通知。
    #[serde(rename = "mode.change")]
    ModeChange(ModeChange),

    /// `kind = "status.query"` — client → daemon、session 状態問い合わせ。
    #[serde(rename = "status.query")]
    StatusQuery(StatusQuery),

    /// `kind = "status.response"` — daemon → client、status 返却。
    #[serde(rename = "status.response")]
    StatusResponse(StatusResponse),

    /// `kind = "tail.request"` — client → daemon、scrollback の流し読み開始。
    #[serde(rename = "tail.request")]
    TailRequest(TailRequest),

    /// `kind = "tail.data"` — daemon → client、tail chunk。
    #[serde(rename = "tail.data")]
    TailData(TailData),

    /// `kind = "tail.end"` — daemon → client、tail 終了通知。
    #[serde(rename = "tail.end")]
    TailEnd(TailEnd),

    /// `kind = "wait.request"` — client → daemon、出力条件待ち。
    #[serde(rename = "wait.request")]
    WaitRequest(WaitRequest),

    /// `kind = "wait.result"` — daemon → client、条件成立 / timeout / cancel。
    #[serde(rename = "wait.result")]
    WaitResult(WaitResult),

    /// `kind = "detach"` — client → daemon、自身 (or `others`/`all`) を detach。
    #[serde(rename = "detach")]
    Detach(Detach),

    /// `kind = "kill"` — client → daemon、子に signal → daemon exit。
    #[serde(rename = "kill")]
    Kill(Kill),
}

/// CBOR control message の encode/decode error。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    fn error_roundtrip() {
        let msg = ControlMessage::Error(ErrorMessage {
            code: ErrorCode::LockDenied,
            message: "held by client 7".into(),
            details: None,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn error_with_details_roundtrip() {
        let msg = ControlMessage::Error(ErrorMessage {
            code: ErrorCode::BackpressureDisconnect,
            message: "client buffer full".into(),
            details: Some(ciborium::Value::Map(vec![
                (
                    ciborium::Value::Text("queued_bytes".into()),
                    ciborium::Value::Integer(8_000_000i64.into()),
                ),
                (
                    ciborium::Value::Text("limit".into()),
                    ciborium::Value::Integer(8_388_608i64.into()),
                ),
            ])),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn resize_roundtrip() {
        let msg = ControlMessage::Resize(Resize {
            cols: 132,
            rows: 50,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn signal_roundtrip() {
        let msg = ControlMessage::Signal(Signal { signum: 2 }); // SIGINT
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn lock_acquire_full_roundtrip() {
        let msg = ControlMessage::LockAcquire(LockAcquire {
            wait: true,
            timeout_abs_ms: Some(30_000),
            timeout_idle_ms: Some(5_000),
            process_bound: true,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn lock_response_acquired_roundtrip() {
        let msg = ControlMessage::LockResponse(LockResponse {
            result: LockResult::Acquired,
            token: Some("tok-xyz".into()),
            queue_position: None,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn lock_response_queued_roundtrip() {
        let msg = ControlMessage::LockResponse(LockResponse {
            result: LockResult::Queued,
            token: None,
            queue_position: Some(2),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn lock_release_roundtrip() {
        let msg = ControlMessage::LockRelease(LockRelease {
            token: "tok-xyz".into(),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn leader_notify_roundtrip() {
        let msg = ControlMessage::LeaderNotify(LeaderNotify { client_id: Some(7) });
        assert_eq!(roundtrip(&msg), msg);

        let none = ControlMessage::LeaderNotify(LeaderNotify { client_id: None });
        assert_eq!(roundtrip(&none), none);
    }

    #[test]
    fn mode_change_locked_roundtrip() {
        let msg = ControlMessage::ModeChange(ModeChange {
            session_mode: SessionMode::Locked,
            lock_holder: Some(7),
            client_mode: Some(Mode::Ro),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn status_query_roundtrip() {
        let msg = ControlMessage::StatusQuery(StatusQuery {});
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn status_response_roundtrip() {
        let msg = ControlMessage::StatusResponse(StatusResponse {
            session_id: "demo".into(),
            child_pid: Some(12345),
            clients: vec![
                ClientInfo {
                    client_id: 0,
                    mode: Mode::Rw,
                    leader: true,
                },
                ClientInfo {
                    client_id: 1,
                    mode: Mode::Ro,
                    leader: false,
                },
            ],
            scrollback_bytes: 65536,
            lock_holder: None,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn tail_request_roundtrip() {
        let msg = ControlMessage::TailRequest(TailRequest {
            since_ms: Some(5_000),
            since_strict: true,
            follow: true,
            strip_ansi: false,
            last_bytes: None,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn tail_data_roundtrip_preserves_bytes() {
        // ANSI escape + binary bytes 透過の確認
        let raw: Vec<u8> = vec![
            0x1b, b'[', b'3', b'1', b'm', b'h', b'i', 0x00, 0xff, 0x1b, b'[', b'0', b'm',
        ];
        let msg = ControlMessage::TailData(TailData {
            bytes: raw.clone(),
            timestamp_ms: 1_700_000_000_000,
        });
        let got = roundtrip(&msg);
        assert_eq!(got, msg);
        if let ControlMessage::TailData(d) = got {
            assert_eq!(d.bytes, raw);
        }
    }

    #[test]
    fn tail_end_roundtrip() {
        for reason in [
            TailEndReason::Eof,
            TailEndReason::BufferTruncated,
            TailEndReason::ClientCancel,
            TailEndReason::ChildExited,
        ] {
            let msg = ControlMessage::TailEnd(TailEnd { reason });
            assert_eq!(roundtrip(&msg), msg);
        }
    }

    #[test]
    fn wait_request_text_roundtrip() {
        let msg = ControlMessage::WaitRequest(WaitRequest {
            predicate: WaitPredicate::Text {
                value: "ready>".into(),
            },
            timeout_ms: Some(10_000),
            options: WaitMatchOptions {
                strip_escapes: true,
                newline_convert_lf: false,
            },
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn wait_request_pattern_roundtrip() {
        let msg = ControlMessage::WaitRequest(WaitRequest {
            predicate: WaitPredicate::Pattern {
                regex: r"^\$\s+$".into(),
            },
            timeout_ms: None,
            options: WaitMatchOptions::default(),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn wait_request_idle_roundtrip() {
        let msg = ControlMessage::WaitRequest(WaitRequest {
            predicate: WaitPredicate::Idle { ms: 500 },
            timeout_ms: Some(30_000),
            options: WaitMatchOptions::default(),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn wait_match_options_default_matches_documented_values() {
        // R4-C6: `WaitMatchOptions::default()` の戻り値が doc コメント
        // (strip_escapes = true / newline_convert_lf = false) および CLI 既定値と
        // 一致することを保証する。`#[derive(Default)]` だと strip_escapes が false に
        // なるため、手動 impl Default で揃えている。
        let opts = WaitMatchOptions::default();
        assert!(
            opts.strip_escapes,
            "strip_escapes default must be true (matches doc and CLI default)"
        );
        assert!(
            !opts.newline_convert_lf,
            "newline_convert_lf default must be false (matches doc and CLI default)"
        );
    }

    #[test]
    fn wait_result_roundtrip() {
        let msg = ControlMessage::WaitResult(WaitResult {
            outcome: WaitOutcome::Matched,
            matched_offset: Some(42),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn detach_roundtrip() {
        for target in [
            DetachTarget::Myself,
            DetachTarget::Others,
            DetachTarget::All,
        ] {
            let msg = ControlMessage::Detach(Detach { target });
            assert_eq!(roundtrip(&msg), msg);
        }
    }

    #[test]
    fn kill_roundtrip() {
        let msg = ControlMessage::Kill(Kill {
            signum: Some(15), // SIGTERM
        });
        assert_eq!(roundtrip(&msg), msg);

        let default_kill = ControlMessage::Kill(Kill { signum: None });
        assert_eq!(roundtrip(&default_kill), default_kill);
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

    // ──────────────────────────────────────────────────────────────────────
    // R4-H8: token を含む payload の Debug 出力が raw token を含まないこと。
    //
    // ログ・panic message 等で secret token が漏洩しないよう、`Debug` は
    // 手書き impl で token を `<redacted>` に置換する。本テスト群は raw token
    // 文字列が Debug 出力に含まれないことを保証する regression test。
    // ──────────────────────────────────────────────────────────────────────

    const SECRET_TOKEN: &str = "super-secret-token-value-do-not-leak";

    #[test]
    fn handshake_request_debug_redacts_token() {
        let req = HandshakeRequest {
            caps: vec!["data".into()],
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: Some(SECRET_TOKEN.into()),
        };
        let s = format!("{req:?}");
        assert!(
            !s.contains(SECRET_TOKEN),
            "raw token must not appear in Debug output: {s}"
        );
        assert!(
            s.contains("<redacted>"),
            "Debug output must contain <redacted> marker: {s}"
        );
    }

    #[test]
    fn handshake_request_debug_none_token() {
        let req = HandshakeRequest {
            caps: vec![],
            mode: Mode::Ro,
            exclusive: false,
            detach_others: false,
            token: None,
        };
        let s = format!("{req:?}");
        // None の場合は "<redacted>" を出さない (情報量を維持)
        assert!(s.contains("None"), "None token must render as None: {s}");
        assert!(
            !s.contains("<redacted>"),
            "None token must not be wrapped as redacted: {s}"
        );
    }

    #[test]
    fn lock_response_debug_redacts_token() {
        let resp = LockResponse {
            result: LockResult::Acquired,
            token: Some(SECRET_TOKEN.into()),
            queue_position: None,
        };
        let s = format!("{resp:?}");
        assert!(
            !s.contains(SECRET_TOKEN),
            "raw token must not appear in Debug output: {s}"
        );
        assert!(
            s.contains("<redacted>"),
            "Debug output must contain <redacted> marker: {s}"
        );
    }

    #[test]
    fn lock_release_debug_redacts_token() {
        let rel = LockRelease {
            token: SECRET_TOKEN.into(),
        };
        let s = format!("{rel:?}");
        assert!(
            !s.contains(SECRET_TOKEN),
            "raw token must not appear in Debug output: {s}"
        );
        assert!(
            s.contains("<redacted>"),
            "Debug output must contain <redacted> marker: {s}"
        );
    }

    #[test]
    fn control_message_debug_redacts_token_via_variant() {
        // ControlMessage は #[derive(Debug)] のままだが、内部 struct が
        // 手書き Debug を持つので variant 経由でも redact されることを確認。
        let msg = ControlMessage::LockResponse(LockResponse {
            result: LockResult::Acquired,
            token: Some(SECRET_TOKEN.into()),
            queue_position: None,
        });
        let s = format!("{msg:?}");
        assert!(
            !s.contains(SECRET_TOKEN),
            "raw token must not leak through ControlMessage Debug: {s}"
        );
        assert!(
            s.contains("<redacted>"),
            "ControlMessage Debug must show redacted marker: {s}"
        );
    }
}
