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
    /// ANSI escape は strip するが、cell の空白 (= padding) と行構造はそのまま
    /// 保持した plaintext (= TUI 自動処理用、claude TUI PoC feedback)。
    TextPlain,
}

/// `ScreenDumpRequest.layer` の選択肢 (DR-0013 §9)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ScreenDumpLayer {
    /// 現在 visible な viewport のみ。
    Visible,
    /// scrollback 全行のみ (= visible より過去)。Phase C で vt100 内蔵 scrollback ring を
    /// `config.screen_vt100_scrollback_rows` (default 1000 行) 経由で配線済。
    /// `screen_vt100_scrollback_rows == 0` 設定なら空 payload が返る。
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
/// format × layer の dispatch:
///
/// | format | Visible | Scrollback | Both |
/// |--------|---------|------------|------|
/// | Ansi | `state_formatted()` | scrollback rows を ANSI escape で再構築 | scrollback + visible 連結 |
/// | Binary | 空白除去 plaintext | scrollback の空白除去 plaintext | 連結 |
/// | TextPlain | cell 空白 + 行末空白保持 | scrollback の cell 空白 + 行末空白保持 | 連結 |
/// | Cbor | `ScreenSnapshot` (cells = visible) | `ScreenSnapshot` (cells = scrollback) | `ScreenSnapshot` (cells = scrollback + visible) |
/// | Json | 未実装 (= `FormatNotImplemented`) | 同左 | 同左 |
///
/// - `Both` layer は scrollback rows を **先頭** に、visible rows を後続に置く形で連結する
///   (= 古い → 新しい時系列順を維持)。
/// - Cbor の `cells` 座標は連結後の行を 0-origin で振り直す (= caller は単一 grid として扱える)。
///
/// `&mut ScreenState` を要求するのは scrollback rows を抽出するために vt100
/// `set_scrollback` を一時的に呼ぶ必要があるため (= 副作用は内部で復元するので
/// 論理的には state 不変、API 制約上のみ mutable 借用)。
///
/// 戻り値は `Result<Vec<u8>, ScreenDumpError>`。caller が error を error.* 経由で
/// client に通知する。
pub(crate) fn build_screen_dump(
    state: &mut ScreenState,
    format: ScreenDumpFormat,
    layer: ScreenDumpLayer,
) -> Result<Vec<u8>, ScreenDumpError> {
    // Json はどの layer でも MVP scope 外 (= 早期 return で他の分岐を簡素化)。
    if matches!(format, ScreenDumpFormat::Json) {
        return Err(ScreenDumpError::FormatNotImplemented(format));
    }
    // Cbor は format 全体で 1 つの ScreenSnapshot を組み立てる (= layer 別に cells を差し替え)。
    if matches!(format, ScreenDumpFormat::Cbor) {
        let snap = build_layered_snapshot(state, layer);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&snap, &mut buf).map_err(|_| ScreenDumpError::EncodeFailed)?;
        return Ok(buf);
    }
    // text 系 (Ansi / Binary / TextPlain) の layer 別 dispatch。
    match layer {
        ScreenDumpLayer::Visible => match format {
            ScreenDumpFormat::Ansi => Ok(state.state_formatted()),
            ScreenDumpFormat::Binary => Ok(build_plain_text_from_rows(
                &state.snapshot_visible_rows(),
                /* trim_trailing = */ true,
            )),
            ScreenDumpFormat::TextPlain => Ok(build_plain_text_from_rows(
                &state.snapshot_visible_rows(),
                /* trim_trailing = */ false,
            )),
            // Cbor / Json は上で処理済 (= unreachable)
            ScreenDumpFormat::Cbor | ScreenDumpFormat::Json => unreachable!(),
        },
        ScreenDumpLayer::Scrollback => {
            let sb_rows = state.snapshot_scrollback_rows();
            match format {
                ScreenDumpFormat::Ansi => Ok(rows_to_ansi(&sb_rows)),
                ScreenDumpFormat::Binary => Ok(build_plain_text_from_rows(&sb_rows, true)),
                ScreenDumpFormat::TextPlain => Ok(build_plain_text_from_rows(&sb_rows, false)),
                ScreenDumpFormat::Cbor | ScreenDumpFormat::Json => unreachable!(),
            }
        }
        ScreenDumpLayer::Both => {
            let sb_rows = state.snapshot_scrollback_rows();
            let visible_rows = state.snapshot_visible_rows();
            let mut combined = sb_rows;
            combined.extend(visible_rows);
            match format {
                ScreenDumpFormat::Ansi => Ok(rows_to_ansi(&combined)),
                ScreenDumpFormat::Binary => Ok(build_plain_text_from_rows(&combined, true)),
                ScreenDumpFormat::TextPlain => Ok(build_plain_text_from_rows(&combined, false)),
                ScreenDumpFormat::Cbor | ScreenDumpFormat::Json => unreachable!(),
            }
        }
    }
}

/// layer 別に cells を差し替えた `ScreenSnapshot` を組み立てる (= Cbor 用)。
///
/// - `Visible`: 既存 `build_screen_snapshot` と等価 (visible viewport の cells)
/// - `Scrollback`: scrollback rows を cells に置く。`rows` field は実 scrollback 行数
///   (= 表示用 viewport size とは別)
/// - `Both`: scrollback + visible 連結。座標は 0-origin で振り直す
///
/// cursor / mode / buffer / current_seqno は **常に現在の state** を反映する
/// (= 過去の state ではない、scrollback 中の cursor 履歴は vt100 が持たない)。
fn build_layered_snapshot(state: &mut ScreenState, layer: ScreenDumpLayer) -> ScreenSnapshot {
    let (visible_rows_size, cols) = state.size();
    let cursor_snap = state.snapshot_cursor();
    let mode_snap = state.snapshot_mode();
    let seqno = state.current_seqno();

    let rows_data: Vec<Vec<RowCellSnap>> = match layer {
        ScreenDumpLayer::Visible => state.snapshot_visible_rows(),
        ScreenDumpLayer::Scrollback => state.snapshot_scrollback_rows(),
        ScreenDumpLayer::Both => {
            let sb_rows = state.snapshot_scrollback_rows();
            let visible_rows = state.snapshot_visible_rows();
            let mut combined = sb_rows;
            combined.extend(visible_rows);
            combined
        }
    };
    let total_rows: u16 = u16::try_from(rows_data.len()).unwrap_or(u16::MAX);
    // viewport rows は `Visible` layer の場合だけ実 viewport size、それ以外は
    // 実際に出力する行数を rows field に載せる (= caller が cells の座標範囲を
    // この値で解釈できるようにするため)。
    let rows_field = match layer {
        ScreenDumpLayer::Visible => visible_rows_size,
        _ => total_rows,
    };

    let mut cells = Vec::new();
    for (r_idx, row) in rows_data.iter().enumerate() {
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
    ScreenSnapshot {
        rows: rows_field,
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
        current_seqno: seqno,
    }
}

/// rows × cols の cell grid を ANSI escape sequence 化する (= scrollback / both layer 用)。
///
/// vt100 `state_formatted()` は内部 grid 全体に対する formatted output だが、
/// scrollback offset を切り替えての formatted 出力 API は vt100 0.16 では公開されて
/// いない。本実装では各 cell の SGR 属性を `\x1b[` escape で再構築し、行末に `\r\n` を
/// 入れて連結する。
///
/// 制限: SGR の **bold / italic / underline / inverse** のみ反映。色情報は
/// `RowCellSnap` が保持していないため落とす (= MVP scope。色を完全に保持したい
/// なら snapshot 経路の cbor を使う想定)。caller が `cat` で再生したときに装飾は
/// 落ちて見えるが、文字列内容は正しく再現される。
fn rows_to_ansi(rows: &[Vec<RowCellSnap>]) -> Vec<u8> {
    let mut out = Vec::new();
    // cursor を 1,1 に移して描画開始 (= state_formatted() に近い前置き)。
    out.extend_from_slice(b"\x1b[H");
    let mut prev_attrs: u8 = 0;
    for row in rows.iter() {
        for cell in row.iter() {
            if cell.is_wide_continuation {
                continue; // 先頭 cell が 2 col 分の contents を持つので skip
            }
            // SGR change が必要なら escape を吐く
            if cell.attrs != prev_attrs {
                out.extend_from_slice(b"\x1b[0m"); // reset
                if cell.attrs & 1 != 0 {
                    out.extend_from_slice(b"\x1b[1m"); // bold
                }
                if cell.attrs & (1 << 1) != 0 {
                    out.extend_from_slice(b"\x1b[3m"); // italic
                }
                if cell.attrs & (1 << 2) != 0 {
                    out.extend_from_slice(b"\x1b[4m"); // underline
                }
                if cell.attrs & (1 << 3) != 0 {
                    out.extend_from_slice(b"\x1b[7m"); // inverse
                }
                prev_attrs = cell.attrs;
            }
            if cell.contents.is_empty() {
                out.push(b' ');
            } else {
                out.extend_from_slice(cell.contents.as_bytes());
            }
        }
        out.extend_from_slice(b"\r\n");
    }
    if prev_attrs != 0 {
        out.extend_from_slice(b"\x1b[0m"); // 末尾の attr reset
    }
    out
}

/// 与えられた rows × cols の cell grid を plaintext 化する。
///
/// - `trim_trailing = true` (= `Binary` format): 各 row の末尾空白を trim する
///   (= grep 向けの空白除去)
/// - `trim_trailing = false` (= `TextPlain` format): 行末空白を保持する
///   (= TUI 盤面の padding をそのまま温存)
///
/// 全 row に共通: 空 cell は半角 space、wide 継続 cell は skip、改行で行分け。
fn build_plain_text_from_rows(rows: &[Vec<RowCellSnap>], trim_trailing: bool) -> Vec<u8> {
    let est_cols = rows.first().map(|r| r.len()).unwrap_or(0);
    let mut out = Vec::with_capacity(rows.len() * (est_cols + 1));
    for row in rows.iter() {
        let mut line = String::with_capacity(est_cols);
        for cell in row.iter() {
            if cell.is_wide_continuation {
                continue;
            }
            if cell.contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.contents);
            }
        }
        if trim_trailing {
            let trimmed = line.trim_end_matches(' ');
            out.extend_from_slice(trimmed.as_bytes());
        } else {
            out.extend_from_slice(line.as_bytes());
        }
        out.push(b'\n');
    }
    out
}

/// `build_screen_dump` の error。
///
/// 注: `LayerNotImplemented` variant は Phase B 時代に scrollback/both を未実装で
/// 返すために存在したが、Phase C で配線完了して不要になったため削除済 (= 全 layer
/// が dispatch されるため layer 起因の error は発生しない、format 起因の error と
/// encode 失敗のみが残る)。
#[derive(Debug, thiserror::Error)]
pub(crate) enum ScreenDumpError {
    /// 未実装の format (= `Json`)。
    #[error("dump format not implemented: {0:?}")]
    FormatNotImplemented(ScreenDumpFormat),
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
        let out = build_screen_dump(&mut s, ScreenDumpFormat::Ansi, ScreenDumpLayer::Visible)
            .expect("ok");
        // ANSI dump は state_formatted の raw bytes (= ESC で始まる)
        assert!(out.starts_with(b"\x1b"));
    }

    #[test]
    fn dump_binary_returns_plain_text() {
        let mut s = ScreenState::new(3, 10, 0);
        s.process(b"hi\r\n");
        s.process(b"hello");
        let out = build_screen_dump(&mut s, ScreenDumpFormat::Binary, ScreenDumpLayer::Visible)
            .expect("ok");
        // plain text 経路: trim 済の "hi\nhello\n\n"
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(text.contains("hi"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn dump_text_plain_preserves_row_padding() {
        // 3x10 viewport に "hi" を 1 行目だけ書く。`TextPlain` は cell の空白を
        // 保持するため、各 row は cols=10 の char 列 + 改行で出力される。
        let mut s = ScreenState::new(3, 10, 0);
        s.process(b"hi");
        let out = build_screen_dump(
            &mut s,
            ScreenDumpFormat::TextPlain,
            ScreenDumpLayer::Visible,
        )
        .expect("ok");
        let text = std::str::from_utf8(&out).expect("utf8");
        // 期待: "hi" + 8 spaces + "\n" + 10 spaces + "\n" + 10 spaces + "\n"
        let expected = format!(
            "hi{}\n{}\n{}\n",
            " ".repeat(8),
            " ".repeat(10),
            " ".repeat(10)
        );
        assert_eq!(
            text, expected,
            "text-plain must preserve row padding + newlines"
        );
        // 行数 = rows
        assert_eq!(text.matches('\n').count(), 3);
    }

    #[test]
    fn dump_text_plain_has_no_ansi_escapes() {
        // 装飾を意図的に発行 (= ESC[31m 赤色 + ESC[1m bold)、`TextPlain` は escape
        // を含まないことを確認する。
        let mut s = ScreenState::new(2, 6, 0);
        s.process(b"\x1b[1;31mERR\x1b[0m");
        let out = build_screen_dump(
            &mut s,
            ScreenDumpFormat::TextPlain,
            ScreenDumpLayer::Visible,
        )
        .expect("ok");
        // ESC (0x1b) は 1 byte も含まれてはいけない
        assert!(
            !out.contains(&0x1b),
            "text-plain must strip ANSI escapes, got: {:?}",
            std::str::from_utf8(&out).unwrap_or("<invalid utf8>")
        );
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(
            text.starts_with("ERR"),
            "visible chars must be preserved: {text:?}"
        );
    }

    #[test]
    fn dump_text_plain_handles_wide_char_continuation() {
        // 全角文字 "あ" (= 2 col 幅) を入れて、継続 cell が空白で埋まらず
        // 文字列が正しく 1 つだけ出ることを確認。
        // 2x6 viewport: "あa" は 3 col (= 2+1) 使い、残り 3 col は空白。
        let mut s = ScreenState::new(2, 6, 0);
        s.process("あa".as_bytes());
        let out = build_screen_dump(
            &mut s,
            ScreenDumpFormat::TextPlain,
            ScreenDumpLayer::Visible,
        )
        .expect("ok");
        let text = std::str::from_utf8(&out).expect("utf8");
        // 1 行目 = "あa" + 3 spaces + "\n"、2 行目 = 6 spaces + "\n"
        let expected = format!("あa{}\n{}\n", " ".repeat(3), " ".repeat(6));
        assert_eq!(text, expected);
    }

    #[test]
    fn dump_cbor_encodes_snapshot() {
        let mut s = ScreenState::new(5, 40, 0);
        s.process(b"hi");
        let out = build_screen_dump(&mut s, ScreenDumpFormat::Cbor, ScreenDumpLayer::Visible)
            .expect("ok");
        // 復号して内容を確認
        let snap: ScreenSnapshot = ciborium::de::from_reader(out.as_slice()).expect("decode");
        assert_eq!(snap.cells.len(), 2);
    }

    #[test]
    fn dump_json_returns_not_implemented() {
        let mut s = ScreenState::new(5, 40, 0);
        let err = build_screen_dump(&mut s, ScreenDumpFormat::Json, ScreenDumpLayer::Visible)
            .expect_err("must error");
        match err {
            ScreenDumpError::FormatNotImplemented(ScreenDumpFormat::Json) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// scrollback_len=0 (= scrollback 無効) で `--layer=scrollback` を呼ぶと、
    /// text 系 (Ansi/Binary/TextPlain) は空 payload、Cbor は cells が空の Snapshot
    /// を返す (= error にはならない、設定上 scrollback を取らない構成への配慮)。
    #[test]
    fn dump_scrollback_empty_when_scrollback_disabled() {
        let mut s = ScreenState::new(3, 10, 0);
        for i in 0..20 {
            s.process(format!("L{i}\r\n").as_bytes());
        }
        // text-plain は空 string (= 0 行) を返す
        let out = build_screen_dump(
            &mut s,
            ScreenDumpFormat::TextPlain,
            ScreenDumpLayer::Scrollback,
        )
        .expect("ok");
        assert!(
            out.is_empty(),
            "scrollback disabled must return empty payload, got: {:?}",
            std::str::from_utf8(&out)
        );
        // cbor も cells が空、rows=0
        let cbor = build_screen_dump(&mut s, ScreenDumpFormat::Cbor, ScreenDumpLayer::Scrollback)
            .expect("ok");
        let snap: ScreenSnapshot = ciborium::de::from_reader(cbor.as_slice()).expect("decode");
        assert_eq!(snap.cells.len(), 0);
        assert_eq!(snap.rows, 0);
    }

    /// `--layer=scrollback` で text/plain format を呼ぶと、visible からスクロールアウトした
    /// 過去 marker が含まれ、新しいほど後方に並ぶ (= 古い → 新しい順)。
    #[test]
    fn dump_scrollback_text_plain_returns_old_rows() {
        let mut s = ScreenState::new(3, 10, 10);
        for i in 0..15 {
            s.process(format!("L{i}\r\n").as_bytes());
        }
        // visible は L13/L14/空、scrollback には L3..L12 が貯まる想定 (= 10 行 ring)。
        let out = build_screen_dump(
            &mut s,
            ScreenDumpFormat::TextPlain,
            ScreenDumpLayer::Scrollback,
        )
        .expect("ok");
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(
            text.contains("L3"),
            "scrollback should include old marker L3, got: {text:?}"
        );
        assert!(
            text.contains("L12"),
            "scrollback should include latest scrollback row L12, got: {text:?}"
        );
        // visible 側の L13/L14 は scrollback には含まれない
        assert!(
            !text.contains("L13"),
            "L13 should be visible, not scrollback, got: {text:?}"
        );
        // 古い → 新しい順なので L3 が L12 より先に現れる
        let pos_l3 = text.find("L3").expect("L3 present");
        let pos_l12 = text.find("L12").expect("L12 present");
        assert!(
            pos_l3 < pos_l12,
            "L3 should appear before L12 (chronological order): {text:?}"
        );
    }

    /// `--layer=scrollback --format=ansi` で payload を吐くと、ANSI escape を含み
    /// 過去 marker も含まれる。
    #[test]
    fn dump_scrollback_ansi_format() {
        let mut s = ScreenState::new(3, 10, 10);
        for i in 0..15 {
            s.process(format!("L{i}\r\n").as_bytes());
        }
        let out = build_screen_dump(&mut s, ScreenDumpFormat::Ansi, ScreenDumpLayer::Scrollback)
            .expect("ok");
        // 先頭は cursor home escape `\x1b[H`
        assert!(
            out.starts_with(b"\x1b[H"),
            "ANSI scrollback should start with cursor home, got first bytes: {:?}",
            &out[..out.len().min(8)]
        );
        // L3 marker を含む
        assert!(
            out.windows(b"L3".len()).any(|w| w == b"L3"),
            "ANSI scrollback should contain L3 marker"
        );
    }

    /// `--layer=both` は scrollback + visible を連結する。古い scrollback rows が先、
    /// 新しい visible rows が後。
    #[test]
    fn dump_both_concatenates_scrollback_and_visible() {
        let mut s = ScreenState::new(3, 10, 10);
        for i in 0..15 {
            s.process(format!("L{i}\r\n").as_bytes());
        }
        let out = build_screen_dump(&mut s, ScreenDumpFormat::TextPlain, ScreenDumpLayer::Both)
            .expect("ok");
        let text = std::str::from_utf8(&out).expect("utf8");
        // scrollback (L3..L12) + visible (L13, L14, _) の全 marker が含まれる
        for marker in ["L3", "L12", "L13", "L14"] {
            assert!(
                text.contains(marker),
                "both should contain {marker}, got: {text:?}"
            );
        }
        // L3 (= scrollback 最古) が L14 (= visible 最新) より前に来る
        let pos_l3 = text.find("L3").expect("L3 present");
        let pos_l14 = text.find("L14").expect("L14 present");
        assert!(
            pos_l3 < pos_l14,
            "scrollback L3 should appear before visible L14 (chronological order): {text:?}"
        );
        // 行数 = scrollback (10) + visible (3) = 13 行
        assert_eq!(
            text.matches('\n').count(),
            13,
            "both layer should output scrollback_rows + visible_rows lines, got: {text:?}"
        );
    }

    /// `--layer=both --format=cbor` は scrollback + visible の cells を 1 つの
    /// `ScreenSnapshot` に連結し、座標は連結後の 0-origin で振り直される。
    #[test]
    fn dump_both_cbor_uses_unified_coords() {
        let mut s = ScreenState::new(3, 10, 10);
        for i in 0..15 {
            s.process(format!("L{i}\r\n").as_bytes());
        }
        let out =
            build_screen_dump(&mut s, ScreenDumpFormat::Cbor, ScreenDumpLayer::Both).expect("ok");
        let snap: ScreenSnapshot = ciborium::de::from_reader(out.as_slice()).expect("decode");
        // rows field は scrollback (10) + visible (3) = 13 行を反映
        assert_eq!(snap.rows, 13, "both layer rows should be sb + visible");
        // cells には L3..L14 marker の "L" / 数字 cell が含まれる
        let text: String = snap.cells.iter().map(|cp| cp.cell.text.as_str()).collect();
        for marker_char in ["L", "3", "4", "5"] {
            assert!(
                text.contains(marker_char),
                "cells should include marker char {marker_char}, got: {text:?}"
            );
        }
    }
}
