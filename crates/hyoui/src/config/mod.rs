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
//! web gateway の設定は別ファイル ([`WebFile`]) で、unit ごとに 1 つ持つ
//! (DR-0038 決定 1)。既定の置き場は `$XDG_CONFIG_HOME/hyoui/web/` で、上の
//! `config.toml` は読まない (= gateway は PTY session の設定を使わない)。
//!
//! ## `extends` (DR-0038 決定 3)
//!
//! どちらのファイルも `extends = "<path>"` で土台を指せる。表は鍵ごとに潜って重ね、
//! それ以外 (数・文字列・真偽・配列) は丸ごと置き換える。相対パスは書いたファイルの
//! 隣から解き、`~` は `$HOME` で開く。辿ったファイルを正規化した実体で覚え、循環は
//! その場で止める。指した先が無ければ、どのファイルがどのパスを指したかを言う。
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
}

/// session policy 設定 (= TOML の `[session]` 配下、DR-0029 §4 / DR-0032 §1)。
///
/// `hyoui run` が daemon に渡す既定値を持つ。CLI flag (`--on-child-suspend`) が
/// あればそちらが優先する (= DR-0024 の flag 最小化方針、config は default 提供)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SessionConfig {
    /// 子が suspend (stopped) した時のふるまい (DR-0032 §1)。
    #[serde(default)]
    pub on_child_suspend: OnChildSuspendSetting,

    /// 呼び出し元に `TERM` が無い (未設定 / 空) 時に子へ設定する端末種別
    /// (DR-0039 決定 1)。default `"xterm-256color"`。呼び出し元に `TERM` があれば
    /// `hyoui run` / `hyoui run --login` ともそれを引き継ぎ、この値は使わない。
    #[serde(default = "default_term_fallback")]
    pub term_fallback: String,
}

/// `[session] term_fallback` の既定値。
#[must_use]
pub fn default_term_fallback() -> String {
    "xterm-256color".to_string()
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            on_child_suspend: OnChildSuspendSetting::default(),
            term_fallback: default_term_fallback(),
        }
    }
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

/// web gateway の設定ファイル 1 つ (= unit の中身、DR-0038 決定 1)。
///
/// `hyoui web daemon add <name>` が登録するのはこのファイルの path で、値は
/// 登録簿に写さない。`daemon run` / 監督者 / `list` が読むたびにここから引く。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct WebFile {
    /// TOML の `[web]` セクション。
    #[serde(default)]
    pub web: WebConfig,
}

/// web gateway 設定 (= TOML の `[web]` 配下、DR-0027 §Decision.2 / DR-0038 決定 1)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct WebConfig {
    /// listen する host:port (= default `127.0.0.1:43690` = 0xAAAA、DR-0027)。
    ///
    /// 「この unit がどこに居るか」でもあり、CLI と監督者の問い合わせ先 (`/healthz`
    /// / `/version`) もここから組み立てる (DR-0038 決定 6)。
    #[serde(default = "default_web_listen")]
    pub listen: String,

    /// 静的アセットの開発モード配信元 (= 指定時はローカル dir を都度読む、
    /// DR-0027 §4)。`None` (default) ならリリースビルドに埋め込まれた assets を返す。
    ///
    /// TOML には「値なし」を表す形が無いため、`None` の時は serialize 時に
    /// key ごと省略する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets_dir: Option<PathBuf>,

    /// この unit を起動する実行ファイル (DR-0038 決定 2)。`daemon add` が登録簿の
    /// `binary_path` に写す正本。無ければ `add` した時点の自分自身。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<PathBuf>,

    /// この config が属する面の状態の root (DR-0038 決定 9)。
    ///
    /// `daemon add` / `daemon run` は今の面の状態の root と realpath で比べ、食い違えば
    /// 断る。面同士は互いの登録簿を見られないので、別の面の config を登録・起動した
    /// 事故に気付ける場所は config 自身しかない。面をまたいで共有する土台
    /// (`base.toml`) には書かない。値の要否は読み手 (= CLI の add / run) が判断し、
    /// ここでは任意として読む (= `list` / `status` / 監督者は listen だけを使う)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
}

/// `[web]` の listen の既定値。
#[must_use]
pub fn default_web_listen() -> String {
    "127.0.0.1:43690".to_string()
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            listen: default_web_listen(),
            assets_dir: None,
            binary_path: None,
            state_dir: None,
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
    /// 読もうとした config ファイルが無い (= 明示された path、`add` / 名前付き `run`)。
    NotFound {
        /// 探した path。
        path: PathBuf,
    },
    /// `extends` が指したファイルが無い (DR-0038 決定 3)。
    ExtendsNotFound {
        /// `extends` を書いたファイル。
        from: PathBuf,
        /// 指された path (= 解いた後)。
        target: PathBuf,
    },
    /// `extends` が文字列でない (DR-0038 決定 3)。
    BadExtends {
        /// `extends` を書いたファイル。
        path: PathBuf,
    },
    /// `extends` の鎖が自分に戻ってきた (DR-0038 決定 3)。
    ExtendsCycle {
        /// 辿った順のファイル (= 最後が最初に戻った先)。
        chain: Vec<PathBuf>,
    },
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
               `\"show_child_action_menu\"` (旧 false、= 起こさず child action menu を出す) \
               に書き換えてください",
    },
    RemovedKey {
        section: "web",
        name: "listen",
        display: "[web] listen",
        hint: WEB_MOVED_HINT,
    },
    RemovedKey {
        section: "web",
        name: "assets_dir",
        display: "[web] assets_dir",
        hint: WEB_MOVED_HINT,
    },
];

/// `config.toml` の `[web]` が web の config ファイルへ移ったことの案内 (DR-0038 決定 1)。
const WEB_MOVED_HINT: &str = "web gateway の設定は unit ごとの config ファイルに移りました。\
     `hyoui web daemon add <name>` が `$XDG_CONFIG_HOME/hyoui/web/<name>.toml` を\
     書いて登録します";

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
            Self::NotFound { path } => {
                write!(f, "config file not found: {}", path.display())
            }
            Self::ExtendsNotFound { from, target } => write!(
                f,
                "{} extends {}, which does not exist",
                from.display(),
                target.display()
            ),
            Self::BadExtends { path } => {
                write!(f, "`extends` in {} must be a path string", path.display())
            }
            Self::ExtendsCycle { chain } => write!(
                f,
                "`extends` loops back on itself: {}",
                chain
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
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
            Self::RemovedKey { .. }
            | Self::NotFound { .. }
            | Self::ExtendsNotFound { .. }
            | Self::BadExtends { .. }
            | Self::ExtendsCycle { .. } => None,
        }
    }
}

/// config ファイルの解決パスを返す。
///
/// `$XDG_CONFIG_HOME` 指定があればそちら、無ければ `$HOME/.config/hyoui/config.toml`。
/// どちらの env も無ければ `None` (= config 読み込み不能、`Config::default` で動く)。
pub fn resolve_path() -> Option<PathBuf> {
    resolve_path_in(&crate::paths::Env::current())
}

/// [`resolve_path`] の env を引数で受ける形 (= test で process env を触らない)。
fn resolve_path_in(env: &crate::paths::Env) -> Option<PathBuf> {
    env.config_dir().map(|dir| dir.join("config.toml"))
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

/// 明示パスから config を読み込む (= unit test / 内部実装用)。ファイル不在は default。
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

/// web の config ファイルを読む (DR-0038 決定 1 / 3)。
///
/// 明示された path を読むので、無ければ [`ConfigError::NotFound`]。`extends` を辿り、
/// `[web]` の `assets_dir` / `binary_path` / `state_dir` の相対パスは**それを書いた
/// ファイルの隣**から解く (= 起動時の cwd で意味が変わらない)。
pub fn load_web(path: &Path) -> Result<WebFile, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::NotFound {
                path: path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    parse_web_str(&text, path)
}

/// TOML 文字列から [`WebFile`] を読む (= `path` は `extends` と相対パスの起点)。
pub fn parse_web_str(s: &str, path: &Path) -> Result<WebFile, ConfigError> {
    let env = crate::paths::Env::current();
    let table = layered_table(s, path, WEB_PATH_KEYS, &env, &mut Vec::new())?;
    toml::Value::Table(table)
        .try_into()
        .map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })
}

/// web の config で path として解く鍵 (= 書いたファイルの隣から解く対象)。
const WEB_PATH_KEYS: &[&[&str]] = &[
    &["web", "assets_dir"],
    &["web", "binary_path"],
    &["web", "state_dir"],
];

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

/// TOML 文字列から Config を deserialize する (= `path` は `extends` の起点とエラー表示)。
///
/// 一度 [`toml::Table`] にして `extends` を畳み、廃止 key を検査してから (DR-0032 §1)
/// `Config` へ deserialize する (= 廃止 key を unknown field として silent に無視しないため)。
pub fn parse_str(s: &str, path: &Path) -> Result<Config, ConfigError> {
    let env = crate::paths::Env::current();
    let table = layered_table(s, path, &[], &env, &mut Vec::new())?;
    check_removed_keys(&table, path)?;
    toml::Value::Table(table)
        .try_into()
        .map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })
}

/// `extends` を辿って 1 つの表に畳む (DR-0038 決定 3)。
///
/// `visited` は辿ったファイルの正規化済み実体 (= `./a.toml` / `a.toml` / symlink 越しの
/// 同じファイルを同一と数える)。`path_keys` に挙げた鍵の相対パスは、そのファイルの
/// 隣から解いてから重ねる (= 重ねた後では、どのファイルが書いたかが分からない)。
fn layered_table(
    s: &str,
    path: &Path,
    path_keys: &[&[&str]],
    env: &crate::paths::Env,
    visited: &mut Vec<PathBuf>,
) -> Result<toml::Table, ConfigError> {
    let identity = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if visited.contains(&identity) {
        let mut chain = visited.clone();
        chain.push(identity);
        return Err(ConfigError::ExtendsCycle { chain });
    }
    visited.push(identity);

    let mut table: toml::Table = toml::from_str(s).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    resolve_path_keys(&mut table, path_keys, base_dir, env);

    let extends = match table.remove("extends") {
        None => return Ok(table),
        Some(toml::Value::String(target)) => target,
        Some(_) => {
            return Err(ConfigError::BadExtends {
                path: path.to_path_buf(),
            });
        }
    };
    let target = resolve_relative(Path::new(&extends), base_dir, env);
    let text = match std::fs::read_to_string(&target) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::ExtendsNotFound {
                from: path.to_path_buf(),
                target,
            });
        }
        Err(e) => {
            return Err(ConfigError::Read {
                path: target,
                source: e,
            });
        }
    };
    let mut base = layered_table(&text, &target, path_keys, env, visited)?;
    merge_tables(&mut base, table);
    Ok(base)
}

/// `~` を開き、相対なら `base_dir` から解く。
fn resolve_relative(path: &Path, base_dir: &Path, env: &crate::paths::Env) -> PathBuf {
    let expanded = env.expand_tilde(path);
    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

/// `path_keys` に挙げた鍵の文字列値を path として解き直す。
fn resolve_path_keys(
    table: &mut toml::Table,
    path_keys: &[&[&str]],
    base_dir: &Path,
    env: &crate::paths::Env,
) {
    for keys in path_keys {
        if let Some(toml::Value::String(value)) = value_at_mut(table, keys) {
            let resolved = resolve_relative(Path::new(value.as_str()), base_dir, env);
            *value = resolved.to_string_lossy().into_owned();
        }
    }
}

/// 鍵の列で表を潜った先の値。途中が表でなければ `None`。
fn value_at_mut<'a>(table: &'a mut toml::Table, keys: &[&str]) -> Option<&'a mut toml::Value> {
    let (first, rest) = keys.split_first()?;
    let value = table.get_mut(*first)?;
    if rest.is_empty() {
        return Some(value);
    }
    value_at_mut(value.as_table_mut()?, rest)
}

/// `overlay` を `base` に重ねる: 表は鍵ごとに潜り、それ以外は丸ごと置き換える。
///
/// 配列を要素ごとに混ぜないのは、並びそのものが意味を持つ値 (優先順や除外の列) で
/// 「土台の要素がどこに割り込むか」を予想しながら書く設定にしないため
/// (llm-gateway DR-0013 と同じ規則)。
fn merge_tables(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_table)), toml::Value::Table(overlay_table)) => {
                merge_tables(base_table, overlay_table);
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
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
        assert_eq!(c.session.term_fallback, "xterm-256color");
    }

    /// DR-0039 決定 1: `[session] term_fallback` で呼び出し元に TERM が無い時の値を
    /// 変えられる。書かなければ既定値 (= `[session]` に他の key だけ書いた時も)。
    #[test]
    fn parse_session_term_fallback() {
        let c = parse_str(
            "[session]\nterm_fallback = \"screen-256color\"\n",
            &dummy_path(),
        )
        .unwrap();
        assert_eq!(c.session.term_fallback, "screen-256color");
        let c = parse_str(
            "[session]\non_child_suspend = \"auto_resume_always\"\n",
            &dummy_path(),
        )
        .unwrap();
        assert_eq!(c.session.term_fallback, "xterm-256color");
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
    fn web_file_overrides_listen() {
        let s = r#"
[web]
listen = "0.0.0.0:8080"
"#;
        let w = parse_web_str(s, &dummy_path()).unwrap();
        assert_eq!(w.web.listen, "0.0.0.0:8080");
    }

    #[test]
    fn web_file_missing_section_uses_default_listen() {
        let w = parse_web_str("", &dummy_path()).unwrap();
        assert_eq!(w.web.listen, "127.0.0.1:43690");
        assert_eq!(w.web.binary_path, None);
        assert_eq!(w.web.assets_dir, None);
    }

    /// web の設定は `config.toml` から web の config ファイルへ移った (DR-0038 決定 1)。
    /// 書いてあれば黙って無視せず、移し先を案内して止まる。
    #[test]
    fn web_keys_in_the_main_config_point_to_the_web_config_file() {
        for (s, key) in [
            ("[web]\nlisten = \"127.0.0.1:1\"\n", "[web] listen"),
            ("[web]\nassets_dir = \"/x\"\n", "[web] assets_dir"),
        ] {
            match parse_str(s, &dummy_path()) {
                Err(e @ ConfigError::RemovedKey { .. }) => {
                    let msg = e.to_string();
                    assert!(msg.contains(key), "{msg}");
                    assert!(msg.contains("hyoui web daemon add"), "{msg}");
                }
                other => panic!("`{key}` は RemovedKey で拒否されるべき: {other:?}"),
            }
        }
    }

    /// 書かれたファイルを置くための一時 dir。
    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    /// 表は鍵ごとに潜り、手前 (= extends を書いた側) が勝つ (DR-0038 決定 3)。
    #[test]
    fn extends_merges_tables_key_by_key() {
        let d = dir();
        write(
            d.path(),
            "base.toml",
            "[web]\nlisten = \"127.0.0.1:1\"\nbinary_path = \"/opt/hyoui\"\n",
        );
        let unit = write(
            d.path(),
            "unit.toml",
            "extends = \"base.toml\"\n[web]\nlisten = \"127.0.0.1:2\"\n",
        );
        let w = load_web(&unit).unwrap();
        assert_eq!(w.web.listen, "127.0.0.1:2");
        assert_eq!(w.web.binary_path, Some(PathBuf::from("/opt/hyoui")));
    }

    /// 配列は要素ごとに混ぜず丸ごと置き換える (= 並びが意味を持つ値を壊さない)。
    #[test]
    fn extends_replaces_arrays_whole() {
        let d = dir();
        write(
            d.path(),
            "base.toml",
            "[scrub_env.targets.claude]\nkill_glob = [\"A\", \"B\"]\nkeep_glob = [\"K\"]\n",
        );
        let top = write(
            d.path(),
            "config.toml",
            "extends = \"base.toml\"\n[scrub_env.targets.claude]\nkill_glob = [\"C\"]\n",
        );
        let c = load_from(&top).unwrap();
        let t = c.scrub_env.targets.get("claude").unwrap();
        assert_eq!(t.kill_glob, vec!["C"]);
        // 書かなかった鍵は土台のまま残る (= 表は鍵ごと)。
        assert_eq!(t.keep_glob, vec!["K"]);
    }

    /// 何段でも重ねられ、手前ほど勝つ。
    #[test]
    fn extends_chains_through_several_files() {
        let d = dir();
        write(
            d.path(),
            "a.toml",
            "[web]\nlisten = \"a:1\"\nassets_dir = \"/a\"\n",
        );
        write(
            d.path(),
            "b.toml",
            "extends = \"a.toml\"\n[web]\nlisten = \"b:1\"\n",
        );
        let c = write(d.path(), "c.toml", "extends = \"b.toml\"\n");
        let w = load_web(&c).unwrap();
        assert_eq!(w.web.listen, "b:1");
        assert_eq!(w.web.assets_dir, Some(PathBuf::from("/a")));
    }

    /// `extends` の相対パスは書いたファイルの隣から解く (= cwd を見ない)。
    #[test]
    fn extends_is_resolved_next_to_the_file_that_wrote_it() {
        let d = dir();
        write(
            d.path(),
            "shared/base.toml",
            "[web]\nlisten = \"127.0.0.1:9\"\n",
        );
        // 下の段から上の段を指す相対パス。
        write(d.path(), "shared/mid.toml", "extends = \"./base.toml\"\n");
        let unit = write(
            d.path(),
            "units/stable.toml",
            "extends = \"../shared/mid.toml\"\n",
        );
        assert_eq!(load_web(&unit).unwrap().web.listen, "127.0.0.1:9");
    }

    /// path の値 (`assets_dir` / `binary_path`) も、それを書いたファイルの隣から解く。
    /// 重ねた後で解くと、土台に書いた相対パスが派生側の位置で解かれてしまう。
    #[test]
    fn path_values_are_resolved_next_to_the_file_that_wrote_them() {
        let d = dir();
        write(
            d.path(),
            "shared/base.toml",
            "[web]\nassets_dir = \"assets\"\nbinary_path = \"bin/hyoui\"\n",
        );
        let unit = write(
            d.path(),
            "units/unstable.toml",
            "extends = \"../shared/base.toml\"\n",
        );
        // `..` は字面で畳まない (= symlink を挟むと OS の解決と食い違う)。指している
        // 実体が同じかで見るため、指される側を実在させて正規化して比べる。
        std::fs::create_dir_all(d.path().join("shared/assets")).unwrap();
        let shared_bin = write(d.path(), "shared/bin/hyoui", "");
        let units_bin = write(d.path(), "units/hyoui", "");
        let real = |p: Option<PathBuf>| std::fs::canonicalize(p.expect("set")).unwrap();

        let w = load_web(&unit).unwrap();
        assert_eq!(
            real(w.web.assets_dir),
            std::fs::canonicalize(d.path().join("shared/assets")).unwrap()
        );
        assert_eq!(
            real(w.web.binary_path),
            std::fs::canonicalize(&shared_bin).unwrap()
        );

        // 派生側が書けば、派生側の隣から解く。
        let unit = write(
            d.path(),
            "units/own.toml",
            "extends = \"../shared/base.toml\"\n[web]\nbinary_path = \"hyoui\"\n",
        );
        assert_eq!(
            real(load_web(&unit).unwrap().web.binary_path),
            std::fs::canonicalize(&units_bin).unwrap()
        );
    }

    /// `state_dir` も path の鍵として読む: 書いたファイルの隣から解き、`~` は `$HOME`
    /// で開く (DR-0038 決定 9)。書かなければ `None` (= 要否は読み手が判断する)。
    #[test]
    fn state_dir_is_a_path_value() {
        let d = dir();
        let unit = write(
            d.path(),
            "web/stable.toml",
            "[web]\nstate_dir = \"state/hyoui\"\n",
        );
        assert_eq!(
            load_web(&unit).unwrap().web.state_dir,
            Some(d.path().join("web/state/hyoui"))
        );
        let absolute = write(
            d.path(),
            "web/abs.toml",
            "[web]\nstate_dir = \"/s/hyoui\"\n",
        );
        assert_eq!(
            load_web(&absolute).unwrap().web.state_dir,
            Some(PathBuf::from("/s/hyoui"))
        );
        let bare = write(d.path(), "web/bare.toml", "[web]\n");
        assert_eq!(load_web(&bare).unwrap().web.state_dir, None);
    }

    /// 自分自身・A→B→A・symlink 越しの同じファイルは循環として止める。
    #[test]
    fn extends_cycles_stop_where_they_loop() {
        let d = dir();
        let selfish = write(d.path(), "self.toml", "extends = \"self.toml\"\n");
        assert!(matches!(
            load_web(&selfish),
            Err(ConfigError::ExtendsCycle { .. })
        ));

        write(d.path(), "a.toml", "extends = \"b.toml\"\n");
        let b = write(d.path(), "b.toml", "extends = \"./a.toml\"\n");
        match load_web(&b) {
            Err(e @ ConfigError::ExtendsCycle { .. }) => {
                let msg = e.to_string();
                assert!(msg.contains("a.toml") && msg.contains("b.toml"), "{msg}");
            }
            other => panic!("A→B→A は循環: {other:?}"),
        }

        let real = write(d.path(), "real.toml", "extends = \"link.toml\"\n");
        std::os::unix::fs::symlink(&real, d.path().join("link.toml")).unwrap();
        assert!(matches!(
            load_web(&real),
            Err(ConfigError::ExtendsCycle { .. })
        ));
    }

    /// 指した先が無ければ、どのファイルがどのパスを指したかを言う。
    #[test]
    fn a_missing_extends_target_names_the_file_that_pointed_at_it() {
        let d = dir();
        let unit = write(d.path(), "unit.toml", "extends = \"nowhere.toml\"\n");
        match load_web(&unit) {
            Err(ConfigError::ExtendsNotFound { from, target }) => {
                assert_eq!(from, unit);
                assert_eq!(target, d.path().join("nowhere.toml"));
            }
            other => panic!("ExtendsNotFound のはず: {other:?}"),
        }
        let bad = write(d.path(), "bad.toml", "extends = 1\n");
        assert!(matches!(
            load_web(&bad),
            Err(ConfigError::BadExtends { .. })
        ));
    }

    /// web の config は明示された path を読むので、無ければ default ではなくエラー。
    #[test]
    fn a_missing_web_file_is_an_error() {
        let d = dir();
        assert!(matches!(
            load_web(&d.path().join("absent.toml")),
            Err(ConfigError::NotFound { .. })
        ));
    }

    /// `config.toml` も同じ規則で `extends` を辿り、廃止 key の検査は畳んだ後に行う
    /// (= 土台に書かれた廃止 key も見逃さない)。
    #[test]
    fn the_main_config_follows_extends_and_checks_removed_keys_after_merging() {
        let d = dir();
        write(d.path(), "base.toml", "[session]\nauto_resume = true\n");
        let top = write(d.path(), "config.toml", "extends = \"base.toml\"\n");
        assert!(matches!(
            load_from(&top),
            Err(ConfigError::RemovedKey { .. })
        ));
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
            "term_fallback",
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

    /// 実効設定は PTY session 側の設定だけで、web の設定は含まない (DR-0038 決定 1)。
    #[test]
    fn to_toml_has_no_web_section() {
        let s = to_toml(&Config::default()).unwrap();
        assert!(!s.contains("[web]"), "unexpected [web]:\n{s}");
    }

    fn env(xdg: Option<&str>, home: Option<&str>) -> crate::paths::Env {
        crate::paths::Env::from_lookup(|name| match name {
            "XDG_CONFIG_HOME" => xdg.map(std::ffi::OsString::from),
            "HOME" => home.map(std::ffi::OsString::from),
            _ => None,
        })
    }

    #[test]
    fn resolve_path_uses_xdg_when_present() {
        let p = resolve_path_in(&env(Some("/custom/xdg"), None)).unwrap();
        assert_eq!(p, PathBuf::from("/custom/xdg/hyoui/config.toml"));
    }

    #[test]
    fn resolve_path_falls_back_to_home_when_xdg_unset() {
        let p = resolve_path_in(&env(None, Some("/custom/home"))).unwrap();
        assert_eq!(p, PathBuf::from("/custom/home/.config/hyoui/config.toml"));
    }

    #[test]
    fn resolve_path_falls_back_to_home_when_xdg_empty() {
        // 異常ケース: XDG=空文字は未設定扱い。
        let p = resolve_path_in(&env(Some(""), Some("/h"))).unwrap();
        assert_eq!(p, PathBuf::from("/h/.config/hyoui/config.toml"));
    }

    #[test]
    fn resolve_path_returns_none_when_both_missing() {
        assert!(resolve_path_in(&env(None, None)).is_none());
        assert!(resolve_path_in(&env(Some(""), None)).is_none());
        assert!(resolve_path_in(&env(None, Some(""))).is_none());
    }
}
