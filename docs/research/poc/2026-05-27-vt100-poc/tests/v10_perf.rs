//! 検証 10: パフォーマンス感覚 (= benchmark ではなく目安)
//! 100KB の claude TUI dump を feed → 時間、state_formatted 生成時間。

use std::time::Instant;

fn build_100kb_input() -> Vec<u8> {
    // 仮想的な TUI dump 風の入力
    let mut v = Vec::with_capacity(120_000);
    v.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[H");
    let mut i = 0;
    while v.len() < 100_000 {
        // cursor 移動 + 色付け + 文字列
        let line = format!(
            "\x1b[{};1H\x1b[3{}m[line {:04}]\x1b[0m some long body text 0123456789 ",
            (i % 24) + 1,
            i % 8,
            i
        );
        v.extend_from_slice(line.as_bytes());
        // 日本語も入れる
        if i % 10 == 0 {
            v.extend_from_slice("日本語混在テスト".as_bytes());
        }
        // 部分更新もあちこちに
        if i % 7 == 0 {
            v.extend_from_slice(b"\x1b[5;30H\x1b[7mUPDATED\x1b[0m");
        }
        i += 1;
    }
    v
}

#[test]
fn v10_perf_feed_100kb() {
    let input = build_100kb_input();
    eprintln!("v10 input size: {} bytes", input.len());

    // debug build なので参考値。release では 5-10x 速いはず
    let mut p = vt100::Parser::new(24, 80, 1000);
    let t = Instant::now();
    p.process(&input);
    let feed_us = t.elapsed().as_micros();
    eprintln!("v10 feed time (debug): {feed_us} us = {:.2} ms", feed_us as f64 / 1000.0);

    let t = Instant::now();
    let formatted = p.screen().state_formatted();
    let fmt_us = t.elapsed().as_micros();
    eprintln!("v10 state_formatted (debug): {fmt_us} us = {:.2} ms, output {} bytes",
        fmt_us, formatted.len());

    // ScreenSnap CBOR 生成も計測
    use poc_vt100::ScreenSnap;
    let t = Instant::now();
    let snap = ScreenSnap::from_screen(p.screen());
    let snap_us = t.elapsed().as_micros();
    let t = Instant::now();
    let cbor = snap.to_cbor().unwrap();
    let cbor_us = t.elapsed().as_micros();
    eprintln!("v10 ScreenSnap::from_screen (debug): {snap_us} us");
    eprintln!("v10 snap.to_cbor (debug): {cbor_us} us = {} bytes CBOR", cbor.len());
}
