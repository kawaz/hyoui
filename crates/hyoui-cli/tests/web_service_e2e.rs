//! `hyoui web service` CLI boundary の CI-safe E2E (= DR-0034 決定 6)。
//!
//! 実 service manager を変更する register / start / stop は、本番の label を踏まない
//! よう dogfooding host で隔離 label を使って手動実行する。ここでは隔離 HOME に対する
//! status と help routing を実 binary で固定する。

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn hyoui(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hyoui"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .stdin(Stdio::null())
        .output()
        .expect("spawn hyoui")
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}): {} / stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// 未登録の状態でも status は答え、登録の有無と OS 側の見え方を分けて出す。
///
/// macOS の launchctl job は `gui/$UID/<label>` に属するため、隔離 HOME でも同 UID の
/// 実 job が載っている場合がある。隔離できるのは plist path と `registered` 判定だけ
/// なので、そこだけを固定する。
#[test]
fn status_reports_definition_state_in_isolated_home() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(&["web", "service", "status"], home.path());
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = json(&output);

    // 隔離 HOME に定義は無い。
    assert_eq!(status["registered"], false);
    // 監督者の頼み口にも届かないので、抱えている unit は空。
    assert_eq!(status["running"], false);
    assert_eq!(status["instances"], serde_json::json!([]));
    // 登録が無ければ「監督者」という対象自体が無いので版も出ない。
    assert!(status["version"].is_null(), "{status}");
    // OS 側から取れる情報は `service` に入れ、top-level の running とは分ける。
    assert!(status["service"]["loaded"].is_boolean(), "{status}");
    assert!(status["service"]["running"].is_boolean(), "{status}");

    let label = status["label"].as_str().expect("label");
    let path = status["path"].as_str().expect("path");
    // label は接頭辞 + 状態 root の hash (16 進 8 桁)。OS ごとに名前を分けない
    // (DR-0038 決定 4)。どの root から作ったかを並べて出す。
    let hash = label
        .strip_prefix("com.github.kawaz.hyoui.web.supervise.")
        .unwrap_or_else(|| panic!("unexpected label {label}"));
    assert_eq!(hash.len(), 8, "{label}");
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{label}");
    let root = status["root"].as_str().expect("root");
    assert!(root.ends_with(".local/state/hyoui"), "{root}");
    if cfg!(target_os = "macos") {
        assert!(path.contains("Library/LaunchAgents"), "{path}");
        assert!(path.ends_with(&format!("{label}.plist")), "{path}");
    } else if cfg!(target_os = "linux") {
        assert!(
            path.ends_with(&format!("systemd/user/{label}.service")),
            "{path}"
        );
    }
}

/// 状態 root が違えば監督者の label も違う (= 面ごとに並べて載せられる)。
#[test]
fn each_state_root_gets_its_own_label() {
    let first = tempfile::tempdir().expect("isolated HOME");
    let second = tempfile::tempdir().expect("isolated HOME");
    let label = |home: &Path| {
        json(&hyoui(&["web", "service", "status"], home))["label"]
            .as_str()
            .expect("label")
            .to_string()
    };
    assert_ne!(label(first.path()), label(second.path()));
    // 同じ root なら同じ label。
    assert_eq!(label(first.path()), label(first.path()));
}

/// 旧 label は引き取らない (= DR-0034 決定 11 / DR-0038 移行節、移行は人の手作業)。
#[test]
fn old_labels_are_not_used() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let status = json(&hyoui(&["web", "service", "status"], home.path()));
    let rendered = status.to_string();
    for old in [
        "com.github.kawaz.hyoui-web\"",
        "jp.kawaz.hyoui-web.supervise",
        "hyoui-web-supervise",
    ] {
        assert!(
            !rendered.contains(old),
            "old label `{old}` appears: {rendered}"
        );
    }
    // 旧 verb の意味も残さない: `register` は listen を受けない。
    let output = hyoui(
        &["web", "service", "register", "--listen=127.0.0.1:1"],
        home.path(),
    );
    assert!(!output.status.success());
}

/// parent/leaf の引数なし・help はそれぞれの surface を表示して成功する。
#[test]
fn help_routes_through_web_service_tree() {
    let home = tempfile::tempdir().expect("isolated HOME");
    for (args, needle) in [
        (&["web", "service"][..], "register"),
        (&["web", "service", "register", "--help"][..], "--binary"),
        (&["web", "service", "unregister", "--help"][..], "remove"),
        (&["web", "service", "start", "--help"][..], "supervisor"),
        (&["web", "service", "stop", "--help"][..], "full outage"),
        (&["web", "service", "restart", "--help"][..], "full outage"),
        (
            &["web", "service", "status", "--help"][..],
            "control socket",
        ),
        (&["web", "service", "log", "--help"][..], "--follow"),
    ] {
        let output = hyoui(args, home.path());
        assert!(output.status.success(), "args={args:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(needle), "args={args:?}, stdout={stdout}");
    }

    // 親の help は子の全 verb を並べる。
    let parent = hyoui(&["web", "service"], home.path());
    let stdout = String::from_utf8_lossy(&parent.stdout);
    for verb in [
        "register",
        "unregister",
        "start",
        "stop",
        "restart",
        "status",
        "log",
    ] {
        assert!(stdout.contains(verb), "{verb} missing from {stdout}");
    }
}

/// 未登録の隔離 HOME では `restart` は register への道を示して断る (= 決定 6)。
///
/// 上げ直しは launchd / systemd に頼む操作なので、頼む相手 (定義) が無い時に
/// backend のメッセージへ落とさず、何をすればよいかを言う口を固定する。
#[test]
fn restart_refuses_when_nothing_is_registered() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(&["web", "service", "restart"], home.path());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hyoui web service register"), "{stderr}");
}
