//! 検証 2: state_formatted() の往復
//! 入力を feed → state_formatted() で bytes 生成 → 別 Parser に feed → 一致確認。
//! これが DR-0013 の attach 復元 protocol のコア。

fn diag(p: &vt100::Parser) -> String {
    let s = p.screen();
    let (rows, cols) = s.size();
    let (cr, cc) = s.cursor_position();
    let mut out = format!("size={rows}x{cols} cursor=({cr},{cc}) alt={} ", s.alternate_screen());
    for r in 0..rows {
        for c in 0..cols {
            let ch = s.cell(r, c).map(|x| x.contents().to_string()).unwrap_or_default();
            out.push_str(&ch);
        }
        out.push('|');
    }
    out
}

#[test]
fn v02_state_formatted_roundtrip_simple() {
    let input = b"hello\x1b[1;31mRED\x1b[0m world\r\nline2";
    let mut p1 = vt100::Parser::new(5, 40, 100);
    p1.process(input);

    let formatted = p1.screen().state_formatted();
    eprintln!("v02 formatted bytes len: {}", formatted.len());

    let mut p2 = vt100::Parser::new(5, 40, 100);
    p2.process(&formatted);

    let d1 = diag(&p1);
    let d2 = diag(&p2);
    eprintln!("v02 original: {d1}");
    eprintln!("v02 restored: {d2}");
    assert_eq!(d1, d2, "diag mismatch after state_formatted roundtrip");

    // cell の attributes も比較
    let s1 = p1.screen();
    let s2 = p2.screen();
    for r in 0..5 {
        for c in 0..40 {
            let c1 = s1.cell(r, c).unwrap();
            let c2 = s2.cell(r, c).unwrap();
            assert_eq!(c1.contents(), c2.contents(), "char mismatch at ({r},{c})");
            assert_eq!(c1.fgcolor(), c2.fgcolor(), "fg mismatch at ({r},{c})");
            assert_eq!(c1.bgcolor(), c2.bgcolor(), "bg mismatch at ({r},{c})");
            assert_eq!(c1.bold(), c2.bold(), "bold mismatch at ({r},{c})");
        }
    }
    eprintln!("v02: ✓ state_formatted roundtrip ok");
}

#[test]
fn v02_state_formatted_with_alt_screen() {
    // alt screen + cursor 移動入り
    let input = b"primary\r\n\x1b[?1049h\x1b[2J\x1b[Halt-screen text\x1b[5;10H@";
    let mut p1 = vt100::Parser::new(10, 40, 100);
    p1.process(input);
    assert!(p1.screen().alternate_screen(), "should be in alt screen");

    let formatted = p1.screen().state_formatted();
    let mut p2 = vt100::Parser::new(10, 40, 100);
    p2.process(&formatted);

    let alt1 = p1.screen().alternate_screen();
    let alt2 = p2.screen().alternate_screen();
    eprintln!("v02 alt: p1.alt={alt1} p2.alt={alt2}");
    eprintln!("v02 alt formatted bytes (hex): {}",
        formatted.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    eprintln!("v02 alt formatted bytes (ascii): {:?}",
        String::from_utf8_lossy(&formatted));
    let d1 = diag(&p1);
    let d2 = diag(&p2);
    eprintln!("v02 alt original: {d1}");
    eprintln!("v02 alt restored: {d2}");
    // 観測ベースで判定 (= ✗ なら設計上重要)
    if alt1 != alt2 {
        eprintln!("v02 alt: ✗ alt-screen flag NOT preserved by state_formatted()");
    } else if d1 != d2 {
        eprintln!("v02 alt: 部分的 - alt flag は OK だが画面内容に差分");
    } else {
        eprintln!("v02 alt: ✓ alt screen roundtrip ok");
    }
    // PoC なので失敗時も継続したい → assert ではなく観測ログのみ
    // ただしこのテスト自体は「分析」目的なので panic させない
}
