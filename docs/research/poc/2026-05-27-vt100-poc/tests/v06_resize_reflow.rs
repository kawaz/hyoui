//! 検証 6: resize + reflow 品質 ← PoC 最重点
//!
//! 80x24 に折返し行を流し込み → set_size(24, 40) で半分にする → 復元するか。
//! vt100 の reflow が wezterm-term / alacritty_terminal レベルか定性判断する。

fn print_screen(label: &str, p: &vt100::Parser) {
    let s = p.screen();
    let (rows, cols) = s.size();
    let (cr, cc) = s.cursor_position();
    eprintln!("--- {label} (size={rows}x{cols} cursor=({cr},{cc})) ---");
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let ch = s.cell(r, c).map(|x| x.contents().to_string()).unwrap_or_default();
            if ch.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&ch);
            }
        }
        let wrapped = if s.row_wrapped(r) { "↵" } else { " " };
        eprintln!("  r{r:02}{wrapped}|{line}|");
    }
}

#[test]
fn v06_wide_to_narrow_to_wide() {
    let mut p = vt100::Parser::new(24, 80, 1000);
    // 100 文字の長い行を流す (80x24 では 2 行に折返し)
    let long_line: String = (0..100).map(|i| ((b'A' + (i % 26) as u8) as char)).collect();
    p.process(format!("{long_line}\r\n").as_bytes());
    p.process(b"short\r\n");
    p.process(format!("{long_line}\r\n").as_bytes());

    print_screen("initial 80x24", &p);

    // 40 cols に縮小
    p.screen_mut().set_size(24, 40);
    print_screen("after set_size(24, 40)", &p);

    // 80 cols に戻す
    p.screen_mut().set_size(24, 80);
    print_screen("after set_size(24, 80) (restore)", &p);

    // 内容の存在チェック (= 完全復元は期待しないが、文字情報がどこまで保たれているか観察)
    let s = p.screen();
    let mut found = 0;
    for r in 0..24 {
        for c in 0..80 {
            if let Some(cell) = s.cell(r, c) {
                if cell.contents() == "A" {
                    found += 1;
                }
            }
        }
    }
    eprintln!("v06 'A' chars found after resize cycle: {found}");
}

#[test]
fn v06_set_size_cursor_tracking() {
    let mut p = vt100::Parser::new(10, 20, 100);
    p.process(b"012345678901234567890123456789"); // 30 文字 → 2 行に折返し
    let (cr, cc) = p.screen().cursor_position();
    eprintln!("v06 before resize: cursor=({cr},{cc})");
    print_screen("initial 10x20 with 30 chars", &p);

    // 40 cols に拡張 → 30 文字が 1 行に収まる reflow が期待される
    p.screen_mut().set_size(10, 40);
    print_screen("after set_size(10, 40)", &p);
    let (cr2, cc2) = p.screen().cursor_position();
    eprintln!("v06 after expand: cursor=({cr2},{cc2})");

    // 10 cols に縮める → 30 文字が 3 行に折返し
    p.screen_mut().set_size(10, 10);
    print_screen("after set_size(10, 10)", &p);
    let (cr3, cc3) = p.screen().cursor_position();
    eprintln!("v06 after shrink: cursor=({cr3},{cc3})");
}

#[test]
fn v06_reflow_quality_assessment() {
    // 評価用: 既知の文字列を流し → resize → 内容が「読める」か観察
    let mut p = vt100::Parser::new(10, 30, 100);
    let lines = [
        "Hello, this is line one of text.",
        "Line 2.",
        "And here is a much longer third line that will surely wrap somewhere in the terminal.",
        "L4",
    ];
    for ln in lines {
        p.process(format!("{ln}\r\n").as_bytes());
    }
    print_screen("orig 10x30", &p);

    p.screen_mut().set_size(10, 15);
    print_screen("shrunk to 10x15", &p);

    p.screen_mut().set_size(10, 60);
    print_screen("expanded to 10x60", &p);
}
