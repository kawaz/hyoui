//! 置き場所 (config / state / runtime) を env から導く唯一の口 (DR-0038 決定 5)。
//!
//! 場所の導出に効く env は [`LocationVar`] に列挙したものだけで、導出はすべて
//! [`Env`] (= その列挙を読んだ snapshot) を通す。`web service register` が unit に
//! 固定する変数もこの列挙そのもので、unit 生成側に別のリストを持たない
//! (reference `cli-daemon-subcommands`「`service register` は場所を決める env を
//! unit に固定し、変わったら止まる」)。
//!
//! 空文字の値は未設定と同じに扱う (= `var_os` が空を返す異常ケースに揃える)。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// 置き場所の導出に効く env。
///
/// 足す時は [`Env`] の導出がそれを読むようにすること。ここに無い変数を導出が
/// 読むと、launchd / systemd から起きた監督者と shell から起きた CLI で場所が
/// 食い違っても誰も気づけない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocationVar {
    /// `$HOME` (= XDG が無い時の fallback の起点)。
    Home,
    /// `$XDG_CONFIG_HOME` (= config の置き場)。
    XdgConfigHome,
    /// `$XDG_STATE_HOME` (= 状態と session socket の置き場)。
    XdgStateHome,
    /// `$XDG_RUNTIME_DIR` (= session socket の第 1 候補、DR-0018)。
    XdgRuntimeDir,
}

impl LocationVar {
    /// 全変数 (= unit に固定する一覧の正本)。
    pub const ALL: [LocationVar; 4] = [
        LocationVar::Home,
        LocationVar::XdgConfigHome,
        LocationVar::XdgStateHome,
        LocationVar::XdgRuntimeDir,
    ];

    /// env の名前。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LocationVar::Home => "HOME",
            LocationVar::XdgConfigHome => "XDG_CONFIG_HOME",
            LocationVar::XdgStateHome => "XDG_STATE_HOME",
            LocationVar::XdgRuntimeDir => "XDG_RUNTIME_DIR",
        }
    }
}

/// [`LocationVar`] の値の snapshot。導出はすべてここを通す。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    home: Option<OsString>,
    xdg_config_home: Option<OsString>,
    xdg_state_home: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
}

impl Env {
    /// この process の env を読む。
    #[must_use]
    pub fn current() -> Self {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    /// 名前から値を引く関数で組み立てる (= test が process env を触らずに済む口)。
    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Self {
        let mut read = |var: LocationVar| lookup(var.name()).filter(|value| !value.is_empty());
        Self {
            home: read(LocationVar::Home),
            xdg_config_home: read(LocationVar::XdgConfigHome),
            xdg_state_home: read(LocationVar::XdgStateHome),
            xdg_runtime_dir: read(LocationVar::XdgRuntimeDir),
        }
    }

    /// 変数 1 つの値 (= 空文字は `None`)。
    #[must_use]
    pub fn get(&self, var: LocationVar) -> Option<&OsStr> {
        match var {
            LocationVar::Home => self.home.as_deref(),
            LocationVar::XdgConfigHome => self.xdg_config_home.as_deref(),
            LocationVar::XdgStateHome => self.xdg_state_home.as_deref(),
            LocationVar::XdgRuntimeDir => self.xdg_runtime_dir.as_deref(),
        }
    }

    /// `$HOME`。
    #[must_use]
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref().map(Path::new)
    }

    /// `$XDG_RUNTIME_DIR`。
    #[must_use]
    pub fn runtime_dir(&self) -> Option<&Path> {
        self.xdg_runtime_dir.as_deref().map(Path::new)
    }

    /// `${XDG_CONFIG_HOME:-$HOME/.config}`。どちらも無ければ `None`。
    #[must_use]
    pub fn config_home(&self) -> Option<PathBuf> {
        match (&self.xdg_config_home, &self.home) {
            (Some(xdg), _) => Some(PathBuf::from(xdg)),
            (None, Some(home)) => Some(Path::new(home).join(".config")),
            (None, None) => None,
        }
    }

    /// `${XDG_STATE_HOME:-$HOME/.local/state}`。どちらも無ければ `None`。
    #[must_use]
    pub fn state_home(&self) -> Option<PathBuf> {
        match (&self.xdg_state_home, &self.home) {
            (Some(xdg), _) => Some(PathBuf::from(xdg)),
            (None, Some(home)) => Some(Path::new(home).join(".local/state")),
            (None, None) => None,
        }
    }

    /// hyoui の config dir (`<config_home>/hyoui`)。
    #[must_use]
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_home().map(|dir| dir.join("hyoui"))
    }

    /// hyoui の state dir (`<state_home>/hyoui`)。session socket の base でもある
    /// (DR-0018)。
    #[must_use]
    pub fn state_dir(&self) -> Option<PathBuf> {
        self.state_home().map(|dir| dir.join("hyoui"))
    }

    /// web の config の既定の置き場 (`<config_dir>/web`、DR-0038 決定 4)。
    #[must_use]
    pub fn web_config_dir(&self) -> Option<PathBuf> {
        self.config_dir().map(|dir| dir.join("web"))
    }

    /// web の状態の置き場 (`<state_dir>/web`、DR-0038 決定 4)。
    ///
    /// 登録簿 `units/`、`logs/`、passkey の `auth.json` / `pending.json`、監督者の
    /// socket をここにまとめる。どちらの env も無ければ相対 path
    /// (`.local/state/hyoui/web`) に倒す — 起動を断るより、cwd 相対でも動く方を
    /// 選ぶ (`$HOME` の無い環境はほぼ無い)。
    #[must_use]
    pub fn web_state_dir(&self) -> PathBuf {
        self.state_dir()
            .unwrap_or_else(|| PathBuf::from(".local/state/hyoui"))
            .join("web")
    }

    /// `~` / `~/...` を `$HOME` で開く。それ以外はそのまま返す。
    ///
    /// config に書いた path は台をまたぐ dotfiles に置かれるので、`~` で書ける
    /// 必要がある。`$HOME` が無ければ開けないのでそのまま返す。
    #[must_use]
    pub fn expand_tilde(&self, path: &Path) -> PathBuf {
        let Some(home) = self.home() else {
            return path.to_path_buf();
        };
        let mut components = path.components();
        match components.next() {
            Some(std::path::Component::Normal(first)) if first == "~" => {
                home.join(components.as_path())
            }
            _ => path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> Env {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(v)))
            .collect();
        Env::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn xdg_wins_over_home_and_empty_means_unset() {
        let e = env(&[
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_STATE_HOME", ""),
        ]);
        assert_eq!(e.config_dir(), Some(PathBuf::from("/c/hyoui")));
        assert_eq!(e.state_dir(), Some(PathBuf::from("/h/.local/state/hyoui")));
        assert_eq!(e.get(LocationVar::XdgStateHome), None);
    }

    /// web の置き場は CLI の階層 (`hyoui web ...`) と同じく `hyoui/web/` の下 (決定 4)。
    #[test]
    fn web_places_live_under_hyoui_web() {
        let e = env(&[("HOME", "/h")]);
        assert_eq!(
            e.web_config_dir(),
            Some(PathBuf::from("/h/.config/hyoui/web"))
        );
        assert_eq!(
            e.web_state_dir(),
            PathBuf::from("/h/.local/state/hyoui/web")
        );

        let e = env(&[
            ("HOME", "/h"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CONFIG_HOME", "/c"),
        ]);
        assert_eq!(e.web_config_dir(), Some(PathBuf::from("/c/hyoui/web")));
        assert_eq!(e.web_state_dir(), PathBuf::from("/s/hyoui/web"));

        assert_eq!(
            Env::default().web_state_dir(),
            PathBuf::from(".local/state/hyoui/web")
        );
        assert_eq!(Env::default().web_config_dir(), None);
    }

    #[test]
    fn every_location_var_is_read_by_the_snapshot() {
        let pairs: Vec<(&str, String)> = LocationVar::ALL
            .iter()
            .map(|var| (var.name(), format!("/v/{}", var.name())))
            .collect();
        let e = Env::from_lookup(|name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        });
        for var in LocationVar::ALL {
            assert_eq!(
                e.get(var),
                Some(OsStr::new(&format!("/v/{}", var.name()))),
                "{var:?}"
            );
        }
    }

    #[test]
    fn tilde_expands_only_as_the_first_component() {
        let e = env(&[("HOME", "/h")]);
        assert_eq!(e.expand_tilde(Path::new("~")), PathBuf::from("/h"));
        assert_eq!(
            e.expand_tilde(Path::new("~/bin/hyoui")),
            PathBuf::from("/h/bin/hyoui")
        );
        assert_eq!(e.expand_tilde(Path::new("a/~/b")), PathBuf::from("a/~/b"));
        assert_eq!(e.expand_tilde(Path::new("~x/b")), PathBuf::from("~x/b"));
        assert_eq!(
            Env::default().expand_tilde(Path::new("~/x")),
            PathBuf::from("~/x")
        );
    }
}
