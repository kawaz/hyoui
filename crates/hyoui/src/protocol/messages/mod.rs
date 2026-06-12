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

/// CBOR decode 時の最大再帰深度 (R5-AUD-C1)。
///
/// ciborium の default は 256 で、`panic = "abort"` 設定 + 16 MiB frame 上限と
/// 組み合わさると、認証前 (= handshake token 検証より前) の `decode_from` 経路で
/// nested CBOR (array of array …) を投げられて daemon worker thread の stack を
/// 枯渇させ、process 全体を abort できる DoS が成立する。
///
/// 制御メッセージの schema (DR-0008 §2.3) はすべて shallow で、最深部の
/// `ErrorMessage::details` (`Option<ciborium::Value>`) を含めても実運用で 8 段
/// 必要になることはない。16 段の余裕を持たせて拒否ライン (= 16 を超える深さの
/// CBOR は [`ControlMessageError::Decode`] で reject) とする。
///
/// この値を上げる場合は stack overflow リスクの再評価が必要 (R5-AUD-C1 を参照)。
pub const MAX_CBOR_RECURSION_DEPTH: usize = 16;

mod control;
mod error;
mod handshake;
mod lifecycle;
mod lock;
mod record;
mod screen;
mod session_lifecycle;
mod settings;
mod status;
mod tail;

pub use control::{Resize, Signal};
pub use error::{ErrorCode, ErrorMessage};
pub use handshake::{
    HandshakeRequest, HandshakeResponse, MAX_CAP_LEN, MAX_CAPS_COUNT, MAX_TOKEN_LEN, Mode,
};
pub use lifecycle::{Detach, DetachAck, DetachTarget, Kill, KillAck};
pub use lock::{
    LeaderNotify, LockAcquire, LockRelease, LockResponse, LockResult, ModeChange, SessionMode,
};
pub use record::{
    InputSecrecy, RecordDirection, RecordFormat, RecordInfo, RecordListRequest, RecordListResponse,
    RecordStartRequest, RecordStartResponse, RecordStopAllRequest, RecordStopRequest,
    RecordStopResponse,
};
pub use screen::{
    BufferKind as ScreenBufferKind, CursorSnap as ScreenCursorSnap, DumpRect,
    ModeSnap as ScreenModeSnap, ScreenDumpFormat, ScreenDumpLayer, ScreenDumpRequest,
    ScreenDumpResponse, SnapshotComponent, StateSnapshotRequest, StateSnapshotResponse,
    WindowSize as ScreenWindowSize,
};
pub use session_lifecycle::{
    SessionChildResumeRequest, SessionChildStoppedNotify, SessionExitNotify,
};
pub use settings::{SetAck, SetRequest};
pub use status::{ChildLiveState, ClientInfo, OnChildSuspendPolicy, StatusQuery, StatusResponse};
pub use tail::{TailData, TailEnd, TailEndReason, TailRequest};

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

    /// `kind = "detach"` — client → daemon、自身 (or `others`/`all`) を detach。
    #[serde(rename = "detach")]
    Detach(Detach),

    /// `kind = "detach.ack"` — daemon → client、`Detach{Others/All}` の受理応答
    /// (= drop 対象数を返す、DR-0020 §4)。`Detach{Myself}` には返さない。
    #[serde(rename = "detach.ack")]
    DetachAck(DetachAck),

    /// `kind = "kill"` — client → daemon、子に signal → daemon exit。
    #[serde(rename = "kill")]
    Kill(Kill),

    /// `kind = "kill.ack"` — daemon → client、`Kill { wait: false }` の即時応答。
    /// signal 送信受理を ack して client を即 return させる (= terminate は daemon
    /// 内で非同期進行)。
    #[serde(rename = "kill.ack")]
    KillAck(KillAck),

    /// `kind = "screen.dump.request"` — client → daemon、画面 bytes を取得
    /// (DR-0013 §9、cap `screen-dump-v1` 要)。
    #[serde(rename = "screen.dump.request")]
    ScreenDumpRequest(ScreenDumpRequest),

    /// `kind = "screen.dump.response"` — daemon → client、画面 bytes 返却。
    #[serde(rename = "screen.dump.response")]
    ScreenDumpResponse(ScreenDumpResponse),

    /// `kind = "screen.snapshot.request"` — client → daemon、構造化 state 取得
    /// (DR-0013 §9、cap `state-snapshot-v1` 要)。
    #[serde(rename = "screen.snapshot.request")]
    StateSnapshotRequest(StateSnapshotRequest),

    /// `kind = "screen.snapshot.response"` — daemon → client、構造化 state 返却。
    #[serde(rename = "screen.snapshot.response")]
    StateSnapshotResponse(StateSnapshotResponse),

    /// `kind = "session.exit.notify"` — daemon → all clients、子 PTY exit 通知
    /// (DR-0015 §2.1、cap `session-exit-v1` 要)。
    #[serde(rename = "session.exit.notify")]
    SessionExitNotify(SessionExitNotify),

    /// `kind = "session.child.stopped.notify"` — daemon → leader、子 self-stop 通知
    /// (DR-0015 §2.2、cap `child-state-v1` 要)。leader が follow / auto-resume policy
    /// を発動する。
    #[serde(rename = "session.child.stopped.notify")]
    SessionChildStoppedNotify(SessionChildStoppedNotify),

    /// `kind = "session.child.resume.request"` — leader → daemon、子 SIGCONT 要求
    /// (DR-0015 §2.2、cap `child-state-v1` 要)。
    #[serde(rename = "session.child.resume.request")]
    SessionChildResumeRequest(SessionChildResumeRequest),

    /// `kind = "record.start.request"` — client → daemon、tty I/O 録画開始
    /// (DR-0016 §7、cap `record-v1` 要)。
    #[serde(rename = "record.start.request")]
    RecordStartRequest(RecordStartRequest),

    /// `kind = "record.start.response"` — daemon → client、`record_id` 採番返却
    /// (DR-0016 §7、cap `record-v1` 要)。
    #[serde(rename = "record.start.response")]
    RecordStartResponse(RecordStartResponse),

    /// `kind = "record.stop.request"` — client → daemon、特定 `record_id` 停止
    /// (DR-0016 §7、cap `record-v1` 要)。
    #[serde(rename = "record.stop.request")]
    RecordStopRequest(RecordStopRequest),

    /// `kind = "record.stop.all.request"` — client → daemon、同 session の全 record
    /// 一括停止 (DR-0016 §7、cap `record-v1` 要)。
    #[serde(rename = "record.stop.all.request")]
    RecordStopAllRequest(RecordStopAllRequest),

    /// `kind = "record.stop.response"` — daemon → client、停止数 ACK
    /// (= `record.stop.request` / `record.stop.all.request` 双方の成功応答、
    /// DR-0016 §7、cap `record-v1` 要)。成功も応答することで client の hang を防ぐ。
    #[serde(rename = "record.stop.response")]
    RecordStopResponse(RecordStopResponse),

    /// `kind = "record.list.request"` — client → daemon、active record 一覧要求
    /// (DR-0016 §7、cap `record-v1` 要)。
    #[serde(rename = "record.list.request")]
    RecordListRequest(RecordListRequest),

    /// `kind = "record.list.response"` — daemon → client、active record 一覧
    /// (DR-0016 §7、cap `record-v1` 要)。
    #[serde(rename = "record.list.response")]
    RecordListResponse(RecordListResponse),

    /// `kind = "set.request"` — client → daemon、runtime 設定変更要求
    /// (DR-0019 Update、cap `set-v1` 要)。
    #[serde(rename = "set.request")]
    SetRequest(SetRequest),

    /// `kind = "set.ack"` — daemon → client、設定変更の受理 ack
    /// (DR-0019 Update、cap `set-v1` 要)。
    #[serde(rename = "set.ack")]
    SetAck(SetAck),
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
    /// 再帰深度は [`MAX_CBOR_RECURSION_DEPTH`] で制限されており、これを超える
    /// nested CBOR は [`ControlMessageError::Decode`]
    /// (= `ciborium::de::Error::RecursionLimitExceeded`) として拒否される
    /// (R5-AUD-C1: 認証前 stack overflow DoS 対策)。
    ///
    /// # Errors
    ///
    /// * [`ControlMessageError::Decode`] — CBOR parse 失敗、未知 kind、型不一致、
    ///   または再帰深度が [`MAX_CBOR_RECURSION_DEPTH`] を超過。
    pub fn decode_from<R: Read>(r: R) -> Result<Self, ControlMessageError> {
        ciborium::de::from_reader_with_recursion_limit(r, MAX_CBOR_RECURSION_DEPTH)
            .map_err(ControlMessageError::Decode)
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
        // DR-0012: wire は signal name string
        let msg = ControlMessage::Signal(Signal {
            signal: "SIGINT".into(),
        });
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
        // cwd / argv は required field (= v1.0 breaking OK 方針)。concrete value で roundtrip 確認。
        let msg = ControlMessage::StatusResponse(StatusResponse {
            session_id: "demo".into(),
            child_pid: Some(12345),
            child_pgid: Some(12345),
            daemon_pid: 999,
            child_stopped: false,
            child_state: ChildLiveState::Running,
            clients: vec![
                ClientInfo {
                    client_id: 0,
                    mode: Mode::Rw,
                    leader: true,
                    connected_at_unix_ms: 0,
                },
                ClientInfo {
                    client_id: 1,
                    mode: Mode::Ro,
                    leader: false,
                    connected_at_unix_ms: 0,
                },
            ],
            scrollback_bytes: 65536,
            lock_holder: None,
            cwd: "/Users/kawaz/work".into(),
            argv: vec!["bash".into(), "-l".into()],
            on_child_suspend: Some(OnChildSuspendPolicy::AutoResume),
            daemon_version: "0.6.4".into(),
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
        // DR-0012: wire は signal name string
        let msg = ControlMessage::Kill(Kill {
            signal: Some("SIGTERM".into()),
            wait: false,
        });
        assert_eq!(roundtrip(&msg), msg);

        let default_kill = ControlMessage::Kill(Kill {
            signal: None,
            wait: false,
        });
        assert_eq!(roundtrip(&default_kill), default_kill);

        // --wait 指定 (= 従来挙動、EOF まで待つ)
        let wait_kill = ControlMessage::Kill(Kill {
            signal: Some("SIGKILL".into()),
            wait: true,
        });
        assert_eq!(roundtrip(&wait_kill), wait_kill);
    }

    /// 即時応答 kill の ack frame (= daemon → client) が roundtrip する。
    #[test]
    fn kill_ack_roundtrip() {
        let msg = ControlMessage::KillAck(KillAck {
            signal: "SIGTERM".into(),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    /// `detach.ack` (= daemon → client、DR-0020 §4 / Fable Minor3) が roundtrip する。
    #[test]
    fn detach_ack_roundtrip() {
        for count in [0u64, 1, 3] {
            let msg = ControlMessage::DetachAck(DetachAck {
                dropped_count: count,
            });
            assert_eq!(roundtrip(&msg), msg);
        }
    }

    /// 旧 client (= `wait` field 無し) が送る Kill CBOR を新 daemon が decode
    /// すると `wait = false` (= `#[serde(default)]`) になる (= 互換)。
    #[test]
    fn kill_decodes_legacy_without_wait_field_as_false() {
        // `wait` field を持たない旧 Kill 表現を別 struct で再現して CBOR 化。
        #[derive(serde::Serialize)]
        #[serde(rename_all = "kebab-case")]
        struct LegacyKill {
            kind: &'static str,
            signal: String,
        }
        let mut buf = Vec::new();
        ciborium::ser::into_writer(
            &LegacyKill {
                kind: "kill",
                signal: "SIGTERM".into(),
            },
            &mut buf,
        )
        .expect("encode legacy");
        let decoded = ControlMessage::decode_from(buf.as_slice()).expect("decode legacy kill");
        assert_eq!(
            decoded,
            ControlMessage::Kill(Kill {
                signal: Some("SIGTERM".into()),
                wait: false,
            })
        );
    }

    #[test]
    fn session_exit_notify_roundtrip() {
        // DR-0015 §2.1: 通常 exit と signal 死亡の両方
        let normal = ControlMessage::SessionExitNotify(SessionExitNotify {
            exit_status: 0,
            signal: None,
        });
        assert_eq!(roundtrip(&normal), normal);

        let signaled = ControlMessage::SessionExitNotify(SessionExitNotify {
            exit_status: 143, // 128 + SIGTERM
            signal: Some("SIGTERM".into()),
        });
        assert_eq!(roundtrip(&signaled), signaled);
    }

    #[test]
    fn session_child_stopped_notify_roundtrip() {
        // DR-0015 §2.2: signal None / Some 両方
        let with_signal = ControlMessage::SessionChildStoppedNotify(SessionChildStoppedNotify {
            pid: 12345,
            signal: Some("SIGTSTP".into()),
        });
        assert_eq!(roundtrip(&with_signal), with_signal);

        let no_signal = ControlMessage::SessionChildStoppedNotify(SessionChildStoppedNotify {
            pid: 42,
            signal: None,
        });
        assert_eq!(roundtrip(&no_signal), no_signal);
    }

    #[test]
    fn session_child_resume_request_roundtrip() {
        // DR-0015 §2.2: payload 空
        let msg = ControlMessage::SessionChildResumeRequest(SessionChildResumeRequest::default());
        assert_eq!(roundtrip(&msg), msg);
    }

    // ──────────────────────────────────────────────────────────────────────
    // DR-0016 §7: record.* protocol foundation (= 6 message + cap `record-v1`)
    // 未 cap で送ると daemon は `error` kind=`unsupported-capability` を返す前提。
    // ここでは wire 互換 (CBOR roundtrip + kind dispatch) のみ検証する。
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn record_start_request_roundtrip_minimal() {
        // 最小: option field 全部 None、direction=Both / format=Jsonl / 既定 redaction。
        let msg = ControlMessage::RecordStartRequest(RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: "/tmp/rec.jsonl".into(),
            max_bytes: None,
            max_duration_ms: None,
            input_secrecy: InputSecrecy::RedactAfterPrompt,
            prompt_pattern: None,
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_start_request_roundtrip_full() {
        // 全 option 指定 + raw format + 単一 direction。
        let msg = ControlMessage::RecordStartRequest(RecordStartRequest {
            direction: RecordDirection::Stdout,
            format: RecordFormat::Raw,
            output_path: "/var/tmp/dump.bin".into(),
            max_bytes: Some(104_857_600),
            max_duration_ms: Some(3_600_000),
            input_secrecy: InputSecrecy::NeverRecordStdin,
            prompt_pattern: Some("(?i)mfa code".into()),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_start_response_roundtrip() {
        let msg = ControlMessage::RecordStartResponse(RecordStartResponse { record_id: 7 });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_stop_request_roundtrip() {
        let msg = ControlMessage::RecordStopRequest(RecordStopRequest { record_id: 42 });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_stop_all_request_roundtrip() {
        // empty payload (= `--all` 用、`RecordStopAllRequest {}`)
        let msg = ControlMessage::RecordStopAllRequest(RecordStopAllRequest::default());
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_stop_response_roundtrip() {
        // stop / stop.all 双方の成功 ACK (= 成功を無音にすると client hang)
        let msg = ControlMessage::RecordStopResponse(RecordStopResponse { stopped: 3 });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_list_request_roundtrip() {
        let msg = ControlMessage::RecordListRequest(RecordListRequest::default());
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_list_response_roundtrip_empty() {
        // active record なし
        let msg = ControlMessage::RecordListResponse(RecordListResponse { records: vec![] });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn record_list_response_roundtrip_with_records() {
        // RecordInfo の全 field roundtrip (= 各種 direction / format 含む)。
        let msg = ControlMessage::RecordListResponse(RecordListResponse {
            records: vec![
                RecordInfo {
                    record_id: 1,
                    direction: RecordDirection::Both,
                    format: RecordFormat::Jsonl,
                    output_path: "/tmp/a.jsonl".into(),
                    started_unix_ms: 1_700_000_000_000,
                    started_by_client_id: 3,
                    raw_bytes_recorded: 12_345,
                    file_bytes_written: 24_690,
                    last_flushed_unix_ms: 1_700_000_001_000,
                },
                RecordInfo {
                    record_id: 2,
                    direction: RecordDirection::Stdin,
                    format: RecordFormat::Raw,
                    output_path: "/tmp/b.raw".into(),
                    started_unix_ms: 1_700_000_002_000,
                    started_by_client_id: 4,
                    raw_bytes_recorded: 0,
                    file_bytes_written: 0,
                    last_flushed_unix_ms: 0,
                },
            ],
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn input_secrecy_all_variants_roundtrip() {
        // wire 値 (kebab-case) を経由した 3 variant の roundtrip 確認。
        for secrecy in [
            InputSecrecy::RedactAfterPrompt,
            InputSecrecy::RecordAll,
            InputSecrecy::NeverRecordStdin,
        ] {
            let msg = ControlMessage::RecordStartRequest(RecordStartRequest {
                direction: RecordDirection::Stdin,
                format: RecordFormat::Jsonl,
                output_path: "/tmp/x.jsonl".into(),
                max_bytes: None,
                max_duration_ms: None,
                input_secrecy: secrecy,
                prompt_pattern: None,
            });
            assert_eq!(roundtrip(&msg), msg);
        }
    }

    #[test]
    fn record_direction_all_variants_roundtrip() {
        for dir in [
            RecordDirection::Stdin,
            RecordDirection::Stdout,
            RecordDirection::Both,
        ] {
            let info = RecordInfo {
                record_id: 9,
                direction: dir,
                format: RecordFormat::Jsonl,
                output_path: "/tmp/d.jsonl".into(),
                started_unix_ms: 1,
                started_by_client_id: 1,
                raw_bytes_recorded: 0,
                file_bytes_written: 0,
                last_flushed_unix_ms: 0,
            };
            let msg = ControlMessage::RecordListResponse(RecordListResponse {
                records: vec![info],
            });
            assert_eq!(roundtrip(&msg), msg);
        }
    }

    #[test]
    fn record_start_request_kind_wire_format() {
        // wire 上の `kind` discriminator が dotted naming `record.start.request` で
        // encode されていることを確認 (= 既存 `screen.dump.*` と同パターン)。
        let msg = ControlMessage::RecordStartRequest(RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: "/tmp/x.jsonl".into(),
            max_bytes: None,
            max_duration_ms: None,
            input_secrecy: InputSecrecy::RedactAfterPrompt,
            prompt_pattern: None,
        });
        let bytes = msg.encode_to_vec().expect("encode");
        let needle = b"record.start.request";
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "wire payload must contain `record.start.request` kind: {bytes:?}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // DR-0019 Update: set.* protocol (= 2 message + cap `set-v1`)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn set_request_roundtrip_via_control_message() {
        let msg = ControlMessage::SetRequest(SetRequest {
            key: "on-child-suspend".into(),
            value: "auto-resume".into(),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn set_ack_roundtrip_via_control_message() {
        let msg = ControlMessage::SetAck(SetAck {
            key: "on-child-suspend".into(),
            value: "notify".into(),
        });
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn set_request_kind_wire_format() {
        // wire 上の `kind` discriminator が dotted naming `set.request` で encode
        // されていることを確認 (= 既存 `record.*` と同パターン)。
        let msg = ControlMessage::SetRequest(SetRequest {
            key: "on-child-suspend".into(),
            value: "notify".into(),
        });
        let bytes = msg.encode_to_vec().expect("encode");
        let needle = b"set.request";
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "wire payload must contain `set.request` kind: {bytes:?}"
        );
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

    // ──────────────────────────────────────────────────────────────────────
    // R5-AUD-C1: CBOR decode の再帰深度制限。
    //
    // 認証 (= handshake token 検証) より前に呼ばれる `decode_from` 経路で、
    // 深くネストした CBOR を送られると、ciborium の default (256) や無制限
    // 設定では daemon worker thread の stack を食い潰し、`panic = "abort"`
    // で daemon プロセス全体が落ちる DoS が成立する。
    //
    // `MAX_CBOR_RECURSION_DEPTH` (= 16) を超える nested input は
    // `ControlMessageError::Decode` (内部 = `Error::RecursionLimitExceeded`)
    // として **panic ではなく** Result で reject されることを保証する。
    // ──────────────────────────────────────────────────────────────────────

    /// CBOR でネストした array (`[[[...]]]`) を `depth` 段組み立てる。
    /// 各層は `array(1)` (= header 0x81) + 最内殻に空 array (0x80) を置く。
    /// 結果として再帰深度は `depth` 段 (空 array が 1 段としてカウント) になる。
    fn nested_array_cbor(depth: usize) -> Vec<u8> {
        // `array(1)` (= header 0x81) を depth-1 段、最内殻に空 array (0x80) を 1 段。
        let outer = depth.saturating_sub(1);
        let mut buf = vec![0x81u8; outer]; // array(1) × (depth - 1)
        buf.push(0x80); // 最内殻の array(0)
        buf
    }

    #[test]
    fn cbor_decode_rejects_deeply_nested_input() {
        // MAX_CBOR_RECURSION_DEPTH を大きく超える深さで decode → reject 必須
        // (= panic でなく ControlMessageError::Decode に変換される)
        let depth = MAX_CBOR_RECURSION_DEPTH + 64;
        let bytes = nested_array_cbor(depth);
        let err = ControlMessage::decode_from(bytes.as_slice())
            .expect_err("deeply nested CBOR must be rejected");
        match err {
            ControlMessageError::Decode(ciborium::de::Error::RecursionLimitExceeded) => {}
            ControlMessageError::Decode(other) => {
                // 念のため: 別経路 (Semantic / Syntax) で reject されるのも可
                // (型不一致で先に弾かれる可能性があるため)。ただし最低限 Decode で
                // 受け止められていることを確認。
                eprintln!("decoded as different error variant: {other:?}");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn cbor_decode_rejects_extremely_deep_input_without_stack_overflow() {
        // 旧実装 (default ciborium = 256 限度、または limit 未設定で実質無制限)
        // では 100k 段 nested を投げると stack overflow / abort していた。
        // limit を導入したので、`Err` で安全に reject されることを保証する
        // (= panic も abort もしない)。
        let depth = 100_000;
        let bytes = nested_array_cbor(depth);
        let result = ControlMessage::decode_from(bytes.as_slice());
        assert!(
            result.is_err(),
            "extremely deep CBOR must return Err, not panic"
        );
    }

    #[test]
    fn cbor_decode_accepts_normal_depth() {
        // 通常運用で出てくる深さの ControlMessage (= handshake.request /
        // error with details) は問題なく decode できることを保証する。
        // handshake.request: map → caps array → strings (= 概ね 2-3 段)
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: vec!["data".into(), "lock".into(), "tail-v1".into()],
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: Some("tok".into()),
        });
        let bytes = req.encode_to_vec().expect("encode");
        let decoded = ControlMessage::decode_from(bytes.as_slice()).expect("decode");
        assert_eq!(decoded, req);

        // error.details に深さ 3 (= map → array → scalar) を入れても受理
        // (MAX_CBOR_RECURSION_DEPTH = 16 で十分余裕がある)
        let err = ControlMessage::Error(ErrorMessage {
            code: ErrorCode::LockDenied,
            message: "x".into(),
            details: Some(ciborium::Value::Map(vec![(
                ciborium::Value::Text("k".into()),
                ciborium::Value::Array(vec![ciborium::Value::Integer(1.into())]),
            )])),
        });
        let bytes = err.encode_to_vec().expect("encode");
        let decoded = ControlMessage::decode_from(bytes.as_slice()).expect("decode");
        assert_eq!(decoded, err);
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
