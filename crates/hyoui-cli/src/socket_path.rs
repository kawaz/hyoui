//! daemon socket path の解決 helper。
//!
//! `RunConfig.socket = Some(path)` ならそれを使う。`None` の場合は
//! 環境変数から候補 dir を決め、`<dir>/<session>.sock` を返す:
//! 1. `$XDG_RUNTIME_DIR/hyoui/<session>.sock` (typically Linux)
//! 2. それも不可なら `$HOME/.hyoui/sock/<session>.sock`
//!
//! parent dir は mode 0700 で必要に応じて mkdir (`UnixSock::listen` の前提)。

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// 自動生成 session_id (= 単発 `hyoui run` で衝突しない値)。
pub fn auto_session_id() -> String {
    format!("run-{}", std::process::id())
}

/// daemon socket path を決定する (env 読み取り版)。
///
/// `explicit = Some(p)` ならそのまま、`None` なら自動 path:
/// `$XDG_RUNTIME_DIR/hyoui/<sid>.sock` → fallback `$HOME/.hyoui/sock/<sid>.sock`。
/// parent dir は mode 0700 で作成する。
///
/// # Errors
///
/// dir 作成失敗時に [`std::io::Error`]。`$XDG_RUNTIME_DIR` も `$HOME` も
/// 取れない場合は `NotFound`。
pub fn resolve(explicit: Option<&str>, session_id: &str) -> std::io::Result<PathBuf> {
    resolve_with_env(
        explicit,
        session_id,
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("HOME"),
    )
}

/// `resolve` の env を呼び出し側に注入できる test 用版。
pub fn resolve_with_env(
    explicit: Option<&str>,
    session_id: &str,
    xdg_runtime_dir: Option<OsString>,
    home: Option<OsString>,
) -> std::io::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    let dir = pick_socket_dir(xdg_runtime_dir, home)?;
    ensure_mode_0700(&dir)?;
    Ok(dir.join(format!("{session_id}.sock")))
}

fn pick_socket_dir(
    xdg_runtime_dir: Option<OsString>,
    home: Option<OsString>,
) -> std::io::Result<PathBuf> {
    if let Some(xdg) = xdg_runtime_dir {
        return Ok(PathBuf::from(xdg).join("hyoui"));
    }
    let home =
        home.ok_or_else(|| std::io::Error::other("neither $XDG_RUNTIME_DIR nor $HOME is set"))?;
    Ok(PathBuf::from(home).join(".hyoui").join("sock"))
}

fn ensure_mode_0700(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let perm = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(dir, perm)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_session_id_contains_pid() {
        let sid = auto_session_id();
        assert!(sid.starts_with("run-"));
        assert!(sid.len() > "run-".len());
    }

    #[test]
    fn explicit_path_passes_through() {
        let got = resolve_with_env(Some("/tmp/x.sock"), "demo", None, None).expect("resolve");
        assert_eq!(got, PathBuf::from("/tmp/x.sock"));
    }

    #[test]
    fn xdg_runtime_dir_used_when_set() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-xdg-test-")
            .tempdir()
            .expect("tempdir");
        let got = resolve_with_env(
            None,
            "mysession",
            Some(tmp.path().as_os_str().to_os_string()),
            None,
        )
        .expect("resolve");
        assert_eq!(got, tmp.path().join("hyoui").join("mysession.sock"));
        let parent = got.parent().unwrap();
        let meta = std::fs::metadata(parent).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn home_used_when_xdg_unset() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-home-test-")
            .tempdir()
            .expect("tempdir");
        let got = resolve_with_env(
            None,
            "mysession",
            None,
            Some(tmp.path().as_os_str().to_os_string()),
        )
        .expect("resolve");
        assert_eq!(
            got,
            tmp.path()
                .join(".hyoui")
                .join("sock")
                .join("mysession.sock")
        );
        let parent = got.parent().unwrap();
        let meta = std::fs::metadata(parent).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn errors_when_neither_env_set() {
        let err = resolve_with_env(None, "x", None, None).expect_err("must err");
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}
