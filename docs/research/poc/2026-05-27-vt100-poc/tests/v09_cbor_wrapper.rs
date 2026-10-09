//! 検証 9: CBOR snapshot wrapper 試作
//! lib.rs の ScreenSnap / CellSnap を使い、encode → decode → 比較。

use poc_vt100::{CellSnap, ColorSnap, ScreenSnap};

#[test]
fn v09_cbor_roundtrip() {
    let mut p = vt100::Parser::new(10, 40, 100);
    p.process(b"hello\x1b[1;31mRED\x1b[0m world\r\nLine2 with \x1b[4munderline\x1b[0m");
    p.process("日本語\r\n".as_bytes());
    p.process(b"\x1b[?25l"); // hide cursor

    let snap = ScreenSnap::from_screen(p.screen());
    eprintln!("v09 snap: rows={} cols={} cursor=({},{}) hide={}",
        snap.rows, snap.cols, snap.cursor_row, snap.cursor_col, !snap.cursor_visible);

    let cbor = snap.to_cbor().expect("encode");
    eprintln!("v09 cbor bytes: {}", cbor.len());

    let snap2 = ScreenSnap::from_cbor(&cbor).expect("decode");
    assert_eq!(snap, snap2, "CBOR roundtrip should preserve");

    // 特定 cell の attr 確認
    let red_r = &snap.cells[0][5]; // "R" of "RED"
    assert_eq!(red_r.ch, "R");
    assert_eq!(red_r.fg, ColorSnap::Idx(1)); // 31 = red index 1
    assert!(red_r.bold);

    eprintln!("v09: ✓ CBOR wrapper roundtrip ok, snap = {} bytes CBOR", cbor.len());
}

#[test]
fn v09_implementation_volume() {
    // wrapper の実装ボリュームを記録する目的
    // lib.rs 全体で何行か:
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read lib.rs");
    let lines = src.lines().count();
    let nonempty = src.lines().filter(|l| !l.trim().is_empty() && !l.trim().starts_with("//")).count();
    eprintln!("v09 lib.rs: {lines} total lines, {nonempty} non-comment-non-blank lines");
    // 50 行程度の見込みに対し実測
}

#[test]
fn v09_cbor_size_vs_state_formatted() {
    // 同じ画面の CBOR snapshot vs state_formatted() の bytes 量比較
    let mut p = vt100::Parser::new(24, 80, 1000);
    // 適度に色付き行を流す
    for i in 0..20 {
        let line = format!("\x1b[{};1H\x1b[3{}mLine {i:02}: hello world", i + 1, i % 8);
        p.process(line.as_bytes());
    }

    let formatted = p.screen().state_formatted();
    let snap = ScreenSnap::from_screen(p.screen());
    let cbor = snap.to_cbor().unwrap();

    eprintln!("v09 size compare:");
    eprintln!("  state_formatted: {} bytes", formatted.len());
    eprintln!("  CBOR snapshot:   {} bytes", cbor.len());
    eprintln!("  ratio (cbor/fmt): {:.2}", cbor.len() as f64 / formatted.len() as f64);
}

#[test]
fn v09_cellsnap_default() {
    let cs = CellSnap::default();
    assert_eq!(cs.ch, "");
    assert_eq!(cs.fg, ColorSnap::Default);
}
