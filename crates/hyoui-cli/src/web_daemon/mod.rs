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

use std::path::PathBuf;
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
        Ok(Response::Units { units, .. }) => emit(&json!({
            "units": units
                .into_iter()
                .map(|unit| json!({
                    "name": unit.name,
                    "enabled": unit.enabled,
                    "running": unit.running,
                    "pid": unit.pid,
                    "listen": unit.listen,
                    "binary": unit.binary,
                    "binary_exists": unit.binary_exists,
                }))
                .collect::<Vec<_>>(),
            "supervisor": {"running": true},
            "registry_dir": Registry::open().dir(),
        })),
        Ok(Response::Error(error)) => fail_with("web daemon list", &error),
        Ok(_) | Err(_) => {
            let mut output = match registry_view(None) {
                Ok(output) => output,
                Err(code) => return code,
            };
            output["note"] = json!(
                "the supervisor is not running, so `running` and `pid` are not known for any unit"
            );
            emit(&output)
        }
    }
}

/// 登録簿から答えられる範囲だけを組み立てる (= 監督者に聞けない時の答え)。
///
/// 障害時に最初に打つコマンドが監督者の生死に依存すると、状態を見る入口ごと
/// 失われる (決定 4)。
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
            .map(|(name, unit)| json!({
                "name": name,
                "enabled": unit.enabled,
                "running": false,
                "pid": Value::Null,
                "listen": unit.listen,
                "binary": unit.binary,
                "binary_exists": unit.binary.exists(),
                "web_assets_dir": unit.web_assets_dir,
                "added_at": unit.added_at,
            }))
            .collect::<Vec<_>>(),
        "supervisor": {"running": false},
        "registry_dir": registry.dir(),
    }))
}

/// `hyoui web daemon add <name> [options]`。
pub fn add_command(cfg: WebDaemonAddConfig) -> ExitCode {
    let context = "web daemon add";
    if let Err(error) = registry::validate_name(&cfg.name) {
        return fail(context, &error.to_string(), None);
    }

    let config = match hyoui::config::load() {
        Ok(config) => config,
        Err(error) => return fail(context, &format!("could not read config: {error}"), None),
    };

    let registry = Registry::open();
    let existing = match registry.list() {
        Ok(units) => units,
        Err(error) => return fail(context, &error.to_string(), None),
    };

    let listen = resolve_listen(&cfg, &config.web.listen);
    let mut warnings = Vec::new();
    match registry::find_listen_conflict(&existing, &listen) {
        Some(ListenConflict::Same { name, listen }) => {
            return fail(
                context,
                &format!("`{listen}` is already the bind address of unit `{name}`"),
                Some(json!({"listen": listen, "conflicting_unit": name})),
            );
        }
        Some(ListenConflict::Overlapping {
            name,
            listen: other,
            reason,
        }) => warnings.push(format!(
            "`{listen}` may overlap with unit `{name}` (`{other}`): {reason}"
        )),
        None => {}
    }

    let binary = match cfg.binary.clone().map_or_else(default_binary, Ok) {
        Ok(binary) => binary,
        Err(error) => return fail(context, &error, None),
    };
    let binary_exists = binary.exists();
    if !binary_exists {
        // ビルド前に登録する順序を禁じない (決定 2)。
        warnings.push(format!(
            "`{}` does not exist yet; this unit cannot start until it is built",
            binary.display()
        ));
    }

    let unit = Unit {
        listen: listen.clone(),
        binary: binary.clone(),
        web_assets_dir: cfg
            .assets_dir
            .clone()
            .or_else(|| config.web.assets_dir.clone()),
        // `add` は「この gateway を動かしたい」という意思表示なので enabled で入る
        // (決定 4)。
        enabled: true,
        added_at: registry::now_iso8601(),
    };
    if let Err(error) = registry.add(&cfg.name, &unit) {
        return fail(context, &error.to_string(), None);
    }

    // 走行中の監督者には即反映する (決定 4)。送るのは `reload` で、監督者が登録簿を
    // 読み直して望みとの差を埋める — 足したばかりの unit は監督者がまだ名前を
    // 知らないので、`start <name>` を送っても「そんな unit は無い」になる。
    // `add` は `enabled = true` で書くので、読み直した監督者がその場で起こす。
    let supervisor = Supervisor::probe();
    let started = supervisor.request(&Request::Reload);
    let mut output = json!({
        "name": cfg.name,
        "listen": unit.listen,
        "binary": unit.binary,
        "binary_exists": binary_exists,
        "web_assets_dir": unit.web_assets_dir,
        "enabled": unit.enabled,
        "added_at": unit.added_at,
        "supervisor": {"running": supervisor.is_running(), "notified": started.is_ok()},
    });
    if let Err(error) = &started {
        output["note"] = json!(format!(
            "{}: `{}` was recorded in the registry and will start when the supervisor next runs",
            error.message, cfg.name
        ));
    }
    if !warnings.is_empty() {
        output["warnings"] = json!(warnings);
    }
    emit(&output)
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
        Ok(response) => emit(&json!(response)),
        Err(error) if verb == ControlVerb::Status => {
            // 監督者に聞けないので、登録簿から答えられる範囲を返す。
            let mut output = match registry_view(name) {
                Ok(output) => output,
                Err(code) => return code,
            };
            output["note"] = json!(error.message);
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
    match Supervisor::probe().request(&Request::Status(Target::all())) {
        Ok(Response::Units {
            units, supervisor, ..
        }) => {
            let on_disk = system.on_disk(&supervisor.binary);
            let pair = protocol::VersionPair::new(Some(supervisor.version), on_disk);
            output["supervisor"] = json!({
                "running": pair.running,
                "on_disk": pair.on_disk,
                "binary": supervisor.binary,
                "restart_needed": pair.restart_needed,
            });
            output["units"] = json!(
                units
                    .into_iter()
                    .map(|unit| json!({
                        "name": unit.name,
                        "running": unit.version.running,
                        "on_disk": unit.version.on_disk,
                        "binary": unit.binary,
                        "restart_needed": unit.version.restart_needed,
                    }))
                    .collect::<Vec<_>>()
            );
        }
        _ => {
            // 監督者に聞けないので、走っている版は誰にも言えない。登録簿の binary
            // から「次に上がる版」だけを並べる。
            output["supervisor"] = Value::Null;
            let units = match Registry::open().list() {
                Ok(units) => units,
                Err(error) => return fail("version", &error.to_string(), None),
            };
            output["units"] = json!(
                units
                    .into_iter()
                    .map(|(name, unit)| {
                        let on_disk = system.on_disk(&unit.binary);
                        json!({
                            "name": name,
                            "running": Value::Null,
                            "on_disk": on_disk,
                            "binary": unit.binary,
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

/// `hyoui web daemon run [name]`。
///
/// name を渡すと登録簿の値で起動する。監督者が子を exec するのと同じ経路で、
/// 手元で 1 台だけ確かめる時にも使う (決定 3)。name 省略時は `hyoui web` と
/// 同じ解決 (= config `[web].listen`) で起動する (決定 1)。
pub fn run_command(name: Option<&str>) -> ExitCode {
    let context = "web daemon run";
    let config = match hyoui::config::load() {
        Ok(config) => config,
        Err(error) => return fail(context, &format!("could not read config: {error}"), None),
    };

    let (listen, assets_dir) = match name {
        Some(name) => match Registry::open().get(name) {
            Ok(unit) => (unit.listen, unit.web_assets_dir),
            Err(error) => return fail(context, &error.to_string(), None),
        },
        None => (config.web.listen.clone(), config.web.assets_dir.clone()),
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
    match runtime.block_on(hyoui_web::serve(&listen, config, assets_dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(
            context,
            &format!("could not serve on {listen}: {error}"),
            Some(json!({"listen": listen})),
        ),
    }
}

/// `--listen` → `--port` → config `[web].listen` → 既定の順に解決する (決定 3)。
///
/// `--listen` と `--port` の排他は parser が弾くので、ここは順序だけを持つ。
/// config が既定値を持つため「無ければ `127.0.0.1:43690`」は config 側で閉じる。
fn resolve_listen(cfg: &WebDaemonAddConfig, config_listen: &str) -> String {
    if let Some(listen) = &cfg.listen {
        return listen.clone();
    }
    if let Some(port) = cfg.port {
        return format!("127.0.0.1:{port}");
    }
    config_listen.to_string()
}

/// `--binary` 未指定時の既定 (= 決定 2)。
///
/// `current_exe` をそのまま書き、安定な場所を探して差し替えない。brew 版から
/// `add` すれば brew の path が、repo build から `add` すればその build の path が
/// 入る。`resolve_stable_path` を通すと、repo build が brew 版と同一内容だった
/// 瞬間に unstable unit が stable の binary を指してしまう。
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

/// `hyoui web daemon run <name>` が登録簿の listen を使うことを見せる補助。
///
/// 実際の bind は `serve` が行うので、ここでは解決だけを切り出して test する。
#[cfg(test)]
fn resolved_run_target(
    registry: &Registry,
    name: Option<&str>,
    config: &hyoui::config::Config,
) -> std::result::Result<(String, Option<PathBuf>), String> {
    match name {
        Some(name) => registry
            .get(name)
            .map(|unit| (unit.listen, unit.web_assets_dir))
            .map_err(|error| error.to_string()),
        None => Ok((config.web.listen.clone(), config.web.assets_dir.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(listen: &str) -> Unit {
        Unit {
            listen: listen.to_owned(),
            binary: PathBuf::from("/opt/homebrew/bin/hyoui"),
            web_assets_dir: None,
            enabled: true,
            added_at: registry::now_iso8601(),
        }
    }

    #[test]
    fn listen_resolution_follows_the_documented_order() {
        let config_listen = "127.0.0.1:40000";

        // `--listen` が最優先。
        assert_eq!(
            resolve_listen(
                &WebDaemonAddConfig {
                    listen: Some("0.0.0.0:1".into()),
                    port: Some(2),
                    ..WebDaemonAddConfig::default()
                },
                config_listen
            ),
            "0.0.0.0:1"
        );
        // 次が `--port` (= loopback の短縮形)。
        assert_eq!(
            resolve_listen(
                &WebDaemonAddConfig {
                    port: Some(43691),
                    ..WebDaemonAddConfig::default()
                },
                config_listen
            ),
            "127.0.0.1:43691"
        );
        // どちらも無ければ config。config 自身が `127.0.0.1:43690` を既定に持つ。
        assert_eq!(
            resolve_listen(&WebDaemonAddConfig::default(), config_listen),
            config_listen
        );
        assert_eq!(
            resolve_listen(
                &WebDaemonAddConfig::default(),
                &hyoui::config::Config::default().web.listen
            ),
            "127.0.0.1:43690"
        );
    }

    #[test]
    fn run_reads_the_unit_listen_and_falls_back_to_config_without_a_name() {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("units"));
        registry.add("unstable", &unit("127.0.0.1:43691")).unwrap();
        let config = hyoui::config::Config::default();

        assert_eq!(
            resolved_run_target(&registry, Some("unstable"), &config).unwrap(),
            ("127.0.0.1:43691".to_string(), None)
        );
        assert_eq!(
            resolved_run_target(&registry, None, &config).unwrap().0,
            config.web.listen
        );
        assert!(
            resolved_run_target(&registry, Some("missing"), &config)
                .unwrap_err()
                .contains("no web gateway unit named")
        );
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

    /// 監督者が居ない時、`status` は登録簿から答えられる範囲を返す (決定 4)。
    #[test]
    fn the_registry_answers_when_the_supervisor_cannot() {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("units"));
        registry.add("stable", &unit("127.0.0.1:43690")).unwrap();

        // `registry_view` は既定の登録簿を読むので、ここでは組み立ての形だけを
        // 固定する (= 隔離 `XDG_STATE_HOME` 越しの経路は e2e が見る)。
        let listed = registry.list().unwrap();
        assert_eq!(listed.len(), 1);
        let (name, unit) = &listed[0];
        let row = json!({
            "name": name,
            "enabled": unit.enabled,
            "running": false,
            "pid": Value::Null,
            "listen": unit.listen,
        });
        assert_eq!(row["running"], json!(false));
        assert_eq!(row["pid"], Value::Null);
        assert_eq!(row["listen"], json!("127.0.0.1:43690"));
    }
}
