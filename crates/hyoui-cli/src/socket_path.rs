//! daemon socket path の解決 helper (DR-0041 決定 4)。
//!
//! `--socket=<path>` が明示されていればそれを使う。無ければ
//! `<状態の root>/sessions/<session>.sock` を返す。状態の root (= 面) は
//! [`hyoui::paths::Env::state_root`] が `HYOUI_STATE_DIR` → `$XDG_STATE_HOME/hyoui`
//! (絶対パスの時だけ) → `$HOME/.local/state/hyoui` の順に決める。
//!
//! - `sessions/` は機能別の置き場で、root 直下に session の socket を置かない
//!   (= `web/` 等と並ぶ)。name lock (`<id>.lock`) と dir lock (`.dir.lock`) も同じ dir
//! - `sessions/` は **新規作成時** mode 0700。既存なら所有者と mode を verify
//! - root は無ければ mode 0700 で作る。既にある root の mode は見ない (=
//!   `HYOUI_STATE_DIR` で利用者が指した dir をそのまま使う。socket を守るのは
//!   `sessions/` の 0700)
//! - unix socket の `sun_path` の上限はフルパスで判定しない。収まらない時は
//!   `hyoui::sys::socket` が dir の fd 基準の相対名で bind / connect する (決定 5)
//!
//! `$TMPDIR` / `$XDG_RUNTIME_DIR` は使わない。runtime dir はログインに紐づく寿命で、
//! ログインを越えて動き続ける session と合わない (決定 6)。

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// hyoui が振る session id (= UUID の標準形、DR-0041 決定 2)。
pub fn auto_session_id() -> String {
    hyoui::cli::new_session_id()
}

/// `session_id` validator の io::Error 化 wrapper。
///
/// canonical な validator は [`hyoui::cli::validate_session_id`] にある
/// (= CLI parser と本 module の双方から呼ぶための共有ロジック)。
///
/// # Errors
///
/// `session_id` が UUID の標準形でない場合、`std::io::ErrorKind::InvalidInput`。
pub fn validate_session_id(session_id: &str) -> std::io::Result<()> {
    hyoui::cli::validate_session_id(session_id)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

/// daemon socket path を決定する。
///
/// `explicit = Some(p)` ならそのまま、`None` なら `<状態の root>/sessions/<sid>.sock`
/// (`sessions/` は作る)。
///
/// # Errors
///
/// `session_id` が UUID の標準形でない、状態の root を決められない、dir の作成・検証で
/// 失敗した時。
pub fn resolve(explicit: Option<&str>, session_id: &str) -> std::io::Result<PathBuf> {
    resolve_with_env(explicit, session_id, &EnvSnapshot::current())
}

/// 環境 snapshot (test injection 用)。
#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    /// 場所を決める env (= `web service register` が固定する一覧と同じ、DR-0038 決定 5)。
    pub env: hyoui::paths::Env,
    /// 現在の effective UID。
    pub uid: u32,
}

impl EnvSnapshot {
    /// この process の env から組む。
    pub fn current() -> Self {
        Self {
            env: hyoui::paths::Env::current(),
            uid: nix::unistd::geteuid().as_raw(),
        }
    }
}

/// [`resolve`] の env を呼び出し側で注入できる test 用版。
///
/// # Errors
///
/// [`resolve`] と同じ。
pub fn resolve_with_env(
    explicit: Option<&str>,
    session_id: &str,
    env: &EnvSnapshot,
) -> std::io::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    // session_id を `PathBuf::join` に渡す前に検証する。不正値なら dir 作成すら
    // 行わず即 reject (= traversal の副作用なし)。
    validate_session_id(session_id)?;
    let dir = ensure_sessions_dir(env)?;
    Ok(dir.join(format!("{session_id}.sock")))
}

/// `sessions/` を (root ごと) 用意して返す。
fn ensure_sessions_dir(env: &EnvSnapshot) -> std::io::Result<PathBuf> {
    let root = env.env.state_root()?;
    if std::fs::symlink_metadata(&root).is_err() {
        match create_private_dir(&root) {
            // 並行した run が先に作った (= 既にある root の mode は見ない)。
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            r => r?,
        }
    }
    let sessions = root.join("sessions");
    ensure_socket_dir(&sessions, env.uid)?;
    Ok(sessions)
}

/// `hyoui list` 等が走査する `sessions/` (= 実在する時だけ)。
///
/// 列挙は dir を作らない (= 一度も run していない面で `list` を打っても何も作らない)。
pub fn existing_sessions_dir() -> Option<PathBuf> {
    let dir = hyoui::paths::Env::current().sessions_dir().ok()?;
    dir.is_dir().then_some(dir)
}

/// `dir` を mode 0700 で作る (= 途中の dir も作る。mode を保証するのは末端だけ)。
///
/// 末端は mkdir の時点で 0700 にする (= umask 022 等で一旦 0755 の dir が見える瞬間を作らない。
/// 作ってから chmod する 2 段だと、並行した run がその瞬間の dir を見て mode 違いで断る)。
/// mkdir の mode は umask で bit が削られるだけなので 0700 を超えない。続く chmod は umask が
/// 持ち主の bit まで削った時の補正。末端が既にあれば `AlreadyExists` を返す。
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// `dir` を「mode 0700 + 所有者 = euid」で利用可能にする。
///
/// - dir が存在しない → mode 0700 で作る
/// - dir が既存 (= 前の run、または並行した run が先に作った) → 所有者と mode を verify、
///   不一致なら error (= 攻撃面回避)
///
/// 有無を見てから作る 2 段にせず、先に作って `AlreadyExists` なら verify に回す (= 有無の
/// 判定と作成の間に、並行した run が dir を作る隙間を作らない)。
fn ensure_socket_dir(dir: &Path, expected_uid: u32) -> std::io::Result<()> {
    match create_private_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_socket_dir(dir, expected_uid)
        }
        r => r,
    }
}

/// 既にある socket dir が「dir、mode 0700、所有者 = euid」か。
fn verify_socket_dir(dir: &Path, expected_uid: u32) -> std::io::Result<()> {
    let meta = std::fs::metadata(dir)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    const SID: &str = "0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f";

    fn snapshot(pairs: &[(&str, &Path)]) -> EnvSnapshot {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.as_os_str().to_os_string()))
            .collect();
        EnvSnapshot {
            env: hyoui::paths::Env::from_lookup(|name| map.get(name).cloned()),
            uid: nix::unistd::geteuid().as_raw(),
        }
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("hyoui-sp-")
            .tempdir()
            .expect("tempdir")
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// 自動の id は UUID の標準形で、毎回違う (DR-0041 決定 2)。
    #[test]
    fn auto_session_id_is_a_canonical_uuid() {
        let ids: std::collections::HashSet<String> = (0..8).map(|_| auto_session_id()).collect();
        assert_eq!(ids.len(), 8, "ids must differ: {ids:?}");
        for id in &ids {
            hyoui::cli::validate_session_id(id)
                .unwrap_or_else(|e| panic!("auto id {id:?} must validate: {e}"));
        }
    }

    #[test]
    fn explicit_path_passes_through() {
        let env = snapshot(&[]);
        let got = resolve_with_env(Some("/tmp/x.sock"), SID, &env).expect("resolve");
        assert_eq!(got, PathBuf::from("/tmp/x.sock"));
        // `--socket` 明示時は session id を検証しない (= 呼出側責任)。
        let got = resolve_with_env(Some("/tmp/x.sock"), "..", &env).expect("resolve");
        assert_eq!(got, PathBuf::from("/tmp/x.sock"));
    }

    /// socket は `<root>/sessions/<id>.sock`。`HYOUI_STATE_DIR` はそのまま root になる。
    #[test]
    fn socket_lives_under_sessions_of_hyoui_state_dir() {
        let tmp = tempdir();
        let root = tmp.path().join("face");
        let env = snapshot(&[
            ("HYOUI_STATE_DIR", &root),
            ("HOME", Path::new("/nonexistent")),
        ]);
        let got = resolve_with_env(None, SID, &env).expect("resolve");
        assert_eq!(got, root.join("sessions").join(format!("{SID}.sock")));
        assert_eq!(mode_of(&root), 0o700, "a new root is private");
        assert_eq!(mode_of(&root.join("sessions")), 0o700);
    }

    /// `HYOUI_STATE_DIR` が無ければ `$XDG_STATE_HOME/hyoui`、それも無ければ
    /// `$HOME/.local/state/hyoui` (DR-0041 決定 6)。
    #[test]
    fn root_falls_back_to_xdg_state_home_then_home() {
        let tmp = tempdir();
        let env = snapshot(&[
            ("XDG_STATE_HOME", tmp.path()),
            ("HOME", Path::new("/nonexistent")),
        ]);
        let got = resolve_with_env(None, SID, &env).expect("resolve");
        assert_eq!(
            got,
            tmp.path()
                .join("hyoui/sessions")
                .join(format!("{SID}.sock"))
        );

        let home = tempdir();
        let env = snapshot(&[("HOME", home.path())]);
        let got = resolve_with_env(None, SID, &env).expect("resolve");
        assert_eq!(
            got,
            home.path()
                .join(".local/state/hyoui/sessions")
                .join(format!("{SID}.sock"))
        );
    }

    /// root を決められなければ、相対 path を作らずに失敗する。
    #[test]
    fn missing_root_is_an_error() {
        let err = resolve_with_env(None, SID, &snapshot(&[])).expect_err("must err");
        assert!(err.to_string().contains("HYOUI_STATE_DIR"), "{err}");
        assert!(err.to_string().contains("HOME"), "{err}");
    }

    /// 既にある root (= 利用者が指した dir) の mode は問わない。`sessions/` は 0700 を要る。
    #[test]
    fn an_existing_root_is_used_as_is_but_sessions_must_be_private() {
        let tmp = tempdir();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = snapshot(&[("HYOUI_STATE_DIR", tmp.path())]);
        resolve_with_env(None, SID, &env).expect("an existing 0755 root is fine");
        assert_eq!(mode_of(tmp.path()), 0o755, "the root's mode is left alone");
        resolve_with_env(None, SID, &env).expect("an existing 0700 sessions/ is reused");

        std::fs::set_permissions(
            tmp.path().join("sessions"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let err = resolve_with_env(None, SID, &env).expect_err("0755 sessions/ must err");
        assert!(err.to_string().contains("mode"), "{err}");
    }

    /// UUID の標準形でない id は dir を作る前に弾く。
    #[test]
    fn resolve_rejects_a_non_uuid_session_id_before_touching_the_disk() {
        let tmp = tempdir();
        let root = tmp.path().join("face");
        let env = snapshot(&[("HYOUI_STATE_DIR", &root)]);
        for bad in ["../../.ssh/control", "demo", &SID.to_ascii_uppercase()] {
            let err = resolve_with_env(None, bad, &env).expect_err("must err");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert!(!root.exists(), "nothing is created for a rejected id");
    }

    /// フルパスが `sun_path` の上限を超えても解決は断らない (DR-0041 決定 5)。
    #[test]
    fn a_root_longer_than_sun_path_is_accepted() {
        let tmp = tempdir();
        let mut root = tmp.path().to_path_buf();
        while root.as_os_str().len() <= hyoui::sys::socket::sun_path_max() {
            root.push("a-long-directory-name");
        }
        let env = snapshot(&[("HYOUI_STATE_DIR", &root)]);
        let got = resolve_with_env(None, SID, &env).expect("long roots resolve");
        assert!(got.as_os_str().len() > hyoui::sys::socket::sun_path_max());
    }
}
