//! 検証 5: scrollback アクセス
//! 大量の行を流し込んで scrollback に追い出す。
//! Screen::contents() / set_scrollback / scrollback() の動作確認。
//! 既存 hyoui の last_evicted_age 相当の取得可能性も評価。

#[test]
fn v05_scrollback_basic() {
    let scrollback_len = 100;
    let mut p = vt100::Parser::new(10, 40, scrollback_len);
    // 50 行流し込む (画面 10 行なので 40 行が scrollback に押し出される)
    for i in 0..50 {
        let line = format!("line-{i:03}\r\n");
        p.process(line.as_bytes());
    }

    let s = p.screen();
    // 現画面の最後の行は line-049 のはず
    let last_line: String = (0..40)
        .map(|c| s.cell(9, c).map(|x| x.contents().to_string()).unwrap_or_default())
        .collect();
    let trimmed = last_line.trim_end();
    eprintln!("v05 last row of viewport: {trimmed:?}");
    // cursor は \n の後で row=9 col=0 だが、最後の line-049\n が scroll up を起こす
    // ので画面 row 8 に line-049 が残るパターン。実際の row を出力してみる
    for r in 0..10 {
        let line: String = (0..40)
            .map(|c| s.cell(r, c).map(|x| x.contents().to_string()).unwrap_or_default())
            .collect();
        eprintln!("v05 viewport row {r}: {:?}", line.trim_end());
    }

    // scrollback 取得: set_scrollback で過去位置に
    {
        let s_mut = p.screen_mut();
        s_mut.set_scrollback(20);
        assert_eq!(s_mut.scrollback(), 20);
    }
    eprintln!("v05 after set_scrollback(20):");
    for r in 0..10 {
        let line: String = (0..40)
            .map(|c| p.screen().cell(r, c).map(|x| x.contents().to_string()).unwrap_or_default())
            .collect();
        eprintln!("  row {r}: {:?}", line.trim_end());
    }

    // contents() で全文 (scrollback offset 反映状態) を取得
    let contents = p.screen().contents();
    eprintln!("v05 contents() lines: {}", contents.lines().count());

    // reset
    p.screen_mut().set_scrollback(0);
    assert_eq!(p.screen().scrollback(), 0);

    eprintln!("v05: ✓ scrollback API works");
}

/// last_evicted_age 相当 API の有無を機械的に確認。
/// vt100 の public API には evicted age を取れるものがない可能性が高い (= 自前で count する必要)。
#[test]
fn v05_evicted_age_capability_probe() {
    let mut p = vt100::Parser::new(5, 20, 50);
    for i in 0..30 {
        p.process(format!("L{i}\r\n").as_bytes());
    }
    // 評価メモ: vt100 の Screen には evicted_count / total_lines_seen 的な API が公開されていない。
    // → 自前 counter を Parser ラッパで持つ必要がある (= hyoui 側で簡単に追加可能)。
    let s = p.screen();
    let contents = s.contents();
    let line_count = contents.lines().count();
    eprintln!("v05_evicted: scrollback contents lines = {line_count}");
    // contents は scrollback + viewport の combined。viewport=5 行なので 50+5=55 弱が見えるはず
    assert!(line_count <= 55);
    eprintln!("v05_evicted: ✗ no public evicted-age API → wrapper で feed 行数を自前 count 必要");
}
