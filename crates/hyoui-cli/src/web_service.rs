//! `hyoui web service` の OS service 管理層 (= DR-0034 決定 6)。
//!
//! OS に載るのは**監督者 1 つだけ**で、unit ごとの plist / systemd unit は作らない。
//! 台数が増えても OS 側の定義は増えないので、gateway を更新しても `register` を
//! やり直す必要がない (= 「binary 更新をまたいで安定する契約」)。
//!
//! [`ServiceDefinition`] と 2 renderer は全 OS で compile する純粋ロジック、
//! [`Backend`] の shell-out だけを macOS / Linux で `cfg` 分離する。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use stable_which::{Candidate, ScoringPolicy, resolve_stable_path};

/// 監督者の OS 登録名の部品 (= label を組み立てる唯一の場所)。
///
/// 逆引き domain の接頭辞と、hyoui の下で監督者を指す部分を分けて持つ。名前を
/// 変える時はここだけを直す (DR-0038 決定 4、接頭辞と区切りの形は裁定待ち)。
macro_rules! label_domain {
    () => {
        "jp.kawaz"
    };
}
macro_rules! label_app {
    () => {
        "hyoui-web"
    };
}

/// 監督者の launchd label (= `<domain>.<app>.supervise`)。
pub const MACOS_LABEL: &str = concat!(label_domain!(), ".", label_app!(), ".supervise");
/// 監督者の systemd user unit 名 (= `<app>-supervise`)。
pub const LINUX_LABEL: &str = concat!(label_app!(), "-supervise");

/// launchd / systemd user に共通するサービスの意味記述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDefinition {
    pub label: String,
    pub program_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub log_path: Option<String>,
    pub associated_bundle_identifiers: Option<String>,
}

impl ServiceDefinition {
    /// 監督者 1 つを載せる定義 (= 決定 6)。
    ///
    /// argv は `<binary> web daemon supervise` の 4 語で、unit の数や名前は入らない。
    /// unit を足しても消しても、この定義は 1 文字も変わらない。`env` は
    /// [`pinned_env`] が作る固定値 (DR-0038 決定 5)。
    pub fn for_supervisor(
        program: &str,
        log_path: Option<String>,
        env: BTreeMap<String, String>,
    ) -> Self {
        Self::labelled(&default_label(), program, log_path, env)
    }

    /// label を明示して組み立てる (= 隔離 label での実機確認と golden test 用)。
    pub fn labelled(
        label: &str,
        program: &str,
        log_path: Option<String>,
        env: BTreeMap<String, String>,
    ) -> Self {
        let program_args = vec![
            program.to_string(),
            "web".to_string(),
            "daemon".to_string(),
            "supervise".to_string(),
        ];
        Self {
            label: label.to_string(),
            program_args,
            env,
            log_path,
            associated_bundle_identifiers: None,
        }
    }
}

/// この OS の組み込み label。
pub fn builtin_label() -> &'static str {
    if cfg!(target_os = "macos") {
        MACOS_LABEL
    } else {
        LINUX_LABEL
    }
}

/// 監督者の label。`HYOUI_WEB_SERVICE_LABEL` が与えられていればそれを使う。
///
/// Design rationale: launchd の job は `gui/$UID/<label>` に属し、**隔離 HOME を
/// 渡しても domain は共有される**。label を差し替える口が無いと、`register` /
/// `stop` を実機で確かめる操作が常に本番の常駐と同じ job を触ることになる
/// (= 確かめるために止めることになる)。DR-0014 の検証主義を満たすために、別 label
/// で並べて確かめられる口を持つ。
///
/// 値はファイル名に使うので `[A-Za-z0-9._-]` に限り、外れた値は無視して組み込みの
/// label に戻る (= 黙って path を作らせない)。
pub fn default_label() -> String {
    std::env::var("HYOUI_WEB_SERVICE_LABEL")
        .ok()
        .filter(|label| {
            !label.is_empty()
                && label.len() <= 128
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
        .unwrap_or_else(|| builtin_label().to_string())
}

/// 定義に固定する env (DR-0038 決定 5)。
///
/// 場所の導出に効く変数は [`hyoui::paths::LocationVar`] の列挙そのもので、ここに
/// 別のリストを持たない。値の無い変数は書かない (= 「無い」ことも固定される。
/// launchd / systemd は shell の env を継承しないので、書かなければ無い)。`PATH` も
/// 同じ枡で固定し、重複と空要素を落として並びを保つ (= register を繰り返しても
/// 積もらない)。
pub fn pinned_env(
    locations: &hyoui::paths::Env,
    path: Option<&std::ffi::OsStr>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = hyoui::paths::LocationVar::ALL
        .iter()
        .filter_map(|var| {
            locations
                .get(*var)
                .map(|value| (var.name().to_string(), value.to_string_lossy().into_owned()))
        })
        .collect();
    if let Some(path) = path.map(|value| normalize_path_var(&value.to_string_lossy()))
        && !path.is_empty()
    {
        env.insert("PATH".to_string(), path);
    }
    env
}

/// `PATH` の重複と空要素を落とす (= 最初に現れた位置を残す)。
pub fn normalize_path_var(path: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    path.split(':')
        .filter(|entry| !entry.is_empty() && seen.insert(*entry))
        .collect::<Vec<_>>()
        .join(":")
}

/// 既存の定義に固定された場所の env と、これから書く値の食い違い (DR-0038 決定 5)。
///
/// 比べるのは [`hyoui::paths::LocationVar`] だけ。`PATH` は場所を導かないので
/// 比べない (= shell ごとに違うのが普通で、違っても置き場は食い違わない)。
pub fn pinned_location_drift(
    registered: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<LocationDifference> {
    hyoui::paths::LocationVar::ALL
        .iter()
        .filter_map(|var| {
            let name = var.name();
            let before = registered.get(name);
            let after = current.get(name);
            (before != after).then(|| LocationDifference {
                name: name.to_string(),
                registered: before.cloned(),
                current: after.cloned(),
            })
        })
        .collect()
}

/// 固定された値と今の値の差 1 つ (`None` = 未設定)。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LocationDifference {
    /// 変数名。
    pub name: String,
    /// 定義に固定されている値。
    pub registered: Option<String>,
    /// 今の shell の値。
    pub current: Option<String>,
}

/// 監督者のログの置き場 (= 決定 6、DR-0038 決定 4)。
///
/// 子 gateway のログと同じ `logs/` に、label の名前で置く (= unit 名は `.` を含め
/// ないので衝突しない。label を変えれば隔離 label の実機確認でも分かれる)。ここに
/// 来るのは監督者自身が書いたものだけ。
pub fn log_path_for(env: &hyoui::paths::Env, label: &str) -> String {
    env.web_state_dir()
        .join("logs")
        .join(format!("{label}.log"))
        .to_string_lossy()
        .into_owned()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn launchd_definition_path(home: &Path, label: &str) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn systemd_definition_path(config_home: &Path, label: &str) -> PathBuf {
    let unit = if label.ends_with(".service") {
        label.to_string()
    } else {
        format!("{label}.service")
    };
    config_home.join("systemd/user").join(unit)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn render_launchd_plist(def: &ServiceDefinition) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n",
    );
    out.push_str("\t<key>Label</key>\n");
    out.push_str(&format!("\t<string>{}</string>\n", xml_escape(&def.label)));
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in &def.program_args {
        out.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(arg)));
    }
    out.push_str("\t</array>\n");
    out.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    out.push_str("\t<key>KeepAlive</key>\n\t<true/>\n");
    if !def.env.is_empty() {
        out.push_str("\t<key>EnvironmentVariables</key>\n\t<dict>\n");
        for (key, value) in &def.env {
            out.push_str(&format!("\t\t<key>{}</key>\n", xml_escape(key)));
            out.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(value)));
        }
        out.push_str("\t</dict>\n");
    }
    if let Some(bundle_id) = &def.associated_bundle_identifiers {
        out.push_str("\t<key>AssociatedBundleIdentifiers</key>\n");
        out.push_str(&format!("\t<string>{}</string>\n", xml_escape(bundle_id)));
    }
    if let Some(log_path) = &def.log_path {
        let escaped = xml_escape(log_path);
        out.push_str("\t<key>StandardOutPath</key>\n");
        out.push_str(&format!("\t<string>{escaped}</string>\n"));
        out.push_str("\t<key>StandardErrorPath</key>\n");
        out.push_str(&format!("\t<string>{escaped}</string>\n"));
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_quote(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '%' => escaped.push_str("%%"),
            other => escaped.push(other),
        }
    }
    format!("\"{escaped}\"")
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn render_systemd_unit(def: &ServiceDefinition) -> String {
    let exec = def
        .program_args
        .iter()
        .map(|arg| systemd_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let mut out =
        String::from("[Unit]\nDescription=hyoui HTTP gateway\n\n[Service]\nType=simple\n");
    out.push_str(&format!("ExecStart={exec}\n"));
    out.push_str("Restart=always\n");
    for (key, value) in &def.env {
        out.push_str("Environment=");
        out.push_str(&systemd_quote(&format!("{key}={value}")));
        out.push('\n');
    }
    out.push_str("\n[Install]\nWantedBy=default.target\n");
    out
}

/// OS 側から取れる情報 (= 決定 6 の `service` 入れ物)。
///
/// `registered` (定義ファイルが実在する) と `loaded` (OS に載っている) は別物で、
/// ファイルが有るのに載っていない状態がある。同じ語を階層で分けているのは、この
/// 食い違い自体を見せるため。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// 定義ファイル (plist / systemd unit) が実在するか。
    pub registered: bool,
    /// OS がその定義を読み込んでいるか。
    pub loaded: bool,
    /// OS から見て走っているか。
    pub running: bool,
    /// OS が答えた pid。
    pub pid: Option<u32>,
    /// 最後の終了状態 (= OS が覚えている範囲)。
    pub last_exit: Option<i32>,
    /// 定義ファイルの path (= 人が launchctl / systemctl を直接叩く時の手がかり)。
    pub definition_path: PathBuf,
}

/// ログの取り出し方 (= OS で経路が違う、決定 6)。
pub enum LogSource {
    /// 追記されるファイルを読む (macOS)。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    File(PathBuf),
    /// コマンドの出力を読む (Linux の journalctl)。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Command {
        /// 実行するプログラム。
        program: String,
        /// 引数。`--follow` の分は呼び出し側が足す。
        args: Vec<String>,
    },
}

pub trait Backend {
    fn definition_path(&self, label: &str) -> Result<PathBuf, String>;
    /// 定義を置いて OS に載せ、起動する。
    fn install(&self, def: &ServiceDefinition) -> Result<(), String>;
    /// OS から外して定義を消す。
    fn unregister(&self, label: &str) -> Result<(), String>;
    /// 載せて起動する (= 既に載っていれば起こし直す)。
    fn start(&self, label: &str) -> Result<(), String>;
    /// 止めて、再 login / 再 bootstrap を跨いでも上がらないようにする。
    fn stop(&self, label: &str) -> Result<(), String>;
    /// 止めて上げ直す (= 全断を伴う入れ替え、決定 6)。
    fn restart(&self, label: &str) -> Result<(), String>;
    fn status(&self, label: &str) -> Result<ServiceStatus, String>;
    /// ログの取り出し方。
    fn log_source(&self, label: &str) -> Result<LogSource, String>;
}

/// 定義を描いて、違っていれば載せ直す (= 決定 6 の冪等な `register`)。
///
/// 既にそのまま置かれ OS にも載っていれば何もせず `false` を返す。「既に登録されて
/// いる」を理由に断ると、中身を直したいだけの操作に `unregister` を挟ませることに
/// なるので、冪等にして `changed` で伝える。
pub fn register(backend: &dyn Backend, def: &ServiceDefinition) -> Result<bool, String> {
    let path = backend.definition_path(&def.label)?;
    let rendered = render_definition(def);
    let current = std::fs::read_to_string(&path).ok();
    let status = backend.status(&def.label)?;

    if current.as_deref() == Some(rendered.as_str()) && status.loaded {
        return Ok(false);
    }
    backend.install(def)?;
    Ok(true)
}

/// `register` を断った理由。
#[derive(Debug)]
pub enum RegisterError {
    /// 既存の定義が別の場所を固定している (DR-0038 決定 5)。
    Drift {
        /// 既存の定義ファイル。
        path: PathBuf,
        /// 変数ごとの差。
        differences: Vec<LocationDifference>,
    },
    /// OS に載せられなかった。
    Failed(String),
}

/// 既存の定義が固定した場所と食い違っていれば止まり、そうでなければ [`register`] する。
///
/// 黙って書き換えると、監督者と既存の client の片方が別の dir を見続ける。止める側に
/// 倒すのは、この食い違いが起きた時に気づく手段が他に無いから (reference
/// `cli-daemon-subcommands`)。`force` で押し切る。
pub fn register_checked(
    backend: &dyn Backend,
    def: &ServiceDefinition,
    force: bool,
) -> Result<bool, RegisterError> {
    if !force {
        let path = backend
            .definition_path(&def.label)
            .map_err(RegisterError::Failed)?;
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let differences = pinned_location_drift(&env_in_definition(&existing), &def.env);
            if !differences.is_empty() {
                return Err(RegisterError::Drift { path, differences });
            }
        }
    }
    register(backend, def).map_err(RegisterError::Failed)
}

/// この OS 向けの定義テキスト。
pub fn render_definition(def: &ServiceDefinition) -> String {
    #[cfg(target_os = "macos")]
    {
        render_launchd_plist(def)
    }
    #[cfg(not(target_os = "macos"))]
    {
        render_systemd_unit(def)
    }
}

#[cfg(target_os = "macos")]
pub fn backend() -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(LaunchdBackend))
}

#[cfg(target_os = "linux")]
pub fn backend() -> Result<Box<dyn Backend>, String> {
    Ok(Box::new(SystemdBackend))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn backend() -> Result<Box<dyn Backend>, String> {
    Err("web service is supported only on macOS and Linux".to_string())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn home_dir() -> Result<PathBuf, String> {
    hyoui::paths::Env::current()
        .home()
        .map(Path::to_path_buf)
        .ok_or_else(|| "$HOME is not set; cannot resolve the per-user service path".to_string())
}

#[cfg(target_os = "linux")]
fn systemd_config_home() -> Result<PathBuf, String> {
    hyoui::paths::Env::current().config_home().ok_or_else(|| {
        "neither $XDG_CONFIG_HOME nor $HOME is set; cannot resolve the per-user service path"
            .to_string()
    })
}

fn command_output(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run `{program} {}`: {error}", args.join(" ")))
}

fn command_success(program: &str, args: &[&str]) -> Result<(), String> {
    let output = command_output(program, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(target_os = "macos")]
struct LaunchdBackend;

#[cfg(target_os = "macos")]
impl LaunchdBackend {
    fn target(label: &str) -> String {
        format!("gui/{}/{label}", nix::unistd::Uid::effective().as_raw())
    }
}

#[cfg(target_os = "macos")]
impl Backend for LaunchdBackend {
    fn definition_path(&self, label: &str) -> Result<PathBuf, String> {
        Ok(launchd_definition_path(&home_dir()?, label))
    }

    fn install(&self, def: &ServiceDefinition) -> Result<(), String> {
        let path = self.definition_path(&def.label)?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("service definition has no parent: {}", path.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        if let Some(log_path) = &def.log_path
            && let Some(parent) = Path::new(log_path).parent()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }

        // 同 label の手書き plist も含め、先に既存 job を外してから定義を置換する。
        let target = Self::target(&def.label);
        let _ = command_output("launchctl", &["bootout", &target]);
        std::fs::write(&path, render_launchd_plist(def))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        // `stop` が置いた「上げない」指示を解除してから載せる。
        let _ = command_output("launchctl", &["enable", &target]);
        let domain = format!("gui/{}", nix::unistd::Uid::effective().as_raw());
        command_success(
            "launchctl",
            &["bootstrap", &domain, &path.to_string_lossy()],
        )
    }

    fn unregister(&self, label: &str) -> Result<(), String> {
        let target = Self::target(label);
        let _ = command_output("launchctl", &["bootout", &target]);
        let path = self.definition_path(label)?;
        let removed = match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
        };
        // `stop` が置いた「上げない」指示を解除する。定義を消した後なので載る相手は
        // 居ないが、この指示は label に紐づいて残り、人が plist を手で戻した時に
        // 効いてしまう (= 戻したのに上がらない)。launchd は一度触った label を
        // `print-disabled` に残し続けるので消すことはできず、解除するだけ。
        let _ = command_output("launchctl", &["enable", &target]);
        removed
    }

    fn start(&self, label: &str) -> Result<(), String> {
        let target = Self::target(label);
        let path = self.definition_path(label)?;
        if !path.is_file() {
            return Err(format!(
                "{} does not exist; run `hyoui web service register` first",
                path.display()
            ));
        }
        // `stop` の対 (= `enable`) を先に打ってから載せる。既に載っていれば
        // `bootstrap` は失敗するので、その時は `kickstart` で起こす。
        let _ = command_output("launchctl", &["enable", &target]);
        let domain = format!("gui/{}", nix::unistd::Uid::effective().as_raw());
        if command_success(
            "launchctl",
            &["bootstrap", &domain, &path.to_string_lossy()],
        )
        .is_ok()
        {
            return Ok(());
        }
        command_success("launchctl", &["kickstart", "-k", &target])
    }

    fn stop(&self, label: &str) -> Result<(), String> {
        // `launchctl stop` は使わない: `KeepAlive=true` なので送られた SIGTERM の
        // 後 launchd が即座に上げ直す (= 止まらない)。`bootout` で外し、`disable`
        // で次の login でも `RunAtLoad` により復活しないようにする (決定 6)。
        let target = Self::target(label);
        command_success("launchctl", &["bootout", &target])?;
        let _ = command_output("launchctl", &["disable", &target]);
        Ok(())
    }

    fn restart(&self, label: &str) -> Result<(), String> {
        // launchd に「入れ替え」の verb は無く、`bootout` + `disable` → `enable` +
        // `bootstrap` の 2 手になる (決定 6 の verb 表)。降ろす側の失敗は見ない:
        // 載っていなければ降ろす必要が無く、その時 restart は start と同じ意味に
        // なればよい (= 冪等)。上げ直しの成否だけを結果とする。
        let _ = self.stop(label);
        self.start(label)
    }

    fn status(&self, label: &str) -> Result<ServiceStatus, String> {
        let path = self.definition_path(label)?;
        let target = Self::target(label);
        let output = command_output("launchctl", &["print", &target])?;
        let printed = String::from_utf8_lossy(&output.stdout);
        let loaded = output.status.success();
        let pid = loaded.then(|| parse_launchctl_pid(&printed)).flatten();
        Ok(ServiceStatus {
            registered: path.is_file(),
            loaded,
            running: pid.is_some(),
            pid,
            last_exit: loaded
                .then(|| parse_launchctl_last_exit(&printed))
                .flatten(),
            definition_path: path,
        })
    }

    fn log_source(&self, label: &str) -> Result<LogSource, String> {
        // 書き手 (= 監督者) が使っている path は定義に焼かれている。今の shell の
        // env から導くと、固定値と食い違った時に別のファイルを読む。
        let from_definition = self
            .definition_path(label)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| log_path_in_definition(&text));
        Ok(LogSource::File(PathBuf::from(
            from_definition.unwrap_or_else(|| log_path_for(&hyoui::paths::Env::current(), label)),
        )))
    }
}

#[cfg(target_os = "linux")]
struct SystemdBackend;

#[cfg(target_os = "linux")]
fn systemd_unit_name(label: &str) -> String {
    if label.ends_with(".service") {
        label.to_string()
    } else {
        format!("{label}.service")
    }
}

#[cfg(target_os = "linux")]
impl Backend for SystemdBackend {
    fn definition_path(&self, label: &str) -> Result<PathBuf, String> {
        Ok(systemd_definition_path(&systemd_config_home()?, label))
    }

    fn install(&self, def: &ServiceDefinition) -> Result<(), String> {
        let path = self.definition_path(&def.label)?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("service definition has no parent: {}", path.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        std::fs::write(&path, render_systemd_unit(def))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        let unit = systemd_unit_name(&def.label);
        command_success("systemctl", &["--user", "daemon-reload"])?;
        command_success("systemctl", &["--user", "enable", &unit])?;
        command_success("systemctl", &["--user", "restart", &unit])
    }

    fn start(&self, label: &str) -> Result<(), String> {
        // systemd は launchd と逆で、明示 `start` / `stop` をそのまま尊重する。
        command_success("systemctl", &["--user", "start", &systemd_unit_name(label)])
    }

    fn stop(&self, label: &str) -> Result<(), String> {
        // `Restart=always` でも明示 `stop` は尊重されるので、`disable` は要らない。
        command_success("systemctl", &["--user", "stop", &systemd_unit_name(label)])
    }

    fn restart(&self, label: &str) -> Result<(), String> {
        // systemd は入れ替えを 1 語で持ち、止まっている相手にも使える (= 冪等)。
        command_success(
            "systemctl",
            &["--user", "restart", &systemd_unit_name(label)],
        )
    }

    fn log_source(&self, label: &str) -> Result<LogSource, String> {
        Ok(LogSource::Command {
            program: "journalctl".to_string(),
            args: vec![
                "--user".to_string(),
                "-u".to_string(),
                systemd_unit_name(label),
            ],
        })
    }

    fn unregister(&self, label: &str) -> Result<(), String> {
        let unit = systemd_unit_name(label);
        let _ = command_output("systemctl", &["--user", "disable", "--now", &unit]);
        let path = self.definition_path(label)?;
        let removed = match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(format!("cannot remove {}: {error}", path.display())),
        };
        if removed {
            command_success("systemctl", &["--user", "daemon-reload"])?;
        }
        Ok(())
    }

    fn status(&self, label: &str) -> Result<ServiceStatus, String> {
        let path = self.definition_path(label)?;
        let unit = systemd_unit_name(label);
        // systemd に聞けない時 (= systemctl が無い / user manager の bus に届かない) は
        // OS 側の見え方を「載っていない・走っていない」として答える。status は未登録でも
        // 答える口で (= DR-0034 決定 6)、`registered` は定義ファイルだけで決まるため、
        // OS に聞けないことを理由に全体を失敗させない。bus に届かない場合は systemctl
        // 自体は起動でき、非 0 終了 + 空 stdout で同じ結論になる。systemctl が無い場合も
        // それと揃える (= 聞けない理由で答えの形を変えない)。
        let query = |args: &[&str]| {
            command_output("systemctl", args)
                .ok()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        };
        let running = query(&["--user", "is-active", &unit]).as_deref() == Some("active");
        let show = |property: &str| query(&["--user", "show", "-p", property, "--value", &unit]);
        let loaded = show("LoadState").as_deref() == Some("loaded");
        let pid = running
            .then(|| show("MainPID"))
            .flatten()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid != 0);
        Ok(ServiceStatus {
            registered: path.is_file(),
            loaded,
            running,
            pid,
            last_exit: show("ExecMainStatus").and_then(|value| value.parse::<i32>().ok()),
            definition_path: path,
        })
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_launchctl_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.trim().parse().ok())
    })
}

/// `launchctl print` の最後の終了状態。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_launchctl_last_exit(text: &str) -> Option<i32> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("last exit code = ")
            .and_then(|value| value.trim().parse().ok())
    })
}

/// 定義ファイルに焼かれている実行ファイルの path を読む。
///
/// 監督者が止まっている間、「次に上がる版」を知る手がかりはこれだけ (= 走っている
/// 本人に聞けない)。読むのは `ProgramArguments` / `ExecStart` の先頭 1 語だけで、
/// unit の属性を定義ファイルから復元する経路は持たない (決定 2)。
pub fn program_in_definition(text: &str) -> Option<String> {
    // launchd: <key>ProgramArguments</key> <array> <string>…</string>
    if let Some(after) = text.split_once("<key>ProgramArguments</key>") {
        let (_, rest) = after;
        let (_, inside) = rest.split_once("<array>")?;
        let (first, _) = inside.split_once("</string>")?;
        let (_, value) = first.split_once("<string>")?;
        return Some(value.trim().to_string()).filter(|value| !value.is_empty());
    }
    // systemd: ExecStart="…" "web" "daemon" "supervise"
    let exec = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("ExecStart="))?;
    let first = exec.trim();
    let program = match first.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map(|(value, _)| value)?,
        None => first.split_whitespace().next()?,
    };
    Some(program.to_string()).filter(|value| !value.is_empty())
}

/// 定義に固定された env を読む (= `EnvironmentVariables` / `Environment=`)。
///
/// 読むのは自分が描いた形だけで、手書きの定義を汎用に解釈する parser ではない
/// (DR-0034 決定 2 と同じ線引き)。
pub fn env_in_definition(text: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some((_, rest)) = text.split_once("<key>EnvironmentVariables</key>") {
        let Some((_, inside)) = rest.split_once("<dict>") else {
            return env;
        };
        let Some((dict, _)) = inside.split_once("</dict>") else {
            return env;
        };
        let mut rest = dict;
        while let Some((_, after_key)) = rest.split_once("<key>") {
            let Some((key, after)) = after_key.split_once("</key>") else {
                break;
            };
            let Some((_, after_open)) = after.split_once("<string>") else {
                break;
            };
            let Some((value, tail)) = after_open.split_once("</string>") else {
                break;
            };
            env.insert(xml_unescape(key.trim()), xml_unescape(value));
            rest = tail;
        }
        return env;
    }
    for line in text.lines() {
        let Some(quoted) = line.trim().strip_prefix("Environment=") else {
            continue;
        };
        let Some(inner) = quoted
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            continue;
        };
        let unquoted = systemd_unquote(inner);
        if let Some((key, value)) = unquoted.split_once('=') {
            env.insert(key.to_string(), value.to_string());
        }
    }
    env
}

/// 定義に焼かれた監督者のログの path (launchd の `StandardOutPath`)。
pub fn log_path_in_definition(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("<key>StandardOutPath</key>")?;
    let (_, after) = rest.split_once("<string>")?;
    let (value, _) = after.split_once("</string>")?;
    Some(xml_unescape(value)).filter(|value| !value.is_empty())
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn systemd_unquote(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            },
            '%' => {
                // `%%` は `%` 1 つ (= systemd_quote の対)。
                if chars.clone().next() == Some('%') {
                    chars.next();
                }
                out.push('%');
            }
            other => out.push(other),
        }
    }
    out
}

pub struct ResolvedProgram {
    pub path: PathBuf,
    pub warning: Option<String>,
}

// -----------------------------------------------------------------------------
// verb の実装 (= 出力は JSON、エラーは JSON を stderr に出して非 0)
// -----------------------------------------------------------------------------

use crate::web_daemon::probe::{SystemProbe, VersionProbe};
use crate::web_daemon::protocol::{self, Request, Response, Target, VersionPair};
use crate::web_daemon::registry;
use serde_json::{Value, json};
use std::process::ExitCode;

/// 定義に焼かれた実行ファイルを読む (= 監督者が止まっていても分かる)。
fn program_of_registered_definition(backend: &dyn Backend, label: &str) -> Option<PathBuf> {
    let path = backend.definition_path(label).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    program_in_definition(&text).map(PathBuf::from)
}

/// 登録済みの定義に固定された場所が、今の shell と食い違っていれば警告文を返す
/// (DR-0038 決定 5、client 側の検知)。
///
/// 監督者に届かない時の手がかりはこれだけ (= 場所が食い違っていれば socket の
/// 位置も食い違うので、監督者は名乗れない)。定義が無ければ比べない。
pub fn registered_location_warnings() -> Vec<String> {
    let Ok(backend) = backend() else {
        return Vec::new();
    };
    let Ok(path) = backend.definition_path(&default_label()) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let current = pinned_env(&hyoui::paths::Env::current(), None);
    pinned_location_drift(&env_in_definition(&text), &current)
        .into_iter()
        .map(|difference| {
            format!(
                "the supervisor is registered with {name}={} but this shell has {name}={}; they derive different locations (re-run `hyoui web service register --force` from the shell whose locations should win)",
                difference.registered.as_deref().unwrap_or("(unset)"),
                difference.current.as_deref().unwrap_or("(unset)"),
                name = difference.name,
            )
        })
        .collect()
}

/// 監督者の版の組を作る (= 決定 7a、`service status` と `hyoui version` の共通経路)。
///
/// `running` は走っている本人が制御 socket で答えたもの、`on_disk` は OS 側の定義に
/// 焼かれた path の実行ファイルに `--version` を聞いたもの。登録が無ければ `None`。
pub fn supervisor_version() -> Option<(VersionPair, PathBuf)> {
    let backend = backend().ok()?;
    let label = default_label();
    let binary = program_of_registered_definition(backend.as_ref(), &label)?;
    let running = match protocol::request(
        &registry::supervisor_socket_path(),
        &Request::Status(Target::all()),
    ) {
        Ok((Response::Units { supervisor, .. }, _)) => Some(supervisor.version),
        _ => None,
    };
    let on_disk = SystemProbe.on_disk(&binary);
    Some((VersionPair::new(running, on_disk), binary))
}

/// `hyoui web service register [--binary=<path>] [--force]`。
pub fn register_command(binary: Option<PathBuf>, force: bool) -> ExitCode {
    let context = "web service register";
    let backend = match backend() {
        Ok(backend) => backend,
        Err(error) => return fail(context, &error),
    };

    let mut warnings = Vec::new();
    let program = match binary {
        Some(path) => path,
        None => {
            // 監督者に焼くのは安定な場所 (= unit 側が current_exe をそのまま焼くのと
            // 逆)。監督者はどの版でも同じ仕事をするので、brew を跨いで同じ path を
            // 指していてよい (決定 3)。
            let current = match std::env::current_exe() {
                Ok(path) => path,
                Err(error) => {
                    return fail(context, &format!("cannot resolve own binary path: {error}"));
                }
            };
            match resolve_program(&current) {
                Ok(resolved) => {
                    if let Some(warning) = resolved.warning {
                        warnings.push(warning);
                    }
                    resolved.path
                }
                Err(error) => return fail(context, &error),
            }
        }
    };

    let label = default_label();
    // launchd は StandardOutPath でファイルに落とす、systemd は journald が拾うので
    // 定義に log path を書かない (決定 6)。
    let locations = hyoui::paths::Env::current();
    let path_var = std::env::var_os("PATH");
    let definition = ServiceDefinition::for_supervisor(
        &program.to_string_lossy(),
        cfg!(target_os = "macos").then(|| log_path_for(&locations, &label)),
        pinned_env(&locations, path_var.as_deref()),
    );

    let changed = match register_checked(backend.as_ref(), &definition, force) {
        Ok(changed) => changed,
        Err(RegisterError::Drift { path, differences }) => {
            return fail_json(
                context,
                json!({
                    "command": context,
                    "kind": "location_env_drift",
                    "error": format!(
                        "{} pins different locations than this shell; nothing was changed",
                        path.display()
                    ),
                    "path": path,
                    "differences": differences,
                    "hint": "run it from the shell whose locations the supervisor should use, or pass --force to replace the pinned values",
                }),
            );
        }
        Err(RegisterError::Failed(error)) => return fail(context, &error),
    };
    let path = backend.definition_path(&label).unwrap_or_default();

    let mut output = json!({
        "label": label,
        "path": path,
        "binary_path": program,
        "changed": changed,
        "argv": definition.program_args,
        "env": definition.env,
    });
    if !warnings.is_empty() {
        output["warnings"] = json!(warnings);
    }
    emit(&output)
}

/// `hyoui web service unregister`。
pub fn unregister_command() -> ExitCode {
    let context = "web service unregister";
    let backend = match backend() {
        Ok(backend) => backend,
        Err(error) => return fail(context, &error),
    };
    let label = default_label();
    let path = backend.definition_path(&label).unwrap_or_default();
    let was_registered = path.is_file();
    if let Err(error) = backend.unregister(&label) {
        return fail(context, &error);
    }
    emit(&json!({
        "label": label,
        "path": path,
        "changed": was_registered,
    }))
}

/// 監督者そのものに対する操作 (= 決定 6、unit 単位の `daemon` 側とは別)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorVerb {
    Start,
    Stop,
    Restart,
}

impl SupervisorVerb {
    fn context(self) -> &'static str {
        match self {
            Self::Start => "web service start",
            Self::Stop => "web service stop",
            Self::Restart => "web service restart",
        }
    }
}

/// `hyoui web service start` / `stop` / `restart`。
pub fn control_command(verb: SupervisorVerb) -> ExitCode {
    let context = verb.context();
    let backend = match backend() {
        Ok(backend) => backend,
        Err(error) => return fail(context, &error),
    };
    let label = default_label();
    // 上げる側の verb は、定義が無ければ OS に頼む相手が居ない。backend ごとの
    // メッセージ (launchd は path、systemd は unit not found) に任せると何を
    // すればよいか伝わらないので、ここで register への道を示して断る。
    if matches!(verb, SupervisorVerb::Restart)
        && let Ok(path) = backend.definition_path(&label)
        && !path.is_file()
    {
        return fail(
            context,
            &format!(
                "{} does not exist; run `hyoui web service register` first",
                path.display()
            ),
        );
    }
    let result = match verb {
        SupervisorVerb::Start => backend.start(&label),
        SupervisorVerb::Stop => backend.stop(&label),
        SupervisorVerb::Restart => backend.restart(&label),
    };
    if let Err(error) = result {
        return fail(context, &error);
    }
    let service = backend.status(&label).ok();
    emit(&json!({
        "label": label,
        "service": service.as_ref().map(service_json),
    }))
}

/// `hyoui web service status` (= 決定 6)。
pub fn status_command() -> ExitCode {
    let context = "web service status";
    let backend = match backend() {
        Ok(backend) => backend,
        Err(error) => return fail(context, &error),
    };
    let label = default_label();
    let service = match backend.status(&label) {
        Ok(service) => service,
        Err(error) => return fail(context, &error),
    };

    // top-level の `running` は **監督者の頼み口に届いたか**。OS が loaded と言って
    // いても頼み口が開いていなければ頼めないので、届いたかどうかを正とする。
    let asked = protocol::request(
        &registry::supervisor_socket_path(),
        &Request::Status(Target::all()),
    );
    let (running, instances, supervisor_running_version, supervisor_locations) = match asked {
        Ok((
            Response::Units {
                units, supervisor, ..
            },
            _,
        )) => (
            true,
            json!(units),
            Some(supervisor.version),
            Some(supervisor.locations),
        ),
        _ => (false, json!([]), None, None),
    };

    let binary = program_of_registered_definition(backend.as_ref(), &label);
    let version = binary.as_ref().map(|binary| {
        let pair = VersionPair::new(supervisor_running_version, SystemProbe.on_disk(binary));
        json!({
            "running": pair.running,
            "on_disk": pair.on_disk,
            "binary_path": binary,
            "restart_needed": pair.restart_needed,
        })
    });

    let mut output = json!({
        "label": label,
        "path": service.definition_path,
        "registered": service.registered,
        "running": running,
        "pid": service.pid,
        "service": service_json(&service),
        "version": version,
        "instances": instances,
    });
    // 定義に固定された場所と、届いた監督者が名乗った場所の両方を今の shell と比べる
    // (DR-0038 決定 5)。
    let mut warnings = registered_location_warnings();
    if let Some(locations) = &supervisor_locations {
        warnings.extend(protocol::location_drift(locations));
    }
    if !warnings.is_empty() {
        for warning in &warnings {
            eprintln!("hyoui: warning: {warning}");
        }
        output["warnings"] = json!(warnings);
    }
    emit(&output)
}

/// `hyoui web service log [--follow]` (= 決定 6)。
pub fn log_command(follow: bool) -> ExitCode {
    let context = "web service log";
    let backend = match backend() {
        Ok(backend) => backend,
        Err(error) => return fail(context, &error),
    };
    let source = match backend.log_source(&default_label()) {
        Ok(source) => source,
        Err(error) => return fail(context, &error),
    };

    match source {
        LogSource::File(path) => {
            if !path.is_file() {
                return fail(
                    context,
                    &format!(
                        "{} does not exist yet; the supervisor writes it once it runs",
                        path.display()
                    ),
                );
            }
            let mut args = vec![];
            if follow {
                args.push("-f".to_string());
            }
            args.push(path.to_string_lossy().into_owned());
            stream_command("tail", &args, context)
        }
        LogSource::Command { program, mut args } => {
            if follow {
                args.push("--follow".to_string());
            }
            stream_command(&program, &args, context)
        }
    }
}

/// 子プロセスの出力をそのまま流す (= ログは hyoui が整形しない)。
fn stream_command(program: &str, args: &[String], context: &str) -> ExitCode {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            // `tail -f` を Ctrl+C で抜けた場合もここに来る (= 失敗ではない)。
            ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
        }
        Err(error) => fail(context, &format!("could not run `{program}`: {error}")),
    }
}

fn service_json(service: &ServiceStatus) -> Value {
    json!({
        "loaded": service.loaded,
        "running": service.running,
        "pid": service.pid,
        "last_exit": service.last_exit,
    })
}

fn emit(value: &Value) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hyoui: could not serialize output: {error}");
            ExitCode::from(1)
        }
    }
}

/// 組み立て済みのエラー JSON を stderr に書いて非 0 で終わる。
fn fail_json(context: &str, body: Value) -> ExitCode {
    match serde_json::to_string_pretty(&body) {
        Ok(text) => eprintln!("{text}"),
        Err(_) => eprintln!("hyoui: {context}: {body}"),
    }
    ExitCode::from(1)
}

fn fail(context: &str, message: &str) -> ExitCode {
    match serde_json::to_string_pretty(&json!({"command": context, "error": message})) {
        Ok(text) => eprintln!("{text}"),
        Err(_) => eprintln!("hyoui: {context}: {message}"),
    }
    ExitCode::from(1)
}

pub fn resolve_program(current_exe: &Path) -> Result<ResolvedProgram, String> {
    let candidate: Candidate = resolve_stable_path(current_exe, ScoringPolicy::SameBinary)
        .map_err(|error| format!("cannot resolve a stable hyoui path: {error}"))?;
    let warning = (!candidate.is_stable()).then(|| {
        format!(
            "hyoui: web service: warning: no durable install path found; baking {} into the service. Install hyoui on PATH before relying on startup persistence.",
            candidate.path().display()
        )
    });
    Ok(ResolvedProgram {
        path: candidate.path().to_path_buf(),
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations(pairs: &[(&str, &str)]) -> hyoui::paths::Env {
        let pairs: Vec<(String, std::ffi::OsString)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), std::ffi::OsString::from(v)))
            .collect();
        hyoui::paths::Env::from_lookup(|name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    fn sample_env() -> BTreeMap<String, String> {
        pinned_env(
            &locations(&[
                ("HOME", "/Users/test"),
                ("XDG_CONFIG_HOME", "/Users/test/.config"),
                ("XDG_STATE_HOME", "/Users/test/.local/state"),
            ]),
            Some(std::ffi::OsStr::new("/opt/homebrew/bin:/usr/bin:/bin")),
        )
    }

    fn sample_definition() -> ServiceDefinition {
        ServiceDefinition::labelled(
            MACOS_LABEL,
            "/opt/homebrew/bin/hyoui",
            Some(
                "/Users/test/.local/state/hyoui/web/logs/jp.kawaz.hyoui-web.supervise.log"
                    .to_string(),
            ),
            sample_env(),
        )
    }

    /// launchd の純関数群は host OS に関係なく compile・実行される契約を固定する。
    ///
    /// live backend は macOS 限定だが、path / escaping / renderer / status parsing は
    /// Linux CI でも同じ入力に同じ結果を返す必要がある (= DR-0031 §4)。
    #[test]
    fn launchd_pure_helpers_run_cross_platform() {
        assert_eq!(
            launchd_definition_path(Path::new("/home/test"), MACOS_LABEL),
            PathBuf::from("/home/test/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist")
        );
        assert_eq!(xml_escape("a&b<c>"), "a&amp;b&lt;c&gt;");
        let rendered = render_launchd_plist(&sample_definition());
        assert!(rendered.contains("<key>RunAtLoad</key>"));
        assert!(rendered.contains("<key>KeepAlive</key>"));
        assert_eq!(
            parse_launchctl_pid("state = running\npid = 4242\n"),
            Some(4242)
        );
    }

    /// label の値は組み立てても変わらない (= OS に登録済みの契約名を動かさない)。
    #[test]
    fn labels_keep_their_registered_values() {
        assert_eq!(MACOS_LABEL, "jp.kawaz.hyoui-web.supervise");
        assert_eq!(LINUX_LABEL, "hyoui-web-supervise");
    }

    /// OS 名差は label だけで、各 backend の定義 basename と 1:1 に対応する。
    #[test]
    fn labels_and_definition_paths_are_deterministic() {
        assert_eq!(
            launchd_definition_path(Path::new("/Users/test"), MACOS_LABEL),
            PathBuf::from("/Users/test/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist")
        );
        assert_eq!(
            systemd_definition_path(Path::new("/home/test/.config"), LINUX_LABEL),
            PathBuf::from("/home/test/.config/systemd/user/hyoui-web-supervise.service")
        );
        assert_eq!(
            systemd_definition_path(Path::new("/x"), "already.service"),
            PathBuf::from("/x/systemd/user/already.service")
        );
    }

    /// 監督者の argv は 4 語で固定され、unit の数や名前は入らない (= 決定 6)。
    ///
    /// これが「binary 更新をまたいで安定する契約」の実体で、unit を足しても消しても
    /// この定義は変わらないので `register` をやり直す必要がない。
    #[test]
    fn definition_builds_the_supervisor_command() {
        let def = sample_definition();
        assert_eq!(
            def.program_args,
            ["/opt/homebrew/bin/hyoui", "web", "daemon", "supervise"]
        );
        // 既定 label で組んでも同じ argv になる。
        assert_eq!(
            ServiceDefinition::for_supervisor("/opt/homebrew/bin/hyoui", None, BTreeMap::new())
                .program_args,
            def.program_args
        );
    }

    /// 固定する env は場所を導く変数の列挙そのもの + PATH。値の無い変数は書かない
    /// (DR-0038 決定 5)。
    #[test]
    fn pinned_env_is_exactly_the_location_vars_that_are_set_plus_path() {
        let env = pinned_env(
            &locations(&[
                ("HOME", "/h"),
                ("XDG_STATE_HOME", "/s"),
                ("XDG_RUNTIME_DIR", ""),
            ]),
            Some(std::ffi::OsStr::new("/a:/b")),
        );
        assert_eq!(
            env,
            BTreeMap::from([
                ("HOME".to_string(), "/h".to_string()),
                ("XDG_STATE_HOME".to_string(), "/s".to_string()),
                ("PATH".to_string(), "/a:/b".to_string()),
            ])
        );

        // 列挙の全変数が拾われる (= unit 生成側に別のリストが無いことの裏返し)。
        let all: Vec<(&str, &str)> = hyoui::paths::LocationVar::ALL
            .iter()
            .map(|var| (var.name(), "/x"))
            .collect();
        let env = pinned_env(&locations(&all), None);
        for var in hyoui::paths::LocationVar::ALL {
            assert_eq!(
                env.get(var.name()).map(String::as_str),
                Some("/x"),
                "{var:?}"
            );
        }
        assert!(!env.contains_key("PATH"));
    }

    /// PATH は重複と空要素を落とし、最初に現れた位置を保つ (= 積もらない)。
    #[test]
    fn path_is_normalized_without_duplicates() {
        assert_eq!(
            normalize_path_var("/a:/b::/a:/c:/b:"),
            "/a:/b:/c".to_string()
        );
        assert_eq!(normalize_path_var(""), "");
        // 正規化は冪等。
        let once = normalize_path_var("/x:/y:/x");
        assert_eq!(normalize_path_var(&once), once);
    }

    /// 書いた env を両 format から読み戻せる (= re-register の差分検知の入力)。
    #[test]
    fn pinned_env_round_trips_through_both_definitions() {
        let mut def = sample_definition();
        def.env
            .insert("XDG_RUNTIME_DIR".into(), "/run/a&b<c>%d\"e".into());
        assert_eq!(env_in_definition(&render_launchd_plist(&def)), def.env);
        assert_eq!(env_in_definition(&render_systemd_unit(&def)), def.env);
        assert!(env_in_definition("nothing here").is_empty());
    }

    /// 場所の変数が違えば変数ごとに差分を返し、PATH の違いは差分にしない。
    #[test]
    fn location_drift_ignores_path_and_reports_each_variable() {
        let registered = sample_env();
        let mut current = registered.clone();
        current.insert("PATH".into(), "/elsewhere".into());
        assert!(pinned_location_drift(&registered, &current).is_empty());

        current.insert("XDG_STATE_HOME".into(), "/other/state".into());
        current.remove("XDG_CONFIG_HOME");
        current.insert("XDG_RUNTIME_DIR".into(), "/run/user/1".into());
        let drift = pinned_location_drift(&registered, &current);
        assert_eq!(
            drift,
            vec![
                LocationDifference {
                    name: "XDG_CONFIG_HOME".into(),
                    registered: Some("/Users/test/.config".into()),
                    current: None,
                },
                LocationDifference {
                    name: "XDG_STATE_HOME".into(),
                    registered: Some("/Users/test/.local/state".into()),
                    current: Some("/other/state".into()),
                },
                LocationDifference {
                    name: "XDG_RUNTIME_DIR".into(),
                    registered: None,
                    current: Some("/run/user/1".into()),
                },
            ]
        );
    }

    /// 監督者のログは web の状態の置き場の `logs/` に label の名前で置く。
    #[test]
    fn the_supervisor_log_lives_with_the_unit_logs() {
        assert_eq!(
            log_path_for(&locations(&[("HOME", "/h")]), MACOS_LABEL),
            "/h/.local/state/hyoui/web/logs/jp.kawaz.hyoui-web.supervise.log"
        );
        assert_eq!(
            log_path_in_definition(&render_launchd_plist(&sample_definition())).as_deref(),
            Some("/Users/test/.local/state/hyoui/web/logs/jp.kawaz.hyoui-web.supervise.log")
        );
    }

    /// 定義を一時 dir に置き、OS には一切触らない backend (= register の判断だけを見る)。
    struct FakeBackend {
        dir: tempfile::TempDir,
        installed: std::cell::RefCell<Vec<ServiceDefinition>>,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                installed: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl Backend for FakeBackend {
        fn definition_path(&self, label: &str) -> Result<PathBuf, String> {
            Ok(self.dir.path().join(format!("{label}.def")))
        }
        fn install(&self, def: &ServiceDefinition) -> Result<(), String> {
            std::fs::write(self.definition_path(&def.label)?, render_definition(def))
                .map_err(|error| error.to_string())?;
            self.installed.borrow_mut().push(def.clone());
            Ok(())
        }
        fn unregister(&self, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn start(&self, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn stop(&self, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn restart(&self, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn status(&self, label: &str) -> Result<ServiceStatus, String> {
            let path = self.definition_path(label)?;
            Ok(ServiceStatus {
                registered: path.is_file(),
                loaded: path.is_file(),
                running: false,
                pid: None,
                last_exit: None,
                definition_path: path,
            })
        }
        fn log_source(&self, _: &str) -> Result<LogSource, String> {
            unreachable!()
        }
    }

    fn definition_with(env: &[(&str, &str)]) -> ServiceDefinition {
        ServiceDefinition::labelled(
            "jp.kawaz.hyoui-web.test",
            "/opt/homebrew/bin/hyoui",
            None,
            env.iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }

    /// 初回は固定して載せる。同じ場所での再登録は通り、場所が違えば差分を返して
    /// 何も書き換えない。`--force` で置き換える (DR-0038 決定 5)。
    #[test]
    fn re_register_stops_on_location_drift_unless_forced() {
        let backend = FakeBackend::new();
        let first = definition_with(&[("HOME", "/h"), ("XDG_STATE_HOME", "/s"), ("PATH", "/a")]);
        assert!(register_checked(&backend, &first, false).unwrap());
        assert_eq!(backend.installed.borrow().len(), 1);

        // 同じ場所なら PATH が違っても止まらない (= PATH は場所を導かない)。
        let same_place =
            definition_with(&[("HOME", "/h"), ("XDG_STATE_HOME", "/s"), ("PATH", "/b")]);
        assert!(register_checked(&backend, &same_place, false).unwrap());
        assert_eq!(backend.installed.borrow().len(), 2);

        // 場所が違えば止まり、定義は前のまま。
        let moved = definition_with(&[("HOME", "/h"), ("XDG_STATE_HOME", "/elsewhere")]);
        match register_checked(&backend, &moved, false) {
            Err(RegisterError::Drift { differences, .. }) => assert_eq!(
                differences,
                vec![LocationDifference {
                    name: "XDG_STATE_HOME".into(),
                    registered: Some("/s".into()),
                    current: Some("/elsewhere".into()),
                }]
            ),
            other => panic!("expected drift: {other:?}"),
        }
        assert_eq!(backend.installed.borrow().len(), 2);
        let on_disk =
            std::fs::read_to_string(backend.definition_path("jp.kawaz.hyoui-web.test").unwrap())
                .unwrap();
        assert_eq!(
            env_in_definition(&on_disk)
                .get("XDG_STATE_HOME")
                .map(String::as_str),
            Some("/s")
        );

        // 押し切れば置き換わる。
        assert!(register_checked(&backend, &moved, true).unwrap());
        assert_eq!(backend.installed.borrow().len(), 3);
        assert_eq!(backend.installed.borrow()[2].env, moved.env);
    }

    /// 固定値を持たない定義 (= env の固定を入れる前に書かれたもの) も、場所が「無い」と
    /// 固定されていたものとして比べる (= 黙って上書きしない)。
    #[test]
    fn a_definition_without_pinned_locations_also_needs_force() {
        let backend = FakeBackend::new();
        let legacy = definition_with(&[("PATH", "/usr/bin")]);
        assert!(register_checked(&backend, &legacy, false).unwrap());
        let pinned = definition_with(&[("HOME", "/h"), ("PATH", "/usr/bin")]);
        assert!(matches!(
            register_checked(&backend, &pinned, false),
            Err(RegisterError::Drift { .. })
        ));
        assert!(register_checked(&backend, &pinned, true).unwrap());
    }

    /// launchd の golden は RunAtLoad + KeepAlive、共通 log、固定した env を固定する。
    #[test]
    fn launchd_plist_golden() {
        let mut def = sample_definition();
        def.label = MACOS_LABEL.to_string();
        assert_eq!(
            render_launchd_plist(&def),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\
\t<string>jp.kawaz.hyoui-web.supervise</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>/opt/homebrew/bin/hyoui</string>\n\
\t\t<string>web</string>\n\
\t\t<string>daemon</string>\n\
\t\t<string>supervise</string>\n\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
\t<key>KeepAlive</key>\n\
\t<true/>\n\
\t<key>EnvironmentVariables</key>\n\
\t<dict>\n\
\t\t<key>HOME</key>\n\
\t\t<string>/Users/test</string>\n\
\t\t<key>PATH</key>\n\
\t\t<string>/opt/homebrew/bin:/usr/bin:/bin</string>\n\
\t\t<key>XDG_CONFIG_HOME</key>\n\
\t\t<string>/Users/test/.config</string>\n\
\t\t<key>XDG_STATE_HOME</key>\n\
\t\t<string>/Users/test/.local/state</string>\n\
\t</dict>\n\
\t<key>StandardOutPath</key>\n\
\t<string>/Users/test/.local/state/hyoui/web/logs/jp.kawaz.hyoui-web.supervise.log</string>\n\
\t<key>StandardErrorPath</key>\n\
\t<string>/Users/test/.local/state/hyoui/web/logs/jp.kawaz.hyoui-web.supervise.log</string>\n\
</dict>\n\
</plist>\n"
        );
    }

    /// systemd の golden は Restart=always + default.target + journald を固定する。
    #[test]
    fn systemd_unit_golden() {
        let mut def = sample_definition();
        def.label = LINUX_LABEL.to_string();
        assert_eq!(
            render_systemd_unit(&def),
            "[Unit]\n\
Description=hyoui HTTP gateway\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart=\"/opt/homebrew/bin/hyoui\" \"web\" \"daemon\" \"supervise\"\n\
Restart=always\n\
Environment=\"HOME=/Users/test\"\n\
Environment=\"PATH=/opt/homebrew/bin:/usr/bin:/bin\"\n\
Environment=\"XDG_CONFIG_HOME=/Users/test/.config\"\n\
Environment=\"XDG_STATE_HOME=/Users/test/.local/state\"\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
        assert!(!render_systemd_unit(&def).contains("StandardOutput="));
    }

    /// 各 format の補間値は document boundary を越えないよう escape する。
    #[test]
    fn renderers_escape_interpolated_values() {
        let def =
            ServiceDefinition::labelled("x&y<z>", "/tmp/a&b<hyoui> %z", None, BTreeMap::new());
        let plist = render_launchd_plist(&def);
        assert!(plist.contains("x&amp;y&lt;z&gt;"));
        assert!(plist.contains("/tmp/a&amp;b&lt;hyoui&gt; %z"));
        // systemd は `%` を specifier として読むので二重化し、値は引用符で囲む。
        let unit = render_systemd_unit(&def);
        assert!(unit.contains("\"/tmp/a&b<hyoui> %%z\""), "{unit}");
        assert_eq!(systemd_quote("a\nb\"c\\d"), "\"a\\nb\\\"c\\\\d\"");
    }

    /// launchctl print の numeric pid だけが running の根拠になる。
    #[test]
    fn launchctl_pid_parser_requires_numeric_pid() {
        assert_eq!(
            parse_launchctl_pid("state = running\npid = 4242\n"),
            Some(4242)
        );
        assert_eq!(parse_launchctl_pid("state = waiting\n"), None);
        assert_eq!(parse_launchctl_pid("pid = nope\n"), None);
    }

    /// OS 側の情報は JSON の `service` 入れ物にそのまま並ぶ (= 決定 6)。
    ///
    /// `registered` (ファイルが有る) と `loaded` (OS に載っている) を別に持つのは、
    /// ファイルが有るのに載っていない状態を見せるため。
    #[test]
    fn os_state_is_reported_without_collapsing_registered_and_loaded() {
        let loaded = ServiceStatus {
            registered: true,
            loaded: true,
            running: true,
            pid: Some(42),
            last_exit: Some(0),
            definition_path: PathBuf::from("/x/service"),
        };
        assert_eq!(
            service_json(&loaded),
            serde_json::json!({"loaded": true, "running": true, "pid": 42, "last_exit": 0})
        );

        // 定義は有るが OS に載っていない (= register 後に bootout された状態)。
        let unloaded = ServiceStatus {
            registered: true,
            loaded: false,
            running: false,
            pid: None,
            last_exit: None,
            definition_path: PathBuf::from("/x/service"),
        };
        assert_eq!(
            service_json(&unloaded),
            serde_json::json!({"loaded": false, "running": false, "pid": null, "last_exit": null})
        );
        assert!(unloaded.registered);
    }

    /// `launchctl print` の最後の終了状態を読む。
    #[test]
    fn last_exit_is_parsed_when_launchd_reports_it() {
        assert_eq!(
            parse_launchctl_last_exit("state = waiting\nlast exit code = 2\n"),
            Some(2)
        );
        assert_eq!(parse_launchctl_last_exit("state = running\n"), None);
    }

    /// 定義に焼かれた実行ファイルを読み返せる (= 監督者が止まっていても分かる)。
    ///
    /// 読むのは先頭 1 語だけで、unit の属性を定義から復元する経路は持たない。
    #[test]
    fn the_program_can_be_read_back_from_either_definition() {
        let def = ServiceDefinition::labelled(
            "jp.kawaz.hyoui-web.supervise",
            "/opt/homebrew/bin/hyoui",
            None,
            sample_env(),
        );
        assert_eq!(
            program_in_definition(&render_launchd_plist(&def)).as_deref(),
            Some("/opt/homebrew/bin/hyoui")
        );
        assert_eq!(
            program_in_definition(&render_systemd_unit(&def)).as_deref(),
            Some("/opt/homebrew/bin/hyoui")
        );
        // 定義でないテキストからは読み取らない。
        assert_eq!(program_in_definition("nothing here"), None);
    }
}
