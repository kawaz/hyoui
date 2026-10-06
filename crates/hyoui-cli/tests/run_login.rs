//! DR-0039 決定 1 e2e: `hyoui run --login` の子起動 (argv[0] / 最小 env / 注入 env /
//! 面の env) と、`hyoui run` / `--login` 共通の子の TERM (呼び出し元を引き継ぎ、無ければ
//! config `[session] term_fallback`)。
//!
//! 子に `sh -c` を明示して env / argv を file に吐かせ、呼び出し元に置いたダミー env が
//! 子に渡らないこと、hyoui 自身は呼び出し元の面 env (`HYOUI_STATE_DIR`) で socket を
//! 置くことを実バイナリで観測する。

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// mode 0700 の runtime dir (= socket dir 要件)。drop で配下の daemon を畳む
/// (= 観測の途中で panic して `cleanup` に届かなくても session を残さない)。
fn runtime_dir() -> SessionDir {
    SessionDir::new("hyoui-login-")
}

/// ダミー env を掛けた呼び出し元から `hyoui run --login --detached <extra...>` を起こす。
fn run_login(runtime: &Path, sid: &str, extra: &[&str]) {
    run_detached(runtime, sid, &["--login"], Some("xterm-ghostty"), extra);
}

/// 呼び出し元の TERM を指定 (`None` は未設定) して `hyoui run --detached <flags...>
/// <extra...>` を起こす。config は `runtime` 配下の `XDG_CONFIG_HOME` から読ませる
/// (= 利用者の実 config を読まない)。
fn run_detached(runtime: &Path, sid: &str, flags: &[&str], term: Option<&str>, extra: &[&str]) {
    let mut c = Command::new(hyoui_bin());
    // stdin は /dev/null なので、そのまま子の stdin になるとログイン shell は EOF で終わる
    // (DR-0042 決定 1)。子の stdin も PTY にして (`--pty-stdin`) 子を生かしたまま
    // `hyoui list` の PID と `ps` の argv を観測するのがこの helper の目的。
    c.args([
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--session-id={sid}"),
    ])
    .args(flags)
    .args(extra)
    .env("HYOUI_STATE_DIR", runtime)
    .env("XDG_CONFIG_HOME", runtime.join("config"))
    .env("HYOUI_E2E_DUMMY", "must-not-leak")
    .env("CLAUDE_CODE_SESSION_ID", "must-not-leak")
    .env("LANG", "ja_JP.UTF-8")
    .env_remove("HYOUI_SESSION_ID")
    .env_remove("HYOUI_LOCK_TOKEN");
    match term {
        Some(t) => c.env("TERM", t),
        None => c.env_remove("TERM"),
    };
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        // daemon が stderr を継承して常駐するので pipe にすると output() が EOF を
        // 待ち続ける。file に逃がす。
        .stderr(std::fs::File::create(runtime.join("run.stderr")).expect("stderr file"));
    let status = c.status().expect("spawn");
    assert!(
        status.success(),
        "run --login --detached が成功すること: {}",
        std::fs::read_to_string(runtime.join("run.stderr")).unwrap_or_default()
    );
    let sock = runtime.join("sessions").join(format!("{sid}.sock"));
    wait_for(|| sock.exists(), "socket", &sock);
}

fn wait_for(cond: impl Fn() -> bool, what: &str, p: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{what} が出現しない: {}", p.display());
}

fn cleanup(runtime: &Path, sid: &str) {
    let _ = Command::new(hyoui_bin())
        .args(["kill", sid, "--signal=KILL"])
        .env("HYOUI_STATE_DIR", runtime)
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `sh -c` で env と `$0` を file に吐く script。`/usr/bin/env` で子の environ を見る。
fn dump_script(out: &Path) -> String {
    format!(
        "/usr/bin/env > '{0}'; printf '%s' \"$0\" > '{0}.argv0'",
        out.display()
    )
}

fn read_env(path: &Path) -> BTreeMap<String, String> {
    wait_for(|| path.exists(), "env dump", path);
    // 書き込み完了待ち (= argv0 file が最後に出来る)。
    let argv0 = PathBuf::from(format!("{}.argv0", path.display()));
    wait_for(|| argv0.exists(), "argv0 dump", &argv0);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn explicit_command_gets_minimal_env_and_injected_env() {
    let runtime = runtime_dir();
    let out = runtime.path().join("env.txt");
    let sid = &hyoui::cli::new_session_id();
    let script = dump_script(&out);
    run_login(runtime.path(), sid, &["--", "sh", "-c", &script]);
    let env = read_env(&out);
    cleanup(runtime.path(), sid);

    // 呼び出し元のダミー env は子に無い。
    assert!(!env.contains_key("HYOUI_E2E_DUMMY"), "{env:?}");
    assert!(!env.contains_key("CLAUDE_CODE_SESSION_ID"), "{env:?}");
    // 面の env (HYOUI_STATE_DIR) も子には渡らない (= hyoui 自身だけが使う)。
    assert!(!env.contains_key("HYOUI_STATE_DIR"), "{env:?}");
    // 最小 env + 呼び出し元の LANG / TERM (xterm-ghostty をそのまま引き継ぐ)。
    for k in ["HOME", "USER", "LOGNAME", "SHELL", "PATH"] {
        assert!(env.contains_key(k), "{k} が無い: {env:?}");
    }
    assert_eq!(env.get("LANG").map(String::as_str), Some("ja_JP.UTF-8"));
    assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-ghostty"));
    // 注入 env (DR-0020) は残る。子へ常時注入するのは HYOUI_SESSION_ID だけ (DR-0041)。
    assert_eq!(
        env.get("HYOUI_SESSION_ID").map(String::as_str),
        Some(sid.as_str())
    );
    let injected: Vec<&String> = env.keys().filter(|k| k.starts_with("HYOUI_")).collect();
    assert_eq!(injected, ["HYOUI_SESSION_ID"], "{env:?}");
    // 明示コマンドの argv[0] は `-` 付けなしでそのまま (sh の $0 = "sh")。
    let argv0 = std::fs::read_to_string(format!("{}.argv0", out.display())).unwrap();
    assert_eq!(argv0, "sh");
}

/// 子の TERM を `run --detached <flags>` で起こして読む。`config` があれば
/// `XDG_CONFIG_HOME/hyoui/config.toml` に書いてから起こす。
fn child_term(flags: &[&str], caller_term: Option<&str>, config: Option<&str>) -> Option<String> {
    let runtime = runtime_dir();
    if let Some(toml) = config {
        let dir = runtime.path().join("config").join("hyoui");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.toml"), toml).expect("config");
    }
    let out = runtime.path().join("env.txt");
    let sid = &hyoui::cli::new_session_id();
    let script = dump_script(&out);
    run_detached(
        runtime.path(),
        sid,
        flags,
        caller_term,
        &["--", "sh", "-c", &script],
    );
    let env = read_env(&out);
    cleanup(runtime.path(), sid);
    env.get("TERM").cloned()
}

/// 普通の run も `--login` も、呼び出し元の TERM をそのまま引き継ぐ。
#[test]
fn term_is_inherited_from_caller_in_run_and_login() {
    for flags in [&[][..], &["--login"][..]] {
        assert_eq!(
            child_term(flags, Some("xterm-ghostty"), None).as_deref(),
            Some("xterm-ghostty"),
            "flags {flags:?}"
        );
    }
}

/// 呼び出し元に TERM が無い (未設定 / 空) 時は既定値 `xterm-256color`。
#[test]
fn term_falls_back_to_default_when_caller_has_none() {
    for flags in [&[][..], &["--login"][..]] {
        for caller in [None, Some("")] {
            assert_eq!(
                child_term(flags, caller, None).as_deref(),
                Some("xterm-256color"),
                "flags {flags:?} caller {caller:?}"
            );
        }
    }
}

/// 既定値は config `[session] term_fallback` で変えられる。呼び出し元に TERM があれば
/// config の値は使わない。
#[test]
fn term_fallback_comes_from_config() {
    let toml = "[session]\nterm_fallback = \"screen-256color\"\n";
    for flags in [&[][..], &["--login"][..]] {
        assert_eq!(
            child_term(flags, None, Some(toml)).as_deref(),
            Some("screen-256color"),
            "flags {flags:?}"
        );
        assert_eq!(
            child_term(flags, Some("vt100"), Some(toml)).as_deref(),
            Some("vt100"),
            "flags {flags:?}"
        );
    }
}

#[test]
fn login_without_command_runs_passwd_shell_as_login_shell() {
    let runtime = runtime_dir();
    let sid = &hyoui::cli::new_session_id();
    run_login(runtime.path(), sid, &[]);

    // 子の argv は `ps` で `-<shell basename>`、exec 先は passwd の shell。
    let pid = status_child_pid(runtime.path(), sid);
    let args = ps_args(pid);
    cleanup(runtime.path(), sid);

    let passwd_shell = passwd_shell();
    let base = Path::new(&passwd_shell)
        .file_name()
        .unwrap()
        .to_string_lossy();
    assert_eq!(args.trim(), format!("-{base}"), "ps args");
}

#[test]
fn socket_follows_callers_face_env_not_child_env() {
    // 面の env (HYOUI_STATE_DIR) は hyoui 自身の socket 置き場に効く。
    let runtime = runtime_dir();
    let sid = &hyoui::cli::new_session_id();
    run_login(runtime.path(), sid, &["--", "sleep", "30"]);
    assert!(
        runtime
            .path()
            .join("sessions")
            .join(format!("{sid}.sock"))
            .exists()
    );
    cleanup(runtime.path(), sid);
}

fn passwd_shell() -> String {
    let out = Command::new("sh")
        .args([
            "-c",
            "dscl . -read /Users/$(id -un) UserShell 2>/dev/null | awk '{print $2}'",
        ])
        .output()
        .ok();
    let s = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !s.is_empty() {
        return s;
    }
    let out = Command::new("sh")
        .args(["-c", "getent passwd $(id -un) | cut -d: -f7"])
        .output()
        .expect("getent");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn status_child_pid(runtime: &Path, sid: &str) -> i32 {
    let out = Command::new(hyoui_bin())
        .args(["list"])
        .env("HYOUI_STATE_DIR", runtime)
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .expect("list");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let line = text
        .lines()
        .find(|l| l.starts_with(sid))
        .unwrap_or_else(|| panic!("list に {sid} が無い: {text}"));
    line.split_whitespace()
        .nth(2)
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("PID 列が読めない: {line}"))
}

fn ps_args(pid: i32) -> String {
    let out = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout).to_string()
}
