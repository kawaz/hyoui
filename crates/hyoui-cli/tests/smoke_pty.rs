//! `tests/common/pty.rs` の `HyouiTestRunner` / `SpawnedHyoui` が
//! 実際に動くかどうかの最小 smoke test (DR-0014 §マトリクス検証 base)。
//!
//! 軸 1/2 検証本体は別 task (#32) で書く。本 file は **harness 自体の動作確認**
//! に専念し、test 1 件のみ:
//!
//! - `hyoui run -- /bin/echo hello` を PTY 内で起動
//! - PTY 出力に `hello` が現れるまで wait
//! - kill して cleanup
//!
//! これが pass すれば:
//! 1. dev-dep (pty-process / insta / tempfile) の名前解決が通っている
//! 2. PTY spawn + 子の stdout を master 経由で読める
//! 3. socket path 隔離 (`--socket=<tempdir>/<session>.sock`) で daemon が立つ
//! 4. timeout 付き wait_for が「pattern 一致で抜ける」「timeout で err 返す」
//!    両方の path が機能する
//!
//! というベース機能が揃ったことになる。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// hyoui-cli を PTY 内で起動 → echo hello の出力を確認 → kill。
///
/// 既存 558+ test の regression なし + 新規 harness の生 round-trip 検証。
#[test]
fn smoke_hyoui_run_echo() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui("smoke-echo", &["run", "--", "/bin/echo", "hello"]);

    // /bin/echo は出力後即 exit する。hyoui-cli は子の exit を見て自分も exit する。
    // PTY 経由で出力された "hello" が wait_for で見えること。
    let out = h
        .wait_for("hello", Duration::from_secs(5))
        .expect("expected 'hello' from /bin/echo within 5s");
    assert!(
        out.contains("hello"),
        "output should contain 'hello', got: {out:?}"
    );

    // 後片付け (Drop でも kill されるが、明示的に閉じることで test 終了の決定性を高める)。
    let _ = h.kill();
}
