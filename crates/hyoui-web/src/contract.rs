//! browser ↔ gateway の契約の正本 (DR-0035 決定 1)。
//!
//! web 境界に乗る形はすべてここに置く。`ws_attach.rs` / `lib.rs` は
//! `serde_json::json!` の手書きリテラルを持たず、この型を serialize する。
//! 文書 (DR-0035 の表) はここの golden test が固定した JSON から写す。
//!
//! JSON Schema は持たない (DR-0035 決定 1)。JS 側は bundler 無しで schema を
//! 検証する tooling が無く、置けば実装と乖離した時に誰も気付かない写しになる。
//!
//! 命名は `noun.verb` を維持する: 応答は `<noun>.result`、通知は `<noun>.info`。
//! 名前空間 prefix は付けない (= この WS は hyoui 専用)。

use serde::{Deserialize, Serialize};

/// web 境界の契約の世代番号 (DR-0035 決定 3)。
///
/// **上げるのは kind / field の削除と意味変更をした時だけ。** field や kind の
/// 追加では上げない (= 追加だけの変更で古い JS が新機能を使えないのは reload で
/// 解消する状態であって「話せない」ではない)。
///
/// 伝達経路は WS の `hello` frame と `GET /version` の 2 つだけで、応答ヘッダ /
/// query / cookie / assets のファイル名には出さない。
///
/// JS 側の写しは `assets/contract.js`。一致は
/// `tests::rust_and_js_protocol_version_agree` (lib.rs) が固定する (決定 7)。
pub const WEB_PROTOCOL_VERSION: u32 = 1;

// -----------------------------------------------------------------------------
// エラー (DR-0035 決定 2)
// -----------------------------------------------------------------------------

/// エラー 1 型。HTTP body も WS frame も同じ形を使う。
///
/// `code` は kebab-case。daemon 由来の失敗では daemon の
/// [`hyoui::protocol::messages::ErrorCode`] をそのまま通す (= `unsupported-capability`
/// / `mode.not-leader` 等)。gateway 自身が起こした失敗には gateway の語彙を使う。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// 機械可読な失敗種別 (kebab-case、または daemon の dotted code)。
    pub code: String,
    /// 人向けの説明。
    pub message: String,
}

impl ErrorInfo {
    /// `code` と `message` から作る。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// gateway 内部の予期しない失敗 (= HTTP 500 相当)。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(code::INTERNAL_ERROR, message)
    }
}

/// gateway 自身が起こす失敗の `code` 語彙。
///
/// daemon 由来の失敗は daemon の code をそのまま通すので、ここには並べない
/// (= 写しを作らない、DR-0035「増やしたくないもの」)。
pub mod code {
    /// request の形が不正 (= body / query / 寸法)。
    pub const INVALID_REQUEST: &str = "invalid-request";
    /// `specs[]` の要素が parse 不能、または web gateway では受け付けない種別。
    pub const INVALID_INPUT_SPEC: &str = "invalid-input-spec";
    /// その名前の session が無い。
    pub const SESSION_NOT_FOUND: &str = "session-not-found";
    /// entry はあるが stale (= socket 残骸 / handshake 失敗)。
    pub const SESSION_STALE: &str = "session-stale";
    /// asset が無い。
    pub const ASSET_NOT_FOUND: &str = "asset-not-found";
    /// asset path に `..` / 空 component が含まれる。
    pub const INVALID_ASSET_PATH: &str = "invalid-asset-path";
    /// DR-0022 auto-lock を他 client が保持中で取得できなかった。
    pub const LOCK_CONTENTION: &str = "lock-contention";
    /// DR-0022 auto-lock 取得が daemon 応答異常 / I/O で失敗した。
    pub const LOCK_UNAVAILABLE: &str = "lock-unavailable";
    /// gateway 内部の予期しない失敗。
    pub const INTERNAL_ERROR: &str = "internal-error";
    /// 受け取った WS text frame の `kind` が未知、または JSON が不正。
    pub const UNKNOWN_KIND: &str = "unknown-kind";
    /// 契約に載っているが、この gateway では有効になっていない操作
    /// (= 認証が無効な間の `auth.extend`、DR-0035 決定 1)。
    pub const UNSUPPORTED: &str = "unsupported";
}

/// HTTP エラー body の外枠 — `{"error": {"code": ..., "message": ...}}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// 失敗の内容。
    pub error: ErrorInfo,
}

impl From<ErrorInfo> for ErrorEnvelope {
    fn from(error: ErrorInfo) -> Self {
        Self { error }
    }
}

// -----------------------------------------------------------------------------
// HTTP body (DR-0035 決定 1 の routes 表)
// -----------------------------------------------------------------------------

/// `GET /version` の応答 (DR-0034 決定 7 の body に `protocol` を 1 field 足した形)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionResponse {
    /// crate version。
    pub version: String,
    /// build script が埋めた識別子 (無ければ `null`)。
    pub build_id: Option<String>,
    /// web 境界の契約の世代 ([`WEB_PROTOCOL_VERSION`])。
    pub protocol: u32,
}

impl VersionResponse {
    /// この process の版と契約世代を返す。
    pub fn current() -> Self {
        let info = hyoui::version::VersionInfo::current();
        Self {
            version: info.version,
            build_id: info.build_id,
            protocol: WEB_PROTOCOL_VERSION,
        }
    }
}

/// `POST /api/sessions/{id}/input` の request body。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRequest {
    /// `text:` / `hex:` / `key:` / `paste:` の spec 列。
    pub specs: Vec<String>,
}

/// `POST /api/sessions/{id}/input` の成功応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputResponse {
    /// PTY へ送った byte 総数。
    pub sent_bytes: usize,
    /// 受理した spec 数。
    pub specs: usize,
}

/// `POST /api/sessions/{id}/resize` の request body。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeRequest {
    /// 列数 (> 0)。
    pub cols: u16,
    /// 行数 (> 0)。
    pub rows: u16,
}

// -----------------------------------------------------------------------------
// WS text frame: browser → gateway (DR-0035 決定 1)
// -----------------------------------------------------------------------------

/// browser → gateway の text frame。binary frame は PTY input の 1:1 転写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ClientFrame {
    /// browser が提案した grid を leader connection から daemon へ反映する。
    #[serde(rename = "resize")]
    Resize {
        /// 応答と相関させる番号。
        #[serde(rename = "requestId")]
        request_id: u64,
        /// 列数。
        cols: u16,
        /// 行数。
        rows: u16,
    },
    /// 接続を維持したまま resize leader を奪取する (DR-0033)。
    #[serde(rename = "leader.request")]
    LeaderRequest {
        /// 応答と相関させる番号。
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    /// 認証の有効期限を延ばす (DR-0036 決定 5 で使う)。
    ///
    /// 契約の正本を 1 箇所にするため、使うのが DR-0036 でも kind は本 DR の表に
    /// 先に載せる (= 認証を足す時に「どの kind があるか」を別 DR に探しに
    /// 行かせない)。**認証が無効な間は `error` (`code: "unsupported"`) を返す**
    /// (DR-0035 決定 1)。
    #[serde(rename = "auth.extend")]
    AuthExtend {
        /// 応答と相関させる番号。
        #[serde(rename = "requestId")]
        request_id: u64,
        /// 延長に使う token。
        #[serde(rename = "accessToken")]
        access_token: String,
    },
}

// -----------------------------------------------------------------------------
// WS text frame: gateway → browser (DR-0035 決定 1)
// -----------------------------------------------------------------------------

/// gateway が保持する daemon attach の実効 mode。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachMode {
    /// 読み書き。
    #[serde(rename = "rw")]
    Rw,
    /// 読み取り専用。
    #[serde(rename = "ro")]
    Ro,
    /// 読み書きだが resize leader にはならない。
    #[serde(rename = "rw-no-leader")]
    RwNoLeader,
    /// daemon が返した mode を gateway が解釈できなかった。
    #[serde(rename = "unknown")]
    Unknown,
}

impl From<hyoui::protocol::Mode> for AttachMode {
    fn from(mode: hyoui::protocol::Mode) -> Self {
        match mode {
            hyoui::protocol::Mode::Rw => AttachMode::Rw,
            hyoui::protocol::Mode::Ro => AttachMode::Ro,
            hyoui::protocol::Mode::RwNoLeader => AttachMode::RwNoLeader,
            _ => AttachMode::Unknown,
        }
    }
}

/// gateway → browser の text frame。PTY output は binary frame のまま。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ServerFrame {
    /// WS 確立直後、`attach.info` より前に 1 回送る (DR-0035 決定 3)。
    ///
    /// `caps` は **その session の daemon と intersect した cap 集合** (決定 4)。
    /// daemon が handshake 応答で返す値そのままで、新しい cap も message も
    /// 足していない。
    ///
    /// `build_id` は表示のためだけに載せ、**世代不一致の判定には使わない**
    /// (決定 3 — unstable の再ビルドで値が動くたびに誘導を出すと、契約が同じでも
    /// 「リロードしろ」が出続けて警告が形骸化する)。
    #[serde(rename = "hello")]
    Hello {
        /// 契約の世代 ([`WEB_PROTOCOL_VERSION`])。
        protocol: u32,
        /// gateway の crate version。
        version: String,
        /// gateway の build 識別子 (無ければ `null`)。
        build_id: Option<String>,
        /// daemon と intersect 済みの cap 名一覧。
        caps: Vec<String>,
        /// 認証の期限 (ISO 8601)。認証が無効な間は `null` (DR-0036 決定 5 で使う)。
        auth_expires_at: Option<String>,
    },
    /// 実効 mode / leader 状態。`hello` の直後と、`leader.notify` /
    /// `mode.change` 受信時に送る。
    #[serde(rename = "attach.info")]
    AttachInfo {
        /// 実効 mode。
        mode: AttachMode,
        /// この接続が resize leader か。
        leader: bool,
    },
    /// `resize` への応答。
    #[serde(rename = "resize.result")]
    ResizeResult {
        /// 要求と同じ番号。
        #[serde(rename = "requestId")]
        request_id: u64,
        /// 成否。
        ok: bool,
        /// 失敗の内容 (成功時は省略)。
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrorInfo>,
    },
    /// `leader.request` への応答。
    #[serde(rename = "leader.result")]
    LeaderResult {
        /// 要求と同じ番号。
        #[serde(rename = "requestId")]
        request_id: u64,
        /// 成否。
        ok: bool,
        /// 失敗の内容 (成功時は省略)。
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrorInfo>,
    },
    /// 未知 `kind` / 不正 JSON を受けた時 (DR-0035 決定 2)。
    ///
    /// **世代不一致の推定には使わない。** 検出は `hello.protocol` 1 本で、
    /// `error` は「その要求が通らなかった」だけを扱う (決定 2)。2 経路で推定すると、
    /// cap 不足と世代不一致が同じ帯に化ける。
    #[serde(rename = "error")]
    Error {
        /// 相関できる要求があればその番号、無ければ `null`。
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
        /// 失敗の内容。
        error: ErrorInfo,
    },
}

#[cfg(test)]
mod tests {
    //! golden test (DR-0035 決定 7)。各 kind の JSON 表現を固定し、
    //! DR-0035 の契約表を写す元にする。
    //!
    //! 表と golden の食い違いは、golden を直した人が表も直す (同じ変更で両方を触る)。

    use super::*;
    use serde_json::json;

    fn golden<T: Serialize>(value: &T, expected: serde_json::Value) {
        let actual = serde_json::to_value(value).expect("serialize");
        assert_eq!(actual, expected);
    }

    #[test]
    fn client_frame_resize_golden() {
        let frame: ClientFrame = serde_json::from_value(json!({
            "kind": "resize", "requestId": 7, "cols": 120, "rows": 40
        }))
        .expect("decode resize");
        assert_eq!(
            frame,
            ClientFrame::Resize {
                request_id: 7,
                cols: 120,
                rows: 40
            }
        );
        golden(
            &frame,
            json!({"kind": "resize", "requestId": 7, "cols": 120, "rows": 40}),
        );
    }

    #[test]
    fn client_frame_leader_request_golden() {
        let frame: ClientFrame =
            serde_json::from_value(json!({"kind": "leader.request", "requestId": 17}))
                .expect("decode leader.request");
        assert_eq!(frame, ClientFrame::LeaderRequest { request_id: 17 });
        golden(&frame, json!({"kind": "leader.request", "requestId": 17}));
    }

    #[test]
    fn client_frame_rejects_unknown_kind() {
        let decoded = serde_json::from_value::<ClientFrame>(json!({"kind": "nope"}));
        assert!(decoded.is_err(), "unknown kind must not decode");
    }

    #[test]
    fn client_frame_auth_extend_golden() {
        let frame: ClientFrame = serde_json::from_value(
            json!({"kind": "auth.extend", "requestId": 3, "accessToken": "tok"}),
        )
        .expect("decode auth.extend");
        assert_eq!(
            frame,
            ClientFrame::AuthExtend {
                request_id: 3,
                access_token: "tok".to_string()
            }
        );
        golden(
            &frame,
            json!({"kind": "auth.extend", "requestId": 3, "accessToken": "tok"}),
        );
    }

    #[test]
    fn server_frame_hello_golden() {
        golden(
            &ServerFrame::Hello {
                protocol: WEB_PROTOCOL_VERSION,
                version: "0.9.48".to_string(),
                build_id: Some("abc123".to_string()),
                caps: vec!["data".to_string(), "lock".to_string()],
                auth_expires_at: None,
            },
            json!({
                "kind": "hello",
                "protocol": 1,
                "version": "0.9.48",
                "build_id": "abc123",
                "caps": ["data", "lock"],
                "auth_expires_at": null,
            }),
        );
    }

    #[test]
    fn server_frame_attach_info_golden() {
        golden(
            &ServerFrame::AttachInfo {
                mode: AttachMode::Ro,
                leader: false,
            },
            json!({"kind": "attach.info", "mode": "ro", "leader": false}),
        );
        golden(
            &ServerFrame::AttachInfo {
                mode: AttachMode::RwNoLeader,
                leader: false,
            },
            json!({"kind": "attach.info", "mode": "rw-no-leader", "leader": false}),
        );
    }

    #[test]
    fn server_frame_resize_result_golden() {
        golden(
            &ServerFrame::ResizeResult {
                request_id: 7,
                ok: true,
                error: None,
            },
            json!({"kind": "resize.result", "requestId": 7, "ok": true}),
        );
        golden(
            &ServerFrame::ResizeResult {
                request_id: 8,
                ok: false,
                error: Some(ErrorInfo::new("mode.not-leader", "not the resize leader")),
            },
            json!({
                "kind": "resize.result",
                "requestId": 8,
                "ok": false,
                "error": {"code": "mode.not-leader", "message": "not the resize leader"},
            }),
        );
    }

    #[test]
    fn server_frame_leader_result_golden() {
        golden(
            &ServerFrame::LeaderResult {
                request_id: 17,
                ok: true,
                error: None,
            },
            json!({"kind": "leader.result", "requestId": 17, "ok": true}),
        );
        golden(
            &ServerFrame::LeaderResult {
                request_id: 18,
                ok: false,
                error: Some(ErrorInfo::new(
                    "unsupported-capability",
                    "daemon does not support leader-request-v1",
                )),
            },
            json!({
                "kind": "leader.result",
                "requestId": 18,
                "ok": false,
                "error": {
                    "code": "unsupported-capability",
                    "message": "daemon does not support leader-request-v1",
                },
            }),
        );
    }

    #[test]
    fn server_frame_error_golden() {
        golden(
            &ServerFrame::Error {
                request_id: None,
                error: ErrorInfo::new(code::UNKNOWN_KIND, "unknown WS frame kind"),
            },
            json!({
                "kind": "error",
                "requestId": null,
                "error": {"code": "unknown-kind", "message": "unknown WS frame kind"},
            }),
        );
    }

    #[test]
    fn http_bodies_golden() {
        golden(
            &VersionResponse {
                version: "0.9.48".to_string(),
                build_id: None,
                protocol: WEB_PROTOCOL_VERSION,
            },
            json!({"version": "0.9.48", "build_id": null, "protocol": 1}),
        );
        golden(
            &ErrorEnvelope::from(ErrorInfo::new(
                code::SESSION_NOT_FOUND,
                "no session named \"x\"",
            )),
            json!({"error": {"code": "session-not-found", "message": "no session named \"x\""}}),
        );
        golden(
            &InputResponse {
                sent_bytes: 12,
                specs: 2,
            },
            json!({"sent_bytes": 12, "specs": 2}),
        );
        let input: InputRequest =
            serde_json::from_value(json!({"specs": ["text:hi", "key:Enter"]})).expect("decode");
        assert_eq!(input.specs.len(), 2);
        let resize: ResizeRequest =
            serde_json::from_value(json!({"cols": 80, "rows": 24})).expect("decode");
        assert_eq!(resize, ResizeRequest { cols: 80, rows: 24 });
    }
}
