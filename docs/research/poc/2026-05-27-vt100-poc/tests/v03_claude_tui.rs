//! 検証 3: claude TUI 風 ANSI 入力
//! alt screen on + cursor 移動 + color + box drawing + 部分 update。
//! state_formatted() で再現可能かを確認。

fn build_claude_like_input() -> Vec<u8> {
    let mut v = Vec::new();
    // alt screen on, clear, home
    v.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[H");
    // box drawing top
    v.extend_from_slice("┌────────────────────────────────────────┐\r\n".as_bytes());
    v.extend_from_slice("│".as_bytes());
    v.extend_from_slice(b" \x1b[1;36mClaude Code\x1b[0m");
    v.extend_from_slice(b"                              ");
    v.extend_from_slice("│\r\n".as_bytes());
    v.extend_from_slice("├────────────────────────────────────────┤\r\n".as_bytes());
    // body
    for i in 0..5 {
        v.extend_from_slice("│".as_bytes());
        v.extend_from_slice(format!(" line {i:02}                                  ").as_bytes());
        v.extend_from_slice("│\r\n".as_bytes());
    }
    v.extend_from_slice("└────────────────────────────────────────┘\r\n".as_bytes());
    // 部分 update: row 5 col 8 を反転で再描画
    v.extend_from_slice(b"\x1b[5;8H\x1b[7mUPDATED\x1b[0m");
    // status line right-aligned-ish
    v.extend_from_slice(b"\x1b[10;1H\x1b[33m>\x1b[0m typing...");
    v
}

fn diag(p: &vt100::Parser) -> String {
    let s = p.screen();
    let (rows, cols) = s.size();
    let (cr, cc) = s.cursor_position();
    let mut out = format!("size={rows}x{cols} cursor=({cr},{cc}) alt={} hide={}\n", s.alternate_screen(), s.hide_cursor());
    for r in 0..rows {
        out.push('|');
        for c in 0..cols {
            let ch = s.cell(r, c).map(|x| x.contents().to_string()).unwrap_or_default();
            if ch.is_empty() {
                out.push(' ');
            } else {
                out.push_str(&ch);
            }
        }
        out.push_str("|\n");
    }
    out
}

#[test]
fn v03_claude_tui_like() {
    let input = build_claude_like_input();
    let mut p1 = vt100::Parser::new(24, 80, 1000);
    p1.process(&input);

    let s1 = p1.screen();
    assert!(s1.alternate_screen(), "should be in alt screen");

    // (5, 8) 付近に "UPDATED" の U が来るはず (row=4, col=7 in 0-origin)
    let cu = s1.cell(4, 7).unwrap();
    assert_eq!(cu.contents(), "U", "row 5 col 8 should be 'U' from updated");
    assert!(cu.inverse(), "updated text should be inverse");

    // state_formatted roundtrip
    let formatted = s1.state_formatted();
    eprintln!("v03 formatted len: {}", formatted.len());
    let mut p2 = vt100::Parser::new(24, 80, 1000);
    p2.process(&formatted);

    let d1 = diag(&p1);
    let d2 = diag(&p2);
    eprintln!("v03 original:\n{d1}");
    eprintln!("v03 restored:\n{d2}");
    // alt フラグ単独で差分があることは v02 で判明済み。それ以外の本体一致を確認:
    // diag の "alt=true" / "alt=false" 部分だけ無視して比較
    let strip_alt = |s: &str| s.replace("alt=true", "alt=X").replace("alt=false", "alt=X");
    assert_eq!(strip_alt(&d1), strip_alt(&d2), "claude TUI restore mismatch (ignoring alt flag)");

    // formatted bytes を実 terminal で再生してよい形式かザッと確認
    // (= ESC で始まり、box drawing が含まれている)
    assert!(formatted.starts_with(b"\x1b"), "state_formatted should start with ESC");
    let needle = "┌".as_bytes();
    assert!(formatted.windows(needle.len()).any(|w| w == needle), "should contain box drawing");

    eprintln!("v03: ✓ claude TUI restored ok");
}
