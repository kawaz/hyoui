//! DR-0013 §4 Phase A — attach 復元 snapshot 検証。
//!
//! `hyoui run` で動いている session に対して `hyoui screen dump --format=ansi`
//! を複数回叩き、**同じ screen state が再現できる** ことを確認する。これは
//! attach 復元の最小単位の動作確認:
//!
//! - 子が確定的な出力 (`printf "hello\nworld\n"`) を出した後、screen dump で
//!   状態を取得
//! - 同じ screen dump を 2 回連続で叩いて結果が一致する (= idempotence)
//! - normalize_screen_dump で **非決定的要素** (= timestamp / PID 等) を伏せた
//!   結果を insta snapshot として固定
//!
//! ## このマトリクスのスコープ
//!
//! DR-0014 §マトリクス検証の category 多様性は本 file の scope では限定的
//! (= `sh -c` の line-oriented 出力のみ)。他 category (TUI alt screen / REPL)
//! は別 task で増やす。本 file は **screen dump 自体の信頼性**を 1 cell ずつ
//! 確認する基盤 test に専念する。

mod common;

use std::time::Duration;

use common::normalize::normalize_screen_dump;
use common::pty::HyouiTestRunner;

/// 子が定型出力を出すまで待つ helper。`hyoui screen dump` で stdout を取り、
/// 期待文字列が含まれるまで polling (= screen state 反映の同期 token)。
fn wait_for_dump_contains(
    runner_session: &str,
    socket: &std::path::Path,
    needle: &str,
    timeout: Duration,
) -> std::io::Result<Vec<u8>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last: Vec<u8> = Vec::new();
    while std::time::Instant::now() < deadline {
        let socket_arg = format!("--socket={}", socket.display());
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_hyoui"))
            .args(["screen", "dump", &socket_arg, "--format=ansi"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()?;
        if out.status.success() {
            last = out.stdout;
            if last.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                return Ok(last);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = runner_session; // suppress unused warning (= 将来 session 名を log に
    // 入れる拡張用 placeholder)
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "wait_for_dump_contains({needle:?}) timeout, last ({} bytes): {:?}",
            last.len(),
            String::from_utf8_lossy(&last)
        ),
    ))
}

/// matrix cell C1: `printf "hello\nworld\n"` を子に実行させ、screen dump で
/// "hello" と "world" の両方が見えることを確認。
///
/// **観点**: screen state 正本 (= daemon 内の vt100 ScreenState) が子の bytes を
/// 正しく反映しているか。DR-0013 §3 の Phase A 統合の最小確認。
#[test]
#[ignore = "matrix test: PTY/daemon を起動するため CI 環境 (= ubuntu-latest / macos-latest) で不安定。ローカルで `cargo test --workspace -- --ignored` を使って検証する。詳細は DR-0014 §マトリクス検証 + 2026-05-28 hotfix"]
fn restore_simple_echo_visible_in_screen_dump() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "restore-echo",
        &[
            "run",
            "--mode=headless",
            "--",
            "/bin/sh",
            "-c",
            "printf 'hello\\nworld\\n'; sleep 30",
        ],
    );
    // socket を runner から借りる (= harness の `spawn_hyoui` が socket arg を
    // 注入したパス)
    let socket = runner.socket_path("restore-echo");

    let dump = wait_for_dump_contains("restore-echo", &socket, "world", Duration::from_secs(3))
        .expect("hello/world should appear in screen dump");
    let normalized = normalize_screen_dump(&dump);
    // 両 keyword が見えていること (= screen state が子の bytes を取り込んでいる)
    assert!(
        normalized.contains("hello"),
        "normalized dump should contain 'hello', got: {normalized:?}"
    );
    assert!(
        normalized.contains("world"),
        "normalized dump should contain 'world', got: {normalized:?}"
    );

    h.kill().ok();
}

/// matrix cell C2: 同じ screen dump を 2 回連続で叩いた結果が一致する
/// (= idempotence)。これは新規 attach client が同じ state を見られるための前提。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_attach_restore 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn restore_dump_is_idempotent_across_calls() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "restore-idem",
        &[
            "run",
            "--mode=headless",
            "--",
            "/bin/sh",
            "-c",
            "printf 'one\\ntwo\\nthree\\n'; sleep 30",
        ],
    );
    let socket = runner.socket_path("restore-idem");

    // "three" まで反映を待つ
    let d1 = wait_for_dump_contains("restore-idem", &socket, "three", Duration::from_secs(3))
        .expect("three should appear in first dump");
    // 子の出力が固まったタイミングで再 dump (= 子は sleep 中なので state 変化なし)
    std::thread::sleep(Duration::from_millis(100));
    let socket_arg = format!("--socket={}", socket.display());
    let out2 = std::process::Command::new(env!("CARGO_BIN_EXE_hyoui"))
        .args(["screen", "dump", &socket_arg, "--format=ansi"])
        .output()
        .expect("second dump invocation");
    assert!(
        out2.status.success(),
        "second dump should succeed, stderr: {:?}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let d2 = out2.stdout;

    // 子は sleep 中 → state 変化なし → 2 dump は bytes 単位で一致するはず。
    // ただし daemon が SequenceNo 等の動的 metadata を含める可能性に備え、
    // normalize 後の文字列で比較する (= 揺らぐ field を伏せた上で確認)。
    let n1 = normalize_screen_dump(&d1);
    let n2 = normalize_screen_dump(&d2);
    assert_eq!(
        n1, n2,
        "two consecutive screen dumps should be identical (no state change between calls)"
    );

    h.kill().ok();
}

/// matrix cell C3: screen dump が ANSI sequence を含む形で出力されること
/// (= `--format=ansi` の動作確認)。
///
/// 期待: 制御 sequence (= cursor / mode setting) が含まれる。これは attach 復元
/// で「terminal を正しい mode に戻す」必要があるため (DR-0013 §4 attach redraw)。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_attach_restore 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn restore_dump_contains_ansi_control_sequences() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "restore-ansi",
        &[
            "run",
            "--mode=headless",
            "--",
            "/bin/sh",
            "-c",
            "printf 'marker\\n'; sleep 30",
        ],
    );
    let socket = runner.socket_path("restore-ansi");

    let dump = wait_for_dump_contains("restore-ansi", &socket, "marker", Duration::from_secs(3))
        .expect("marker should appear");

    // ESC (= 0x1b) が含まれていること
    assert!(
        dump.contains(&0x1b),
        "ansi dump should contain ESC (0x1b), got bytes: {:?}",
        String::from_utf8_lossy(&dump)
    );

    h.kill().ok();
}

/// matrix cell C4: 子の出力が screen に反映された後で、出力を含む normalized
/// snapshot を insta で固定する。子の出力 (= "marker A", "marker B") が cursor
/// 配置と共に正しく見えること。
///
/// この cell は **regression sensor** として働く: 将来 screen state の reformat
/// が起きると insta snapshot が壊れ、変更を意識的に review する trigger になる。
#[test]
#[ignore = "matrix test: CI 不安定のため ignore (matrix_attach_restore 全 test 同様)。ローカルは `cargo test -- --ignored`"]
fn restore_snapshot_normalized() {
    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "restore-snapshot",
        &[
            "run",
            "--mode=headless",
            "--",
            "/bin/sh",
            "-c",
            "printf 'marker A\\nmarker B\\n'; sleep 30",
        ],
    );
    let socket = runner.socket_path("restore-snapshot");

    let dump = wait_for_dump_contains(
        "restore-snapshot",
        &socket,
        "marker B",
        Duration::from_secs(3),
    )
    .expect("marker B should appear");
    let normalized = normalize_screen_dump(&dump);

    // ANSI escape を含む raw 形式の正規化結果を insta で固定。
    // insta snapshot は `cargo insta review` で初回承認する。CI 環境では
    // CARGO_INSTA_FORCE_PASS=1 等で auto-pending させる選択肢もあるが、
    // 本 file では default 挙動 (= snapshot 不在は failure) に従う。
    insta::assert_snapshot!("restore_snapshot_normalized", normalized);

    h.kill().ok();
}
