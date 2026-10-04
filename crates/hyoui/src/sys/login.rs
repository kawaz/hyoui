//! `hyoui run --login` の子起動仕様 (DR-0039 決定 1)。
//!
//! 通常のターミナルアプリと同じログイン shell として起動するため、(1) shell は passwd
//! から引く (呼び出し元や gateway の `$SHELL` は信用しない)、(2) argv[0] は
//! `-<shell の basename>`、(3) 子の environ は呼び出し元から引き継がず最小から始める。
//! rc は shell 自身が読む。コマンドを明示した場合はそのコマンドを argv[0] そのままで、
//! environ だけ最小にする。
//!
//! hyoui 自身 (daemon) の environ には触れない。ここが作るのは子に渡す environ だけ。
//! 判定は引数で受け取った値だけで行う pure 関数に寄せ、実環境の読み取りは
//! [`current_user`] / [`initial_path`] / [`LoginCaller::from_env`] に閉じ込める。

use std::path::Path;

use super::error::{Error, Result};

/// passwd から引いたログインユーザ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginUser {
    /// ユーザ名 (`USER` / `LOGNAME`)。
    pub name: String,
    /// ホーム dir (`HOME`)。
    pub home: String,
    /// ログイン shell の絶対 path (`SHELL`、exec 先)。
    pub shell: String,
}

/// 呼び出し元の env のうち、最小 env に引き継ぐ値。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoginCaller {
    /// `LANG` (在れば引き継ぎ、無ければ付けない)。
    pub lang: Option<String>,
    /// `TERM` (PTY の端末種別。在れば引き継ぐ)。
    pub term: Option<String>,
    /// 明示コマンドの `PATH` 探索に使う呼び出し元の `PATH` (子には渡さない)。
    pub path_for_lookup: Option<String>,
}

impl LoginCaller {
    /// 現在の process env から読む (= daemon child、起動時の env)。
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            lang: get("LANG"),
            term: get("TERM"),
            path_for_lookup: get("PATH"),
        }
    }
}

/// `--login` の解決結果。`argv` は子の argv、`path` が exec 先、`env` が子の environ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginPlan {
    /// 子の argv (argv[0] を含む)。
    pub argv: Vec<String>,
    /// `execve` する実体の path。
    pub path: String,
    /// 子の environ (hyoui の注入 env は含まない、呼び出し側が足す)。
    pub env: Vec<(String, String)>,
}

/// 実 uid の passwd entry を引く (`getpwuid`)。
pub fn current_user() -> Result<LoginUser> {
    let u = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .map_err(Error::from)?
        .ok_or(Error::Invalid("passwd entry for the current uid not found"))?;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    Ok(LoginUser {
        name: u.name,
        home: s(&u.dir),
        shell: s(&u.shell),
    })
}

/// login shell の argv[0] (= `-<basename>`)。
pub fn login_argv0(shell: &str) -> String {
    let base = Path::new(shell)
        .file_name()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_else(|| shell.to_string());
    format!("-{base}")
}

/// login が組み立てる初期 `PATH`。macOS は `/etc/paths` → `/etc/paths.d/*` (名前順) の
/// 順 (`path_helper` と同じ)、それ以外は固定の既定値。
pub fn initial_path() -> String {
    if cfg!(target_os = "macos") {
        let mut parts = read_path_file(Path::new("/etc/paths"));
        let mut files: Vec<_> = std::fs::read_dir("/etc/paths.d")
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            parts.extend(read_path_file(&f));
        }
        let joined = join_path_entries(parts);
        if !joined.is_empty() {
            return joined;
        }
    }
    DEFAULT_PATH.to_string()
}

/// `/etc/paths` が読めない / 非 macOS のときの既定 `PATH`。
pub const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

fn read_path_file(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .map(|s| parse_path_lines(&s))
        .unwrap_or_default()
}

/// 1 行 1 entry、空行を除いて trim。
pub fn parse_path_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// entry を順序保存・重複除去で `:` 連結する。
pub fn join_path_entries(entries: Vec<String>) -> String {
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        .filter(|e| seen.insert(e.clone()))
        .collect::<Vec<_>>()
        .join(":")
}

/// 最小 env (`HOME` / `USER` / `LOGNAME` / `SHELL` / `PATH` + 在れば `LANG` / `TERM`)。
pub fn minimal_env(user: &LoginUser, path: &str, caller: &LoginCaller) -> Vec<(String, String)> {
    let mut env = vec![
        ("HOME".to_string(), user.home.clone()),
        ("USER".to_string(), user.name.clone()),
        ("LOGNAME".to_string(), user.name.clone()),
        ("SHELL".to_string(), user.shell.clone()),
        ("PATH".to_string(), path.to_string()),
    ];
    if let Some(l) = &caller.lang {
        env.push(("LANG".to_string(), l.clone()));
    }
    if let Some(t) = &caller.term {
        env.push(("TERM".to_string(), t.clone()));
    }
    env
}

/// `name` を `path_list` (= `:` 区切り) から探す。`/` を含む name はそのまま返す。
pub fn resolve_in_path(name: &str, path_list: &str) -> Option<String> {
    if name.contains('/') {
        return Some(name.to_string());
    }
    path_list
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| is_executable_file(p))
        .map(|p| p.to_string_lossy().into_owned())
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// `--login` を解決する。`command` が空なら login shell (argv[0] は `-<shell>`)、
/// 明示されていればそのコマンドを argv そのままで、environ だけ最小にする。
pub fn plan(
    user: &LoginUser,
    initial_path: &str,
    caller: &LoginCaller,
    command: &[String],
) -> LoginPlan {
    let env = minimal_env(user, initial_path, caller);
    if command.is_empty() {
        return LoginPlan {
            argv: vec![login_argv0(&user.shell)],
            path: user.shell.clone(),
            env,
        };
    }
    // 明示コマンドの実体は、呼び出し元の PATH → 最小 env の PATH の順に探す
    // (= `hyoui run -- zsh` が呼び出し元で見える command を同じに解決する)。
    let path = caller
        .path_for_lookup
        .as_deref()
        .and_then(|p| resolve_in_path(&command[0], p))
        .or_else(|| resolve_in_path(&command[0], initial_path))
        .unwrap_or_else(|| command[0].clone());
    LoginPlan {
        argv: command.to_vec(),
        path,
        env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> LoginUser {
        LoginUser {
            name: "alice".into(),
            home: "/home/alice".into(),
            shell: "/bin/zsh".into(),
        }
    }

    fn get<'a>(env: &'a [(String, String)], k: &str) -> Option<&'a str> {
        env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn argv0_is_dash_basename() {
        assert_eq!(login_argv0("/bin/zsh"), "-zsh");
        assert_eq!(login_argv0("/opt/homebrew/bin/fish"), "-fish");
        assert_eq!(login_argv0("bash"), "-bash");
    }

    #[test]
    fn login_without_command_uses_passwd_shell() {
        let p = plan(&user(), "/usr/bin:/bin", &LoginCaller::default(), &[]);
        assert_eq!(p.argv, vec!["-zsh"]);
        assert_eq!(p.path, "/bin/zsh");
    }

    #[test]
    fn explicit_command_keeps_argv_without_dash() {
        let cmd: Vec<String> = ["/bin/zsh", "-f"].map(String::from).to_vec();
        let p = plan(&user(), "/usr/bin:/bin", &LoginCaller::default(), &cmd);
        assert_eq!(p.argv, cmd);
        assert_eq!(p.path, "/bin/zsh");
    }

    #[test]
    fn env_is_minimal_and_excludes_caller_values() {
        let caller = LoginCaller {
            lang: Some("ja_JP.UTF-8".into()),
            term: Some("xterm-256color".into()),
            path_for_lookup: Some("/caller/bin".into()),
        };
        let p = plan(&user(), "/usr/bin:/bin", &caller, &[]);
        let names: Vec<&str> = p.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            ["HOME", "USER", "LOGNAME", "SHELL", "PATH", "LANG", "TERM"]
        );
        assert_eq!(get(&p.env, "PATH"), Some("/usr/bin:/bin"));
        assert_eq!(get(&p.env, "SHELL"), Some("/bin/zsh"));
    }

    #[test]
    fn lang_and_term_omitted_when_caller_has_none() {
        let p = plan(&user(), "/bin", &LoginCaller::default(), &[]);
        assert_eq!(get(&p.env, "LANG"), None);
        assert_eq!(get(&p.env, "TERM"), None);
        assert_eq!(p.env.len(), 5);
    }

    #[test]
    fn path_lines_parse_and_dedup() {
        assert_eq!(
            parse_path_lines("/usr/local/bin\n\n  /usr/bin \n/bin\n"),
            ["/usr/local/bin", "/usr/bin", "/bin"]
        );
        let j = join_path_entries(["/a", "/b", "/a", "/c"].map(String::from).to_vec());
        assert_eq!(j, "/a:/b:/c");
    }

    #[test]
    fn resolve_in_path_finds_executable_and_passes_slash_through() {
        assert_eq!(resolve_in_path("./x", "/bin").as_deref(), Some("./x"));
        assert_eq!(
            resolve_in_path("sh", "/nonexistent:/bin").as_deref(),
            Some("/bin/sh")
        );
        assert_eq!(resolve_in_path("no-such-cmd-hyoui", "/bin:/usr/bin"), None);
    }

    #[test]
    fn initial_path_is_nonempty_and_absolute_entries() {
        let p = initial_path();
        assert!(!p.is_empty());
        assert!(p.split(':').all(|e| e.starts_with('/')));
    }

    #[test]
    fn current_user_has_absolute_shell_and_home() {
        let u = current_user().expect("passwd");
        assert!(!u.name.is_empty());
        assert!(u.shell.starts_with('/'));
        assert!(u.home.starts_with('/'));
    }
}
