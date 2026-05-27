//! `screen.dump.request` / `screen.dump.response` / `screen.snapshot.request` /
//! `screen.snapshot.response` payload (DR-0013 §9 + DR-0008 §10 protocol 連動)。
//!
//! cap flag negotiation:
//! - `screen-dump-v1` — `screen.dump.*` 系を要求できる
//! - `state-snapshot-v1` — `screen.snapshot.*` 系を要求できる
//!
//! 未 cap で送ると daemon は `error` kind=`unsupported-capability` を返す。

use serde::{Deserialize, Serialize};

/// `screen.dump.request` の `format` 選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ScreenDumpFormat {
    /// `state_formatted()` の raw ANSI sequence (= stdout 流せば画面復元)。
    Ansi,
    /// 空白除去 + 改行 plaintext (= grep 用)。
    Binary,
    /// JSON encode (= 未実装、format-not-implemented を返す MVP scope 外)。
    Json,
    /// CBOR encode された structured snapshot。
    Cbor,
}

/// `screen.dump.request` の `layer` 選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ScreenDumpLayer {
    /// 現在 visible な viewport のみ。
    Visible,
    /// scrollback (= 過去) のみ。**Phase B では未実装** (= vt100 0.16 内蔵 ring が
    /// 統合途中、Phase C で配線)。
    Scrollback,
    /// scrollback + visible 連結。**Phase B では未実装**。
    Both,
}

/// `screen.dump.request` (= client → daemon)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScreenDumpRequest {
    /// 出力 format。
    pub format: ScreenDumpFormat,
    /// 対象 layer (= visible / scrollback / both)。
    pub layer: ScreenDumpLayer,
    /// 矩形指定 (= 未指定なら full viewport)。Phase B は実装するが、現状 layer 含めて
    /// 全画面のみ対応 (= rect は受信したら無視する仕様)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<DumpRect>,
    /// PDU serial 番号 (DR-0013 §10 PDU serial 土台)。client が応答対応付け用に
    /// 任意の u64 を採番、daemon は応答に同じ値を載せる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
}

/// `rect` 指定 (Phase B では未使用、forward-compat フィールド)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DumpRect {
    /// 矩形左上 col。
    pub x: u16,
    /// 矩形左上 row。
    pub y: u16,
    /// 矩形 width (cols 単位)。
    pub w: u16,
    /// 矩形 height (rows 単位)。
    pub h: u16,
}

/// `screen.dump.response` (= daemon → client)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScreenDumpResponse {
    /// `format` に応じた bytes (Ansi: raw、Binary: plaintext、Cbor: CBOR encoded
    /// `ScreenSnapshot`)。
    #[serde(with = "super::tail::serde_bytes")]
    pub payload: Vec<u8>,
    /// 対応する request の serial を echo back。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
}

/// `screen.snapshot.request` の include 選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SnapshotComponent {
    /// cell grid (= CBOR encoded `ScreenSnapshot.cells`)。
    Cells,
    /// cursor 位置 + visibility。
    Cursor,
    /// mode flag (= alt / app_keypad / cursor / bracketed_paste / hide_cursor)。
    Mode,
    /// scrollback rows (= 未実装、Phase C 配線後に有効化)。
    Scrollback,
    /// viewport size (rows × cols)。
    WindowSize,
    /// 現在 buffer kind (primary or alternate)。
    Buffer,
    /// SequenceNo (= current_seqno、incremental sync 連携用)。
    SequenceNo,
}

/// `screen.snapshot.request` payload (= client → daemon)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StateSnapshotRequest {
    /// 含めたい component の集合 (= 空なら error、最低 1 つ指定必須)。
    pub include: Vec<SnapshotComponent>,
    /// PDU serial 番号 (= `ScreenDumpRequest::serial` と同じ意味)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
}

/// `screen.snapshot.response` payload (= daemon → client)。
///
/// 各 field は include に含まれていれば `Some(_)`、含まれていなければ `None`
/// (= `serde(skip_serializing_if = "Option::is_none")` で省略)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StateSnapshotResponse {
    /// `cells` を含めた場合の sparse cells (= CBOR encoded `ScreenSnapshot`)。
    /// 構造化は daemon 側の `snapshot.rs` を参照、ここでは bytes として運ぶ
    /// (= 中身を直接 serialize すると hyoui 内部型を protocol 層に露出する)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "opt_bytes")]
    pub cells: Option<Vec<u8>>,
    /// cursor 情報 (row, col, visible)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CursorSnap>,
    /// mode flag (alt / app_keypad / cursor / bracketed_paste / hide_cursor)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeSnap>,
    /// scrollback (= 未実装、Phase C 配線後に CBOR encoded `Vec<Vec<CellSnap>>` を入れる)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "opt_bytes")]
    pub scrollback: Option<Vec<u8>>,
    /// window_size (rows, cols)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_size: Option<WindowSize>,
    /// 現在 buffer (= primary / alternate)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer: Option<BufferKind>,
    /// daemon ScreenState の current_seqno。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_no: Option<u64>,
    /// 対応 request の serial を echo back。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
}

/// cursor の wire 表現。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CursorSnap {
    /// 0-origin row index。
    pub row: u16,
    /// 0-origin col index。
    pub col: u16,
    /// `?25h` で true、`?25l` で false。
    pub visible: bool,
}

/// mode flag の wire 表現。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ModeSnap {
    /// `?1049h` / `?47h` / `?1047h` で true (= alt screen 中)。
    pub alternate_screen: bool,
    /// application keypad mode (= DECKPAM)。
    pub application_keypad: bool,
    /// application cursor key mode (= DECCKM `?1h`)。
    pub application_cursor: bool,
    /// bracketed paste mode (`?2004h`)。
    pub bracketed_paste: bool,
    /// `?25l` で true (cursor 非表示)。
    pub hide_cursor: bool,
}

/// `window_size` の wire 表現。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WindowSize {
    /// viewport 行数。
    pub rows: u16,
    /// viewport 列数。
    pub cols: u16,
}

/// 現在 buffer kind。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum BufferKind {
    /// 通常 buffer (= alt screen mode off)。
    Primary,
    /// alternate screen buffer (`?1049h` 等)。
    Alternate,
}

/// `Option<Vec<u8>>` を CBOR byte string で encode/decode する helper。
mod opt_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::DeserializeOwned};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_bytes(bytes),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        // ciborium が Null を None に、byte string / array<u8> を Some(...) に decode する。
        // 直接 Option<Vec<u8>> として deserialize すると array<u8> 経路になるので、
        // 一度 ciborium::Value を経由するのが堅い。
        let v: Option<ciborium::Value> = Option::deserialize(d)?;
        match v {
            None => Ok(None),
            Some(ciborium::Value::Null) => Ok(None),
            Some(ciborium::Value::Bytes(b)) => Ok(Some(b)),
            Some(ciborium::Value::Array(arr)) => {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    if let ciborium::Value::Integer(i) = item {
                        let n: i128 = i.into();
                        if let Ok(byte) = u8::try_from(n) {
                            out.push(byte);
                            continue;
                        }
                    }
                    return Err(serde::de::Error::custom("expected byte in array"));
                }
                Ok(Some(out))
            }
            _ => Err(serde::de::Error::custom("expected byte string or null")),
        }
    }

    // DeserializeOwned bound だけ存在を assert する unused import を消すための
    // 使い回し helper (= dead_code 防止)。
    #[allow(dead_code)]
    fn _assert<T: DeserializeOwned>() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(v: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(v, &mut buf).expect("encode");
        ciborium::de::from_reader(buf.as_slice()).expect("decode")
    }

    #[test]
    fn dump_request_roundtrip_minimal() {
        let req = ScreenDumpRequest {
            format: ScreenDumpFormat::Cbor,
            layer: ScreenDumpLayer::Visible,
            rect: None,
            serial: None,
        };
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn dump_request_roundtrip_with_rect_serial() {
        let req = ScreenDumpRequest {
            format: ScreenDumpFormat::Ansi,
            layer: ScreenDumpLayer::Visible,
            rect: Some(DumpRect {
                x: 0,
                y: 0,
                w: 80,
                h: 24,
            }),
            serial: Some(42),
        };
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn snapshot_request_roundtrip() {
        let req = StateSnapshotRequest {
            include: vec![
                SnapshotComponent::Cells,
                SnapshotComponent::Cursor,
                SnapshotComponent::Mode,
                SnapshotComponent::WindowSize,
                SnapshotComponent::Buffer,
                SnapshotComponent::SequenceNo,
            ],
            serial: Some(7),
        };
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn snapshot_response_roundtrip_partial() {
        let resp = StateSnapshotResponse {
            cells: None,
            cursor: Some(CursorSnap {
                row: 3,
                col: 5,
                visible: true,
            }),
            mode: Some(ModeSnap {
                alternate_screen: false,
                application_keypad: false,
                application_cursor: false,
                bracketed_paste: true,
                hide_cursor: false,
            }),
            scrollback: None,
            window_size: Some(WindowSize { rows: 24, cols: 80 }),
            buffer: Some(BufferKind::Primary),
            sequence_no: Some(123),
            serial: Some(7),
        };
        assert_eq!(roundtrip(&resp), resp);
    }

    #[test]
    fn snapshot_response_roundtrip_with_cells_payload() {
        let resp = StateSnapshotResponse {
            cells: Some(vec![0xa1, 0x00, 0x01]), // arbitrary CBOR bytes
            cursor: None,
            mode: None,
            scrollback: None,
            window_size: None,
            buffer: None,
            sequence_no: None,
            serial: None,
        };
        let got = roundtrip(&resp);
        assert_eq!(got, resp);
    }

    #[test]
    fn dump_response_roundtrip() {
        let resp = ScreenDumpResponse {
            payload: vec![0x1b, b'[', b'?', b'2', b'5', b'h'],
            serial: Some(99),
        };
        assert_eq!(roundtrip(&resp), resp);
    }
}
