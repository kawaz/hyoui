//! winsize 未設定 (= 0x0) の PTY 上で `hyoui run` が起動できることの回帰 test。
//!
//! issue 2026-06-11 / 2026-07-30: `TIOCSWINSZ` を一度も呼ばれていない PTY
//! (= `pty.openpty()` / `script -q /dev/null` / CI harness 経由) で起動すると、
//! 0x0 がそのまま vt100 grid に渡って `attempt to subtract with overflow` で
//! daemon が panic し、socket すら作られなかった。
//!
//! 修正の骨子は「0 は『サイズが取れなかった』の意味として扱う」正規化:
//! `sys::tty_size` が 0x0 で `None` を返し、初期サイズ経路が daemon default
//! (80x24) に倒れる。本 test はその end-to-end (= 実 PTY + 実 daemon) を押さえる。

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// 0x0 PTY 上で `hyoui run -- /bin/cat` が panic せずに立ち上がり、
/// status が引ける (= daemon が socket を bind できている) こと。
#[test]
fn run_on_unsized_pty_starts_with_default_size() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui_unsized_pty("zero-size-run", &["run", "--", "/bin/cat"]);

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "0x0 PTY でも daemon が起動して attach client が leader になるべき \
         (= panic して socket が作られない旧挙動の回帰): pty out={:?}",
        String::from_utf8_lossy(&h.drain_output())
    );

    let status = h.status_text().expect("status should succeed on 0x0 PTY");
    assert!(
        status.contains("child-state: running"),
        "子 process が起動しているべき: status={status:?}"
    );

    // 0x0 は「取れなかった」扱いなので daemon default 80x24 に倒れる
    // (= 1x1 に clamp して「起動はしたが画面が潰れている」ではない)。
    let snapshot = h
        .screen_snapshot_text()
        .expect("screen snapshot should succeed on 0x0 PTY");
    assert!(
        snapshot.contains("\"rows\": 24") && snapshot.contains("\"cols\": 80"),
        "0x0 は daemon default 80x24 に倒れるべき: snapshot={snapshot:?}"
    );

    let _ = h.kill();
}
