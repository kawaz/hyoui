//! DR-0016 Phase 7: `hyoui record` CLI 整合性 integration test。
//!
//! daemon 側 hook 配線 (Phase 4) 完了前でも検証できる **parse 段 reject** を
//! 中心に検証する。daemon との完全 round-trip (= record start → list → stop の
//! 連動) は Phase 4 完了後に別 file (`record_e2e.rs` 等) で追加する。
//!
//! 検証する pre-connect rejection (= exit code 2、stderr に hint):
//! - `--output ./relative` (= 絶対 path 必須)
//! - `--format=raw` + default `--both` (= raw 単一 direction 限定)
//! - `--id 1 --all` (= 排他)
//! - `record start` で `--output` 省略
//! - 未知 `record` subcommand
//!
//! また `record --help` / `record start --help` 等の help 経路は exit 0 で
//! 該当 usage を stdout に出すことを確認する。

use std::process::Command;

/// `target/debug/hyoui` の絶対 path を cargo から取得する。
fn hyoui_bin() -> &'static str {
    env!("CARGO_BIN_EXE_hyoui")
}

/// 指定 argv で hyoui を実行し、(exit_code, stdout, stderr) を返す。
fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(hyoui_bin())
        .args(args)
        .output()
        .expect("failed to spawn hyoui binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `--output ./relative.jsonl` は parse 段で reject (= exit 2、絶対 path 要求 hint)。
#[test]
fn record_start_relative_output_rejected() {
    let (code, _stdout, stderr) = run(&["record", "start", "demo", "--output", "./rel.jsonl"]);
    assert_eq!(code, 2, "relative path must exit 2; stderr={stderr}");
    assert!(
        stderr.contains("absolute"),
        "stderr must mention absolute requirement: {stderr}"
    );
}

/// `--format=raw` + default `--both` は parse 段で reject (= exit 2)。
#[test]
fn record_start_raw_both_rejected_integration() {
    let (code, _stdout, stderr) = run(&[
        "record",
        "start",
        "demo",
        "--output",
        "/tmp/rec.bin",
        "--format=raw",
    ]);
    assert_eq!(code, 2, "raw+both must exit 2; stderr={stderr}");
    assert!(
        stderr.contains("raw") && stderr.contains("--both"),
        "stderr must explain raw+both conflict: {stderr}"
    );
}

/// `--id 1 --all` の排他指定は parse 段で reject (= exit 2)。
#[test]
fn record_stop_id_and_all_rejected_integration() {
    let (code, _stdout, stderr) = run(&["record", "stop", "demo", "--id", "1", "--all"]);
    assert_eq!(code, 2, "--id + --all must exit 2; stderr={stderr}");
    assert!(
        stderr.contains("--id") && stderr.contains("--all"),
        "stderr must mention both --id and --all: {stderr}"
    );
}

/// `record start` で `--output` 省略は parse 段で reject (= exit 2)。
#[test]
fn record_start_output_missing_rejected_integration() {
    let (code, _stdout, stderr) = run(&["record", "start", "demo"]);
    assert_eq!(code, 2, "missing --output must exit 2; stderr={stderr}");
    assert!(
        stderr.contains("--output"),
        "stderr must mention --output: {stderr}"
    );
}

/// 未知 `record` subcommand は edit distance suggest 付き reject (= exit 2)。
#[test]
fn record_unknown_subcommand_rejected_integration() {
    let (code, _stdout, stderr) = run(&["record", "startt", "demo"]);
    assert_eq!(code, 2, "unknown subcommand must exit 2; stderr={stderr}");
    assert!(
        stderr.contains("unknown subcommand"),
        "stderr must mention unknown subcommand: {stderr}"
    );
    // suggest closest で `start` が推奨される
    assert!(
        stderr.contains("start"),
        "stderr must suggest `start`: {stderr}"
    );
}

/// `record --help` は exit 0 で stdout に親 help を出す。
#[test]
fn record_help_outputs_parent_usage() {
    let (code, stdout, _stderr) = run(&["record", "--help"]);
    assert_eq!(code, 0, "record --help must exit 0");
    assert!(stdout.contains("start"), "help must list start: {stdout}");
    assert!(stdout.contains("stop"), "help must list stop: {stdout}");
    assert!(stdout.contains("list"), "help must list list: {stdout}");
}

/// 引数なしの `record` は exit 0 で親 help を表示する (= cli-design-preferences:
/// 「引数なし実行時は --help を表示」、help 経路は SUCCESS)。
#[test]
fn record_no_args_outputs_parent_usage() {
    let (code, stdout, _stderr) = run(&["record"]);
    assert_eq!(code, 0, "record (no args) must exit 0 with help");
    assert!(stdout.contains("start"), "help must list start: {stdout}");
}

/// `record start --help` は exit 0 で start usage を出す。
#[test]
fn record_start_help_outputs_usage() {
    let (code, stdout, _stderr) = run(&["record", "start", "--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("--output"),
        "usage must list --output: {stdout}"
    );
    assert!(
        stdout.contains("--input-secrecy"),
        "usage must list --input-secrecy: {stdout}"
    );
}

/// `hyoui --help` の top level に `record` が列挙される (= TOP_LEVEL_SUBCOMMANDS
/// 整合性確認、usage_top 経路)。
#[test]
fn top_help_lists_record_subcommand() {
    let (code, stdout, _stderr) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("record"),
        "top help must list `record`: {stdout}"
    );
}
