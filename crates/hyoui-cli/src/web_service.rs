//! `hyoui web service register|unregister|status` の OS service 管理層。
//!
//! [`ServiceDefinition`] と 2 renderer は全 OS で compile する純粋ロジック、
//! [`Backend`] の shell-out だけを macOS / Linux で `cfg` 分離する (= DR-0031)。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use stable_which::{Candidate, ScoringPolicy, resolve_stable_path};

pub const MACOS_LABEL: &str = "com.github.kawaz.hyoui-web";
pub const LINUX_LABEL: &str = "hyoui-web";
const SERVICE_PATH: &str =
    "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

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
    pub fn for_web(program: &str, listen: Option<&str>, log_path: Option<String>) -> Self {
        let mut program_args = vec![program.to_string(), "web".to_string()];
        if let Some(listen) = listen {
            program_args.push("--listen".to_string());
            program_args.push(listen.to_string());
        }
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), SERVICE_PATH.to_string());
        Self {
            label: default_label().to_string(),
            program_args,
            env,
            log_path,
            associated_bundle_identifiers: None,
        }
    }
}

pub fn default_label() -> &'static str {
    if cfg!(target_os = "macos") {
        MACOS_LABEL
    } else {
        LINUX_LABEL
    }
}

pub fn default_log_path() -> Option<String> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Logs/hyoui-web/output.log")
            .to_string_lossy()
            .into_owned()
    })
}

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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub registered: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub definition_path: PathBuf,
}

impl ServiceStatus {
    pub fn render(&self, label: &str) -> String {
        format!(
            "label:      {label}\nregistered: {}\nrunning:    {}\npid:        {}\ndefinition: {}\n",
            if self.registered { "yes" } else { "no" },
            if self.running { "yes" } else { "no" },
            self.pid
                .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
            self.definition_path.display()
        )
    }
}

pub trait Backend {
    fn definition_path(&self, label: &str) -> Result<PathBuf, String>;
    fn register(&self, def: &ServiceDefinition) -> Result<(), String>;
    fn unregister(&self, label: &str) -> Result<(), String>;
    fn status(&self, label: &str) -> Result<ServiceStatus, String>;
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

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "$HOME is not set; cannot resolve the per-user service path".to_string())
}

#[cfg(target_os = "linux")]
fn systemd_config_home() -> Result<PathBuf, String> {
    Ok(std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(home_dir()?.join(".config")))
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

    fn register(&self, def: &ServiceDefinition) -> Result<(), String> {
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
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
        }
    }

    fn status(&self, label: &str) -> Result<ServiceStatus, String> {
        let path = self.definition_path(label)?;
        let output = command_output("launchctl", &["print", &Self::target(label)])?;
        let pid = if output.status.success() {
            parse_launchctl_pid(&String::from_utf8_lossy(&output.stdout))
        } else {
            None
        };
        Ok(ServiceStatus {
            registered: path.is_file(),
            running: pid.is_some(),
            pid,
            definition_path: path,
        })
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

    fn register(&self, def: &ServiceDefinition) -> Result<(), String> {
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
        let active = command_output("systemctl", &["--user", "is-active", &unit])?;
        let running = String::from_utf8_lossy(&active.stdout).trim() == "active";
        let pid = if running {
            let output = command_output(
                "systemctl",
                &["--user", "show", "-p", "MainPID", "--value", &unit],
            )?;
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid != 0)
        } else {
            None
        };
        Ok(ServiceStatus {
            registered: path.is_file(),
            running,
            pid,
            definition_path: path,
        })
    }
}

pub fn parse_launchctl_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.trim().parse().ok())
    })
}

pub struct ResolvedProgram {
    pub path: PathBuf,
    pub warning: Option<String>,
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

    fn sample_definition() -> ServiceDefinition {
        ServiceDefinition::for_web(
            "/opt/homebrew/bin/hyoui",
            Some("127.0.0.1:54321"),
            Some("/Users/test/Library/Logs/hyoui-web/output.log".to_string()),
        )
    }

    /// OS 名差は label だけで、各 backend の定義 basename と 1:1 に対応する。
    #[test]
    fn labels_and_definition_paths_are_deterministic() {
        assert_eq!(
            launchd_definition_path(Path::new("/Users/test"), MACOS_LABEL),
            PathBuf::from("/Users/test/Library/LaunchAgents/com.github.kawaz.hyoui-web.plist")
        );
        assert_eq!(
            systemd_definition_path(Path::new("/home/test/.config"), LINUX_LABEL),
            PathBuf::from("/home/test/.config/systemd/user/hyoui-web.service")
        );
        assert_eq!(
            systemd_definition_path(Path::new("/x"), "already.service"),
            PathBuf::from("/x/systemd/user/already.service")
        );
    }

    /// register の listen 指定は `<program> web --listen <value>` として順序を固定する。
    #[test]
    fn definition_builds_web_command_and_minimal_path() {
        let def = sample_definition();
        assert_eq!(
            def.program_args,
            [
                "/opt/homebrew/bin/hyoui",
                "web",
                "--listen",
                "127.0.0.1:54321"
            ]
        );
        assert_eq!(def.env.len(), 1);
        assert_eq!(def.env.get("PATH").map(String::as_str), Some(SERVICE_PATH));
    }

    /// launchd の golden は RunAtLoad + KeepAlive、共通 log、最小 PATH を固定する。
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
\t<string>com.github.kawaz.hyoui-web</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>/opt/homebrew/bin/hyoui</string>\n\
\t\t<string>web</string>\n\
\t\t<string>--listen</string>\n\
\t\t<string>127.0.0.1:54321</string>\n\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
\t<key>KeepAlive</key>\n\
\t<true/>\n\
\t<key>EnvironmentVariables</key>\n\
\t<dict>\n\
\t\t<key>PATH</key>\n\
\t\t<string>/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n\
\t</dict>\n\
\t<key>StandardOutPath</key>\n\
\t<string>/Users/test/Library/Logs/hyoui-web/output.log</string>\n\
\t<key>StandardErrorPath</key>\n\
\t<string>/Users/test/Library/Logs/hyoui-web/output.log</string>\n\
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
ExecStart=\"/opt/homebrew/bin/hyoui\" \"web\" \"--listen\" \"127.0.0.1:54321\"\n\
Restart=always\n\
Environment=\"PATH=/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\"\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
        assert!(!render_systemd_unit(&def).contains("StandardOutput="));
    }

    /// 各 format の補間値は document boundary を越えないよう escape する。
    #[test]
    fn renderers_escape_interpolated_values() {
        let mut def = ServiceDefinition::for_web("/tmp/a&b<hyoui>", Some("a b%z\n"), None);
        def.label = "x&y<z>".to_string();
        let plist = render_launchd_plist(&def);
        assert!(plist.contains("x&amp;y&lt;z&gt;"));
        assert!(plist.contains("/tmp/a&amp;b&lt;hyoui&gt;"));
        let unit = render_systemd_unit(&def);
        assert!(unit.contains("\"a b%%z\\n\""));
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

    /// status は未登録・停止時の空 pid を `-` として表示する。
    #[test]
    fn status_render_covers_registered_and_unregistered_states() {
        let registered = ServiceStatus {
            registered: true,
            running: true,
            pid: Some(42),
            definition_path: PathBuf::from("/x/service"),
        };
        assert!(registered.render("label").contains("pid:        42"));
        let absent = ServiceStatus {
            registered: false,
            running: false,
            pid: None,
            definition_path: PathBuf::from("/x/service"),
        };
        let text = absent.render("label");
        assert!(text.contains("registered: no"));
        assert!(text.contains("pid:        -"));
    }
}
