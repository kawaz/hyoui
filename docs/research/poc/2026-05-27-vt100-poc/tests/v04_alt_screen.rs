//! 検証 4: alternate screen 切替
//! 1049h で alt on → 描画 → 1049l で primary 復帰。primary が保たれているか。

#[test]
fn v04_alt_screen_switch() {
    let mut p = vt100::Parser::new(10, 40, 100);
    // primary 描画
    p.process(b"\x1b[Hprimary line 1\r\nprimary line 2");
    assert!(!p.screen().alternate_screen());

    let primary_cell_before = p.screen().cell(0, 0).unwrap().contents().to_string();
    assert_eq!(primary_cell_before, "p");

    // alt on
    p.process(b"\x1b[?1049h\x1b[2J\x1b[HALT 1\r\nALT 2");
    assert!(p.screen().alternate_screen());
    assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "A");

    // alt off
    p.process(b"\x1b[?1049l");
    assert!(!p.screen().alternate_screen(), "should be back to primary");

    // primary が保たれているか
    let cell_after = p.screen().cell(0, 0).unwrap().contents().to_string();
    assert_eq!(cell_after, "p", "primary content should be preserved through alt-screen excursion");

    eprintln!("v04: ✓ alt screen save/restore ok");
}
