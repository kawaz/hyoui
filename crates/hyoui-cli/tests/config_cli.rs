//! `hyoui config path` / `hyoui config show` の e2e (= 実バイナリ経由)。
//!
//! path 解決は env (`XDG_CONFIG_HOME` / `HOME`) 依存なので、process を分ける
//! e2e で検証する (= lib 側の pure 関数 test では env を触れない)。

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// `xdg` / `home` を明示した env で `hyoui config <args...>` を実行する。
fn run_config(args: &[&str], xdg: Option<&Path>, home: Option<&Path>) -> Output {
    let mut cmd = Command::new(hyoui_bin());
    cmd.arg("config")
        .args(args)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(x) = xdg {
        cmd.env("XDG_CONFIG_HOME", x);
    }
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    cmd.output().expect("spawn hyoui")
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn config_path_uses_xdg_config_home() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_config(&["path"], Some(tmp.path()), None);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out).trim(),
        tmp.path().join("hyoui/config.toml").display().to_string()
    );
}

#[test]
fn config_path_falls_back_to_home() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_config(&["path"], None, Some(tmp.path()));
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out).trim(),
        tmp.path()
            .join(".config/hyoui/config.toml")
            .display()
            .to_string()
    );
}

#[test]
fn config_path_prints_path_even_when_file_is_missing() {
    // 主用途は「どこに作ればよいか」を知ること = 不在でも stdout はパスのみ、
    // 注記は stderr へ回して `$(hyoui config path)` を壊さない。
    let tmp = tempfile::tempdir().unwrap();
    let out = run_config(&["path"], Some(tmp.path()), None);
    assert!(out.status.success());
    assert_eq!(stdout_of(&out).lines().count(), 1);
    assert!(
        stderr_of(&out).contains("no config file there yet"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn config_path_without_any_env_fails() {
    let out = run_config(&["path"], None, None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_of(&out).contains("cannot resolve a config path"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn config_show_without_file_prints_defaults_as_valid_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_config(&["show"], Some(tmp.path()), None);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let text = stdout_of(&out);

    let parsed: toml::Value = toml::from_str(&text).expect("output must be valid TOML");
    assert_eq!(parsed["scrub_env"]["enabled"].as_bool(), Some(true));
    // DR-0029 §4 (2026-07-30 改訂): 連打窓の default は 1000ms。
    assert_eq!(
        parsed["attach"]["ctrlz_guard_delay"].as_str(),
        Some("1000ms")
    );
    assert_eq!(parsed["session"]["auto_resume"].as_bool(), Some(false));
    assert_eq!(
        parsed["web"]["listen"].as_str(),
        Some("127.0.0.1:43690"),
        "defaults must be printed even with no config file"
    );
    // builtin は config key ではなくコメントとして出る。
    assert!(text.contains("# builtin env scrub defaults"), "{text}");
    assert!(text.contains("#     CLAUDECODE"), "{text}");
}

#[test]
fn config_show_with_file_prints_effective_values_including_untouched_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hyoui");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[web]\nlisten = \"0.0.0.0:1234\"\n\n[scrub_env.targets.claude]\nkill_glob = [\"FOO_*\"]\n",
    )
    .unwrap();

    let out = run_config(&["show"], Some(tmp.path()), None);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let text = stdout_of(&out);
    let parsed: toml::Value = toml::from_str(&text).expect("output must be valid TOML");

    // 設定した値。
    assert_eq!(parsed["web"]["listen"].as_str(), Some("0.0.0.0:1234"));
    assert_eq!(
        parsed["scrub_env"]["targets"]["claude"]["kill_glob"][0].as_str(),
        Some("FOO_*")
    );
    // 触っていない項目も実効値 (= default) で出る。
    assert_eq!(
        parsed["scrub_env"]["targets"]["claude"]["inherit_builtin"].as_bool(),
        Some(true)
    );
    assert_eq!(parsed["attach"]["ctrlz_guard"].as_bool(), Some(true));
    assert_eq!(parsed["session"]["auto_resume"].as_bool(), Some(false));
    // ヘッダは読み込み元を示す。
    assert!(
        text.lines().next().unwrap().contains("config.toml"),
        "{text}"
    );
}

#[test]
fn config_show_rejects_broken_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hyoui");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "this is not valid toml ===").unwrap();

    let out = run_config(&["show"], Some(tmp.path()), None);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_of(&out).contains("parse failed"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn config_show_output_round_trips_as_a_config_file() {
    // 出力をそのまま config file として保存できる (= 実効値が保存できる形)。
    let src = tempfile::tempdir().unwrap();
    let dir = src.path().join("hyoui");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[attach]\nctrlz_guard_delay = \"1.5s\"\nctrlz_guard = false\n",
    )
    .unwrap();
    let first = run_config(&["show"], Some(src.path()), None);
    assert!(first.status.success());

    let dst = tempfile::tempdir().unwrap();
    let dst_dir = dst.path().join("hyoui");
    std::fs::create_dir_all(&dst_dir).unwrap();
    // ヘッダ行 (コメント) 込みでそのまま書き戻す。
    std::fs::write(dst_dir.join("config.toml"), stdout_of(&first)).unwrap();

    let second = run_config(&["show"], Some(dst.path()), None);
    assert!(second.status.success(), "stderr: {}", stderr_of(&second));

    // ヘッダ行は読み込み元パスを含むので、それ以降 (= 設定本体) を比較する。
    let body = |o: &Output| stdout_of(o).lines().skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(body(&first), body(&second));
}

#[test]
fn config_no_args_shows_help() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_config(&[], Some(tmp.path()), None);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let text = stdout_of(&out);
    assert!(text.contains("hyoui config <subcommand>"), "{text}");
    assert!(text.contains("path"), "{text}");
    assert!(text.contains("show"), "{text}");
}
