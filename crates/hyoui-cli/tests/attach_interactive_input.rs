//! interactive tty attach の打鍵経路の regression test。
//!
//! 保証すること: attach client の run loop が stdin 打鍵を raw_data frame として
//! daemon に forward した後、daemon が write 完了ごとに返す `TYPE_RAW_ACK` frame
//! (DR-0021 の完了点 ack) を **読み捨てて生存し続ける** こと。
//!
//! この経路は `hyoui input` (= ack を同期消費する自動化 API) と独立で、e2e が
//! 1 件も無いと「daemon が新 frame type を返すようになった時に interactive 打鍵が
//! 全滅する」退行を検出できない。実際に DR-0021 land 時、run loop が RawAck を
//! unknown frame 扱いして初回打鍵で client が異常終了する退行が起きた
//! (docs/issue/2026-07-03-bug-attach-run-loop-drops-rawack.md)。
//!
//! `#[ignore]`: 子 process + 実 PTY + daemon 起動を伴うため、`--ignored` 実行。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// PTY 内で `hyoui run -- cat` を起動し、2 回打鍵しても client が生きて
/// 出力を中継し続けることを確認する。
///
/// - 1 打鍵目: raw_data forward → daemon の RawAck が client に届く。ここで
///   client が ack を処理できないと異常終了し、2 打鍵目の echo は永遠に出ない
/// - 2 打鍵目の echo 確認 = 「ack 受信後も run loop が継続している」ことの証跡
#[ignore = "子 process 起動 + 実 PTY + daemon を使う、--ignored 実行 (DR-0021 regression)"]
#[test]
fn interactive_typing_survives_raw_ack() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("interactive-ack", &["run", "--", "cat"]);

    // attach 成立の目印 (= stderr hint、DR-0020) を待ってから打鍵する。
    let _ = h.wait_for("detach", Duration::from_secs(5));

    h.send_bytes(b"first-ping\n").expect("send first line");
    let out1 = h
        .wait_for("first-ping", Duration::from_secs(5))
        .expect("first keystroke should be echoed back (client must survive RawAck)");
    assert!(out1.contains("first-ping"), "echo of first line: {out1:?}");

    h.send_bytes(b"second-ping\n").expect("send second line");
    let out2 = h
        .wait_for("second-ping", Duration::from_secs(5))
        .expect("second keystroke should be echoed back (run loop still alive after ack)");
    assert!(
        out2.contains("second-ping"),
        "echo of second line: {out2:?}"
    );

    let _ = h.kill();
}
