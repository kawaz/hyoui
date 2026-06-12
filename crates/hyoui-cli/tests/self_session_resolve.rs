//! DR-0020 §2 e2e: session 引数の省略時解決 (`明示 > $HYOUI_SESSION_ID > fallback`)。
//!
//! - 中から (= `$HYOUI_SESSION_ID` set) `hyoui status` を session 省略で叩くと
//!   自セッションに解決される。
//! - stale env (= env が指す session が不存在) は既存 fallback に落とさず明示エラー。
//! - 外から (= env なし) の省略実行は従来通り `session id が必要` エラー (= 挙動不変)。
//!
//! daemon socket は namespace 経路 (`$XDG_RUNTIME_DIR/hyoui/<sid>.sock`) に置く。
//! env 解決はこの path 規約で socket を引くため、`--socket` 明示の harness とは
//! 別に自前 TempDir + Command を組む。

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// mode 0700 の TempDir を作る (= `ensure_socket_dir` 要件)。
fn runtime_dir() -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("hyoui-selfres-")
        .tempdir()
        .expect("tempdir");
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    d
}

/// `run --detached --session=<sid>` で daemon を起こし、socket 出現を待つ。
fn spawn_detached(runtime: &std::path::Path, sid: &str) {
    let status = Command::new(hyoui_bin())
        .args([
            "run",
            "--detached",
            &format!("--session={sid}"),
            "--",
            "sh",
            "-c",
            "sleep 30",
        ])
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn detached daemon");
    assert!(status.success(), "run --detached が成功すること");

    // socket 出現を待つ (= namespace=default なら <runtime>/hyoui/<sid>.sock)。
    let sock = runtime.join("hyoui").join(format!("{sid}.sock"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if sock.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("socket が出現しない: {}", sock.display());
}

/// daemon を kill して後始末する (= socket の daemon を畳む)。
fn cleanup(runtime: &std::path::Path, sid: &str) {
    let _ = Command::new(hyoui_bin())
        .args(["kill", sid])
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[test]
fn status_resolves_self_session_from_env() {
    let runtime = runtime_dir();
    let sid = "selfres-from-env";
    spawn_detached(runtime.path(), sid);

    // session 引数を省略し、$HYOUI_SESSION_ID で自セッションを指す。
    let out = Command::new(hyoui_bin())
        .args(["status"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", sid)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("status");

    cleanup(runtime.path(), sid);

    assert!(
        out.status.success(),
        "中からの status は成功すべき。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(sid),
        "status が自セッション id を返すべき。stdout={stdout:?}"
    );
}

#[test]
fn status_stale_env_errors_without_fallback() {
    let runtime = runtime_dir();
    // daemon を一切起こさない = env が指す session は不存在 (= stale)。
    let out = Command::new(hyoui_bin())
        .args(["status"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", "selfres-nonexistent")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("status");

    assert!(
        !out.status.success(),
        "stale env では明示エラーになるべき (= fallback に落とさない)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stale env") || stderr.contains("見つかりません"),
        "stale env の理由が示されるべき。stderr={stderr:?}"
    );
}

#[test]
fn status_without_env_keeps_required_error() {
    let runtime = runtime_dir();
    // env なし + 引数なし = 従来通り「session id が必要」エラー (= 外挙動不変)。
    let out = Command::new(hyoui_bin())
        .args(["status"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("status");

    assert!(!out.status.success(), "env も引数もなければ従来通りエラー");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("session id") && stderr.contains("必要"),
        "従来の required メッセージを維持すべき。stderr={stderr:?}"
    );
}

// ── DR-0020 §3: attach の self default 禁止 (ネスト防止) ────────────────────

#[test]
fn attach_self_session_via_explicit_arg_is_rejected() {
    let runtime = runtime_dir();
    let sid = "selfres-attach-self";
    spawn_detached(runtime.path(), sid);

    // 中から (= $HYOUI_SESSION_ID set) 明示引数で自セッションに attach → 拒否。
    let out = Command::new(hyoui_bin())
        .args(["attach", sid])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", sid)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("attach");

    cleanup(runtime.path(), sid);

    assert!(
        !out.status.success(),
        "自セッションへの attach は拒否されるべき"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("自セッション") || stderr.contains("ネスト"),
        "ネスト防止の理由が示されるべき。stderr={stderr:?}"
    );
}

#[test]
fn attach_other_session_from_inside_is_allowed() {
    let runtime = runtime_dir();
    let me = "selfres-attach-me";
    let other = "selfres-attach-other";
    spawn_detached(runtime.path(), me);
    spawn_detached(runtime.path(), other);

    // 中から ($HYOUI_SESSION_ID=me) 別セッション (other) への attach は self ではない。
    // attach は raw mode に入って block するため、stdin を即 EOF にして detach させ、
    // 「self 拒否で即エラー終了しない」ことだけを確認する (= exit が拒否の 2 でない)。
    let out = Command::new(hyoui_bin())
        .args(["attach", other, "--mode=ro"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", me)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("attach other");

    cleanup(runtime.path(), me);
    cleanup(runtime.path(), other);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("自セッション") && !stderr.contains("ネスト"),
        "別セッションへの attach は self 拒否されないべき。stderr={stderr:?}"
    );
}
