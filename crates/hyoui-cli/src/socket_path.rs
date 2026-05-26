//! daemon socket path の解決 helper。
//!
//! `RunConfig.socket = Some(path)` ならそれを使う。`None` の場合は
//! XDG 的な考え方で候補 dir を決め、`<dir>/<session>.sock` を返す:
//!
//! 1. `$XDG_RUNTIME_DIR` が set されていて、かつ実在 dir なら `$XDG_RUNTIME_DIR/hyoui/`
//!    (Linux の典型、`/run/user/<uid>` が systemd-logind 等で mode 0700 で provision される)
//! 2. それ以外 (= macOS や XDG 未設定環境) は **TMPDIR ベース**:
//!    `${TMPDIR:-/tmp}/hyoui-<uid>/<session>.sock`
//!    - 再起動でクリーンされてよい設計 (socket は永続化対象外)
//!    - `-<uid>` で multi-user 衝突回避
//!    - dir は **新規作成時** mode 0700。既存 dir は所有者と mode を verify
//!
//! HOME 直下 (`~/.hyoui` 等) は使わない (= ユーザ HOME を汚さない)。

use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 自動生成 session_id (= 単発 `hyoui run` で衝突しない値)。
pub fn auto_session_id() -> String {
    format!("run-{}", std::process::id())
}

/// `session_id` whitelist validator の io::Error 化 wrapper。
///
/// canonical な validator は [`hyoui::cli::validate_session_id`] にある
/// (= CLI parser と本 module の双方から呼ぶための共有ロジック)。本関数は
/// 戻り値を `std::io::Error` (`ErrorKind::InvalidInput`) にラップして、
/// `resolve_with_env` の戻り型と整合させるためだけの薄い shim。
///
/// # Errors
///
/// `session_id` が whitelist に反する場合、`std::io::ErrorKind::InvalidInput`
/// と人間可読の reason を含む `std::io::Error` を返す。
pub fn validate_session_id(session_id: &str) -> std::io::Result<()> {
    hyoui::cli::validate_session_id(session_id)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

/// daemon socket path を決定する。
///
/// `explicit = Some(p)` ならそのまま、`None` なら自動 path:
/// `$XDG_RUNTIME_DIR/hyoui/<sid>.sock` (= dir 実在時)、
/// それ以外は `${TMPDIR:-/tmp}/hyoui-<uid>/<sid>.sock`。
/// parent dir は **新規作成時のみ** mode 0700 で create、既存 dir は所有者/mode 検証。
///
/// # Errors
///
/// dir 作成・検証で失敗時に [`std::io::Error`]。
pub fn resolve(explicit: Option<&str>, session_id: &str) -> std::io::Result<PathBuf> {
    let env = EnvSnapshot {
        xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
        tmpdir: std::env::var_os("TMPDIR"),
        uid: nix::unistd::geteuid().as_raw(),
    };
    resolve_with_env(explicit, session_id, &env)
}

/// 環境 snapshot (test injection 用)。
#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    /// `$XDG_RUNTIME_DIR` の値 (= 未設定なら None)。
    pub xdg_runtime_dir: Option<OsString>,
    /// `$TMPDIR` の値 (= 未設定なら None、fallback で `/tmp` を使う)。
    pub tmpdir: Option<OsString>,
    /// 現在の effective UID。
    pub uid: u32,
}

/// `resolve` の env を呼び出し側で注入できる test 用版。
pub fn resolve_with_env(
    explicit: Option<&str>,
    session_id: &str,
    env: &EnvSnapshot,
) -> std::io::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    // R5-AUD-C2: session_id を `PathBuf::join` に渡す前に whitelist validate。
    // 不正値が来た場合は dir 作成すら行わず即 reject (= traversal の副作用なし)。
    validate_session_id(session_id)?;
    let dir = pick_socket_dir(env)?;
    ensure_socket_dir(&dir, env.uid)?;
    Ok(dir.join(format!("{session_id}.sock")))
}

/// 候補 dir を選ぶ。
///
/// 1. `XDG_RUNTIME_DIR` が空でなく実在 dir → `$XDG_RUNTIME_DIR/hyoui`
/// 2. それ以外 → `${TMPDIR:-/tmp}/hyoui-<uid>`
fn pick_socket_dir(env: &EnvSnapshot) -> std::io::Result<PathBuf> {
    if let Some(xdg) = env.xdg_runtime_dir.as_ref()
        && !xdg.is_empty()
    {
        let p = PathBuf::from(xdg);
        if p.is_dir() {
            return Ok(p.join("hyoui"));
        }
    }
    let tmp = env
        .tmpdir
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Ok(tmp.join(format!("hyoui-{}", env.uid)))
}

/// `dir` を「mode 0700 + 所有者 = euid」で利用可能にする。
///
/// - dir が存在しない → 新規作成し mode 0700 を設定
/// - dir が既存 → 所有者と mode を verify、不一致なら error (= 攻撃面回避)
fn ensure_socket_dir(dir: &Path, expected_uid: u32) -> std::io::Result<()> {
    match std::fs::metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(std::io::Error::other(format!(
                    "socket dir {dir:?} exists but is not a directory"
                )));
            }
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o700 {
                return Err(std::io::Error::other(format!(
                    "socket dir {dir:?} has mode {mode:o}, expected 0700"
                )));
            }
            if meta.uid() != expected_uid {
                return Err(std::io::Error::other(format!(
                    "socket dir {dir:?} owner uid={} mismatches euid={expected_uid}",
                    meta.uid()
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)?;
            let perm = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(dir, perm)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(xdg: Option<&Path>, tmp: Option<&Path>) -> EnvSnapshot {
        EnvSnapshot {
            xdg_runtime_dir: xdg.map(|p| p.as_os_str().to_os_string()),
            tmpdir: tmp.map(|p| p.as_os_str().to_os_string()),
            uid: nix::unistd::geteuid().as_raw(),
        }
    }

    #[test]
    fn auto_session_id_contains_pid() {
        let sid = auto_session_id();
        assert!(sid.starts_with("run-"));
        assert!(sid.len() > "run-".len());
    }

    #[test]
    fn explicit_path_passes_through() {
        let env = env_with(None, None);
        let got = resolve_with_env(Some("/tmp/x.sock"), "demo", &env).expect("resolve");
        assert_eq!(got, PathBuf::from("/tmp/x.sock"));
    }

    #[test]
    fn xdg_runtime_dir_used_when_set_and_exists() {
        let xdg = tempfile::Builder::new()
            .prefix("hyoui-xdg-test-")
            .tempdir()
            .expect("tempdir");
        let env = env_with(Some(xdg.path()), None);
        let got = resolve_with_env(None, "mysession", &env).expect("resolve");
        assert_eq!(got, xdg.path().join("hyoui").join("mysession.sock"));
        let parent = got.parent().unwrap();
        let meta = std::fs::metadata(parent).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn xdg_runtime_dir_ignored_when_empty() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-tmp-")
            .tempdir()
            .expect("tempdir");
        let env = EnvSnapshot {
            xdg_runtime_dir: Some(OsString::new()), // 空 string は無視
            tmpdir: Some(tmp.path().as_os_str().to_os_string()),
            uid: nix::unistd::geteuid().as_raw(),
        };
        let got = resolve_with_env(None, "x", &env).expect("resolve");
        assert!(
            got.starts_with(tmp.path()),
            "should use tmpdir, got {got:?}"
        );
    }

    #[test]
    fn xdg_runtime_dir_ignored_when_not_a_dir() {
        // 存在しない path → fallback to TMPDIR
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-tmp-")
            .tempdir()
            .expect("tempdir");
        let bogus = PathBuf::from("/this/path/does/not/exist/probably/abc123");
        let env = EnvSnapshot {
            xdg_runtime_dir: Some(bogus.as_os_str().to_os_string()),
            tmpdir: Some(tmp.path().as_os_str().to_os_string()),
            uid: nix::unistd::geteuid().as_raw(),
        };
        let got = resolve_with_env(None, "x", &env).expect("resolve");
        assert!(got.starts_with(tmp.path()), "should fall back to tmpdir");
    }

    #[test]
    fn tmpdir_used_when_xdg_unset() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-tmp-")
            .tempdir()
            .expect("tempdir");
        let uid = nix::unistd::geteuid().as_raw();
        let env = env_with(None, Some(tmp.path()));
        let got = resolve_with_env(None, "mysession", &env).expect("resolve");
        assert_eq!(
            got,
            tmp.path()
                .join(format!("hyoui-{uid}"))
                .join("mysession.sock")
        );
        let parent = got.parent().unwrap();
        let meta = std::fs::metadata(parent).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn tmpdir_defaults_to_slash_tmp() {
        // /tmp は通常 mode 1777 (sticky)。`hyoui-<uid>` という subdirは存在しない
        // 想定なので、resolve は新規作成して mode 0700 にする。
        // ここでは /tmp が書き込み可能か (= CI runner で書ける) を期待。
        let env = env_with(None, None);
        let unique_sid = format!("test-default-tmp-{}-{}", std::process::id(), rand_token());
        let got = resolve_with_env(None, &unique_sid, &env).expect("resolve");
        let uid = nix::unistd::geteuid().as_raw();
        assert!(
            got.starts_with(format!("/tmp/hyoui-{uid}")),
            "expected /tmp/hyoui-<uid>/..., got {got:?}"
        );
        // cleanup: 作った dir 配下を消す (子 sock file は無いので dir のみ削除)
        let _ = std::fs::remove_dir_all(format!("/tmp/hyoui-{uid}"));
    }

    #[test]
    fn ensure_socket_dir_rejects_wrong_mode() {
        // 既存 dir が mode 0755 → error
        let dir = tempfile::Builder::new()
            .prefix("hyoui-mode-test-")
            .tempdir()
            .expect("tempdir");
        let perm = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dir.path(), perm).expect("chmod 0755");
        let uid = nix::unistd::geteuid().as_raw();
        let err = ensure_socket_dir(dir.path(), uid).expect_err("must err");
        assert!(err.to_string().contains("mode"));
    }

    // ------------------------------------------------------------------
    // R5-AUD-C2: session_id whitelist validation regression tests
    // ------------------------------------------------------------------

    #[test]
    fn socket_path_accepts_normal_alphanumeric() {
        // 正常系: ASCII 英数字 + `._-` 全てを使った典型値が通る。
        for sid in [
            "demo",
            "run-12345",
            "session_01",
            "build.2025-05-27",
            "a",
            "A1b2_C3.D4-E5",
        ] {
            validate_session_id(sid)
                .unwrap_or_else(|e| panic!("session_id {sid:?} should be accepted, got {e}"));
        }
    }

    #[test]
    fn socket_path_rejects_dot_dot_traversal() {
        // `..` 単独 / `../foo` / `foo/../bar` 等の path traversal を全部 reject。
        for sid in ["..", "../etc", "../../.ssh/control", "a/../b"] {
            let err = validate_session_id(sid).expect_err(&format!("{sid:?} must err"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "sid={sid:?}");
        }
    }

    #[test]
    fn socket_path_rejects_slash() {
        // `/` (= POSIX separator) / `\` (= Windows separator) を含む値を reject。
        for sid in ["/abs/path", "rel/path", "a\\b", "x/y/z"] {
            let err = validate_session_id(sid).expect_err(&format!("{sid:?} must err"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "sid={sid:?}");
        }
    }

    #[test]
    fn socket_path_rejects_empty_string() {
        let err = validate_session_id("").expect_err("empty string must err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("empty"),
            "message should mention 'empty', got: {err}"
        );
    }

    #[test]
    fn socket_path_rejects_single_dot() {
        // `.` 単独も path traversal component なので reject。
        let err = validate_session_id(".").expect_err("'.' must err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn socket_path_rejects_control_chars() {
        // ANSI escape / 改行 / NUL は whitelist 外なので reject。
        // R5-AUD-M4 で指摘の terminal escape injection 対策も兼ねる。
        for sid in ["a\nb", "a\x1b[31mhack", "a\0b", "a b", "tab\tname"] {
            let err = validate_session_id(sid).expect_err(&format!("{sid:?} must err"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "sid={sid:?}");
        }
    }

    #[test]
    fn socket_path_rejects_too_long() {
        // 65 chars (= MAX + 1) は reject。
        let max = hyoui::cli::MAX_SESSION_ID_LEN;
        let long = "a".repeat(max + 1);
        let err = validate_session_id(&long).expect_err("too long must err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("too long"),
            "message should mention 'too long', got: {err}"
        );

        // 64 chars (= MAX 丁度) は通る。
        let max_ok = "a".repeat(max);
        validate_session_id(&max_ok).expect("MAX length must be accepted");
    }

    #[test]
    fn resolve_rejects_traversal_session_id() {
        // resolve_with_env 経由でも path traversal 値は reject される
        // (= 偶然 dir 検証を bypass しても socket file 生成に到達できない)。
        let env = env_with(None, None);
        let err =
            resolve_with_env(None, "../../.ssh/control", &env).expect_err("traversal must err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn resolve_explicit_path_bypasses_validation() {
        // `--socket=<path>` 指定時は session_id 不要 / validate されない
        // (= explicit path は呼出側責任、本 validator は session_id resolver 専用)。
        let env = env_with(None, None);
        let got = resolve_with_env(Some("/tmp/explicit.sock"), "..", &env)
            .expect("explicit path bypasses session_id validate");
        assert_eq!(got, PathBuf::from("/tmp/explicit.sock"));
    }

    fn rand_token() -> String {
        // 単純な「test ごとに変わる」値、衝突確率は低い
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}
