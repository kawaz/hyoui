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
    if cfg!(target_os = "macos") {
        assert_eq!(label, "jp.kawaz.hyoui-web.supervise");
        assert!(path.contains("Library/LaunchAgents"), "{path}");
        assert!(
            path.ends_with("jp.kawaz.hyoui-web.supervise.plist"),
            "{path}"
        );
    } else if cfg!(target_os = "linux") {
        assert_eq!(label, "hyoui-web-supervise");
        assert!(
            path.ends_with("systemd/user/hyoui-web-supervise.service"),
            "{path}"
        );
    }
}

/// 旧 label は引き取らない (= 決定 11、移行は runbook の手作業)。
#[test]
fn the_old_single_gateway_label_is_not_referenced() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let status = json(&hyoui(&["web", "service", "status"], home.path()));
    let rendered = status.to_string();
    assert!(
        !rendered.contains("com.github.kawaz"),
        "the old label still appears: {rendered}"
    );
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
    for verb in ["register", "unregister", "start", "stop", "status", "log"] {
        assert!(stdout.contains(verb), "{verb} missing from {stdout}");
    }
}
