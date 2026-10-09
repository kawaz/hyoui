//! PoC: vt100 crate evaluation for hyoui DR-0013.
//!
//! 主目的:
//! 1. vt100 0.16.2 の使用感を測る
//! 2. state_formatted() の往復 (= attach 復元) 検証
//! 3. reflow 品質を実機確認
//! 4. Cell/Screen の serde 不在を wrapper で補えるか実証
//!
//! 構成: 各検証は tests/ ディレクトリの統合テストに分離。
//! 本 lib.rs は CBOR snapshot wrapper の実装本体を提供する。

use serde::{Deserialize, Serialize};

/// vt100::Color の serializable mirror
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorSnap {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl Default for ColorSnap {
    fn default() -> Self {
        ColorSnap::Default
    }
}

impl From<vt100::Color> for ColorSnap {
    fn from(c: vt100::Color) -> Self {
        match c {
            vt100::Color::Default => ColorSnap::Default,
            vt100::Color::Idx(i) => ColorSnap::Idx(i),
            vt100::Color::Rgb(r, g, b) => ColorSnap::Rgb(r, g, b),
        }
    }
}

/// 1 セル分の snapshot。grapheme cluster は String 保持。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CellSnap {
    pub ch: String,
    pub fg: ColorSnap,
    pub bg: ColorSnap,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub is_wide: bool,
    pub is_wide_cont: bool,
}

impl From<&vt100::Cell> for CellSnap {
    fn from(c: &vt100::Cell) -> Self {
        CellSnap {
            ch: c.contents().to_string(),
            fg: c.fgcolor().into(),
            bg: c.bgcolor().into(),
            bold: c.bold(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
            is_wide: c.is_wide(),
            is_wide_cont: c.is_wide_continuation(),
        }
    }
}

/// 画面全体の snapshot。reflow/復元のため input mode 一式も保存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSnap {
    pub rows: u16,
    pub cols: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub alternate_screen: bool,
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub cells: Vec<Vec<CellSnap>>, // [row][col]
}

impl ScreenSnap {
    pub fn from_screen(s: &vt100::Screen) -> Self {
        let (rows, cols) = s.size();
        let (cr, cc) = s.cursor_position();
        let mut cells = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols as usize);
            for c in 0..cols {
                let cell = s.cell(r, c).map(CellSnap::from).unwrap_or_default();
                row.push(cell);
            }
            cells.push(row);
        }
        ScreenSnap {
            rows,
            cols,
            cursor_row: cr,
            cursor_col: cc,
            cursor_visible: !s.hide_cursor(),
            alternate_screen: s.alternate_screen(),
            application_keypad: s.application_keypad(),
            application_cursor: s.application_cursor(),
            bracketed_paste: s.bracketed_paste(),
            cells,
        }
    }

    /// CBOR (ciborium) で encode
    pub fn to_cbor(&self) -> Result<Vec<u8>, ciborium::ser::Error<std::io::Error>> {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf)?;
        Ok(buf)
    }

    /// CBOR から decode
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, ciborium::de::Error<std::io::Error>> {
        ciborium::from_reader(bytes)
    }
}
