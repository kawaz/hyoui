//! daemon socket path の解決 helper。
//!
//! `RunConfig.socket = Some(path)` ならそれを使う。`None` の場合は
//! XDG 的な考え方で候補 dir を決め、`<dir>/<session>.sock` を返す:
//!
//! 1. `$XDG_RUNTIME_DIR` が set されていて、かつ実在 dir なら `$XDG_RUNTIME_DIR/hyoui/`
//!    (Linux の典型、`/run/user/<uid>` が systemd-logind 等で mode 0700 で provision される)
//! 2. それ以外は `${XDG_CACHE_HOME:-$HOME/.cache}/hyoui/`:
//!    `${XDG_CACHE_HOME:-$HOME/.cache}/hyoui/<session>.sock`
//!    - macOS が daemon 生存中の `/tmp` を定期掃除して socket file だけを消し、
//!      session を外側から到達不能にするため、ユーザ管理下の cache dir を使う。
//!    - dir は **新規作成時** mode 0700。既存 dir は所有者と mode を verify
//!    - unix socket の `sun_path` 上限 (macOS 104 / Linux 108 bytes) は、完成 path を
//!      [`check_sun_path_len`] で bind 前に検査する。
//!
//! `$TMPDIR` は参照しない。macOS の per-user TMPDIR は長く `sun_path` 予算を
//! 圧迫するうえ、OS の掃除対象になるため socket の生存期間と合わない。
//!
//! # namespace (DR-0018)
//!
//! session を用途グループごとに分離するため、socket dir を namespace で分ける:
//!
//! - `default` namespace (= 既定): 上記の base dir **直下** にそのまま socket を置く
//!   (= 既存 session と完全互換、dir 移動なし)。
//! - それ以外の namespace `<ns>`: base dir の下に `<ns>/` サブ dir を 1 段掘り、
//!   その中に socket を置く (= `<base>/hyoui/<ns>/<session>.sock`)。
//!
//! namespace の解決順は `--namespace=X` flag > env `HYOUI_NAMESPACE` > `default`
//! ([`resolve_namespace`])。ns 名は [`hyoui::cli::validate_namespace`] で whitelist
//! validate される (= path traversal / `/` 禁止)。

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 自動生成 session_id (= 単発 `hyoui run` で衝突しない値)。
///
/// 形式: `run-<pid>-<rand4hex>` (例: `run-12345-9af3`)。
///
/// # R5-M24: なぜ pid 単独ではないか
///
/// 単純な `run-<pid>` だと:
///
/// * **衝突**: 同一 UID で `hyoui run` を高速連続で叩くと、kernel が直近の pid を
///   recycle した瞬間に新旧セッションが同名になりうる (32-bit pid wrap など)。
/// * **予測容易性**: 第三者が `pid` の取りうる範囲を total order で総当りでき、
///   socket dir 列挙無しでも socket path を直撃しうる (同 UID 信頼境界内なので
///   厳密な脅威ではないが defense-in-depth)。
///
/// 4 byte (= 32 bit) のランダム接尾辞を付けることで両方緩和する。urandom が
/// 開けない・読めない極端な環境では pid 単独に fallback して動作継続させる
/// (= silent regression、最悪ケースで旧挙動と同等)。
pub fn auto_session_id() -> String {
    let pid = std::process::id();
    match read_urandom_hex4() {
        Some(suffix) => format!("run-{pid}-{suffix}"),
        // urandom 不在は極めて稀。デバッグ容易性のため pid 単独 fallback。
        None => format!("run-{pid}"),
    }
}

/// `/dev/urandom` から 4 byte 読み、8-char lowercase hex string を返す。
///
/// 失敗時は `None`。caller は最低限の機能を保つために fallback すること。
fn read_urandom_hex4() -> Option<String> {
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).ok()?;
    let mut out = String::with_capacity(8);
    for b in &buf {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    Some(out)
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

/// `namespace` whitelist validator の io::Error 化 wrapper (= DR-0018)。
///
/// canonical な validator は [`hyoui::cli::validate_namespace`] にある。本関数は
/// 戻り値を `std::io::Error` (`ErrorKind::InvalidInput`) にラップして、socket dir
/// 解決系の戻り型と整合させるための薄い shim。
///
/// # Errors
///
/// `namespace` が whitelist に反する場合、`std::io::ErrorKind::InvalidInput` を返す。
pub fn validate_namespace(namespace: &str) -> std::io::Result<()> {
    hyoui::cli::validate_namespace(namespace)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

/// session namespace を解決する (= DR-0018)。
///
/// 優先順位 (高 → 低):
///
/// 1. `flag` (= `--namespace=X`) が `Some(非空)` ならその値
/// 2. env `HYOUI_NAMESPACE` が set + 非空ならその値
/// 3. どちらも無ければ [`hyoui::cli::DEFAULT_NAMESPACE`] (= `"default"`)
///
/// 返り値は **validate 前** の生文字列。caller は socket dir 解決の前段で
/// [`validate_namespace`] (or それを内包する [`resolve_in_namespace`]) を通すこと。
pub fn resolve_namespace(flag: Option<&str>) -> String {
    if let Some(v) = flag
        && !v.is_empty()
    {
        return v.to_string();
    }
    match std::env::var("HYOUI_NAMESPACE") {
        Ok(v) if !v.is_empty() => v,
        _ => hyoui::cli::DEFAULT_NAMESPACE.to_string(),
    }
}

/// daemon socket path を **namespace スコープ**で決定する (= DR-0018)。
///
/// `explicit = Some(p)` ならそのまま、`None` なら自動 path:
/// - `default` namespace → base dir 直下:
///   `$XDG_RUNTIME_DIR/hyoui/<sid>.sock` (= dir 実在時) /
///   `${XDG_CACHE_HOME:-$HOME/.cache}/hyoui/<sid>.sock`
/// - それ以外 → `<base>/<namespace>/<sid>.sock`
///
/// parent dir は **新規作成時のみ** mode 0700 で create、既存 dir は所有者/mode 検証。
/// `namespace` は事前に [`validate_namespace`] で検証される (= path traversal 防止)。
///
/// # Errors
///
/// `namespace` / `session_id` が whitelist 違反、または dir 作成・検証で失敗時に
/// [`std::io::Error`]。
pub fn resolve_in_namespace(
    explicit: Option<&str>,
    session_id: &str,
    namespace: &str,
) -> std::io::Result<PathBuf> {
    let env = EnvSnapshot {
        xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
        xdg_cache_home: std::env::var_os("XDG_CACHE_HOME"),
        home_dir: std::env::var_os("HOME"),
        uid: nix::unistd::geteuid().as_raw(),
        namespace: namespace.to_string(),
    };
    resolve_with_env(explicit, session_id, &env)
}

/// 環境 snapshot (test injection 用)。
#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    /// `$XDG_RUNTIME_DIR` の値 (= 未設定なら None)。
    pub xdg_runtime_dir: Option<OsString>,
    /// `$XDG_CACHE_HOME` の値 (= 未設定なら None)。
    pub xdg_cache_home: Option<OsString>,
    /// `$HOME` の値 (= 未設定なら None)。XDG cache 未設定時に `.cache` を補う。
    pub home_dir: Option<OsString>,
    /// 現在の effective UID。
    pub uid: u32,
    /// 解決済 session namespace (= DR-0018)。`default` なら base dir 直下、
    /// それ以外は base dir 配下に `<namespace>/` サブ dir を掘る。
    pub namespace: String,
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
    // DR-0018: namespace も `PathBuf::join` 前に validate (= path traversal 防止)。
    validate_namespace(&env.namespace)?;
    // namespace dir も base dir と同じ規律 (= mode 0700 / 所有者検証) で ensure する。
    // `create_dir_all` は中間 dir の mode を umask に委ねてしまい 0700 を保証しない
    // ため、base → ns の順で `ensure_socket_dir` を 2 段呼びして両階層とも 0700 を
    // 強制する (= 非 default ns でも base dir の所有者/権限検証が効く)。
    let base = pick_base_dir(env)?;
    ensure_socket_dir(&base, env.uid)?;
    let dir = if env.namespace == hyoui::cli::DEFAULT_NAMESPACE {
        base
    } else {
        let ns_dir = base.join(&env.namespace);
        ensure_socket_dir(&ns_dir, env.uid)?;
        ns_dir
    };
    let path = dir.join(format!("{session_id}.sock"));
    check_sun_path_len(&path, &env.namespace, session_id)?;
    Ok(path)
}

/// `path` の byte 長が `sun_path` に収まるか事前チェックする (= DR-0018 / ENAMETOOLONG bug)。
///
/// 超える場合、現在長 / 上限 / 短くする方法 (ns・session 名) を含む人間可読の
/// `io::Error` を返す。`bind`/`connect` 直前の不親切な ENAMETOOLONG を防ぐ。
/// 上限は `hyoui::sys::socket::sun_path_max()` (= macOS 104 / Linux 108 から
/// NUL 終端を引いた値) を参照する (= libc 依存を hyoui crate 側に閉じる)。
fn check_sun_path_len(path: &Path, namespace: &str, session_id: &str) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let len = path.as_os_str().as_bytes().len();
    let max = hyoui::sys::socket::sun_path_max();
    if len > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "socket path が unix domain socket の上限を超えています \
                 (現在 {len} bytes > 上限 {max} bytes): {}\n\
                 \x20      namespace ({namespace:?}) か session 名 ({session_id:?}) を \
                 短くするか、より短い XDG_RUNTIME_DIR / XDG_CACHE_HOME を指定してください。",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// namespace を含めない base socket dir を選ぶ。
///
/// 1. `XDG_RUNTIME_DIR` が空でなく実在 dir → `$XDG_RUNTIME_DIR/hyoui`
/// 2. `XDG_CACHE_HOME` が空でない → `$XDG_CACHE_HOME/hyoui`
/// 3. それ以外 → `$HOME/.cache/hyoui`
fn pick_base_dir(env: &EnvSnapshot) -> std::io::Result<PathBuf> {
    if let Some(xdg) = env.xdg_runtime_dir.as_ref()
        && !xdg.is_empty()
    {
        let p = PathBuf::from(xdg);
        if p.is_dir() {
            return Ok(p.join("hyoui"));
        }
    }
    pick_cache_base_dir(env)
}

/// `hyoui list` が走査する、現在実在する base socket dir を優先順で返す。
/// resolver と同じ環境 snapshot / fallback 規則を使い、起動と列挙の path drift を防ぐ。
pub fn existing_base_dirs() -> Vec<PathBuf> {
    let env = EnvSnapshot {
        xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
        xdg_cache_home: std::env::var_os("XDG_CACHE_HOME"),
        home_dir: std::env::var_os("HOME"),
        uid: nix::unistd::geteuid().as_raw(),
        namespace: hyoui::cli::DEFAULT_NAMESPACE.to_string(),
    };
    let mut dirs = Vec::new();
    if let Some(runtime) = env.xdg_runtime_dir.as_ref()
        && !runtime.is_empty()
    {
        let runtime_dir = PathBuf::from(runtime).join("hyoui");
        if runtime_dir.is_dir() {
            dirs.push(runtime_dir);
        }
    }
    if let Ok(cache_dir) = pick_cache_base_dir(&env)
        && cache_dir.is_dir()
        && !dirs.contains(&cache_dir)
    {
        dirs.push(cache_dir);
    }
    dirs
}

fn pick_cache_base_dir(env: &EnvSnapshot) -> std::io::Result<PathBuf> {
    if let Some(cache) = env.xdg_cache_home.as_ref()
        && !cache.is_empty()
    {
        return Ok(PathBuf::from(cache).join("hyoui"));
    }
    let home = env
        .home_dir
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "socket dir を解決できません: XDG_CACHE_HOME または HOME を設定してください",
            )
        })?;
    Ok(PathBuf::from(home).join(".cache").join("hyoui"))
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

    fn env_with(xdg: Option<&Path>, cache: Option<&Path>) -> EnvSnapshot {
        env_with_ns(xdg, cache, hyoui::cli::DEFAULT_NAMESPACE)
    }

    fn env_with_ns(xdg: Option<&Path>, cache: Option<&Path>, ns: &str) -> EnvSnapshot {
        EnvSnapshot {
            xdg_runtime_dir: xdg.map(|p| p.as_os_str().to_os_string()),
            xdg_cache_home: cache.map(|p| p.as_os_str().to_os_string()),
            home_dir: None,
            uid: nix::unistd::geteuid().as_raw(),
            namespace: ns.to_string(),
        }
    }

    #[test]
    fn auto_session_id_contains_pid() {
        let sid = auto_session_id();
        assert!(sid.starts_with("run-"));
        assert!(sid.len() > "run-".len());
        let pid_str = std::process::id().to_string();
        // pid 部分が含まれていることを確認 (urandom 有無に関わらず成立)。
        assert!(
            sid.contains(&pid_str),
            "auto_session_id {sid:?} missing pid {pid_str}"
        );
    }

    /// R5-M24: `/dev/urandom` が読める通常環境では 4 byte (= 8 hex char) の
    /// 接尾辞が付き、`run-<pid>-<rand>` 形式になる。
    #[test]
    fn auto_session_id_has_random_suffix_when_urandom_available() {
        // 連続生成して全部同じだったら接尾辞が効いていない (pid は同じプロセスで不変)。
        // urandom 不在環境では 1 種類だけ返るので skip 条件付き。
        if !std::path::Path::new("/dev/urandom").exists() {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            seen.insert(auto_session_id());
        }
        assert!(
            seen.len() > 1,
            "expected randomized suffix, got identical ids: {seen:?}"
        );
        // 形式チェック: 各 id が `run-<pid>-<8 hex>` の構造か。
        let pid = std::process::id();
        let pid_prefix = format!("run-{pid}-");
        for id in &seen {
            assert!(id.starts_with(&pid_prefix), "bad format: {id}");
            let suffix = &id[pid_prefix.len()..];
            assert_eq!(suffix.len(), 8, "suffix should be 8 hex chars: {id}");
            assert!(
                suffix.bytes().all(|b| b.is_ascii_hexdigit()),
                "suffix must be hex: {id}"
            );
        }
    }

    /// R5-M24: 生成された id は `validate_session_id` を通過する
    /// (= filesystem path として安全)。
    #[test]
    fn auto_session_id_passes_validation() {
        let sid = auto_session_id();
        hyoui::cli::validate_session_id(&sid)
            .unwrap_or_else(|e| panic!("auto_session_id {sid:?} failed validation: {e}"));
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
            xdg_cache_home: Some(tmp.path().as_os_str().to_os_string()),
            home_dir: None,
            uid: nix::unistd::geteuid().as_raw(),
            namespace: hyoui::cli::DEFAULT_NAMESPACE.to_string(),
        };
        let got = resolve_with_env(None, "x", &env).expect("resolve");
        assert!(
            got.starts_with(tmp.path()),
            "should use XDG_CACHE_HOME fallback, got {got:?}"
        );
    }

    #[test]
    fn xdg_runtime_dir_ignored_when_not_a_dir() {
        // 存在しない runtime path → XDG cache fallback
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-tmp-")
            .tempdir()
            .expect("tempdir");
        let bogus = PathBuf::from("/this/path/does/not/exist/probably/abc123");
        let env = EnvSnapshot {
            xdg_runtime_dir: Some(bogus.as_os_str().to_os_string()),
            xdg_cache_home: Some(tmp.path().as_os_str().to_os_string()),
            home_dir: None,
            uid: nix::unistd::geteuid().as_raw(),
            namespace: hyoui::cli::DEFAULT_NAMESPACE.to_string(),
        };
        let got = resolve_with_env(None, "x", &env).expect("resolve");
        assert!(
            got.starts_with(tmp.path()),
            "should fall back to XDG cache base"
        );
    }

    /// XDG runtime が使えない場合、XDG cache を fallback として使い、作成する
    /// `hyoui` dir は同 UID のみアクセスできる mode 0700 にする。
    #[test]
    fn xdg_cache_home_used_when_runtime_unset() {
        let cache = tempfile::Builder::new()
            .prefix("hyoui-cache-")
            .tempdir()
            .expect("tempdir");
        let env = env_with(None, Some(cache.path()));
        let got = resolve_with_env(None, "mysession", &env).expect("resolve");
        assert_eq!(got, cache.path().join("hyoui").join("mysession.sock"));
        let parent = got.parent().unwrap();
        let meta = std::fs::metadata(parent).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    /// XDG cache も未設定なら HOME 配下の `.cache/hyoui` を fallback にする。
    #[test]
    fn home_cache_used_when_xdg_dirs_unset() {
        let home = tempfile::Builder::new()
            .prefix("hyoui-home-")
            .tempdir()
            .expect("tempdir");
        let env = EnvSnapshot {
            xdg_runtime_dir: None,
            xdg_cache_home: None,
            home_dir: Some(home.path().as_os_str().to_os_string()),
            uid: nix::unistd::geteuid().as_raw(),
            namespace: hyoui::cli::DEFAULT_NAMESPACE.to_string(),
        };
        let got = resolve_with_env(None, "mysession", &env).expect("resolve");
        assert_eq!(
            got,
            home.path()
                .join(".cache")
                .join("hyoui")
                .join("mysession.sock")
        );
        assert_eq!(
            std::fs::metadata(got.parent().unwrap())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    /// fallback の根になる環境変数が両方無ければ、意図しない相対 path を作らず失敗する。
    #[test]
    fn missing_cache_and_home_is_an_error() {
        let env = env_with(None, None);
        let err = resolve_with_env(None, "mysession", &env).expect_err("must err");
        assert!(err.to_string().contains("XDG_CACHE_HOME"));
        assert!(err.to_string().contains("HOME"));
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

    // ------------------------------------------------------------------
    // DR-0018: namespace resolution / socket dir 分離
    // ------------------------------------------------------------------

    /// `default` namespace は base dir **直下** に socket を置く (= 既存互換、dir 移動なし)。
    #[test]
    fn default_namespace_places_socket_directly_under_base() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-ns-default-")
            .tempdir()
            .expect("tempdir");
        let env = env_with_ns(None, Some(tmp.path()), "default");
        let got = resolve_with_env(None, "mysession", &env).expect("resolve");
        assert_eq!(got, tmp.path().join("hyoui").join("mysession.sock"));
    }

    /// 非 default namespace は base dir 配下に `<ns>/` サブ dir を掘る。
    #[test]
    fn non_default_namespace_uses_subdir() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-ns-sub-")
            .tempdir()
            .expect("tempdir");
        let env = env_with_ns(None, Some(tmp.path()), "workers");
        let got = resolve_with_env(None, "w1", &env).expect("resolve");
        assert_eq!(
            got,
            tmp.path().join("hyoui").join("workers").join("w1.sock")
        );
        // ns dir も base dir も mode 0700。
        let ns_dir = got.parent().unwrap();
        let base_dir = ns_dir.parent().unwrap();
        assert_eq!(
            std::fs::metadata(ns_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(base_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// namespace に `/` を含む値は reject される (= path traversal / 階層予約)。
    #[test]
    fn resolve_rejects_namespace_with_slash() {
        let env = env_with_ns(None, None, "a/b");
        let err = resolve_with_env(None, "x", &env).expect_err("slash ns must err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// namespace に `..` を含む値は reject される (= traversal)。
    #[test]
    fn resolve_rejects_namespace_dotdot() {
        for ns in ["..", "../escape"] {
            let env = env_with_ns(None, None, ns);
            let err = resolve_with_env(None, "x", &env).expect_err(&format!("ns {ns:?} must err"));
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "ns={ns:?}");
        }
    }

    /// 空 namespace は reject される。
    #[test]
    fn resolve_rejects_empty_namespace() {
        let env = env_with_ns(None, None, "");
        let err = resolve_with_env(None, "x", &env).expect_err("empty ns must err");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// `resolve_namespace`: flag > env > default の優先順位。
    #[test]
    fn resolve_namespace_precedence() {
        // flag は env より優先 (= env 値があっても flag が勝つ)。
        // production env を汚さないよう、flag 優先パスのみ env 非依存で検証する。
        assert_eq!(resolve_namespace(Some("flagns")), "flagns");
        // 空 flag は無視 → env or default。
        // 既存 env を読み取って期待値を組む (= test 並列実行で env を書き換えない)。
        let expected = match std::env::var("HYOUI_NAMESPACE") {
            Ok(v) if !v.is_empty() => v,
            _ => "default".to_string(),
        };
        assert_eq!(resolve_namespace(Some("")), expected);
        assert_eq!(resolve_namespace(None), expected);
    }

    /// 極端に長い session 名は sun_path 上限を超えるため、resolve 時点で
    /// friendly error (= 現在長 / 上限 / 短くする方法を含む) を返す。
    #[test]
    fn resolve_rejects_too_long_sun_path() {
        // 長い cache base + `<ns>/<sid>.sock` を上限超えにするため、ns + sid を
        // それぞれ MAX 長近くまで伸ばす。各 component は whitelist 長制限内に収め、
        // 合計 path 長で sun_path 上限を超えさせる。
        let max_comp = hyoui::cli::MAX_SESSION_ID_LEN; // ns/sid 共通の上限想定
        let long_ns = "n".repeat(max_comp);
        let long_sid = "s".repeat(max_comp);
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-toolong-")
            .tempdir()
            .expect("tempdir");
        let env = env_with_ns(None, Some(tmp.path()), &long_ns);
        let err = resolve_with_env(None, &long_sid, &env)
            .expect_err("too long sun_path must err (or component validate err)");
        // component validate (長すぎる ns/sid) か sun_path 事前チェックのどちらかで
        // InvalidInput になる。いずれにせよ bind 前に弾けていることが重要。
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "err={err}");
    }

    /// sun_path 上限ちょうど近辺: 短い ns/sid なら通る (= 正常系の回帰防止)。
    #[test]
    fn resolve_accepts_normal_length_under_limit() {
        let tmp = tempfile::Builder::new()
            .prefix("hyoui-oklen-")
            .tempdir()
            .expect("tempdir");
        let env = env_with_ns(None, Some(tmp.path()), "ns1");
        let got = resolve_with_env(None, "sess1", &env).expect("normal length must resolve");
        assert!(got.to_string_lossy().ends_with("sess1.sock"));
    }

    /// `validate_namespace` の error は `namespace` 文脈の文言になる。
    #[test]
    fn validate_namespace_error_mentions_namespace() {
        let err = validate_namespace("").expect_err("empty must err");
        assert!(
            err.to_string().contains("namespace"),
            "error should mention namespace, got: {err}"
        );
        assert!(
            !err.to_string().contains("session_id"),
            "error should not leak session_id wording, got: {err}"
        );
    }
}
