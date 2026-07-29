//! `hyoui web service` CLI boundary の CI-safe E2E (= DR-0031)。
//!
//! 実 service manager を変更する register E2E は dogfooding host で手動実行する。
//! ここでは隔離 HOME に対する status と help routing を実 binary で固定する。

use std::process::{Command, Output};

fn hyoui(args: &[&str], home: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hyoui"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("spawn hyoui")
}

/// definition の登録有無と service-manager 上の稼働状態は独立して表示する。
///
/// macOS の launchctl job は `gui/$UID/<label>` に属するため、隔離 HOME でも同 UID の
/// 実 job が running の場合がある。隔離できるのは plist path / registered 判定だけ。
#[test]
fn status_reports_definition_state_in_isolated_home() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(&["web", "service", "status"], home.path());
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("registered: no"), "{stdout}");
    assert!(stdout.contains("running:    "), "{stdout}");
    assert!(stdout.contains("pid:        "), "{stdout}");
    if cfg!(target_os = "macos") {
        assert!(stdout.contains("com.github.kawaz.hyoui-web"), "{stdout}");
        assert!(stdout.contains("Library/LaunchAgents"), "{stdout}");
    } else if cfg!(target_os = "linux") {
        assert!(stdout.contains("hyoui-web"), "{stdout}");
        assert!(
            stdout.contains("systemd/user/hyoui-web.service"),
            "{stdout}"
        );
    }
}

/// parent/leaf の引数なし・help はそれぞれの surface を表示して成功する。
#[test]
fn help_routes_through_web_service_tree() {
    let home = tempfile::tempdir().expect("isolated HOME");
    for (args, needle) in [
        (&["web", "service"][..], "register"),
        (&["web", "service", "register", "--help"][..], "--listen"),
        (&["web", "service", "unregister", "--help"][..], "remove"),
        (&["web", "service", "status", "--help"][..], "running state"),
    ] {
        let output = hyoui(args, home.path());
        assert!(output.status.success(), "args={args:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(needle), "args={args:?}, stdout={stdout}");
    }
}
