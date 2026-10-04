//! DR-0039 決定 1 e2e: `hyoui run --login` の子起動 (argv[0] / 最小 env / 注入 env /
//! 面の env)。
//!
//! 子に `sh -c` を明示して env / argv を file に吐かせ、呼び出し元に置いたダミー env が
//! 子に渡らないこと、hyoui 自身は呼び出し元の面 env (`XDG_RUNTIME_DIR`) で socket を
//! 置くことを実バイナリで観測する。

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// mode 0700 の TempDir (= socket dir 要件)。
fn runtime_dir() -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("hyoui-login-")
        .tempdir()
        .expect("tempdir");
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    d
}

/// ダミー env を掛けた呼び出し元から `hyoui run --login --detached <extra...>` を起こす。
fn run_login(runtime: &Path, sid: &str, extra: &[&str]) {
    let mut c = Command::new(hyoui_bin());
    c.args(["run", "--login", "--detached", &format!("--session={sid}")])
        .args(extra)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("HYOUI_E2E_DUMMY", "must-not-leak")
        .env("CLAUDE_CODE_SESSION_ID", "must-not-leak")
        .env("LANG", "ja_JP.UTF-8")
        .env("TERM", "xterm-ghostty")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
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
    let sock = runtime.join("hyoui").join(format!("{sid}.sock"));
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
        .env("XDG_RUNTIME_DIR", runtime)
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
    let sid = "login-explicit";
    let script = dump_script(&out);
    run_login(runtime.path(), sid, &["--", "sh", "-c", &script]);
    let env = read_env(&out);
    cleanup(runtime.path(), sid);

    // 呼び出し元のダミー env は子に無い。
    assert!(!env.contains_key("HYOUI_E2E_DUMMY"), "{env:?}");
    assert!(!env.contains_key("CLAUDE_CODE_SESSION_ID"), "{env:?}");
    // 面の env (XDG_RUNTIME_DIR) も子には渡らない (= hyoui 自身だけが使う)。
    assert!(!env.contains_key("XDG_RUNTIME_DIR"), "{env:?}");
    // 最小 env + 呼び出し元の LANG。TERM は呼び出し元 (xterm-ghostty) を引き継がず固定値。
    for k in ["HOME", "USER", "LOGNAME", "SHELL", "PATH"] {
        assert!(env.contains_key(k), "{k} が無い: {env:?}");
    }
    assert_eq!(env.get("LANG").map(String::as_str), Some("ja_JP.UTF-8"));
    assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
    // 注入 env (DR-0018 / DR-0020) は残る。
    assert_eq!(env.get("HYOUI_SESSION_ID").map(String::as_str), Some(sid));
    assert_eq!(
        env.get("HYOUI_NAMESPACE").map(String::as_str),
        Some("default")
    );
    // 明示コマンドの argv[0] は `-` 付けなしでそのまま (sh の $0 = "sh")。
    let argv0 = std::fs::read_to_string(format!("{}.argv0", out.display())).unwrap();
    assert_eq!(argv0, "sh");
}

#[test]
fn login_without_command_runs_passwd_shell_as_login_shell() {
    let runtime = runtime_dir();
    let sid = "login-shell";
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
    // 面の env (XDG_RUNTIME_DIR) は hyoui 自身の socket 置き場に効く。
    let runtime = runtime_dir();
    let sid = "login-face";
    run_login(runtime.path(), sid, &["--", "sleep", "30"]);
    assert!(
        runtime
            .path()
            .join("hyoui")
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
        .env("XDG_RUNTIME_DIR", runtime)
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
