//! `hyoui web daemon` の各 verb (= DR-0034 P2 / P3)。
//!
//! unit の属性は [`registry`] が正本で、プロセスの生死は [`supervisor`] が持つ。
//! `start` / `stop` / `restart` / `status` / `log` は子を直接叩かず監督者へ要求する
//! (決定 4) — 子の所有者が CLI と監督者の 2 つになると、停止・再起動・状態確認の
//! 経路が分岐するため。
//!
//! 出力は help 以外すべて JSON、エラーは JSON を stderr に出して非 0 で終わる
//! (= reference `cli-daemon-subcommands` の出力規約)。

pub mod probe;
pub mod protocol;
pub mod registry;
pub mod supervisor;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hyoui::cli::{WebDaemonAddConfig, WebDaemonRunSource};
use hyoui::config::WebConfig;
use hyoui::paths::Env;
use serde_json::{Value, json};

use protocol::{ErrorBody, Request, Response, Target};
use registry::{ListenConflict, Registry, Unit};

/// 監督者の生死。
///
/// `enabled` (= 登録簿の desired state) と分けて持つのは、停止指示のまま降りて
/// いるのか、上げたいのに上がらないのかを区別するため (決定 4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Supervisor {
    /// 制御 socket に届いた。
    Running,
    /// 制御 socket に届かない (= 未起動、または socket の残骸)。
    NotRunning,
}

impl Supervisor {
    /// 制御 socket へ繋いで監督者の生死を確かめる。
    ///
    /// socket ファイルの存在では判定しない — 監督者が SIGKILL で消えた後に
    /// socket が残ることがあり、それを「居る」と読むと `list` が嘘をつく。
    fn probe() -> Self {
        match std::os::unix::net::UnixStream::connect(registry::supervisor_socket_path()) {
            Ok(_) => Self::Running,
            Err(_) => Self::NotRunning,
        }
    }

    fn is_running(self) -> bool {
        self == Self::Running
    }

    /// 監督者に unit の起動 / 停止を要求する。
    ///
    /// 監督者が不在でも登録簿の変更は成立するので、送れなかった理由を返して
    /// 呼び出し側が出力に添える (決定 4 の表: 次に監督者が上がった時に起きる)。
    fn request(self, request: &Request) -> std::result::Result<Response, ErrorBody> {
        protocol::request(&registry::supervisor_socket_path(), request)
            .map(|(response, _)| response)
    }
}

/// `hyoui web daemon list`。
///
/// 登録簿を読むだけで答えられるので、監督者が停止していても断らない (決定 4)。
pub fn list_command() -> ExitCode {
    // 監督者に聞けた時だけ `running` / `pid` を足す。聞けていない台に「動いて
    // いる」とは書かない (決定 4)。
    match Supervisor::probe().request(&Request::List(Target::all())) {
        Ok(Response::Units {
            units, supervisor, ..
        }) => {
            let mut output = json!({
                "units": units
                    .into_iter()
                    .map(|unit| json!({
                        "name": unit.name,
                        "enabled": unit.enabled,
                        "running": unit.running,
                        "pid": unit.pid,
                        "config": unit.config,
                        "listen": unit.listen,
                        "config_error": unit.config_error,
                        "binary_path": unit.binary_path,
                        "binary_exists": unit.binary_exists,
                    }))
                    .collect::<Vec<_>>(),
                "supervisor": {"running": true},
                "registry_dir": Registry::open().dir(),
            });
            add_location_warnings(&mut output, &supervisor.locations);
            emit(&output)
        }
        Ok(Response::Error(error)) => fail_with("web daemon list", &error),
        Ok(_) | Err(_) => {
            let mut output = match registry_view(None) {
                Ok(output) => output,
                Err(code) => return code,
            };
            output["note"] = json!(
                "the supervisor is not running, so `running` and `pid` are not known for any unit"
            );
            add_registered_location_warnings(&mut output);
            emit(&output)
        }
    }
}

/// 監督者が名乗った場所が自分の導出と違えば `warnings` に足す (DR-0038 決定 5)。
fn add_location_warnings(
    output: &mut Value,
    locations: &std::collections::BTreeMap<String, Option<String>>,
) {
    let drift = protocol::location_drift(locations);
    if !drift.is_empty() {
        for warning in &drift {
            eprintln!("hyoui: warning: {warning}");
        }
        output["warnings"] = json!(drift);
    }
}

/// 置き場を移す前の web の dir の名前 (DR-0038 移行節)。
///
/// 新しいバイナリはここを読まない。残っているかを見て警告するためだけに名前を持つ。
const LEGACY_DIR_NAME: &str = "hyoui-web";

/// OS 登録名を変える前の監督者の定義名 (launchd label / systemd unit、DR-0038 移行節)。
///
/// 新しいバイナリはこの名前で登録も操作もしない。定義ファイルが残っていれば、旧い
/// 監督者が新しい監督者と並んで同じ port を取り合いうるので警告する。
const LEGACY_SERVICE_LABELS: [&str; 2] = ["jp.kawaz.hyoui-web.supervise", "hyoui-web-supervise"];

/// 古い置き場が残っていれば stderr に警告する (DR-0038 移行節 (3))。
///
/// `hyoui web ...` の全 verb (= 監督者・子の `daemon run`・`status` 等) の入口で呼ぶ。
/// 監督者と子の stderr は監督者のログに入るので、常駐側の痕跡にもなる。
pub fn warn_legacy_state_dir() {
    for warning in legacy_warnings(&hyoui::paths::Env::current()) {
        eprintln!("hyoui: warning: {warning}");
    }
}

/// 古い置き場と、それぞれの移し先。
fn legacy_places(env: &hyoui::paths::Env) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
    let mut places = Vec::new();
    if let Some(state_home) = env.state_home() {
        places.push((state_home.join(LEGACY_DIR_NAME), env.web_state_dir()));
    }
    // 監督者のログは state の `logs/` に移った (DR-0038 決定 4)。
    if let Some(home) = env.home() {
        places.push((
            home.join("Library/Logs").join(LEGACY_DIR_NAME),
            env.web_state_dir().join("logs"),
        ));
    }
    places
}

/// 残っている古い OS 定義 (= 旧 label の plist / systemd unit) の path。
fn legacy_service_definitions(env: &hyoui::paths::Env) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for label in LEGACY_SERVICE_LABELS {
        if let Some(home) = env.home() {
            paths.push(crate::web_service::launchd_definition_path(home, label));
        }
        if let Some(config_home) = env.config_home() {
            paths.push(crate::web_service::systemd_definition_path(
                &config_home,
                label,
            ));
        }
    }
    paths
}

/// 残っている古い置き場ごとの警告文。symlink なら「後で消す」、実体なら「移していない」。
/// どちらも中身は読まない (= 新しい置き場と二重に拾わない)。旧 label の OS 定義が
/// 残っていれば、外す手順を言う。
fn legacy_warnings(env: &hyoui::paths::Env) -> Vec<String> {
    let definitions = legacy_service_definitions(env)
        .into_iter()
        .filter(|path| path.symlink_metadata().is_ok())
        .map(|path| {
            format!(
                "{} is the supervisor definition under an old label; unload and remove it (launchctl bootout / systemctl --user disable --now) so that two supervisors do not fight over the same ports (DR-0038)",
                path.display()
            )
        });
    legacy_places(env)
        .into_iter()
        .filter_map(|(legacy, current)| {
            let meta = legacy.symlink_metadata().ok()?;
            Some(if meta.file_type().is_symlink() {
                format!(
                    "{} is a symlink left for older hyoui binaries; remove it once none of them run (DR-0038)",
                    legacy.display()
                )
            } else {
                format!(
                    "{} is not read by this hyoui; move it to {} and leave a symlink in its place (DR-0038)",
                    legacy.display(),
                    current.display()
                )
            })
        })
        .chain(definitions)
        .collect()
}

/// 監督者に届かない時、OS の定義に固定された場所が今の shell と違えば
/// `warnings` に足す (DR-0038 決定 5)。場所が食い違えば socket の位置も食い違うので、
/// 「届かない」の理由がそれである可能性を言う。
fn add_registered_location_warnings(output: &mut Value) {
    let warnings = crate::web_service::registered_location_warnings();
    if !warnings.is_empty() {
        for warning in &warnings {
            eprintln!("hyoui: warning: {warning}");
        }
        output["warnings"] = json!(warnings);
    }
}

/// 登録簿から答えられる範囲だけを組み立てる (= 監督者に聞けない時の答え)。
///
/// 障害時に最初に打つコマンドが監督者の生死に依存すると、状態を見る入口ごと
/// 失われる (決定 4)。listen は登録簿に無いので各 unit の config から引く。
fn registry_view(name: Option<&str>) -> std::result::Result<Value, ExitCode> {
    let registry = Registry::open();
    let units = match name {
        Some(name) => match registry.get(name) {
            Ok(unit) => vec![(name.to_owned(), unit)],
            Err(error) => return Err(fail("web daemon status", &error.to_string(), None)),
        },
        None => match registry.list() {
            Ok(units) => units,
            Err(error) => return Err(fail("web daemon status", &error.to_string(), None)),
        },
    };

    Ok(json!({
        "units": units
            .into_iter()
            .map(|(name, unit)| {
                let (listen, config_error) = match unit.load_config() {
                    Ok(config) => (Some(config.listen), None),
                    Err(error) => (None, Some(error.to_string())),
                };
                json!({
                    "name": name,
                    "enabled": unit.enabled,
                    "running": false,
                    "pid": Value::Null,
                    "config": unit.config,
                    "listen": listen,
                    "config_error": config_error,
                    "binary_path": unit.binary_path,
                    "binary_exists": unit.binary_path.exists(),
                    "added_at": unit.added_at,
                })
            })
            .collect::<Vec<_>>(),
        "supervisor": {"running": false},
        "registry_dir": registry.dir(),
    }))
}

/// 断る理由と、エラー JSON に添える field (= `fail` に渡す形)。
#[derive(Debug)]
struct Refusal {
    message: String,
    details: Option<Value>,
}

impl Refusal {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }

    fn with(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn fail(self, context: &str) -> ExitCode {
        fail(context, &self.message, self.details)
    }

    /// details に `key` があるか。
    fn details_have(&self, key: &str) -> bool {
        self.details
            .as_ref()
            .is_some_and(|details| details.get(key).is_some())
    }
}

/// 比べるための正規形 (= realpath)。実体が無ければ字面のまま比べる。
fn real_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// config が読めなかった時の断り方。どのファイルをどう直して打ち直すかを hint に書く。
fn config_unreadable(context: &str, config: &Path, error: &hyoui::config::ConfigError) -> Refusal {
    use hyoui::config::ConfigError;

    let hint = match error {
        ConfigError::NotFound { path } => format!(
            "{} does not exist; write it (or give the right path) and run again",
            path.display()
        ),
        ConfigError::ExtendsNotFound { from, target } => format!(
            "fix or remove `extends` in {}, or create {}, then run again",
            from.display(),
            target.display()
        ),
        ConfigError::BadExtends { path } => format!(
            "write `extends` in {} as a path string (`extends = \"base.toml\"`), then run again",
            path.display()
        ),
        ConfigError::ExtendsCycle { chain } => format!(
            "remove one `extends` so the chain no longer loops back ({}), then run again",
            chain
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" -> ")
        ),
        ConfigError::Read { path, .. } => format!(
            "make {} readable by this user, then run again",
            path.display()
        ),
        ConfigError::Parse { path, .. } => format!(
            "fix the TOML in {} at the position given in the error, then run again",
            path.display()
        ),
        ConfigError::RemovedKey { path, hint, .. } => {
            format!("{hint} ({}), then run again", path.display())
        }
    };
    Refusal::new(format!("{context}: {error}")).with(json!({"config": config, "hint": hint}))
}

/// config の `[web].state_dir` が今の面の状態の root と同じかを確かめる (DR-0038 決定 9)。
///
/// 面同士は互いの登録簿を見られないので、別の面の config を (コピー等で) 登録・起動
/// した事故に気付ける場所は config 自身しかない。両方を realpath で正規化して比べる
/// (= symlink 越しに同じ root を指していれば同じ面)。
///
/// **`state_dir` は unit の config ファイル自身に書かれていることを要求する。**
/// `extends` で土台から継いだ値は認めない: 土台は面をまたいで共有するので、土台に
/// 書くと同じ土台を指す他の面の unit が全部それを継いでしまい、面の食い違いを config
/// で捕まえられなくなる。比べる値は `web` (= `extends` を畳んだ後の値) から取るが、
/// トップのファイルが書いた値は畳んでも土台に上書きされないので同じ値である。
fn check_state_dir(web: &WebConfig, config: &Path, env: &Env) -> Result<(), Refusal> {
    let current = env.state_root();
    let declared = declares_state_dir(config)?;
    let state_dir = match (&web.state_dir, declared) {
        (Some(state_dir), true) => state_dir,
        (inherited, _) => {
            let message = if inherited.is_some() {
                format!(
                    "{} inherits `[web].state_dir` through `extends`; a unit's config must name its state root itself, since a shared base is read from every state root",
                    config.display()
                )
            } else {
                format!(
                    "{} has no `[web].state_dir`; a unit's config names the state root it belongs to",
                    config.display()
                )
            };
            return Err(Refusal::new(message).with(json!({
                "config": config,
                "current_state_dir": current,
                "hint": format!(
                    "if this config belongs to the state root this hyoui runs with, add `state_dir = \"{}\"` under [web] in {}",
                    current.display(),
                    config.display()
                ),
            })));
        }
    };
    if real_path(state_dir) == real_path(&current) {
        return Ok(());
    }
    Err(Refusal::new(format!(
        "{} belongs to the state root {}, but this hyoui runs with the state root {}",
        config.display(),
        state_dir.display(),
        current.display()
    ))
    .with(json!({
        "config": config,
        "state_dir": state_dir,
        "current_state_dir": current,
        "hint": "run it with the environment of the state root it belongs to, or use a config written for this one",
    })))
}

/// `config` ファイル単体 (= `extends` を畳む前) の `[web]` に `state_dir` があるか。
fn declares_state_dir(config: &Path) -> Result<bool, Refusal> {
    let context = "could not read the unit's config";
    let text = std::fs::read_to_string(config).map_err(|source| {
        config_unreadable(
            context,
            config,
            &hyoui::config::ConfigError::Read {
                path: config.to_path_buf(),
                source,
            },
        )
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|source| {
        config_unreadable(
            context,
            config,
            &hyoui::config::ConfigError::Parse {
                path: config.to_path_buf(),
                source,
            },
        )
    })?;
    Ok(table
        .get("web")
        .and_then(toml::Value::as_table)
        .is_some_and(|web| web.contains_key("state_dir")))
}

/// `hyoui web daemon add <name> [--listen ..] [--binary ..] | --config <path>`
/// (DR-0038 決定 2 / 9)。
pub fn add_command(cfg: WebDaemonAddConfig) -> ExitCode {
    match add(cfg, &Env::current(), &Registry::open()) {
        Ok(output) => emit(&output),
        Err(refusal) => refusal.fail("web daemon add"),
    }
}

/// add が登録する config の出どころ。
enum AddSource {
    /// `--config` で明示したファイル。
    Explicit(PathBuf),
    /// 置き場に同名のファイルが既にある (= 生成しない)。
    Existing(PathBuf),
    /// 置き場にまだ無いので書く。
    Generate(PathBuf),
}

/// add が生成する config の listen (= `--listen`、無ければ組み込みの既定)。
fn listen_for_new_config(listen: Option<String>) -> String {
    listen.unwrap_or_else(hyoui::config::default_web_listen)
}

/// この add が生成した config。登録まで済まなければ `Drop` で消す。
///
/// 消すのは自分が生成したファイルだけ。add は登録簿の lock の中で走るので、生成から
/// 登録までの間に他の add がこのファイルを既存ファイルとして登録することは無い。
#[derive(Debug)]
struct GeneratedConfig {
    path: PathBuf,
    keep: bool,
}

impl GeneratedConfig {
    fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for GeneratedConfig {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn add(
    cfg: WebDaemonAddConfig,
    env: &Env,
    registry: &Registry,
) -> std::result::Result<Value, Refusal> {
    let WebDaemonAddConfig {
        name,
        config,
        listen: listen_option,
        binary: binary_option,
    } = cfg;
    registry::validate_name(&name).map_err(|error| Refusal::new(error.to_string()))?;

    // 名前と listen の検査、config の生成、登録簿への書き込みを 1 つの lock の中で行う
    // (DR-0038 決定 9)。lock の外で検査すると、並行する add 同士が同じ名前・同じ
    // ポートを通し合い、片方の後始末がもう片方の参照する config を消しうる。
    let lock = registry.try_lock().map_err(|error| match &error {
        registry::Error::Busy { path } => {
            Refusal::new(error.to_string()).with(json!({"kind": "registry_busy", "lock": path}))
        }
        _ => Refusal::new(error.to_string()),
    })?;
    match registry.get(&name) {
        Ok(_) => {
            return Err(Refusal::new(
                registry::Error::AlreadyRegistered { name }.to_string(),
            ));
        }
        Err(registry::Error::UnknownUnit { .. }) => {}
        Err(error) => return Err(Refusal::new(error.to_string())),
    }

    let source = match config {
        Some(path) => AddSource::Explicit(absolute(&path).map_err(Refusal::new)?),
        None => {
            let dir = env.web_config_dir().ok_or_else(|| {
                Refusal::new(
                    "cannot find the web config directory: neither XDG_CONFIG_HOME nor HOME is set",
                )
            })?;
            let path = dir.join(format!("{name}.toml"));
            if path.symlink_metadata().is_ok() {
                AddSource::Existing(path)
            } else {
                AddSource::Generate(path)
            }
        }
    };

    let (config_path, listen, binary_path, generated) = match &source {
        AddSource::Explicit(path) | AddSource::Existing(path) => {
            if listen_option.is_some() || binary_option.is_some() {
                // 黙って既存の値を使うと、`--listen` を書いた意図が config の値に倒れる。
                return Err(Refusal::new(format!(
                    "{} already exists, so --listen and --binary would not be written; edit that file, or add without them",
                    path.display()
                ))
                .with(json!({"config": path})));
            }
            // 登録する時点で読めることを確かめる。読めない config を登録すると、監督者が
            // 起こすたびに子が config で落ち、backoff の理由を探すことになる。
            let web = hyoui::config::load_web(path)
                .map(|file| file.web)
                .map_err(|error| {
                    config_unreadable("could not read the unit's config", path, &error)
                })?;
            check_state_dir(&web, path, env).map_err(|refusal| {
                // 置き場の同名ファイルが別の面のものなら、その面が同じ名前を使っている。
                if matches!(source, AddSource::Existing(_)) && refusal.details_have("state_dir") {
                    another_state_root_uses_the_name(refusal, &name)
                } else {
                    refusal
                }
            })?;
            // binary は config の `binary_path` が正、無ければ登録した時点の自分自身
            // (llm-gateway DR-0028 決定 2 と同じ)。
            let binary = web
                .binary_path
                .clone()
                .map_or_else(default_binary, Ok)
                .map_err(Refusal::new)?;
            (path.clone(), web.listen, binary, false)
        }
        AddSource::Generate(path) => {
            let listen = listen_for_new_config(listen_option);
            let binary = match binary_option {
                Some(binary) => absolute(&binary).map_err(Refusal::new)?,
                None => default_binary().map_err(Refusal::new)?,
            };
            (path.clone(), listen, binary, true)
        }
    };

    let existing = registry
        .list()
        .map_err(|error| Refusal::new(error.to_string()))?;
    let mut warnings = Vec::new();
    // 既存 unit の listen は各 config から引く。読めない unit は比べられないので、
    // 止めずに warning にする (= 読めない config が 1 つあるだけで add を塞がない)。
    let mut listens = Vec::new();
    for (other, unit) in &existing {
        if unit.config == config_path {
            warnings.push(format!(
                "unit `{other}` already reads {}; two units with one config bind the same address",
                config_path.display()
            ));
        }
        match unit.load_config() {
            Ok(config) => listens.push((other.clone(), config.listen)),
            Err(error) => warnings.push(format!(
                "could not compare with unit `{other}` because its config is unreadable: {error}"
            )),
        }
    }
    let choose_another = if generated {
        "choose another address with --listen".to_string()
    } else {
        format!("change `listen` in {}", config_path.display())
    };
    match registry::find_listen_conflict(&listens, &listen) {
        Some(ListenConflict::Same {
            name: other,
            listen: other_listen,
        }) => {
            return Err(Refusal::new(format!(
                "`{other_listen}` is already used by unit `{other}`; {choose_another}"
            ))
            .with(json!({"listen": listen, "conflicting_unit": other})));
        }
        Some(ListenConflict::Overlapping {
            name: other,
            listen: other_listen,
            reason,
        }) => warnings.push(format!(
            "`{listen}` may overlap with unit `{other}` (`{other_listen}`): {reason}"
        )),
        None => {}
    }
    // 登録簿に無いプロセス (別の面の gateway、手で起こした run、他のアプリ) が
    // 掴んでいるポートも、実際に bind を試して確かめる。空いているポートを自動で
    // 選ばない (決定 9)。
    match check_listen_free(&listen) {
        ListenCheck::Free => {}
        ListenCheck::InUse => {
            return Err(Refusal::new(format!(
                "`{listen}` is in use by another process; {choose_another}"
            ))
            .with(json!({"listen": listen})));
        }
        ListenCheck::Unknown(reason) => warnings.push(format!(
            "could not check whether `{listen}` is free: {reason}"
        )),
    }

    let binary_exists = binary_path.exists();
    if !binary_exists {
        // ビルド前に登録する順序を禁じない (DR-0034 決定 2)。
        warnings.push(format!(
            "`{}` does not exist yet; this unit cannot start until it is built",
            binary_path.display()
        ));
    }

    let state_dir = env.state_root();
    let written = if generated {
        let written = write_unit_config(&config_path, &state_dir, &listen, &binary_path)?;
        // 生成した config も、登録する前に `extends` を含めて読めることを確かめる
        // (決定 2)。土台が壊れていると、add は通って run が落ちる unit になる。
        // 読めなければ `written` の Drop が生成したファイルを消す。
        hyoui::config::load_web(&config_path).map_err(|error| {
            config_unreadable(
                "the generated config cannot be read, so the unit was not added",
                &config_path,
                &error,
            )
        })?;
        Some(written)
    } else {
        None
    };

    let unit = Unit {
        config: config_path.clone(),
        binary_path,
        // `add` は「この gateway を動かしたい」という意思表示なので enabled で入る
        // (DR-0034 決定 4)。
        enabled: true,
        added_at: registry::now_iso8601(),
    };
    registry
        .add_locked(&lock, &name, &unit)
        .map_err(|error| Refusal::new(error.to_string()))?;
    if let Some(written) = written {
        written.keep();
    }
    // 監督者への通知は lock の外で行う (= 監督者が登録簿を書くのを待たせない)。
    drop(lock);

    // 走行中の監督者には即反映する (DR-0034 決定 4)。送るのは `reload` で、監督者が
    // 登録簿を読み直して望みとの差を埋める — 足したばかりの unit は監督者がまだ名前を
    // 知らないので、`start <name>` を送っても「そんな unit は無い」になる。
    let supervisor = Supervisor::probe();
    let started = supervisor.request(&Request::Reload);
    let mut output = json!({
        "name": name,
        "config": unit.config,
        "generated": generated,
        "state_dir": state_dir,
        "listen": listen,
        "binary_path": unit.binary_path,
        "binary_exists": binary_exists,
        "enabled": unit.enabled,
        "added_at": unit.added_at,
        "supervisor": {"running": supervisor.is_running(), "notified": started.is_ok()},
    });
    if let Err(error) = &started {
        output["note"] = json!(format!(
            "{}: `{name}` was recorded in the registry and will start when the supervisor next runs",
            error.message
        ));
    }
    if !warnings.is_empty() {
        output["warnings"] = json!(warnings);
    }
    Ok(output)
}

/// 置き場の同名ファイルが別の面の config だった時の言い方 (= 黙って上書きしない)。
fn another_state_root_uses_the_name(mut refusal: Refusal, name: &str) -> Refusal {
    if let Some(Value::Object(details)) = &mut refusal.details {
        details.insert(
            "hint".into(),
            json!(format!(
                "another state root uses the unit name `{name}`; choose another name, or register a config of your own with --config"
            )),
        );
    }
    refusal
}

/// 実際に bind を試した結果。
enum ListenCheck {
    /// bind できた (= すぐ閉じた)。
    Free,
    /// 他のプロセスが listen している。
    InUse,
    /// 確かめられなかった (= 解決できない、権限が無い等)。
    Unknown(String),
}

/// `listen` を今ほかのプロセスが掴んでいないかを、bind を試して確かめる。
///
/// port 0 (= kernel に任せる) は取り合いにならないので確かめない。
fn check_listen_free(listen: &str) -> ListenCheck {
    if registry::listen_is_ephemeral(listen) {
        return ListenCheck::Free;
    }
    match std::net::TcpListener::bind(listen) {
        Ok(_) => ListenCheck::Free,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => ListenCheck::InUse,
        Err(error) => ListenCheck::Unknown(error.to_string()),
    }
}

/// add が生成する unit の config の中身 (DR-0038 決定 9)。
///
/// 土台 (`base.toml`) が隣にあれば `extends` で重ねる。`state_dir` は面ごとの値なので
/// 土台ではなくこのファイルに書く。
fn unit_config_text(
    path: &Path,
    state_dir: &Path,
    listen: &str,
    binary_path: &Path,
) -> std::result::Result<String, Refusal> {
    if !state_dir.is_absolute() {
        return Err(Refusal::new(
            "cannot determine the state root: neither XDG_STATE_HOME nor HOME is set",
        ));
    }
    let quote = |value: &str| toml::Value::String(value.to_owned()).to_string();
    let quote_path = |value: &Path| {
        value
            .to_str()
            .map(quote)
            .ok_or_else(|| Refusal::new(format!("{} is not valid UTF-8", value.display())))
    };
    let mut text = String::new();
    if path.with_file_name("base.toml").is_file() {
        text.push_str("extends = \"base.toml\"\n\n");
    }
    text.push_str("[web]\n");
    text.push_str(&format!("state_dir = {}\n", quote_path(state_dir)?));
    text.push_str(&format!("listen = {}\n", quote(listen)));
    text.push_str(&format!("binary_path = {}\n", quote_path(binary_path)?));
    Ok(text)
}

/// add が生成する unit の config を書く。既にあるファイルは上書きしない。
///
/// 隣の一時ファイルに書き切ってから、同名が無い時だけその名前で公開する
/// (`persist_noclobber`)。書き込み途中で失敗しても `<unit>.toml` は現れず、一時
/// ファイルは `Drop` で消える (= 次の add が半端なファイルを既存として読まない)。
/// 返した [`GeneratedConfig`] は、登録まで済まなければ公開したファイルを消す。
fn write_unit_config(
    path: &Path,
    state_dir: &Path,
    listen: &str,
    binary_path: &Path,
) -> std::result::Result<GeneratedConfig, Refusal> {
    use std::io::Write as _;

    let text = unit_config_text(path, state_dir, listen, binary_path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let cannot_write = |error: &dyn std::fmt::Display| {
        Refusal::new(format!("could not write {}: {error}", path.display())).with(json!({
            "config": path,
            "hint": format!(
                "make {} writable by this user (or move the file that is in the way), then run again; nothing was registered",
                dir.display()
            ),
        }))
    };
    std::fs::create_dir_all(dir).map_err(|error| cannot_write(&error))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".hyoui-add-")
        .suffix(".toml.tmp")
        .tempfile_in(dir)
        .map_err(|error| cannot_write(&error))?;
    temporary
        .write_all(text.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| cannot_write(&error))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| cannot_write(&error.error))?;
    Ok(GeneratedConfig {
        path: path.to_path_buf(),
        keep: false,
    })
}

/// 相対 path を cwd から解いた絶対 path にする (= 登録簿はどこから読まれても同じ
/// ファイルを指す)。symlink は解かない — 利用者が置いた symlink の向き先を差し
/// 替えれば unit も追従する方が、置き場を決める利用者の意図に沿う。
fn absolute(path: &Path) -> std::result::Result<PathBuf, String> {
    let expanded = hyoui::paths::Env::current().expand_tilde(path);
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(expanded))
        .map_err(|error| format!("cannot resolve the current directory: {error}"))
}

/// `hyoui web daemon remove <name>`。
///
/// 監督者が走っているなら先に停止を要求する — 登録簿から消えた子を監督者が
/// 抱えたままになるのを避けるため (決定 4)。
pub fn remove_command(name: &str) -> ExitCode {
    let context = "web daemon remove";
    let registry = Registry::open();
    if let Err(error) = registry.get(name) {
        return fail(context, &error.to_string(), None);
    }

    let supervisor = Supervisor::probe();
    // 監督者がその名前を知らない (= まだ読み直していない) 場合の応答も `Ok` で
    // 返る。抱えていない unit を止める相手は居ないので、そのまま先へ進んでよい。
    // `Err` になるのは監督者に届かなかった時だけ。
    let stopped = supervisor.request(&Request::Stop(Target::named(name.to_owned())));
    if supervisor.is_running()
        && let Err(error) = &stopped
    {
        // 停止を頼めないまま登録簿から消すと、監督者が抱えている子の所在が
        // 登録簿から読めなくなる。消す前に止まる。
        return fail(
            context,
            &error.message,
            Some(json!({"name": name, "supervisor": {"running": true, "notified": false}})),
        );
    }

    if let Err(error) = registry.remove(name) {
        return fail(context, &error.to_string(), None);
    }
    // 子が降りて登録も消えたことを監督者に読み直させる (= 抱えたままにしない)。
    if stopped.is_ok() {
        let _ = supervisor.request(&Request::Reload);
    }

    let mut output = json!({
        "name": name,
        "removed": true,
        "supervisor": {"running": supervisor.is_running(), "notified": stopped.is_ok()},
    });
    if let Err(error) = &stopped {
        output["note"] = json!(error.message);
    }
    emit(&output)
}

/// `hyoui web daemon supervise` — foreground の監督者 (= 決定 3)。
///
/// OS の service manager に載るのはこれ 1 つで、unit ごとの plist は作らない
/// (決定 6)。
pub fn supervise_command() -> ExitCode {
    let context = "web daemon supervise";
    let root = registry::default_root();
    let mut supervisor = supervisor::Supervisor::new(
        Registry::open(),
        root,
        supervisor::Timings::default(),
        Box::new(probe::SystemProbe),
    );
    match supervisor.run(&registry::supervisor_socket_path()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(context, &error.to_string(), None),
    }
}

/// `start` / `stop` / `restart` / `status` を監督者へ要求する。
///
/// `status` だけは監督者不在でも登録簿由来の列を返す (決定 4) — 障害時に最初に
/// 打つコマンドが監督者の生死に依存すると、状態を見る入口が無くなる。
pub fn control_command(verb: ControlVerb, name: Option<&str>) -> ExitCode {
    let context = verb.context();
    let target = name.map_or_else(Target::all, |name| Target::named(name.to_owned()));
    let request = match verb {
        ControlVerb::Start => Request::Start(target),
        ControlVerb::Stop => Request::Stop(target),
        ControlVerb::Restart => Request::Restart(target),
        ControlVerb::Status => Request::Status(target),
    };

    match Supervisor::probe().request(&request) {
        Ok(Response::Error(error)) => fail_with(context, &error),
        Ok(response) => {
            let locations = match &response {
                Response::Units { supervisor, .. } => supervisor.locations.clone(),
                _ => Default::default(),
            };
            let mut output = json!(response);
            add_location_warnings(&mut output, &locations);
            emit(&output)
        }
        Err(error) if verb == ControlVerb::Status => {
            // 監督者に聞けないので、登録簿から答えられる範囲を返す。
            let mut output = match registry_view(name) {
                Ok(output) => output,
                Err(code) => return code,
            };
            output["note"] = json!(error.message);
            add_registered_location_warnings(&mut output);
            emit(&output)
        }
        Err(error) => fail_with(context, &error),
    }
}

/// 監督者への要求を伴う verb。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlVerb {
    /// 起こす。
    Start,
    /// 止める。
    Stop,
    /// 入れ替える。
    Restart,
    /// 状態を聞く。
    Status,
}

impl ControlVerb {
    fn context(self) -> &'static str {
        match self {
            Self::Start => "web daemon start",
            Self::Stop => "web daemon stop",
            Self::Restart => "web daemon restart",
            Self::Status => "web daemon status",
        }
    }
}

/// `hyoui web daemon log [name] [--follow]`。
pub fn log_command(name: Option<&str>, follow: bool) -> ExitCode {
    let context = "web daemon log";
    let target = name.map_or_else(Target::all, |name| Target::named(name.to_owned()));
    let request = Request::Log { target, follow };

    let (response, mut reader) =
        match protocol::request(&registry::supervisor_socket_path(), &request) {
            Ok(pair) => pair,
            Err(error) => return fail_with(context, &error),
        };
    match response {
        Response::Error(error) => return fail_with(context, &error),
        Response::Log { lines, follow } => {
            for line in lines {
                if emit_jsonl(&line).is_err() {
                    return ExitCode::SUCCESS;
                }
            }
            if follow {
                // 監督者が書いた行をそのまま流す。書かれた先を読み直さない (決定 9)。
                while let Ok(Some(line)) = protocol::read_line::<protocol::LogLine>(&mut reader) {
                    if emit_jsonl(&line).is_err() {
                        break;
                    }
                }
            }
        }
        other => return fail(context, &format!("unexpected reply: {other:?}"), None),
    }
    ExitCode::SUCCESS
}

/// `hyoui version` — CLI 自身・監督者・全 unit の版を 1 回で並べる (= 決定 7a)。
///
/// `hyoui --version` (テキスト 1 行) はこの CLI 自身の版を言う口として残る。
pub fn version_command() -> ExitCode {
    use probe::VersionProbe;

    let cli = hyoui::version::VersionInfo::current();
    let system = probe::SystemProbe;

    let mut output = json!({"cli": cli});
    // 監督者の版は OS 側の定義に焼かれた binary を正として組む (= 決定 7a)。登録が
    // 無ければ `supervisor` は `null` — 対象自体が存在しない。監督者が止まっていても
    // 定義は読めるので、`on_disk` (= 次に上がる版) だけは答えられる。
    let registered = crate::web_service::supervisor_version();
    match Supervisor::probe().request(&Request::Status(Target::all())) {
        Ok(Response::Units {
            units, supervisor, ..
        }) => {
            let binary = registered
                .as_ref()
                .map(|(_, binary)| binary.clone())
                .unwrap_or(supervisor.binary_path);
            let on_disk = system.on_disk(&binary);
            let pair = protocol::VersionPair::new(Some(supervisor.version), on_disk);
            output["supervisor"] = json!({
                "running": pair.running,
                "on_disk": pair.on_disk,
                "binary_path": binary,
                "restart_needed": pair.restart_needed,
            });
            output["units"] = json!(
                units
                    .into_iter()
                    .map(|unit| json!({
                        "name": unit.name,
                        "running": unit.version.running,
                        "on_disk": unit.version.on_disk,
                        "binary_path": unit.binary_path,
                        "restart_needed": unit.version.restart_needed,
                    }))
                    .collect::<Vec<_>>()
            );
        }
        _ => {
            // 監督者に聞けないので、走っている版は誰にも言えない。登録簿と OS 側の
            // 定義から「次に上がる版」だけを並べる。
            output["supervisor"] = match &registered {
                Some((pair, binary)) => json!({
                    "running": Value::Null,
                    "on_disk": pair.on_disk,
                    "binary_path": binary,
                    "restart_needed": false,
                }),
                // OS への登録が無ければ「監督者」という対象自体が無い。
                None => Value::Null,
            };
            let units = match Registry::open().list() {
                Ok(units) => units,
                Err(error) => return fail("version", &error.to_string(), None),
            };
            output["units"] = json!(
                units
                    .into_iter()
                    .map(|(name, unit)| {
                        let on_disk = system.on_disk(&unit.binary_path);
                        json!({
                            "name": name,
                            "running": Value::Null,
                            "on_disk": on_disk,
                            "binary_path": unit.binary_path,
                            "restart_needed": false,
                        })
                    })
                    .collect::<Vec<_>>()
            );
            output["note"] = json!(ErrorBody::supervisor_not_running().message);
        }
    }
    emit(&output)
}

/// `hyoui web daemon run <unit> | --config <path> | --no-config` (DR-0038 決定 9)。
///
/// `<unit>` は登録簿が指す config で起動する。監督者が子を exec するのと同じ
/// 経路で、手元で 1 台だけ確かめる時にも使う (DR-0034 決定 3)。
pub fn run_command(source: &WebDaemonRunSource) -> ExitCode {
    let context = "web daemon run";
    let web = match run_target(&Registry::open(), source, &Env::current()) {
        Ok(web) => web,
        Err(refusal) => return refusal.fail(context),
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return fail(
                context,
                &format!("could not build the tokio runtime: {error}"),
                None,
            );
        }
    };
    let listen = web.listen.clone();
    match runtime.block_on(hyoui_web::serve(&listen, web.assets_dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(
            context,
            &format!("could not serve on {listen}: {error}"),
            Some(json!({"listen": listen})),
        ),
    }
}

/// `daemon run` が起動に使う `[web]` を決める。
///
/// 読むのはその config ファイルと `extends` でたどれるファイルだけで、共通の
/// `config.toml` は読まない (DR-0038 決定 1)。config を読む 2 形態はどちらも
/// `state_dir` を今の面と比べる。`--no-config` は config を読まないので比べる相手が無い。
fn run_target(
    registry: &Registry,
    source: &WebDaemonRunSource,
    env: &Env,
) -> std::result::Result<WebConfig, Refusal> {
    let (path, web) = match source {
        WebDaemonRunSource::Unit(name) => {
            let unit = registry
                .get(name)
                .map_err(|error| Refusal::new(error.to_string()))?;
            let web = unit.load_config().map_err(|error| {
                config_unreadable(
                    &format!("could not read the config of unit `{name}`"),
                    &unit.config,
                    &error,
                )
            })?;
            (unit.config, web)
        }
        WebDaemonRunSource::Config(path) => {
            let path = absolute(path).map_err(Refusal::new)?;
            let web = hyoui::config::load_web(&path)
                .map(|file| file.web)
                .map_err(|error| config_unreadable("could not read config", &path, &error))?;
            (path, web)
        }
        WebDaemonRunSource::NoConfig { listen } => {
            let mut web = WebConfig::default();
            if let Some(listen) = listen {
                web.listen.clone_from(listen);
            }
            return Ok(web);
        }
    };
    check_state_dir(&web, &path, env)?;
    Ok(web)
}

/// config に `binary_path` が無い時の既定 (= 登録した時点の自分自身、DR-0038 決定 2)。
///
/// `current_exe` をそのまま書き、安定な場所を探して差し替えない。brew 版から
/// `add` すれば brew の path が、repo build から `add` すればその build の path が
/// 入る。`resolve_stable_path` を通すと、repo build が brew 版と同一内容だった
/// 瞬間に unstable unit が stable の binary を指してしまう (DR-0034 決定 2)。
fn default_binary() -> std::result::Result<PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("cannot resolve own binary path: {error}"))
}

/// JSON 1 つを stdout に書いて成功で終わる。
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

/// JSONL を 1 行書く。相手が読むのをやめたら `Err` (= `log --follow` の終わり)。
fn emit_jsonl<T: serde::Serialize>(value: &T) -> std::io::Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let line = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    stdout.write_all(&line)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// 監督者が返したエラーをそのまま stderr に流す (= kind と hint を保つ)。
fn fail_with(context: &str, error: &ErrorBody) -> ExitCode {
    let mut body = json!({"command": context, "error": error.message, "kind": error.kind});
    if let Some(hint) = &error.hint {
        body["hint"] = json!(hint);
    }
    match serde_json::to_string_pretty(&body) {
        Ok(text) => eprintln!("{text}"),
        Err(_) => eprintln!("hyoui: {context}: {}", error.message),
    }
    ExitCode::from(1)
}

/// エラーを JSON で stderr に書いて非 0 で終わる。
fn fail(context: &str, message: &str, details: Option<Value>) -> ExitCode {
    let mut error = json!({"command": context, "error": message});
    if let Some(Value::Object(details)) = details {
        for (key, value) in details {
            error[key] = value;
        }
    }
    match serde_json::to_string_pretty(&error) {
        Ok(text) => eprintln!("{text}"),
        Err(_) => eprintln!("hyoui: {context}: {message}"),
    }
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn unit(config: PathBuf) -> Unit {
        Unit {
            config,
            binary_path: PathBuf::from("/opt/homebrew/bin/hyoui"),
            enabled: true,
            added_at: registry::now_iso8601(),
        }
    }

    /// HOME だけを持つ env (= 状態の root は `<home>/.local/state/hyoui`)。
    fn home_env(home: &Path) -> Env {
        let home = home.as_os_str().to_os_string();
        Env::from_lookup(|name| (name == "HOME").then(|| home.clone()))
    }

    /// `run <unit>` は登録簿が指す config を、`--config` はその file を読み、どちらも
    /// `state_dir` を今の面と比べる。`--no-config` は config を読まず既定値と
    /// `--listen` だけで起動する (DR-0038 決定 9)。
    #[test]
    fn run_reads_the_unit_or_the_given_config_or_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let env = home_env(directory.path());
        let root = env.state_root();
        let registry = Registry::at(directory.path().join("units"));
        let config = write(
            directory.path(),
            "unstable.toml",
            &format!(
                "[web]\nstate_dir = \"{}\"\nlisten = \"127.0.0.1:43691\"\nassets_dir = \"/a\"\n",
                root.display()
            ),
        );
        registry.add("unstable", &unit(config.clone())).unwrap();

        let source = WebDaemonRunSource::Unit("unstable".into());
        let web = run_target(&registry, &source, &env).unwrap();
        assert_eq!(web.listen, "127.0.0.1:43691");
        assert_eq!(web.assets_dir, Some(PathBuf::from("/a")));

        let source = WebDaemonRunSource::Config(config);
        assert_eq!(
            run_target(&registry, &source, &env).unwrap().listen,
            "127.0.0.1:43691"
        );

        // `--no-config` は config を読まない。
        let source = WebDaemonRunSource::NoConfig { listen: None };
        assert_eq!(
            run_target(&registry, &source, &env).unwrap(),
            WebConfig::default()
        );
        let source = WebDaemonRunSource::NoConfig {
            listen: Some("127.0.0.1:0".into()),
        };
        assert_eq!(
            run_target(&registry, &source, &env).unwrap().listen,
            "127.0.0.1:0"
        );

        let missing = WebDaemonRunSource::Unit("missing".into());
        assert!(
            run_target(&registry, &missing, &env)
                .unwrap_err()
                .message
                .contains("no web gateway unit named")
        );

        // 登録簿が指す config が消えていれば、どの unit の config かを言って断る。
        registry
            .add("gone", &unit(directory.path().join("gone.toml")))
            .unwrap();
        let gone = WebDaemonRunSource::Unit("gone".into());
        let error = run_target(&registry, &gone, &env).unwrap_err().message;
        assert!(
            error.contains("`gone`") && error.contains("gone.toml"),
            "{error}"
        );
    }

    /// 別の面の config (= `state_dir` が今の root と違う) と、`state_dir` の無い
    /// config は run の 2 形態とも断る (DR-0038 決定 9)。
    #[test]
    fn run_refuses_a_config_of_another_state_root() {
        let directory = tempfile::tempdir().unwrap();
        let env = home_env(directory.path());
        let registry = Registry::at(directory.path().join("units"));
        let other = write(
            directory.path(),
            "other.toml",
            "[web]\nstate_dir = \"/elsewhere/hyoui\"\n",
        );
        let bare = write(directory.path(), "bare.toml", "[web]\n");
        registry.add("other", &unit(other.clone())).unwrap();

        for source in [
            WebDaemonRunSource::Unit("other".into()),
            WebDaemonRunSource::Config(other),
        ] {
            let refusal = run_target(&registry, &source, &env).unwrap_err();
            assert!(
                refusal.message.contains("/elsewhere/hyoui")
                    && refusal
                        .message
                        .contains(&env.state_root().display().to_string()),
                "{source:?}: {}",
                refusal.message
            );
        }
        let refusal = run_target(&registry, &WebDaemonRunSource::Config(bare), &env).unwrap_err();
        assert!(
            refusal.message.contains("no `[web].state_dir`"),
            "{}",
            refusal.message
        );
    }

    /// `state_dir` は realpath で比べる: symlink 越しに同じ root を指せば同じ面
    /// (DR-0038 決定 9)。
    #[test]
    fn the_state_dir_is_compared_by_realpath() {
        let directory = tempfile::tempdir().unwrap();
        let env = home_env(directory.path());
        let root = env.state_root();
        std::fs::create_dir_all(&root).unwrap();
        let link = directory.path().join("link-to-root");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        // check はトップのファイルを読むので、値はファイルに書いて渡す。
        let check = |state_dir: &Path| {
            let config = write(
                directory.path(),
                "u.toml",
                &format!("[web]\nstate_dir = \"{}\"\n", state_dir.display()),
            );
            let web = hyoui::config::load_web(&config).unwrap().web;
            check_state_dir(&web, &config, &env)
        };

        assert!(check(&root).is_ok());
        assert!(check(&link).is_ok());
        assert!(check(&root.join("../hyoui")).is_ok());
        let refusal = check(&directory.path().join("x")).unwrap_err();
        assert_eq!(refusal.details.unwrap()["current_state_dir"], json!(root));
        let bare = write(directory.path(), "bare.toml", "[web]\n");
        let refusal = check_state_dir(&WebConfig::default(), &bare, &env).unwrap_err();
        assert!(
            refusal.details.unwrap()["hint"]
                .as_str()
                .unwrap()
                .contains(&format!("state_dir = \"{}\"", root.display()))
        );
    }

    /// `state_dir` は unit の config ファイル自身に書かれていなければならない。土台から
    /// `extends` で継いだ値は、値が今の面と一致していても断る (DR-0038 決定 9)。
    #[test]
    fn a_state_dir_inherited_from_the_base_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let env = home_env(directory.path());
        let root = env.state_root();
        write(
            directory.path(),
            "base.toml",
            &format!("[web]\nstate_dir = \"{}\"\n", root.display()),
        );
        let unit_config = write(
            directory.path(),
            "stable.toml",
            "extends = \"base.toml\"\n[web]\nlisten = \"127.0.0.1:43692\"\n",
        );
        let web = hyoui::config::load_web(&unit_config).unwrap().web;
        assert_eq!(
            web.state_dir,
            Some(root.clone()),
            "the merged value is inherited"
        );
        let refusal = check_state_dir(&web, &unit_config, &env).unwrap_err();
        assert!(
            refusal
                .message
                .contains("inherits `[web].state_dir` through `extends`"),
            "{}",
            refusal.message
        );
        let details = refusal.details.unwrap();
        assert!(
            details["hint"].as_str().unwrap().contains(&format!(
                "add `state_dir = \"{}\"` under [web] in {}",
                root.display(),
                unit_config.display()
            )),
            "{details}"
        );

        // 自分で書けば、土台に同じ鍵があっても通る (= 土台への記述自体は禁止しない)。
        let own = write(
            directory.path(),
            "own.toml",
            &format!(
                "extends = \"base.toml\"\n[web]\nstate_dir = \"{}\"\n",
                root.display()
            ),
        );
        let web = hyoui::config::load_web(&own).unwrap().web;
        assert!(check_state_dir(&web, &own, &env).is_ok());

        // `--config` / `run <unit>` の経路も同じ check を通る。
        let registry = Registry::at(directory.path().join("units"));
        registry.add("stable", &unit(unit_config.clone())).unwrap();
        for source in [
            WebDaemonRunSource::Unit("stable".into()),
            WebDaemonRunSource::Config(unit_config.clone()),
        ] {
            let refusal = run_target(&registry, &source, &env).unwrap_err();
            assert!(refusal.message.contains("inherits"), "{source:?}");
        }
    }

    /// 生成する config の listen の既定は `127.0.0.1:43690`、`--listen` があればそれ
    /// (DR-0038 決定 9)。実機のポート状況に依らない形で固定する。
    #[test]
    fn a_new_config_listens_on_the_default_unless_given() {
        assert_eq!(listen_for_new_config(None), "127.0.0.1:43690");
        assert_eq!(
            listen_for_new_config(Some("127.0.0.1:43692".into())),
            "127.0.0.1:43692"
        );
        let directory = tempfile::tempdir().unwrap();
        let text = unit_config_text(
            &directory.path().join("u.toml"),
            &directory.path().join("state/hyoui"),
            &listen_for_new_config(None),
            Path::new("/opt/hyoui"),
        )
        .unwrap();
        assert!(text.contains("listen = \"127.0.0.1:43690\"\n"), "{text}");
    }

    /// add を呼ぶための隔離した env と登録簿 (= HOME だけを持つ)。
    fn isolated(directory: &Path) -> (Env, Registry) {
        let env = home_env(directory);
        let registry = Registry::at(env.web_state_dir().join("units"));
        (env, registry)
    }

    fn add_config(name: &str, listen: &str) -> WebDaemonAddConfig {
        WebDaemonAddConfig {
            name: name.into(),
            listen: Some(listen.into()),
            ..WebDaemonAddConfig::default()
        }
    }

    /// add は登録簿の lock を待たずに取り、他の add が持っていれば何も書かずに断る。
    /// lock を離せば同じ add が通る (DR-0038 決定 9)。
    #[test]
    fn add_refuses_while_another_add_holds_the_registry_lock() {
        let directory = tempfile::tempdir().unwrap();
        let (env, registry) = isolated(directory.path());
        let config = env.web_config_dir().unwrap().join("stable.toml");

        let held = registry.try_lock().unwrap();
        // 同じ登録簿の lock は 2 つ同時に取れない (= 別プロセスの add も同じ file に取る)。
        assert!(matches!(
            registry.try_lock(),
            Err(registry::Error::Busy { .. })
        ));
        let refusal = add(add_config("stable", "127.0.0.1:0"), &env, &registry).unwrap_err();
        assert!(
            refusal.message.contains("another `hyoui web daemon add`"),
            "{}",
            refusal.message
        );
        assert_eq!(refusal.details.unwrap()["kind"], "registry_busy");
        assert!(!config.exists(), "nothing is written without the lock");
        assert!(registry.list().unwrap().is_empty());

        drop(held);
        add(add_config("stable", "127.0.0.1:0"), &env, &registry).unwrap();
        assert!(config.exists());
        assert_eq!(registry.list().unwrap().len(), 1);
        // add が終われば lock は離れている。
        assert!(registry.try_lock().is_ok());
    }

    /// 同じ名前の登録は、lock の中で確かめてから書くので上書きしない。
    #[test]
    fn add_locked_does_not_replace_an_existing_registration() {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("units"));
        let lock = registry.try_lock().unwrap();
        registry
            .add_locked(&lock, "stable", &unit(PathBuf::from("/c/stable.toml")))
            .unwrap();
        assert!(matches!(
            registry.add_locked(&lock, "stable", &unit(PathBuf::from("/c/other.toml"))),
            Err(registry::Error::AlreadyRegistered { .. })
        ));
        drop(lock);
        assert_eq!(
            registry.get("stable").unwrap().config,
            PathBuf::from("/c/stable.toml")
        );
    }

    /// 生成した config が `extends` を含めて読めなければ、登録せず、生成したファイルを
    /// 消して断る。土台など他人のファイルは消さない (DR-0038 決定 2 / 9)。
    #[test]
    fn add_refuses_a_generated_config_that_cannot_be_read() {
        for (base, needle) in [
            ("[web\n", "fix the TOML in"),
            ("extends = \"missing.toml\"\n", "fix or remove `extends` in"),
            ("extends = \"stable.toml\"\n", "remove one `extends`"),
            ("extends = 1\n", "as a path string"),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (env, registry) = isolated(directory.path());
            let web_dir = env.web_config_dir().unwrap();
            let base_path = write_in(&web_dir, "base.toml", base);
            let refusal = add(add_config("stable", "127.0.0.1:0"), &env, &registry).unwrap_err();
            assert!(
                refusal
                    .message
                    .contains("the generated config cannot be read"),
                "{base:?}: {}",
                refusal.message
            );
            let hint = refusal.details.unwrap()["hint"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(hint.contains(needle), "{base:?}: {hint}");
            assert!(
                !web_dir.join("stable.toml").exists(),
                "{base:?}: the generated config is removed"
            );
            assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base);
            assert!(registry.list().unwrap().is_empty(), "{base:?}");
            assert_eq!(leftover_temporaries(&web_dir), Vec::<String>::new());
        }
    }

    /// 登録まで済まなかった add は、自分が生成したファイルだけを消す。既にあった
    /// ファイルは (登録に失敗しても) 消さない。
    #[test]
    fn a_failed_add_removes_only_the_file_it_generated() {
        let directory = tempfile::tempdir().unwrap();
        let (env, registry) = isolated(directory.path());
        let web_dir = env.web_config_dir().unwrap();

        // 生成したファイルの Drop で消える / keep で残る。
        let state = env.state_root();
        let generated = write_unit_config(
            &web_dir.join("a.toml"),
            &state,
            "127.0.0.1:0",
            Path::new("/x"),
        )
        .unwrap();
        assert!(web_dir.join("a.toml").exists());
        drop(generated);
        assert!(!web_dir.join("a.toml").exists());
        write_unit_config(
            &web_dir.join("b.toml"),
            &state,
            "127.0.0.1:0",
            Path::new("/x"),
        )
        .unwrap()
        .keep();
        assert!(web_dir.join("b.toml").exists());

        // 既存ファイルの add が listen の衝突で断られても、そのファイルは残る。
        add(add_config("first", "127.0.0.1:43693"), &env, &registry).unwrap();
        let existing = write_in(
            &web_dir,
            "second.toml",
            &format!(
                "[web]\nstate_dir = \"{}\"\nlisten = \"127.0.0.1:43693\"\n",
                state.display()
            ),
        );
        let refusal = add(
            WebDaemonAddConfig {
                name: "second".into(),
                ..WebDaemonAddConfig::default()
            },
            &env,
            &registry,
        )
        .unwrap_err();
        assert!(refusal.message.contains("already used by unit `first`"));
        assert!(existing.exists());
    }

    /// 書き込みは一時ファイルから公開するので、公開できなかった時に `<unit>.toml` も
    /// 一時ファイルも残らない (= 次の add が半端なファイルを既存として読まない)。
    #[test]
    fn a_write_that_cannot_be_published_leaves_nothing_behind() {
        let directory = tempfile::tempdir().unwrap();
        let web_dir = directory.path().join("web");
        let state = directory.path().join("state/hyoui");
        // 公開の直前に同名が現れた (= persist_noclobber が断る) 場合。
        let target = write_in(&web_dir, "raced.toml", "someone else's\n");
        let refusal =
            write_unit_config(&target, &state, "127.0.0.1:0", Path::new("/x")).unwrap_err();
        assert!(
            refusal.message.contains("could not write"),
            "{}",
            refusal.message
        );
        assert!(
            refusal.details.unwrap()["hint"]
                .as_str()
                .unwrap()
                .contains("then run again"),
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "someone else's\n"
        );
        assert_eq!(leftover_temporaries(&web_dir), Vec::<String>::new());

        // 書けない dir の場合も、何も残さず hint 付きで断る。
        use std::os::unix::fs::PermissionsExt as _;
        let locked = directory.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_unit_config(
            &locked.join("u.toml"),
            &state,
            "127.0.0.1:0",
            Path::new("/x"),
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let refusal = result.unwrap_err();
        assert!(
            refusal.details.unwrap()["hint"]
                .as_str()
                .unwrap()
                .contains("writable"),
        );
        assert_eq!(
            std::fs::read_dir(&locked).unwrap().count(),
            0,
            "nothing is left in the directory"
        );
    }

    /// config の読み込みに失敗した run は、どのファイルをどう直すかを hint に書く。
    #[test]
    fn unreadable_configs_say_what_to_fix() {
        let directory = tempfile::tempdir().unwrap();
        let (env, registry) = isolated(directory.path());
        let broken = write(directory.path(), "broken.toml", "[web\n");
        let refusal =
            run_target(&registry, &WebDaemonRunSource::Config(broken.clone()), &env).unwrap_err();
        assert!(refusal.message.starts_with("could not read config:"));
        assert!(
            refusal.details.unwrap()["hint"]
                .as_str()
                .unwrap()
                .contains(&format!("fix the TOML in {}", broken.display()))
        );
        let a = write(directory.path(), "a.toml", "extends = \"b.toml\"\n");
        write(directory.path(), "b.toml", "extends = \"a.toml\"\n");
        let refusal = run_target(&registry, &WebDaemonRunSource::Config(a), &env).unwrap_err();
        assert!(
            refusal.details.unwrap()["hint"]
                .as_str()
                .unwrap()
                .contains("remove one `extends`")
        );
    }

    /// `dir` に `name` を書く (dir が無ければ作る)。
    fn write_in(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        write(dir, name, body)
    }

    /// add の一時ファイルの残骸。
    fn leftover_temporaries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .filter(|name| name.starts_with(".hyoui-add-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 生成する config は土台があれば `extends` し、`state_dir` / `listen` /
    /// `binary_path` を書く。既にあるファイルは上書きしない (DR-0038 決定 9)。
    #[test]
    fn the_generated_config_extends_the_base_and_names_its_state_root() {
        let directory = tempfile::tempdir().unwrap();
        let web_dir = directory.path().join("web");
        let state = directory.path().join("state/hyoui");
        let path = web_dir.join("solo.toml");
        write_unit_config(&path, &state, "127.0.0.1:43692", Path::new("/opt/hyoui"))
            .unwrap()
            .keep();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("extends"), "{text}");
        let web = hyoui::config::load_web(&path).unwrap().web;
        assert_eq!(web.state_dir, Some(state.clone()));
        assert_eq!(web.listen, "127.0.0.1:43692");
        assert_eq!(web.binary_path, Some(PathBuf::from("/opt/hyoui")));

        write(&web_dir, "base.toml", "[web]\nassets_dir = \"/assets\"\n");
        let path = web_dir.join("layered.toml");
        write_unit_config(&path, &state, "127.0.0.1:43693", Path::new("/opt/hyoui"))
            .unwrap()
            .keep();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("extends = \"base.toml\"\n"), "{text}");
        let web = hyoui::config::load_web(&path).unwrap().web;
        assert_eq!(web.assets_dir, Some(PathBuf::from("/assets")));
        assert_eq!(web.state_dir, Some(state.clone()));

        // 引用が要る文字を含む値も TOML として読み直せる。
        let odd = Path::new("/opt/a \"b\"\\c");
        let path = web_dir.join("quoted.toml");
        write_unit_config(&path, &state, "127.0.0.1:43694", odd)
            .unwrap()
            .keep();
        assert_eq!(
            hyoui::config::load_web(&path).unwrap().web.binary_path,
            Some(odd.to_path_buf())
        );

        // 既にあるファイルは上書きしない。
        let layered = web_dir.join("layered.toml");
        let before = std::fs::read_to_string(&layered).unwrap();
        assert!(write_unit_config(&layered, &state, "127.0.0.1:1", Path::new("/x")).is_err());
        assert_eq!(std::fs::read_to_string(&layered).unwrap(), before);
    }

    /// 他のプロセスが listen しているポートは使用中、port 0 は確かめない。
    #[test]
    fn a_listening_port_is_in_use_and_port_zero_is_free() {
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = holder.local_addr().unwrap().to_string();
        assert!(matches!(check_listen_free(&taken), ListenCheck::InUse));
        assert!(matches!(
            check_listen_free("127.0.0.1:0"),
            ListenCheck::Free
        ));
        drop(holder);
        assert!(matches!(check_listen_free(&taken), ListenCheck::Free));
        assert!(matches!(
            check_listen_free("not-an-address"),
            ListenCheck::Unknown(_)
        ));
    }

    #[test]
    fn a_relative_config_path_becomes_absolute() {
        let resolved = absolute(Path::new("web/stable.toml")).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("web/stable.toml"));
    }

    /// 古い置き場 (状態 dir と監督者のログ dir) が symlink か実体かで警告を言い分け、
    /// 無ければ黙る (DR-0038 移行節)。
    #[test]
    fn legacy_places_are_reported_but_not_read() {
        let home = tempfile::tempdir().unwrap();
        let home_os = home.path().as_os_str().to_os_string();
        let env = hyoui::paths::Env::from_lookup(|name| (name == "HOME").then(|| home_os.clone()));
        let state = home.path().join(".local/state");
        std::fs::create_dir_all(&state).unwrap();
        assert!(legacy_warnings(&env).is_empty());

        // 移していない状態 dir は「読まない」と言う。
        let legacy_state = state.join(LEGACY_DIR_NAME);
        std::fs::create_dir(&legacy_state).unwrap();
        let warnings = legacy_warnings(&env);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("is not read"), "{warnings:?}");
        assert!(warnings[0].contains("hyoui/web"), "{warnings:?}");

        // 移して symlink にすれば「後で消す」になる。
        std::fs::remove_dir(&legacy_state).unwrap();
        std::fs::create_dir_all(env.web_state_dir().join("logs")).unwrap();
        std::os::unix::fs::symlink(env.web_state_dir(), &legacy_state).unwrap();
        let logs = home.path().join("Library/Logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::os::unix::fs::symlink(env.web_state_dir().join("logs"), logs.join(LEGACY_DIR_NAME))
            .unwrap();
        let warnings = legacy_warnings(&env);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings.iter().all(|w| w.contains("symlink left")),
            "{warnings:?}"
        );

        // 消せば黙る。
        std::fs::remove_file(&legacy_state).unwrap();
        std::fs::remove_file(logs.join(LEGACY_DIR_NAME)).unwrap();
        assert!(legacy_warnings(&env).is_empty());

        // 旧 label の定義が残っていれば外す手順を言い、新しい label の定義には黙る。
        let agents = home.path().join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join(format!(
                "{}.plist",
                crate::web_service::label_for_root(&env.state_root())
            )),
            "",
        )
        .unwrap();
        assert!(legacy_warnings(&env).is_empty());
        let old = agents.join("jp.kawaz.hyoui-web.supervise.plist");
        std::fs::write(&old, "").unwrap();
        let warnings = legacy_warnings(&env);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("old label") && warnings[0].contains("bootout"),
            "{warnings:?}"
        );
        std::fs::remove_file(&old).unwrap();
        let unit_dir = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        std::fs::write(unit_dir.join("hyoui-web-supervise.service"), "").unwrap();
        assert_eq!(legacy_warnings(&env).len(), 1);
    }

    #[test]
    fn the_default_binary_is_this_executable() {
        assert_eq!(default_binary().unwrap(), std::env::current_exe().unwrap());
    }

    /// `enabled` と `running` を分けて持つのは、止めてあるのか上がらないのかを
    /// 区別するため (決定 4)。probe の 2 状態はその `running` 側を答える。
    #[test]
    fn the_supervisor_probe_has_two_states() {
        assert!(Supervisor::Running.is_running());
        assert!(!Supervisor::NotRunning.is_running());
    }
}
