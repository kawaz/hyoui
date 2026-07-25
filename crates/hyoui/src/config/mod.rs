//! User config file (`~/.config/hyoui/config.toml`) loader (DR-0024).
//!
//! hyoui の persistent setting 機構。env scrub と attach UX 設定を扱う。
//!
//! ## Path resolution
//!
//! 1. `$XDG_CONFIG_HOME/hyoui/config.toml` (環境変数指定時)
//! 2. `$HOME/.config/hyoui/config.toml` (XDG 不在時)
//! 3. どちらも resolve できなければ unloadable (= [`Config::default`] を使う)
//!
//! ## 不在 / エラー時 (DR-0024 §7)
//!
//! - ファイル不在 = [`Config::default`] (= builtin-only 動作)
//! - パースエラー = `Err(ConfigError::Parse(_))` 。caller が exit non-zero
//!   で起動を拒否する (= 意図しない設定での起動は害)
//! - unknown field は warn なしで無視 (= 前方互換性、`deny_unknown_fields` を
//!   付けない)

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// hyoui 全体設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Config {
    /// 子 PTY env scrub 設定 (= TOML の `[scrub_env]` セクション)。
    #[serde(default)]
    pub scrub_env: ScrubEnvConfig,

    /// attach client UX 設定 (= TOML の `[attach]` セクション、DR-0029)。
    #[serde(default)]
    pub attach: AttachConfig,

    /// session 単位の policy 設定 (= TOML の `[session]` セクション、DR-0029)。
    #[serde(default)]
    pub session: SessionConfig,

    /// Web gateway 設定 (= TOML の `[web]` セクション、DR-0027)。
    #[serde(default)]
    pub web: WebConfig,
}

/// session policy 設定 (= TOML の `[session]` 配下、DR-0029 §4)。
///
/// `hyoui run` が daemon に渡す既定値を持つ。CLI flag (`--on-child-suspend`) が
/// あればそちらが優先する (= DR-0024 の flag 最小化方針、config は default 提供)。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct SessionConfig {
    /// 子の stop を daemon が観測したら自動で `SIGCONT` を送る (= DR-0019 §3 の
    /// `--on-child-suspend=auto-resume` の既定値)。
    ///
    /// default `false` (= notify のみ)。attach client の有無・stop の由来 (tty の
    /// Ctrl+Z / `hyoui kill --signal=TSTP` / 子の self-suspend) に関わらず効く。
    #[serde(default)]
    pub auto_resume: bool,
}

/// Web gateway 設定 (= TOML の `[web]` 配下、DR-0027 §Decision.2)。
///
/// `hyoui web` subcommand が listen する host:port を持つ。CLI flag
/// `--listen` があれば config を上書きする (= DR-0024 の flag 最小化方針)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct WebConfig {
    /// listen する host:port (= default `127.0.0.1:43690` = 0xAAAA、DR-0027)。
    ///
    /// 前段 Caddy reverse proxy 想定 (= HTTPS / auth は前段が担う)。tailnet の外に
    /// 直接晒す場合は将来 DR で auth を扱う。
    #[serde(default = "default_web_listen")]
    pub listen: String,

    /// 静的アセットの開発モード配信元 (= 指定時はローカル dir を都度読む、
    /// DR-0027 §4)。`None` (default) ならリリースビルドに埋め込まれた assets を返す。
    #[serde(default)]
    pub assets_dir: Option<std::path::PathBuf>,
}

fn default_web_listen() -> String {
    "127.0.0.1:43690".to_string()
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            listen: default_web_listen(),
            assets_dir: None,
        }
    }
}

/// attach client UX 設定 (= TOML の `[attach]` 配下、DR-0029 §3)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AttachConfig {
    /// tty stdin 経路の Ctrl+Z ガードを有効にする。
    ///
    /// `true` (default) なら Ctrl+Z 単発は「子に届けず client を detach」、2 発ごとに
    /// 1 発だけ子へ届く (DR-0029 §2)。`false` で完全 bypass (= Ctrl+Z 素通し、
    /// detach 手段は `hyoui detach` / SIGHUP 等の外側経路のみ)。
    #[serde(default = "default_true")]
    pub ctrlz_guard: bool,

    /// Ctrl+Z を受けてから detach を確定するまでの遅延 (= 連打を待つ窓)。
    ///
    /// `"500ms"` (default) / `"1s"` / `"0"` のような duration 文字列、または整数
    /// (= ミリ秒) で書ける。`0` にすると連打判定を行わず、Ctrl+Z 単発で即 detach
    /// する (= 子には一切届かなくなる)。
    #[serde(
        default = "default_ctrlz_guard_delay",
        deserialize_with = "deserialize_duration"
    )]
    pub ctrlz_guard_delay: std::time::Duration,

    /// detach 遅延中に画面最下行へ残り時間の overlay を出す (= DR-0029 §5)。
    ///
    /// 現在は **未実装** で、値は受理されるが動作に影響しない (= 実装は
    /// docs/issue/2026-07-25-request-attach-overlay-progress.md)。
    #[serde(default = "default_true")]
    pub ctrlz_guard_overlay: bool,

    /// stopped child への rw attach を復帰意思とみなして resume 要求を送る。
    ///
    /// default `true`。ro / rw-no-leader attach では設定に関わらず送らない。
    #[serde(default = "default_true")]
    pub resume_on_reattach: bool,
}

/// env scrub 設定 (= TOML の `[scrub_env]` 配下、DR-0024 §3)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ScrubEnvConfig {
    /// env scrub 全体の on/off (= CLI `--no-scrub-env` と同等)。
    ///
    /// default: `true`。`false` にすると target 設定に関わらず scrub を
    /// 全停止する。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// target 別 scrub 設定。key は `hyoui run -- <cmd>` の argv basename。
    ///
    /// 未指定 target は [`TargetConfig::default`] (= `inherit_builtin = true`、
    /// kill/keep 空) 相当として扱う。
    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,
}

/// target 別 scrub 設定 (= TOML の `[scrub_env.targets.<name>]`、DR-0024 §3)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TargetConfig {
    /// `true` で builtin kill_glob / keep_glob を user 設定と concat する。
    ///
    /// default: `true`。`false` にすると builtin を完全無視して user 設定のみ
    /// 適用 (= builtin が未登録の target では true/false 同義)。
    #[serde(default = "default_true")]
    pub inherit_builtin: bool,

    /// 削除対象 glob patterns (= builtin kill_glob に追加)。`inherit_builtin =
    /// false` のときは builtin を無視して user 設定のみ。
    #[serde(default)]
    pub kill_glob: Vec<String>,

    /// 削除を skip する glob patterns。`inherit_builtin = true` のときは builtin
    /// keep_glob (= 現状空) に user 設定を concat。`inherit_builtin = false` の
    /// ときは builtin を無視して user 設定のみ。
    #[serde(default)]
    pub keep_glob: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_ctrlz_guard_delay() -> std::time::Duration {
    std::time::Duration::from_millis(500)
}

impl Default for AttachConfig {
    fn default() -> Self {
        Self {
            ctrlz_guard: true,
            ctrlz_guard_delay: default_ctrlz_guard_delay(),
            ctrlz_guard_overlay: true,
            resume_on_reattach: true,
        }
    }
}

/// duration 設定値を deserialize する。
///
/// 受理する形:
/// - 文字列 + 単位: `"500ms"` / `"1s"` / `"1.5s"` / `"2m"` (単位省略時はミリ秒)
/// - 整数: `500` (= ミリ秒)
///
/// 負値 / 未知の単位 / 数値でない文字列は Err (= DR-0024 の「不正 config は
/// 起動を拒否」に合流させる)。
fn deserialize_duration<'de, D>(de: D) -> Result<std::time::Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    use serde::de::Error as _;

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Int(u64),
        Str(String),
    }

    match Repr::deserialize(de)? {
        Repr::Int(ms) => Ok(std::time::Duration::from_millis(ms)),
        Repr::Str(s) => parse_duration(&s).ok_or_else(|| {
            D::Error::custom(format!(
                "invalid duration {s:?}: expected e.g. \"500ms\" / \"1s\" / \"1.5s\" / \"2m\" \
                 (単位省略時はミリ秒)"
            ))
        }),
    }
}

/// duration 文字列 (`"500ms"` 等) を [`std::time::Duration`] にする pure 関数。
fn parse_duration(s: &str) -> Option<std::time::Duration> {
    let t = s.trim();
    let digits_end = t
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(digits_end);
    let value: f64 = num.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let millis = match unit.trim() {
        "" | "ms" => value,
        "s" => value * 1000.0,
        "m" => value * 60_000.0,
        _ => return None,
    };
    Some(std::time::Duration::from_millis(millis.round() as u64))
}

impl Default for ScrubEnvConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            targets: BTreeMap::new(),
        }
    }
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            inherit_builtin: true,
            kill_glob: Vec::new(),
            keep_glob: Vec::new(),
        }
    }
}

/// config 読み込み時のエラー (DR-0024 §7)。
#[derive(Debug)]
pub enum ConfigError {
    /// config ファイルの read syscall が失敗 (NotFound 以外、= permission denied 等)。
    Read {
        /// 読み込み試行したパス。
        path: PathBuf,
        /// underlying I/O error。
        source: std::io::Error,
    },
    /// TOML パースエラー (= syntax error / 型不一致)。
    Parse {
        /// 読み込んだパス。
        path: PathBuf,
        /// underlying TOML deserialize error。
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(f, "config file read failed ({}): {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "config file parse failed ({}): {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

/// config ファイルの解決パスを返す。
///
/// `$XDG_CONFIG_HOME` 指定があればそちら、無ければ `$HOME/.config/hyoui/config.toml`。
/// どちらの env も無ければ `None` (= config 読み込み不能、`Config::default` で動く)。
pub fn resolve_path() -> Option<PathBuf> {
    resolve_path_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// [`resolve_path`] の pure 関数化版 (= test で env mutation 不要にするため切り出し)。
///
/// 引数 (xdg, home) は両方 `Option<&OsStr>` で受ける。`Some("")` は未設定扱い
/// (= `var_os` が空文字を返す異常ケースに合わせる)。
fn resolve_path_from(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(x) = xdg
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("hyoui").join("config.toml"));
    }
    let h = home?;
    if h.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(h)
            .join(".config")
            .join("hyoui")
            .join("config.toml"),
    )
}

/// config を読み込む。
///
/// - パス解決不能 / ファイル不在 → `Ok(Config::default())`
/// - read 失敗 (= permission denied 等) → `Err(ConfigError::Read)`
/// - パース失敗 → `Err(ConfigError::Parse)`
///
/// caller (= `hyoui-cli` の `run` 解決経路) はエラー時 exit non-zero で起動を
/// 拒否する責務がある (DR-0024 §7)。
pub fn load() -> Result<Config, ConfigError> {
    let Some(path) = resolve_path() else {
        return Ok(Config::default());
    };
    load_from(&path)
}

/// 明示パスから config を読み込む (= unit test / 内部実装用)。
pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(s) => parse_str(s.as_str(), path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

/// TOML 文字列から Config を直接 deserialize する (= test 用、エラー時の path 付帯のため `path` を取る)。
pub fn parse_str(s: &str, path: &Path) -> Result<Config, ConfigError> {
    toml::from_str(s).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn dummy_path() -> PathBuf {
        PathBuf::from("/tmp/test-config.toml")
    }

    #[test]
    fn default_config_has_scrub_and_attach_defaults() {
        let c = Config::default();
        assert!(c.scrub_env.enabled);
        assert!(c.scrub_env.targets.is_empty());
        assert!(c.attach.ctrlz_guard);
        assert_eq!(c.attach.ctrlz_guard_delay, Duration::from_millis(500));
        assert!(c.attach.ctrlz_guard_overlay);
        assert!(c.attach.resume_on_reattach);
        assert!(!c.session.auto_resume);
        assert_eq!(c.web.listen, "127.0.0.1:43690");
    }

    #[test]
    fn parse_session_auto_resume() {
        let s = r#"
[session]
auto_resume = true
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(c.session.auto_resume);
    }

    #[test]
    fn parse_duration_accepts_units_and_bare_millis() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration(" 250 "), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_duration("0ms"), Some(Duration::ZERO));
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        assert_eq!(parse_duration("fast"), None);
        assert_eq!(parse_duration("500 years"), None);
        assert_eq!(parse_duration("-1s"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn parse_ctrlz_guard_delay_as_integer_is_millis() {
        let s = r#"
[attach]
ctrlz_guard_delay = 120
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert_eq!(c.attach.ctrlz_guard_delay, Duration::from_millis(120));
    }

    #[test]
    fn parse_ctrlz_guard_delay_invalid_string_is_error() {
        let s = r#"
[attach]
ctrlz_guard_delay = "soon"
"#;
        assert!(matches!(
            parse_str(s, &dummy_path()),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn parse_web_section_overrides_listen() {
        let s = r#"
[web]
listen = "0.0.0.0:8080"
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert_eq!(c.web.listen, "0.0.0.0:8080");
    }

    #[test]
    fn parse_web_missing_uses_default() {
        let c = parse_str("", &dummy_path()).unwrap();
        assert_eq!(c.web.listen, "127.0.0.1:43690");
    }

    #[test]
    fn default_target_inherits_builtin() {
        let t = TargetConfig::default();
        assert!(t.inherit_builtin);
        assert!(t.kill_glob.is_empty());
        assert!(t.keep_glob.is_empty());
    }

    #[test]
    fn parse_empty_toml_gives_defaults() {
        let c = parse_str("", &dummy_path()).unwrap();
        assert!(c.scrub_env.enabled);
        assert!(c.scrub_env.targets.is_empty());
    }

    #[test]
    fn parse_partial_attach_config_keeps_missing_field_defaults() {
        let s = r#"
[attach]
ctrlz_guard_delay = "125ms"
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(c.attach.ctrlz_guard);
        assert_eq!(c.attach.ctrlz_guard_delay, Duration::from_millis(125));
        assert!(c.attach.ctrlz_guard_overlay);
        assert!(c.attach.resume_on_reattach);
        assert!(c.scrub_env.enabled);
    }

    #[test]
    fn parse_full_attach_config() {
        let s = r#"
[attach]
ctrlz_guard = false
ctrlz_guard_delay = "1s"
ctrlz_guard_overlay = false
resume_on_reattach = false
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(!c.attach.ctrlz_guard);
        assert_eq!(c.attach.ctrlz_guard_delay, Duration::from_secs(1));
        assert!(!c.attach.ctrlz_guard_overlay);
        assert!(!c.attach.resume_on_reattach);
    }

    #[test]
    fn parse_disable_only() {
        let s = r#"
[scrub_env]
enabled = false
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(!c.scrub_env.enabled);
    }

    #[test]
    fn parse_target_full() {
        let s = r#"
[scrub_env.targets.claude]
inherit_builtin = true
kill_glob = ["CMUXMSG_*"]
keep_glob = ["AI_AGENT"]
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        let t = c.scrub_env.targets.get("claude").unwrap();
        assert!(t.inherit_builtin);
        assert_eq!(t.kill_glob, vec!["CMUXMSG_*"]);
        assert_eq!(t.keep_glob, vec!["AI_AGENT"]);
    }

    #[test]
    fn parse_target_inherit_false() {
        let s = r#"
[scrub_env.targets.claude]
inherit_builtin = false
kill_glob = ["MYTOOL_SECRET"]
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        let t = c.scrub_env.targets.get("claude").unwrap();
        assert!(!t.inherit_builtin);
        assert_eq!(t.kill_glob, vec!["MYTOOL_SECRET"]);
        assert!(t.keep_glob.is_empty());
    }

    #[test]
    fn parse_unknown_field_is_ignored() {
        // DR-0024 §7: unknown field は warn 出さずに無視する (= 前方互換)。
        let s = r#"
some_future_section_key = "ignored"

[scrub_env]
enabled = true
some_future_field = "ignored"

[scrub_env.targets.claude]
inherit_builtin = true
new_field_in_future = 42
kill_glob = ["FOO"]
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(c.scrub_env.enabled);
        let t = c.scrub_env.targets.get("claude").unwrap();
        assert_eq!(t.kill_glob, vec!["FOO"]);
    }

    #[test]
    fn parse_syntax_error_returns_err() {
        let s = "this is not valid toml ===";
        let r = parse_str(s, &dummy_path());
        assert!(matches!(r, Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn parse_type_mismatch_returns_err() {
        let s = r#"
[scrub_env]
enabled = "yes"
"#; // bool が文字列
        let r = parse_str(s, &dummy_path());
        assert!(matches!(r, Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn parse_full_example_from_dr() {
        // DR-0024 §3 の TOML 例がそのまま deserialize できる。
        let s = r#"
[scrub_env]
enabled = true

[scrub_env.targets.claude]
inherit_builtin = true
kill_glob = ["CMUXMSG_*"]
keep_glob = ["AI_AGENT"]

[scrub_env.targets.my-tool]
inherit_builtin = false
kill_glob = ["MYTOOL_SECRET"]
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(c.scrub_env.enabled);
        let claude = c.scrub_env.targets.get("claude").unwrap();
        assert!(claude.inherit_builtin);
        assert_eq!(claude.kill_glob, vec!["CMUXMSG_*"]);
        assert_eq!(claude.keep_glob, vec!["AI_AGENT"]);
        let my_tool = c.scrub_env.targets.get("my-tool").unwrap();
        assert!(!my_tool.inherit_builtin);
        assert_eq!(my_tool.kill_glob, vec!["MYTOOL_SECRET"]);
    }

    #[test]
    fn load_from_nonexistent_path_gives_default() {
        let path = PathBuf::from("/tmp/definitely-does-not-exist-hyoui-test.toml");
        let c = load_from(&path).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn load_from_tempfile_with_valid_toml() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        writeln!(f, "[scrub_env]").unwrap();
        writeln!(f, "enabled = false").unwrap();
        drop(f);
        let c = load_from(&path).unwrap();
        assert!(!c.scrub_env.enabled);
    }

    // path 解決ロジックは pure 関数 `resolve_path_from(xdg, home)` に切り出して
    // env mutation なしで test する (= process global env を弄ると他 test と衝突 +
    // sys/* 外で unsafe を使うことになる)。
    #[test]
    fn resolve_path_from_uses_xdg_when_present() {
        let p = resolve_path_from(Some(OsStr::new("/custom/xdg")), None).unwrap();
        assert_eq!(p, PathBuf::from("/custom/xdg/hyoui/config.toml"));
    }

    #[test]
    fn resolve_path_from_falls_back_to_home_when_xdg_unset() {
        let p = resolve_path_from(None, Some(OsStr::new("/custom/home"))).unwrap();
        assert_eq!(p, PathBuf::from("/custom/home/.config/hyoui/config.toml"));
    }

    #[test]
    fn resolve_path_from_falls_back_to_home_when_xdg_empty() {
        // 異常ケース: XDG=空文字は未設定扱い。
        let p = resolve_path_from(Some(OsStr::new("")), Some(OsStr::new("/h"))).unwrap();
        assert_eq!(p, PathBuf::from("/h/.config/hyoui/config.toml"));
    }

    #[test]
    fn resolve_path_from_returns_none_when_both_missing() {
        assert!(resolve_path_from(None, None).is_none());
        assert!(resolve_path_from(Some(OsStr::new("")), None).is_none());
        assert!(resolve_path_from(None, Some(OsStr::new(""))).is_none());
    }
}
