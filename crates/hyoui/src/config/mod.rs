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
//!
//! ## 実効設定の書き出し (= `hyoui config show`)
//!
//! 各 struct は `Serialize` も持ち、[`to_toml`] で実効値 (= default 込み) を
//! TOML 文字列にできる。出力は同じ loader で読み直せる (= round-trip 可能)。

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// hyoui 全体設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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

/// session policy 設定 (= TOML の `[session]` 配下、DR-0029 §4 / DR-0032 §1)。
///
/// `hyoui run` が daemon に渡す既定値を持つ。CLI flag (`--on-child-suspend`) が
/// あればそちらが優先する (= DR-0024 の flag 最小化方針、config は default 提供)。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SessionConfig {
    /// 子が suspend (stopped) した時のふるまい (DR-0032 §1)。
    #[serde(default)]
    pub on_child_suspend: OnChildSuspendSetting,
}

/// 子 suspend 時のふるまい (= TOML `[session] on_child_suspend`、DR-0032 §1)。
///
/// 利用者が認識する概念は「子が suspend したらどうなるか」の 1 つの選択なので、
/// daemon policy と attach client policy の 2 レイヤを 1 つの enum で表す。
/// **wire には乗らない** (= 読み込み時に [`Self::daemon_policy`] で daemon policy へ、
/// `client::stopped_child_action` で attach 側の挙動へ写像する)。
///
/// [`crate::cli::OnChildSuspend`] とは別物: あちらは daemon policy 2 値
/// (`notify` / `auto-resume`) で、CLI flag / `hyoui set` / protocol の語彙。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnChildSuspendSetting {
    /// daemon が常に即 `SIGCONT` (= attach の有無に関係なく起こす)。
    AutoResumeAlways,
    /// rw attach client が居る間だけ起こす (= 無人時は停止を維持)。default。
    #[default]
    AutoResumeOnAttached,
    /// 起こさず、rw attach client が child action menu を表示する (DR-0032 §2)。
    ShowChildActionMenu,
}

impl OnChildSuspendSetting {
    /// daemon policy への写像 (DR-0032 §1 の表)。
    ///
    /// attach 側の 2 値 (resume / menu) は daemon に伝えても意味がないので、
    /// どちらも `Notify` (= daemon は起こさず leader に通知するだけ) に落ちる。
    #[must_use]
    pub fn daemon_policy(self) -> crate::cli::OnChildSuspend {
        match self {
            Self::AutoResumeAlways => crate::cli::OnChildSuspend::AutoResume,
            Self::AutoResumeOnAttached | Self::ShowChildActionMenu => {
                crate::cli::OnChildSuspend::Notify
            }
        }
    }
}

/// Ctrl+Z 単発 (= ガード窓で ×1 と確定した後) の action (= TOML
/// `[attach] ctrlz_x1_action`、DR-0032 §3)。
///
/// 司るのは「確定後の action」だけで、ガード窓そのもの (単発判定 / 連打 forward /
/// 他キー割り込み) は [`AttachConfig::ctrlz_guard`] / [`AttachConfig::ctrlz_guard_delay`]
/// のまま不変 (DR-0029 §2)。値名の `client_` prefix は action の対象が子ではなく
/// client 自身であることを示す。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CtrlzX1Action {
    /// client 自身を suspend (= `raise(SIGTSTP)`、`fg` で同じ接続に復帰)。default。
    #[default]
    ClientSuspend,
    /// client を畳む (= detach。子は走り続ける)。
    ClientDetach,
    /// 選択プロンプトを出して次の明示キー (^Z / ^C / Esc) を待つ (DR-0032 §3)。
    SelectOnDemand,
}

/// Web gateway 設定 (= TOML の `[web]` 配下、DR-0027 §Decision.2)。
///
/// `hyoui web` subcommand が listen する host:port を持つ。CLI flag
/// `--listen` があれば config を上書きする (= DR-0024 の flag 最小化方針)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct WebConfig {
    /// listen する host:port (= default `127.0.0.1:43690` = 0xAAAA、DR-0027)。
    ///
    /// 前段 Caddy reverse proxy 想定 (= HTTPS / auth は前段が担う)。tailnet の外に
    /// 直接晒す場合は将来 DR で auth を扱う。
    #[serde(default = "default_web_listen")]
    pub listen: String,

    /// 静的アセットの開発モード配信元 (= 指定時はローカル dir を都度読む、
    /// DR-0027 §4)。`None` (default) ならリリースビルドに埋め込まれた assets を返す。
    ///
    /// TOML には「値なし」を表す形が無いため、`None` の時は serialize 時に
    /// key ごと省略する (= `hyoui config show` の出力に現れない)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct AttachConfig {
    /// tty stdin 経路の Ctrl+Z ガードを有効にする。
    ///
    /// `true` (default) なら Ctrl+Z 単発は「子に届けず attach client 自身を
    /// suspend (= 外側 shell に戻る、`fg` で復帰)」、2 発ごとに 1 発だけ子へ届く
    /// (DR-0029 §2)。`false` で完全 bypass (= Ctrl+Z 素通し)。
    #[serde(default = "default_true")]
    pub ctrlz_guard: bool,

    /// Ctrl+Z を受けてから client suspend を確定するまでの遅延 (= 連打を待つ窓)。
    ///
    /// `"1s"` (default) / `"500ms"` / `"0"` のような duration 文字列、または整数
    /// (= ミリ秒) で書ける。`0` にすると連打判定を行わず、Ctrl+Z 単発で即 suspend
    /// する (= 子には一切届かなくなる)。
    #[serde(
        default = "default_ctrlz_guard_delay",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    pub ctrlz_guard_delay: std::time::Duration,

    /// suspend 遅延中に画面最下行へ残り時間の overlay を出す (= DR-0029 §5)。
    ///
    /// 現在は **未実装** で、値は受理されるが動作に影響しない (= 実装は
    /// docs/issue/2026-07-25-request-attach-overlay-progress.md)。
    #[serde(default = "default_true")]
    pub ctrlz_guard_overlay: bool,

    /// Ctrl+Z 単発が確定した後の action (DR-0032 §3)。default `client_suspend`
    /// (= DR-0029 §2 の挙動そのまま)。
    #[serde(default)]
    pub ctrlz_x1_action: CtrlzX1Action,
}

/// env scrub 設定 (= TOML の `[scrub_env]` 配下、DR-0024 §3)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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
    std::time::Duration::from_millis(1000)
}

impl Default for AttachConfig {
    fn default() -> Self {
        Self {
            ctrlz_guard: true,
            ctrlz_guard_delay: default_ctrlz_guard_delay(),
            ctrlz_guard_overlay: true,
            ctrlz_x1_action: CtrlzX1Action::ClientSuspend,
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

/// duration 設定値を serialize する (= [`deserialize_duration`] が読み直せる形)。
///
/// 常に `"<ms>ms"` の文字列で出す (= 単位省略のミリ秒整数と違い、読み手が
/// 単位を推測しなくてよい)。
fn serialize_duration<S>(d: &std::time::Duration, se: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    se.serialize_str(&format!("{}ms", d.as_millis()))
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
    /// 廃止済み key が書かれていた (DR-0032 §1 migration)。
    ///
    /// unknown field 一般は前方互換のため無視するが、廃止 key は「明示設定者の意図が
    /// silent に default へ倒れる」ので起動を拒否して移行先を案内する。
    RemovedKey {
        /// 読み込んだパス。
        path: PathBuf,
        /// 廃止 key の TOML 上の書き方 (= `[session] auto_resume`)。
        key: &'static str,
        /// 移行先の案内 (= 何に書き換えればよいか)。
        hint: &'static str,
    },
}

/// 廃止 key の一覧 (= section, key, 移行案内)。
///
/// DR-0032 §1: 旧 bool 2 個は `[session] on_child_suspend` の enum に統合された。
/// 旧 default の組合せ (`auto_resume = false` + `resume_stopped_child = true`) は
/// enum default `auto_resume_on_attached` と同挙動なので、明示設定していた人だけが
/// この経路に来る。
struct RemovedKey {
    /// TOML の section 名 (= `[session]` の `session`)。
    section: &'static str,
    /// section 内の key 名。
    name: &'static str,
    /// error 表示用の書き方 (= `[session] auto_resume`)。
    display: &'static str,
    /// 移行先の案内。
    hint: &'static str,
}

const REMOVED_KEYS: &[RemovedKey] = &[
    RemovedKey {
        section: "session",
        name: "auto_resume",
        display: "[session] auto_resume",
        hint: "`[session] on_child_suspend = \"auto_resume_always\"` (旧 true) / \
               `\"auto_resume_on_attached\"` (旧 false、= default) に書き換えてください",
    },
    RemovedKey {
        section: "attach",
        name: "resume_stopped_child",
        display: "[attach] resume_stopped_child",
        hint: "`[session] on_child_suspend = \"auto_resume_on_attached\"` (旧 true、= default) / \
               `\"show_child_action_menu\"` (旧 false、= 起こさず child action menu を出す) に \
               書き換えてください",
    },
];

/// 廃止 key が書かれていないか検査する (DR-0032 §1 migration)。
fn check_removed_keys(table: &toml::Table, path: &Path) -> Result<(), ConfigError> {
    for removed in REMOVED_KEYS {
        let present = table
            .get(removed.section)
            .and_then(toml::Value::as_table)
            .is_some_and(|t| t.contains_key(removed.name));
        if present {
            return Err(ConfigError::RemovedKey {
                path: path.to_path_buf(),
                key: removed.display,
                hint: removed.hint,
            });
        }
    }
    Ok(())
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
            Self::RemovedKey { path, key, hint } => {
                write!(
                    f,
                    "config key `{key}` は削除されました ({}): {hint}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::RemovedKey { .. } => None,
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

/// 実効設定を TOML 文字列にする (= `hyoui config show` の本体)。
///
/// 未設定項目も default 値込みで出る (= 差分ではなく「今どう動いているか」)。
/// 出力は [`parse_str`] で読み直せる (= round-trip 可能)。
///
/// serialize が失敗するのは serde 実装側の不整合だけなので、失敗時は Err を
/// そのまま返して caller に判断させる。
pub fn to_toml(config: &Config) -> Result<String, toml::ser::Error> {
    toml::to_string(config)
}

/// TOML 文字列から Config を直接 deserialize する (= test 用、エラー時の path 付帯のため `path` を取る)。
///
/// 一度 [`toml::Table`] にしてから廃止 key を検査し (DR-0032 §1)、その後 `Config` へ
/// deserialize する (= 廃止 key を unknown field として silent に無視しないため)。
pub fn parse_str(s: &str, path: &Path) -> Result<Config, ConfigError> {
    let to_parse_err = |e: toml::de::Error| ConfigError::Parse {
        path: path.to_path_buf(),
        source: e,
    };
    let table: toml::Table = toml::from_str(s).map_err(to_parse_err)?;
    check_removed_keys(&table, path)?;
    toml::Value::Table(table).try_into().map_err(to_parse_err)
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
        assert_eq!(c.attach.ctrlz_guard_delay, Duration::from_millis(1000));
        assert!(c.attach.ctrlz_guard_overlay);
        assert_eq!(c.attach.ctrlz_x1_action, CtrlzX1Action::ClientSuspend);
        assert_eq!(
            c.session.on_child_suspend,
            OnChildSuspendSetting::AutoResumeOnAttached
        );
        assert_eq!(c.web.listen, "127.0.0.1:43690");
    }

    /// DR-0032 §1: enum 3 値がすべて設定語彙 (snake_case) で読める。
    #[test]
    fn parse_session_on_child_suspend_accepts_all_three_values() {
        for (written, expected) in [
            (
                "auto_resume_always",
                OnChildSuspendSetting::AutoResumeAlways,
            ),
            (
                "auto_resume_on_attached",
                OnChildSuspendSetting::AutoResumeOnAttached,
            ),
            (
                "show_child_action_menu",
                OnChildSuspendSetting::ShowChildActionMenu,
            ),
        ] {
            let s = format!("[session]\non_child_suspend = \"{written}\"\n");
            let c = parse_str(&s, &dummy_path()).unwrap();
            assert_eq!(c.session.on_child_suspend, expected, "value {written}");
        }
    }

    /// 未知の enum 値は起動拒否 (= DR-0024 の「不正 config は読まない」流儀)。
    #[test]
    fn parse_session_on_child_suspend_unknown_value_is_error() {
        let s = r#"
[session]
on_child_suspend = "resume_maybe"
"#;
        assert!(matches!(
            parse_str(s, &dummy_path()),
            Err(ConfigError::Parse { .. })
        ));
    }

    /// DR-0032 §1: enum → daemon policy の写像 (= 全対応)。
    #[test]
    fn on_child_suspend_maps_to_daemon_policy() {
        use crate::cli::OnChildSuspend as Policy;
        assert_eq!(
            OnChildSuspendSetting::AutoResumeAlways.daemon_policy(),
            Policy::AutoResume
        );
        assert_eq!(
            OnChildSuspendSetting::AutoResumeOnAttached.daemon_policy(),
            Policy::Notify
        );
        assert_eq!(
            OnChildSuspendSetting::ShowChildActionMenu.daemon_policy(),
            Policy::Notify,
        );
    }

    /// DR-0032 §3: `ctrlz_x1_action` 3 値がすべて読める。
    #[test]
    fn parse_attach_ctrlz_x1_action_accepts_all_three_values() {
        for (written, expected) in [
            ("client_suspend", CtrlzX1Action::ClientSuspend),
            ("client_detach", CtrlzX1Action::ClientDetach),
            ("select_on_demand", CtrlzX1Action::SelectOnDemand),
        ] {
            let s = format!("[attach]\nctrlz_x1_action = \"{written}\"\n");
            let c = parse_str(&s, &dummy_path()).unwrap();
            assert_eq!(c.attach.ctrlz_x1_action, expected, "value {written}");
        }
    }

    /// DR-0032 §1 migration: 廃止された旧 bool 2 個は silent 無視せず起動拒否し、
    /// 移行先を案内する。
    #[test]
    fn removed_bool_keys_are_startup_errors_with_migration_hint() {
        for (s, expected_key) in [
            ("[session]\nauto_resume = true\n", "[session] auto_resume"),
            (
                "[attach]\nresume_stopped_child = false\n",
                "[attach] resume_stopped_child",
            ),
        ] {
            match parse_str(s, &dummy_path()) {
                Err(e @ ConfigError::RemovedKey { .. }) => {
                    let msg = e.to_string();
                    assert!(msg.contains(expected_key), "旧 key 名を出す: {msg}");
                    assert!(msg.contains("on_child_suspend"), "移行先を案内する: {msg}");
                }
                other => panic!("旧 key は RemovedKey で拒否されるべき: {other:?}"),
            }
        }
    }

    /// 旧 key と同名でも別 section なら誤検出しない (= section 込みで判定する)。
    #[test]
    fn removed_key_check_is_section_scoped() {
        let s = r#"
[scrub_env.targets.auto_resume]
kill_glob = ["FOO"]
"#;
        assert!(parse_str(s, &dummy_path()).is_ok());
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
        assert_eq!(c.attach.ctrlz_x1_action, CtrlzX1Action::ClientSuspend);
        assert!(c.scrub_env.enabled);
    }

    #[test]
    fn parse_full_attach_config() {
        let s = r#"
[attach]
ctrlz_guard = false
ctrlz_guard_delay = "1s"
ctrlz_guard_overlay = false
ctrlz_x1_action = "client_detach"
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        assert!(!c.attach.ctrlz_guard);
        assert_eq!(c.attach.ctrlz_guard_delay, Duration::from_secs(1));
        assert!(!c.attach.ctrlz_guard_overlay);
        assert_eq!(c.attach.ctrlz_x1_action, CtrlzX1Action::ClientDetach);
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
    fn to_toml_default_round_trips() {
        let c = Config::default();
        let s = to_toml(&c).unwrap();
        let back = parse_str(&s, &dummy_path()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn to_toml_custom_round_trips() {
        let s = r#"
[scrub_env]
enabled = false

[scrub_env.targets.claude]
inherit_builtin = false
kill_glob = ["FOO_*"]
keep_glob = ["BAR"]

[attach]
ctrlz_guard = false
ctrlz_guard_delay = "1.5s"

[session]
on_child_suspend = "show_child_action_menu"

[web]
listen = "0.0.0.0:9999"
assets_dir = "/tmp/assets"
"#;
        let c = parse_str(s, &dummy_path()).unwrap();
        let rendered = to_toml(&c).unwrap();
        let back = parse_str(&rendered, &dummy_path()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn to_toml_emits_every_default_key() {
        // 「差分ではなく実効値」= 未設定項目も default 込みで出る。
        let s = to_toml(&Config::default()).unwrap();
        for key in [
            "[scrub_env]",
            "enabled",
            "[attach]",
            "ctrlz_guard",
            "ctrlz_guard_delay",
            "ctrlz_guard_overlay",
            "ctrlz_x1_action",
            "[session]",
            "on_child_suspend",
            "[web]",
            "listen",
        ] {
            assert!(s.contains(key), "to_toml output missing {key}:\n{s}");
        }
    }

    #[test]
    fn to_toml_duration_is_millisecond_string() {
        let s = to_toml(&Config::default()).unwrap();
        assert!(
            s.contains("ctrlz_guard_delay = \"1000ms\""),
            "unexpected duration rendering:\n{s}"
        );
    }

    #[test]
    fn to_toml_omits_unset_assets_dir() {
        // TOML に「値なし」は書けないので None は key ごと省略する。
        let s = to_toml(&Config::default()).unwrap();
        assert!(!s.contains("assets_dir"), "unexpected assets_dir:\n{s}");
    }

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
