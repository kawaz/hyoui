//! 検証 1: 基本構築
//! Parser::new で 80x24 を構築し、"hello\n" を feed → 画面状態確認。

#[test]
fn v01_basic_construct_and_feed() {
    let mut p = vt100::Parser::new(24, 80, 1000);
    p.process(b"hello\n");
    let s = p.screen();
    assert_eq!(s.size(), (24, 80));

    // 1 行目に "hello" が入っているか
    let mut row0 = String::new();
    for c in 0..5 {
        row0.push_str(s.cell(0, c).unwrap().contents());
    }
    assert_eq!(row0, "hello");

    // vt100 は raw mode: \n は LF (line feed = row +1) のみで col リセットしない。
    // \r\n が必要 (= claude TUI など PTY 出力は通常 \r\n を出すため実用上問題なし)
    let (r, c) = s.cursor_position();
    assert_eq!((r, c), (1, 5), "raw \\n: row advances but col stays (typical raw mode)");

    // \r\n で動作確認
    let mut p2 = vt100::Parser::new(24, 80, 1000);
    p2.process(b"hello\r\n");
    assert_eq!(p2.screen().cursor_position(), (1, 0), "\\r\\n moves to next row col 0");

    eprintln!("v01: ✓ basic construct ok, raw=\\n: cursor=({r},{c}), \\r\\n: cursor=(1,0)");
}
