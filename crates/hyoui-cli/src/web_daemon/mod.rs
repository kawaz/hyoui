//! `hyoui web daemon run|add|remove|list` (= DR-0034 P2)。
//!
//! unit の属性は [`registry`] が正本で、プロセスの生死は監督者が持つ。本 module は
//! 登録簿の読み書きと、登録簿の値で gateway を foreground 起動する経路を持つ。
//! 監督者への要求 (`start` / `stop` / `restart` / `status` / `log`) は DR-0034 P3。
//!
//! 出力は help 以外すべて JSON、エラーは JSON を stderr に出して非 0 で終わる
//! (= reference `cli-daemon-subcommands` の出力規約)。

pub mod registry;

use std::path::PathBuf;
use std::process::ExitCode;

use hyoui::cli::WebDaemonAddConfig;
use serde_json::{Value, json};

use registry::{ListenConflict, Registry, Unit};

/// 監督者の生死。要求の送信自体は DR-0034 P3。
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

    /// 監督者に unit の起動 / 停止を要求する (= DR-0034 P3 で実装)。
    ///
    /// 呼び出し口だけを先に置く。P2 では要求を送れないので、送れなかった理由を
    /// 返して呼び出し側が出力に添える。監督者が不在でも登録簿の変更は成立する
    /// (決定 4 の表: 次に監督者が上がった時に起きる)。
    fn request(self, verb: &str, name: &str) -> std::result::Result<(), String> {
        match self {
            Self::NotRunning => Err(format!(
                "the supervisor is not running, so `{name}` was only recorded in the registry; \
                 start it with `hyoui web service start` or run `hyoui web daemon supervise`"
            )),
            Self::Running => Err(format!(
                "the supervisor is running but `{verb} {name}` could not be requested: \
                 the control socket protocol is not implemented yet (DR-0034 P3)"
            )),
        }
    }
}

/// `hyoui web daemon list`。
///
/// 登録簿を読むだけで答えられるので、監督者が停止していても断らない (決定 4)。
pub fn list_command() -> ExitCode {
    let registry = Registry::open();
    let units = match registry.list() {
        Ok(units) => units,
        Err(error) => return fail("web daemon list", &error.to_string(), None),
    };
    let supervisor = Supervisor::probe();

    let rows: Vec<Value> = units
        .iter()
        .map(|(name, unit)| {
            json!({
                "name": name,
                "enabled": unit.enabled,
                // 監督者に聞けないものは推し量らない (= 聞けていない台に「動いて
                // いる」と書かない)。P3 で監督者に聞いた値が入る。
                "running": false,
                "pid": Value::Null,
                "listen": unit.listen,
                "binary": unit.binary,
                "binary_exists": unit.binary.exists(),
                "web_assets_dir": unit.web_assets_dir,
                "added_at": unit.added_at,
            })
        })
        .collect();

    let mut output = json!({
        "units": rows,
        "supervisor": {"running": supervisor.is_running()},
        "registry_dir": registry.dir(),
    });
    if !supervisor.is_running() {
        output["note"] = json!(
            "the supervisor is not running, so `running` and `pid` are not known for any unit"
        );
    }
    emit(&output)
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

    let supervisor = Supervisor::probe();
    let started = supervisor.request("start", &cfg.name);
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
    if let Err(note) = started {
        output["note"] = json!(note);
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
    let stopped = supervisor.request("stop", name);
    if supervisor.is_running()
        && let Err(error) = &stopped
    {
        // 停止を頼めないまま登録簿から消すと、監督者が抱えている子の所在が
        // 登録簿から読めなくなる。消す前に止まる。
        return fail(
            context,
            error,
            Some(json!({"name": name, "supervisor": {"running": true, "notified": false}})),
        );
    }

    if let Err(error) = registry.remove(name) {
        return fail(context, &error.to_string(), None);
    }

    let mut output = json!({
        "name": name,
        "removed": true,
        "supervisor": {"running": supervisor.is_running(), "notified": false},
    });
    if let Err(note) = stopped {
        output["note"] = json!(note);
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

    /// 監督者が居ない間も登録簿の変更は成立し、理由が出力に残る (決定 4)。
    #[test]
    fn an_absent_supervisor_explains_why_nothing_started() {
        let note = Supervisor::NotRunning
            .request("start", "unstable")
            .unwrap_err();
        assert!(note.contains("not running"), "{note}");
        assert!(note.contains("hyoui web service start"), "{note}");
        assert!(!Supervisor::NotRunning.is_running());
    }

    /// 走っている監督者への要求は P3 まで送れない。panic ではなく理由を返す。
    #[test]
    fn a_running_supervisor_reports_the_unimplemented_request_path() {
        let note = Supervisor::Running.request("stop", "unstable").unwrap_err();
        assert!(note.contains("not implemented"), "{note}");
        assert!(Supervisor::Running.is_running());
    }

    #[test]
    fn the_default_binary_is_this_executable() {
        assert_eq!(default_binary().unwrap(), std::env::current_exe().unwrap());
    }
}
