//! 検証 8: wide char / grapheme cluster
//! 日本語 + emoji + combining char を含む文字列を feed。
//! Cell::contents() で grapheme cluster 保持されているか確認。

#[test]
fn v08_japanese_wide() {
    let mut p = vt100::Parser::new(5, 40, 100);
    p.process("あいうえお".as_bytes());

    let s = p.screen();
    // 「あ」は wide なので (0, 0) が "あ" + is_wide=true, (0, 1) が wide continuation
    let c0 = s.cell(0, 0).unwrap();
    let c1 = s.cell(0, 1).unwrap();
    eprintln!("v08 cell(0,0): contents={:?} is_wide={} is_wide_cont={}",
              c0.contents(), c0.is_wide(), c0.is_wide_continuation());
    eprintln!("v08 cell(0,1): contents={:?} is_wide={} is_wide_cont={}",
              c1.contents(), c1.is_wide(), c1.is_wide_continuation());

    assert_eq!(c0.contents(), "あ", "first cell should be あ");
    assert!(c0.is_wide(), "あ should be wide");
    assert!(c1.is_wide_continuation(), "next cell should be wide continuation");

    // 5 文字 * 2 cols = 10 cols
    let c8 = s.cell(0, 8).unwrap();
    assert_eq!(c8.contents(), "お");

    let (_, cc) = s.cursor_position();
    eprintln!("v08 cursor after あいうえお: col={cc}");
    assert_eq!(cc, 10, "cursor should advance 10 cols for 5 wide chars");
}

#[test]
fn v08_emoji() {
    let mut p = vt100::Parser::new(5, 40, 100);
    // 単純な emoji
    p.process("🎉".as_bytes());
    let s = p.screen();
    let c0 = s.cell(0, 0).unwrap();
    eprintln!("v08 emoji cell(0,0): contents={:?} is_wide={}", c0.contents(), c0.is_wide());
    // emoji presentation は wide
    assert_eq!(c0.contents(), "🎉");
}

#[test]
fn v08_combining_char() {
    let mut p = vt100::Parser::new(5, 40, 100);
    // "é" を combining (e + U+0301) で表現
    let s = "e\u{0301}"; // 2 code points, 1 grapheme cluster
    p.process(s.as_bytes());
    let scr = p.screen();
    let c0 = scr.cell(0, 0).unwrap();
    eprintln!("v08 combining cell(0,0): contents={:?} (bytes={:?})",
              c0.contents(), c0.contents().as_bytes());
    // grapheme cluster として 1 セルに収まっているか
    // (= "e\u{0301}" 全体が 1 cell の contents になるべき)
    if c0.contents() == "e\u{0301}" {
        eprintln!("v08 combining: ✓ grapheme cluster preserved");
    } else {
        eprintln!("v08 combining: ✗ grapheme split (contents={:?})", c0.contents());
    }
}

#[test]
fn v08_mixed() {
    let mut p = vt100::Parser::new(5, 40, 100);
    p.process("Hi 日本 🎉 e\u{0301}!".as_bytes());
    let s = p.screen();
    for c in 0..20 {
        if let Some(cell) = s.cell(0, c) {
            if cell.has_contents() || cell.is_wide_continuation() {
                eprintln!("v08 mixed cell({c}): contents={:?} wide={} cont={}",
                    cell.contents(), cell.is_wide(), cell.is_wide_continuation());
            }
        }
    }
}
