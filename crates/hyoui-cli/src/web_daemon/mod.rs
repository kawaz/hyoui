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

use hyoui::cli::WebDaemonAddConfig;
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

/// 置き場を移す前の web の状態 dir の名前 (= `$XDG_STATE_HOME/<これ>`、DR-0038 移行節)。
///
/// 新しいバイナリはここを読まない。残っているかを見て警告するためだけに名前を持つ。
const LEGACY_STATE_DIR_NAME: &str = "hyoui-web";

/// 古い置き場が残っていれば stderr に警告する (DR-0038 移行節 (3))。
///
/// symlink なら「移行済み、古いバイナリ用の symlink が残っている = 後で消す」、
/// 実体の dir なら「移行していない = passkey も登録簿も新しい置き場に無い」。
/// どちらも読まず、二重に拾わない。
pub fn warn_legacy_state_dir() {
    let env = hyoui::paths::Env::current();
    if let Some(warning) = legacy_state_warning(&env) {
        eprintln!("hyoui: warning: {warning}");
    }
}

fn legacy_state_warning(env: &hyoui::paths::Env) -> Option<String> {
    let legacy = env.state_home()?.join(LEGACY_STATE_DIR_NAME);
    let meta = legacy.symlink_metadata().ok()?;
    let current = env.web_state_dir();
    Some(if meta.file_type().is_symlink() {
        format!(
            "{} is a symlink left for older hyoui binaries; remove it once none of them run (DR-0038)",
            legacy.display()
        )
    } else {
        format!(
            "{} still holds web state that this hyoui does not read; move it to {} and leave a symlink in its place (DR-0038)",
            legacy.display(),
            current.display()
        )
    })
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

/// `hyoui web daemon add [--name <name>] <config-path>` (DR-0038 決定 2)。
pub fn add_command(cfg: WebDaemonAddConfig) -> ExitCode {
    let context = "web daemon add";
    let config_path = match absolute(&cfg.config) {
        Ok(path) => path,
        Err(error) => return fail(context, &error, None),
    };
    let name = match cfg
        .name
        .clone()
        .map_or_else(|| name_from_config(&config_path), Ok)
    {
        Ok(name) => name,
        Err(error) => return fail(context, &error, None),
    };
    if let Err(error) = registry::validate_name(&name) {
        return fail(
            context,
            &error.to_string(),
            Some(json!({"hint": "give a name with --name <name>"})),
        );
    }

    // 登録する時点で読めることを確かめる。読めない config を登録すると、監督者が
    // 起こすたびに子が config で落ち、backoff の理由を探すことになる。
    let web = match hyoui::config::load_web(&config_path) {
        Ok(file) => file.web,
        Err(error) => {
            return fail(
                context,
                &format!("could not read the unit's config: {error}"),
                Some(json!({"config": config_path})),
            );
        }
    };

    let registry = Registry::open();
    let existing = match registry.list() {
        Ok(units) => units,
        Err(error) => return fail(context, &error.to_string(), None),
    };

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
    match registry::find_listen_conflict(&listens, &web.listen) {
        Some(ListenConflict::Same {
            name: other,
            listen,
        }) => {
            return fail(
                context,
                &format!("`{listen}` is already the bind address of unit `{other}`"),
                Some(json!({"listen": listen, "conflicting_unit": other})),
            );
        }
        Some(ListenConflict::Overlapping {
            name: other,
            listen: other_listen,
            reason,
        }) => warnings.push(format!(
            "`{}` may overlap with unit `{other}` (`{other_listen}`): {reason}",
            web.listen
        )),
        None => {}
    }

    // binary は config の `binary_path` が正、無ければ登録した時点の自分自身
    // (llm-gateway DR-0028 決定 2 と同じ)。
    let binary_path = match web.binary_path.clone().map_or_else(default_binary, Ok) {
        Ok(binary) => binary,
        Err(error) => return fail(context, &error, None),
    };
    let binary_exists = binary_path.exists();
    if !binary_exists {
        // ビルド前に登録する順序を禁じない (DR-0034 決定 2)。
        warnings.push(format!(
            "`{}` does not exist yet; this unit cannot start until it is built",
            binary_path.display()
        ));
    }

    let unit = Unit {
        config: config_path,
        binary_path,
        // `add` は「この gateway を動かしたい」という意思表示なので enabled で入る
        // (DR-0034 決定 4)。
        enabled: true,
        added_at: registry::now_iso8601(),
    };
    if let Err(error) = registry.add(&name, &unit) {
        return fail(context, &error.to_string(), None);
    }

    // 走行中の監督者には即反映する (DR-0034 決定 4)。送るのは `reload` で、監督者が
    // 登録簿を読み直して望みとの差を埋める — 足したばかりの unit は監督者がまだ名前を
    // 知らないので、`start <name>` を送っても「そんな unit は無い」になる。
    let supervisor = Supervisor::probe();
    let started = supervisor.request(&Request::Reload);
    let mut output = json!({
        "name": name,
        "config": unit.config,
        "listen": web.listen,
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
    emit(&output)
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

/// `--name` を省いた時の unit 名 = config の basename から拡張子を除いたもの。
fn name_from_config(config: &Path) -> std::result::Result<String, String> {
    config
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("cannot derive a unit name from {}", config.display()))
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

/// `hyoui web daemon run [name]` (DR-0038 決定 2)。
///
/// name を渡すと登録簿が指す config で起動する。監督者が子を exec するのと同じ
/// 経路で、手元で 1 台だけ確かめる時にも使う (DR-0034 決定 3)。name 省略時は
/// 登録簿を見ず、web の config の既定 path (`$XDG_CONFIG_HOME/hyoui/web/config.toml`)
/// を読む。無ければ組み込みの既定値で起動する。
pub fn run_command(name: Option<&str>) -> ExitCode {
    let context = "web daemon run";
    let web = match run_target(&Registry::open(), name, hyoui::config::default_web_path()) {
        Ok(web) => web,
        Err(error) => return fail(context, &error, None),
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
/// 「既定の unit」は持たない — 登録簿に 1 つしかない時それを選ぶ推測を入れると、
/// 2 つ目を足した瞬間に同じコマンドの意味が変わる (DR-0034 決定 1)。名前を省いた
/// 時に読むのは登録簿ではなく既定の置き場の config。
fn run_target(
    registry: &Registry,
    name: Option<&str>,
    default_path: Option<PathBuf>,
) -> std::result::Result<hyoui::config::WebConfig, String> {
    match name {
        Some(name) => {
            let unit = registry.get(name).map_err(|error| error.to_string())?;
            unit.load_config()
                .map_err(|error| format!("could not read the config of unit `{name}`: {error}"))
        }
        None => match default_path {
            Some(path) if path.exists() => hyoui::config::load_web(&path)
                .map(|file| file.web)
                .map_err(|error| format!("could not read config: {error}")),
            _ => Ok(hyoui::config::WebConfig::default()),
        },
    }
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

    /// 名前付きは登録簿が指す config を、名前なしは既定の置き場の config を読む。
    /// 既定の置き場に無ければ組み込みの既定値 (DR-0038 決定 2)。
    #[test]
    fn run_reads_the_unit_config_and_the_default_place_without_a_name() {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("units"));
        let config = write(
            directory.path(),
            "unstable.toml",
            "[web]\nlisten = \"127.0.0.1:43691\"\nassets_dir = \"/a\"\n",
        );
        registry.add("unstable", &unit(config)).unwrap();

        let web = run_target(&registry, Some("unstable"), None).unwrap();
        assert_eq!(web.listen, "127.0.0.1:43691");
        assert_eq!(web.assets_dir, Some(PathBuf::from("/a")));

        let default = write(
            directory.path(),
            "config.toml",
            "[web]\nlisten = \"127.0.0.1:40000\"\n",
        );
        assert_eq!(
            run_target(&registry, None, Some(default)).unwrap().listen,
            "127.0.0.1:40000"
        );
        assert_eq!(
            run_target(&registry, None, Some(directory.path().join("absent.toml")))
                .unwrap()
                .listen,
            "127.0.0.1:43690"
        );

        assert!(
            run_target(&registry, Some("missing"), None)
                .unwrap_err()
                .contains("no web gateway unit named")
        );

        // 登録簿が指す config が消えていれば、どの unit の config かを言って断る。
        registry
            .add("gone", &unit(directory.path().join("gone.toml")))
            .unwrap();
        let error = run_target(&registry, Some("gone"), None).unwrap_err();
        assert!(
            error.contains("`gone`") && error.contains("gone.toml"),
            "{error}"
        );
    }

    #[test]
    fn the_unit_name_defaults_to_the_config_basename_without_extension() {
        assert_eq!(
            name_from_config(Path::new("/c/hyoui/web/stable.toml")).unwrap(),
            "stable"
        );
        assert_eq!(
            name_from_config(Path::new("/c/config-43691-unstable.toml")).unwrap(),
            "config-43691-unstable"
        );
    }

    #[test]
    fn a_relative_config_path_becomes_absolute() {
        let resolved = absolute(Path::new("web/stable.toml")).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("web/stable.toml"));
    }

    /// 古い置き場が symlink か実体かで警告を言い分け、無ければ黙る (DR-0038 移行節)。
    #[test]
    fn the_legacy_state_dir_is_reported_but_not_read() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        let state_os = state.clone().into_os_string();
        let env = hyoui::paths::Env::from_lookup(|name| {
            (name == "XDG_STATE_HOME").then(|| state_os.clone())
        });
        std::fs::create_dir_all(&state).unwrap();
        assert_eq!(legacy_state_warning(&env), None);

        let legacy = state.join(LEGACY_STATE_DIR_NAME);
        std::fs::create_dir(&legacy).unwrap();
        let warning = legacy_state_warning(&env).expect("a real dir is reported");
        assert!(warning.contains("does not read"), "{warning}");

        std::fs::remove_dir(&legacy).unwrap();
        std::fs::create_dir_all(env.web_state_dir()).unwrap();
        std::os::unix::fs::symlink(env.web_state_dir(), &legacy).unwrap();
        let warning = legacy_state_warning(&env).expect("a symlink is reported");
        assert!(warning.contains("symlink"), "{warning}");
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
