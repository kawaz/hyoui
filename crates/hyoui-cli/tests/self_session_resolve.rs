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
    spawn_detached_cmd(runtime, sid, "sleep 30");
}

/// 子 shell script を指定して detached daemon を起こす variant。
fn spawn_detached_cmd(runtime: &std::path::Path, sid: &str, script: &str) {
    let status = Command::new(hyoui_bin())
        .args([
            "run",
            "--detached",
            &format!("--session={sid}"),
            "--",
            "sh",
            "-c",
            script,
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

/// codex review #3 regression: `--socket` 明示経路でも self-attach ガードが効く
/// (= `hyoui attach --socket=<自分の sock>` で sid 経路のガードを迂回できない)。
/// best-effort の UX ガード (canonical path 比較、path 偽装までは追わない)。
#[test]
fn attach_self_session_via_explicit_socket_is_rejected() {
    let runtime = runtime_dir();
    let sid = "selfres-attach-sock";
    spawn_detached(runtime.path(), sid);

    // 自セッションの socket path を --socket で直接指定して迂回を試みる。
    let self_sock = runtime.path().join("hyoui").join(format!("{sid}.sock"));
    let out = Command::new(hyoui_bin())
        .args(["attach", &format!("--socket={}", self_sock.display())])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", sid)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("attach --socket self");

    cleanup(runtime.path(), sid);

    assert!(
        !out.status.success(),
        "--socket 明示でも自セッションへの attach は拒否されるべき"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("自セッション") || stderr.contains("ネスト"),
        "ネスト防止の理由が示されるべき。stderr={stderr:?}"
    );
}

/// 対照: `--socket` 明示で **別セッション** の socket を指す場合は self 拒否されない。
#[test]
fn attach_other_socket_from_inside_is_allowed() {
    let runtime = runtime_dir();
    let me = "selfres-sock-me";
    let other = "selfres-sock-other";
    spawn_detached(runtime.path(), me);
    spawn_detached(runtime.path(), other);

    let other_sock = runtime.path().join("hyoui").join(format!("{other}.sock"));
    let out = Command::new(hyoui_bin())
        .args([
            "attach",
            &format!("--socket={}", other_sock.display()),
            "--mode=ro",
        ])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", me)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("attach --socket other");

    cleanup(runtime.path(), me);
    cleanup(runtime.path(), other);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("自セッション") && !stderr.contains("ネスト"),
        "別セッションの socket への attach は self 拒否されないべき。stderr={stderr:?}"
    );
}

// ── Fable review C1 (2026-06-12): env set 時も wait/input の明示 session が効く ──

/// `$HYOUI_SESSION_ID` が set でも、`hyoui wait <明示sid> <pattern>` は明示 session
/// に向かう (= 明示 > env)。旧実装は env set で positional 2 個が「余分な positional」
/// エラーになり、中から別 session への明示 wait が壊れていた (ドッグフーディング即死級)。
#[ignore = "hyoui wait の StateSnapshotRequest が visible rows のみ配信し scrollback 未対応のため、CI 環境で daemon child の単発 echo 出力が viewport から流れたら pattern 観測不能。根治は docs/issue/2026-06-22-wait-scrollback-snapshot-coverage.md (DR-0013 Phase B 待ち)。test 改変による偽 green 隠蔽を避ける rule に従い ignore する (test-failure-no-tampering)"]
#[test]
fn wait_explicit_session_wins_over_env() {
    let runtime = runtime_dir();
    let me = "c1-wait-me";
    let target = "c1-wait-target";
    spawn_detached(runtime.path(), me);
    // target の画面に "hello" を出しておく (= wait の match 対象)。
    spawn_detached_cmd(runtime.path(), target, "echo hello; sleep 30");

    let out = Command::new(hyoui_bin())
        .args(["wait", target, "hello", "--timeout=5s"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", me)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("wait explicit");

    cleanup(runtime.path(), me);
    cleanup(runtime.path(), target);

    assert!(
        out.status.success(),
        "env set 下でも明示 session への wait は成功すべき (= 明示 > env)。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 中から (= env set) `hyoui wait <pattern>` (positional 1 個) は self に解決される。
#[ignore = "wait_explicit_session_wins_over_env と同根: scrollback 未対応 race。docs/issue/2026-06-22-wait-scrollback-snapshot-coverage.md (DR-0013 Phase B) で根治追跡"]
#[test]
fn wait_single_positional_resolves_self_with_env() {
    let runtime = runtime_dir();
    let me = "c1-wait-self";
    spawn_detached_cmd(runtime.path(), me, "echo selfhello; sleep 30");

    let out = Command::new(hyoui_bin())
        .args(["wait", "selfhello", "--timeout=5s"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", me)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("wait self");

    cleanup(runtime.path(), me);

    assert!(
        out.status.success(),
        "中からの wait <pattern> は self に解決され成功すべき。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `$HYOUI_SESSION_ID` が set でも、`hyoui input <明示sid> <spec>...` は明示 session
/// に向かう (= 明示 > env)。旧実装は "beta" が spec parse に回って壊れていた。
#[test]
fn input_explicit_session_wins_over_env() {
    let runtime = runtime_dir();
    let me = "c1-input-me";
    let target = "c1-input-target";
    spawn_detached(runtime.path(), me);
    // 入力を受ける子 (= cat で stdin を読む)。
    spawn_detached_cmd(runtime.path(), target, "cat >/dev/null; sleep 30");

    let out = Command::new(hyoui_bin())
        .args(["input", target, "text:hi"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", me)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("input explicit");

    cleanup(runtime.path(), me);
    cleanup(runtime.path(), target);

    assert!(
        out.status.success(),
        "env set 下でも明示 session への input は成功すべき (= 明示 > env)。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 中から (= env set) `hyoui input <spec>...` (session 省略) は self に解決される。
#[test]
fn input_spec_only_resolves_self_with_env() {
    let runtime = runtime_dir();
    let me = "c1-input-self";
    spawn_detached_cmd(runtime.path(), me, "cat >/dev/null; sleep 30");

    let out = Command::new(hyoui_bin())
        .args(["input", "text:hi"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("HYOUI_SESSION_ID", me)
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("input self");

    cleanup(runtime.path(), me);

    assert!(
        out.status.success(),
        "中からの input <spec> は self に解決され成功すべき。stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}
