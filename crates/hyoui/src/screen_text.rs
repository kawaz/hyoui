//! 画面の 1 行分の cell 列を text にする (DR-0006 §9.1 / DR-0013 §9)。
//!
//! `hyoui wait` (と `hyoui input` の `wait:` spec) が照合する text と、`hyoui screen dump`
//! の text 系 format (`binary` / `text`) は、どちらも本 module の [`row_text`] で 1 行を作る。
//! 同じ画面に対して「dump では見えるのに wait が一致しない」食い違いを作らないため、
//! cell 列から文字列への変換規則はここ 1 箇所に置く:
//!
//! - 全角文字は先頭 cell の文字だけを出し、継続 cell ([`TextCell::WideContinuation`]) は
//!   何も出さない (= 端末で連続して見える文字列は text でも連続する)
//! - 空の cell は半角空白 1 個にする
//!
//! cell 列の取り出し方 (daemon は screen state から、wait は snapshot の sparse cells から) は
//! 呼び出し側の責務で、本 module は取り出した列の連結だけを担う。

/// text 化の入力にする 1 cell。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextCell<'a> {
    /// 文字を持つ (or 空の) cell。空文字列は半角空白 1 個として出す。
    Cell(&'a str),
    /// 全角文字の 2 列目 (= 直前の cell の文字が 2 列を占めている)。何も出さない。
    WideContinuation,
}

/// 1 行分の cell 列を text にする。`trim_trailing` なら行末の半角空白を落とす。
pub fn row_text<'a>(cells: impl IntoIterator<Item = TextCell<'a>>, trim_trailing: bool) -> String {
    let mut line = String::new();
    for cell in cells {
        match cell {
            TextCell::Cell("") => line.push(' '),
            TextCell::Cell(s) => line.push_str(s),
            TextCell::WideContinuation => {}
        }
    }
    if trim_trailing {
        let len = line.trim_end_matches(' ').len();
        line.truncate(len);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_continuation_emits_nothing() {
        let cells = [
            TextCell::Cell("停"),
            TextCell::WideContinuation,
            TextCell::Cell("止"),
            TextCell::WideContinuation,
            TextCell::Cell("中"),
            TextCell::WideContinuation,
        ];
        assert_eq!(row_text(cells, true), "停止中");
    }

    #[test]
    fn empty_cell_is_one_space_and_trim_is_optional() {
        let cells = [
            TextCell::Cell("a"),
            TextCell::Cell(""),
            TextCell::Cell("b"),
            TextCell::Cell(""),
            TextCell::Cell(" "),
        ];
        assert_eq!(row_text(cells, false), "a b  ");
        assert_eq!(row_text(cells, true), "a b");
    }
}
