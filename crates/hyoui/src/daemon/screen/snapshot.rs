//! Structured snapshot + compact serialization wrapper (DR-0013 §11 + §9)。
//!
//! ScreenState の cell / cursor / mode 等を「`Screen::state_formatted()` の raw bytes」
//! ではなく **構造化 view** で取り出して serde にかけられる形に整える。CBOR encode は
//! caller (= protocol layer) が行う。本 module は wrapper struct + naive vs 圧縮の
//! size 比較に必要な情報を提供する。
//!
//! ## 圧縮戦略 (DR-0013 §11)
//!
//! - **空 cell skip**: 空白文字 (= contents == "" or " ") かつ attribute も default
//!   なら出力しない (= sparse 表現)
//! - **属性 bit pack**: bold / italic / underline / inverse を 1 byte に pack
//! - **wide 継続 cell の skip**: 全角 cell の継続部分 (= `is_wide_continuation`) は
//!   serialize しない (= 先頭 cell の `is_wide=true` が同行で 2 col 分占有する印)
//!
//! PoC §9 で「naive cell-level CBOR は 283 倍に膨張」と判明したため、圧縮戦略を
//! 入れる前提。RLE は MVP では入れない (= §11 後段)。
//!
//! ## Phase B での scope
//!
//! - 圧縮 wrapper の serde 表現 + `build_screen_snapshot` factory
//! - `ScreenDumpRequest` / `ScreenDumpResponse` の `Binary`/`Ansi` 出力 helper
//!   (= `build_screen_dump`)
//! - naive vs 圧縮の size 比較 test (= regression 防止)
//!
//! ## 持ち越し (Phase C 以降)
//!
//! - palette index / true color の Color variant 圧縮 (= MVP では一旦 vt100 Color
//!   をそのまま encode する。Color の `serde::Serialize` impl は vt100 0.16 では
//!   存在しないため、本 phase では文字列で吐く approximation を使う)
//! - hyperlink / link / extra 属性
//! - RLE / dictionary 圧縮

use serde::{Deserialize, Serialize};

use super::state::{RowCellSnap, ScreenState};

/// `ScreenDumpRequest.format` の選択肢 (DR-0013 §9)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ScreenDumpFormat {
    /// `state_formatted()` の raw bytes (= ANSI sequence) をそのまま返す。
    /// client は stdout に書けば画面を復元可能。
    Ansi,
    /// 空白除去 + 属性無視で plaintext 化した bytes を返す (= grep 用)。
    Binary,
    /// 構造化 cells を JSON (= text) で返す (= debug 目視)。
    Json,
    /// 構造化 cells を CBOR で返す (= 機械処理)。
    Cbor,
}

/// `ScreenDumpRequest.layer` の選択肢 (DR-0013 §9)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ScreenDumpLayer {
    /// 現在 visible な viewport のみ。
    Visible,
    /// scrollback 全行のみ (= visible より過去)。Phase B では vt100 内蔵 scrollback
    /// が `Parser::new(_, _, 0)` で無効化されているため、空配列が返る (= 設計上の
    /// 制約、Phase C で配線)。
    Scrollback,
    /// scrollback + visible の連結。
    Both,
}

/// 構造化 snapshot の wrapper (DR-0013 §11)。serde で encode 可能。
///
/// `cells` は **sparse 表現** で、空 cell (= attribute も default) は省略される。
/// `(row, col, RowCellSnapCompact)` の triplet 配列で表現。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScreenSnapshot {
    /// viewport 行数。
    pub rows: u16,
    /// viewport 列数。
    pub cols: u16,
    /// cursor 位置 + visibility (= primary buffer / alt buffer どちらでも同じ struct)。
    pub cursor: CursorSnapshot,
    /// mode flag (= alt / app_keypad / cursor / bracketed_paste / hide_cursor)。
    pub mode: ModeSnapshot,
    /// sparse cells (= 空白 default cell は省略)。`Vec<(row, col, CellSnapshot)>`。
    pub cells: Vec<CellPos>,
    /// 現在 buffer (primary or alternate)。
    pub buffer: BufferKind,
    /// daemon ScreenState の現在 SequenceNo (DR-0013 §4 Phase B 土台)。
    pub current_seqno: u64,
}

/// cell の sparse 表現要素 (= 1 cell 分 + 座標)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CellPos {
    /// row index (0-origin)。
    pub r: u16,
    /// col index (0-origin)。
    pub c: u16,
    /// cell 中身 (= 文字列 + attribute bits + wide flag)。
    pub cell: CellSnapshot,
}

/// 1 cell 分の圧縮表現。`serde(skip_serializing_if)` で空 / default field を省略。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CellSnapshot {
    /// cell の表示文字列 (combining char / 全角を保つため `String`)。空白は通常省略。
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "t")]
    pub text: String,
    /// 属性 bit pack (bit 0 bold / 1 italic / 2 underline / 3 inverse)。
    /// 0 (= default) は省略。
    #[serde(default, skip_serializing_if = "is_zero_u8", rename = "a")]
    pub attrs: u8,
    /// 全角先頭 cell なら true (= 同行で 2 col 占有)。default は false で省略。
    #[serde(default, skip_serializing_if = "is_false", rename = "w")]
    pub wide: bool,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// cursor の snapshot serializable 版。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorSnapshot {
    pub row: u16,
    pub col: u16,
    /// `?25h` で true、`?25l` で false。
    pub visible: bool,
}

/// mode flag の snapshot serializable 版。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModeSnapshot {
    pub alternate_screen: bool,
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub hide_cursor: bool,
}

/// 現在 buffer kind (primary or alternate)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BufferKind {
    Primary,
    Alternate,
}

/// `ScreenState` から構造化 snapshot を組み立てる。
///
/// 圧縮戦略:
/// - 空 cell (= text 空 + attrs 0 + wide false + wide_continuation false) は出力しない
/// - 全角の継続 cell (= wide_continuation) は出力しない (= 先頭 cell の wide=true が
///   印になる)
///
/// 本 factory は visible viewport のみを serialize する。scrollback の include は
/// Phase C 以降 (vt100 0.16 の API では公開 getter が限定的)。
pub(crate) fn build_screen_snapshot(state: &ScreenState) -> ScreenSnapshot {
    let (rows, cols) = state.size();
    let visible = state.snapshot_visible_rows();
    let mut cells = Vec::new();
    for (r_idx, row) in visible.iter().enumerate() {
        for (c_idx, cell) in row.iter().enumerate() {
            if is_default_cell(cell) {
                continue;
            }
            cells.push(CellPos {
                r: r_idx as u16,
                c: c_idx as u16,
                cell: CellSnapshot {
                    text: cell.contents.clone(),
                    attrs: cell.attrs,
                    wide: cell.is_wide,
                },
            });
        }
    }
    let cursor_snap = state.snapshot_cursor();
    let mode_snap = state.snapshot_mode();
    ScreenSnapshot {
        rows,
        cols,
        cursor: CursorSnapshot {
            row: cursor_snap.row,
            col: cursor_snap.col,
            visible: cursor_snap.visible,
        },
        mode: ModeSnapshot {
            alternate_screen: mode_snap.alternate_screen,
            application_keypad: mode_snap.application_keypad,
            application_cursor: mode_snap.application_cursor,
            bracketed_paste: mode_snap.bracketed_paste,
            hide_cursor: mode_snap.hide_cursor,
        },
        cells,
        buffer: if mode_snap.alternate_screen {
            BufferKind::Alternate
        } else {
            BufferKind::Primary
        },
        current_seqno: state.current_seqno(),
    }
}

/// snapshot の cell が「省略してよい default」か判定。
///
/// - text が空 or " " (= 半角スペース)
/// - attrs 0
/// - wide / wide_continuation 両方 false
///
/// この条件で「画面に何も描かれていない」状態を表す。
fn is_default_cell(cell: &RowCellSnap) -> bool {
    let text_empty = cell.contents.is_empty() || cell.contents == " ";
    text_empty && cell.attrs == 0 && !cell.is_wide && !cell.is_wide_continuation
}

/// `ScreenDumpRequest` を処理して bytes を組み立てる。
///
/// format に応じて:
/// - `Ansi`: `state_formatted()` (alt mode prepend は含めない、redraw.rs と区別)
/// - `Binary`: cells を空白除去 + attribute 無視で plaintext 化
/// - `Json`: `build_screen_snapshot` を `serde_json` 相当で encode … と言いたいが
///   hyoui は serde_json を持たないため、`ciborium` で CBOR diag (= text) を吐く
///   代わりに `Json` は **未実装** として `ProtocolMalformed` 相当を caller 側で
///   返す (= MVP scope 外)
/// - `Cbor`: `build_screen_snapshot` を CBOR encode した bytes
///
/// `layer` は本 phase では `Visible` のみ実装。`Scrollback` / `Both` は caller 側で
/// 拒否する (= cap flag で gating した上で、未実装 layer は error 返却)。
///
/// 戻り値は `Result<Vec<u8>, ScreenDumpError>`。caller が error を error.* 経由で
/// client に通知する。
pub(crate) fn build_screen_dump(
    state: &ScreenState,
    format: ScreenDumpFormat,
    layer: ScreenDumpLayer,
) -> Result<Vec<u8>, ScreenDumpError> {
    if layer != ScreenDumpLayer::Visible {
        return Err(ScreenDumpError::LayerNotImplemented(layer));
    }
    match format {
        ScreenDumpFormat::Ansi => Ok(state.state_formatted()),
        ScreenDumpFormat::Binary => Ok(build_plain_text(state)),
        ScreenDumpFormat::Json => Err(ScreenDumpError::FormatNotImplemented(format)),
        ScreenDumpFormat::Cbor => {
            let snap = build_screen_snapshot(state);
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&snap, &mut buf)
                .map_err(|_| ScreenDumpError::EncodeFailed)?;
            Ok(buf)
        }
    }
}

/// `Binary` format の plaintext 出力 (= 空白除去 + 改行 + attribute 無視)。
///
/// 各 row を「末尾空白を trim して `\n` 区切り」で連結。grep 等の用途に最適化。
fn build_plain_text(state: &ScreenState) -> Vec<u8> {
    let (rows, cols) = state.size();
    let mut out = Vec::with_capacity(rows as usize * cols as usize);
    let visible = state.snapshot_visible_rows();
    for row in visible.iter() {
        let mut line = String::with_capacity(cols as usize);
        for cell in row.iter() {
            if cell.is_wide_continuation {
                // 全角継続は飛ばす (= 先頭 cell の contents が `あ` 等で 1 度に追記済)
                continue;
            }
            if cell.contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.contents);
            }
        }
        // 末尾空白を trim
        let trimmed = line.trim_end_matches(' ');
        out.extend_from_slice(trimmed.as_bytes());
        out.push(b'\n');
    }
    out
}

/// `build_screen_dump` の error。
#[derive(Debug, thiserror::Error)]
pub(crate) enum ScreenDumpError {
    /// 未実装の format。
    #[error("dump format not implemented: {0:?}")]
    FormatNotImplemented(ScreenDumpFormat),
    /// 未実装の layer。
    #[error("dump layer not implemented: {0:?}")]
    LayerNotImplemented(ScreenDumpLayer),
    /// CBOR encode 失敗。
    #[error("dump cbor encode failed")]
    EncodeFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_snapshot_has_no_cells() {
        let s = ScreenState::new(5, 40, 0);
        let snap = build_screen_snapshot(&s);
        assert_eq!(snap.rows, 5);
        assert_eq!(snap.cols, 40);
        // 空 state は cells が省略される (sparse)
        assert!(snap.cells.is_empty(), "empty snap must skip cells");
        assert_eq!(snap.buffer, BufferKind::Primary);
        assert_eq!(snap.current_seqno, 0);
    }

    #[test]
    fn snapshot_records_non_default_cells() {
        let mut s = ScreenState::new(5, 40, 0);
        s.process(b"hi");
        let snap = build_screen_snapshot(&s);
        // 'h' と 'i' の 2 cell が出る
        assert_eq!(snap.cells.len(), 2);
        assert_eq!(snap.cells[0].r, 0);
        assert_eq!(snap.cells[0].c, 0);
        assert_eq!(snap.cells[0].cell.text, "h");
    }

    #[test]
    fn snapshot_skip_default_white_cells() {
        let mut s = ScreenState::new(5, 40, 0);
        s.process(b"a b");
        let snap = build_screen_snapshot(&s);
        // space は省略、a と b の 2 cell のみ
        assert_eq!(snap.cells.len(), 2);
    }

    /// PoC §9 の検証: naive cell-level CBOR (= 全 cell 出力) は 283 倍だったが、
    /// 圧縮 wrapper (= 空 cell skip) で大幅に縮む。本 test は regression 防止。
    #[test]
    fn snapshot_compression_beats_naive_on_empty_state() {
        let s = ScreenState::new(24, 80, 0);
        let snap = build_screen_snapshot(&s);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&snap, &mut buf).expect("encode");
        // 空 state は cells が空配列なので、CBOR は概ね 100 byte 以下
        assert!(
            buf.len() < 200,
            "empty 24x80 snapshot must be < 200 byte (was {} byte)",
            buf.len()
        );
    }

    #[test]
    fn dump_ansi_returns_state_formatted() {
        let mut s = ScreenState::new(5, 40, 0);
        s.process(b"hi");
        let out =
            build_screen_dump(&s, ScreenDumpFormat::Ansi, ScreenDumpLayer::Visible).expect("ok");
        // ANSI dump は state_formatted の raw bytes (= ESC で始まる)
        assert!(out.starts_with(b"\x1b"));
    }

    #[test]
    fn dump_binary_returns_plain_text() {
        let mut s = ScreenState::new(3, 10, 0);
        s.process(b"hi\r\n");
        s.process(b"hello");
        let out =
            build_screen_dump(&s, ScreenDumpFormat::Binary, ScreenDumpLayer::Visible).expect("ok");
        // plain text 経路: trim 済の "hi\nhello\n\n"
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(text.contains("hi"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn dump_cbor_encodes_snapshot() {
        let mut s = ScreenState::new(5, 40, 0);
        s.process(b"hi");
        let out =
            build_screen_dump(&s, ScreenDumpFormat::Cbor, ScreenDumpLayer::Visible).expect("ok");
        // 復号して内容を確認
        let snap: ScreenSnapshot = ciborium::de::from_reader(out.as_slice()).expect("decode");
        assert_eq!(snap.cells.len(), 2);
    }

    #[test]
    fn dump_json_returns_not_implemented() {
        let s = ScreenState::new(5, 40, 0);
        let err = build_screen_dump(&s, ScreenDumpFormat::Json, ScreenDumpLayer::Visible)
            .expect_err("must error");
        match err {
            ScreenDumpError::FormatNotImplemented(ScreenDumpFormat::Json) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn dump_scrollback_layer_returns_not_implemented() {
        let s = ScreenState::new(5, 40, 0);
        let err = build_screen_dump(&s, ScreenDumpFormat::Cbor, ScreenDumpLayer::Scrollback)
            .expect_err("must error");
        match err {
            ScreenDumpError::LayerNotImplemented(ScreenDumpLayer::Scrollback) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
