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

// -----------------------------------------------------------------------------
// HTTP body (DR-0035 決定 1 の routes 表)
// -----------------------------------------------------------------------------

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
    /// 実効 mode / leader 状態。attach 直後と、`leader.notify` /
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
        error: Option<String>,
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
        error: Option<String>,
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
                error: Some("not the resize leader".to_string()),
            },
            json!({
                "kind": "resize.result",
                "requestId": 8,
                "ok": false,
                "error": "not the resize leader",
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
                error: Some("daemon does not support leader.request".to_string()),
            },
            json!({
                "kind": "leader.result",
                "requestId": 18,
                "ok": false,
                "error": "daemon does not support leader.request",
            }),
        );
    }

    #[test]
    fn http_bodies_golden() {
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
