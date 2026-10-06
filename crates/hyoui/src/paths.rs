//! 置き場所 (config / state) を env から導く唯一の口 (DR-0038 決定 5)。
//!
//! 場所の導出に効く env は [`LocationVar`] に列挙したものだけで、導出はすべて
//! [`Env`] (= その列挙を読んだ snapshot) を通す。`web service register` が unit に
//! 固定する変数もこの列挙そのもので、unit 生成側に別のリストを持たない
//! (reference `cli-daemon-subcommands`「`service register` は場所を決める env を
//! unit に固定し、変わったら止まる」)。
//!
//! # 面と状態の root (DR-0041 決定 6)
//!
//! 面は状態の root 1 つで決まり、hyoui の一式 (session の socket、web の監督者・
//! unit・登録簿・passkey・logs) はその root の中で完結する。root は
//! `$HYOUI_STATE_DIR` → `$XDG_STATE_HOME/hyoui` (絶対パスの時だけ) →
//! `$HOME/.local/state/hyoui` の順に決め、どれも無ければエラーにする (cwd 相対には
//! 倒さない)。config は面で分けず、全部の面が `${XDG_CONFIG_HOME:-$HOME/.config}/hyoui`
//! を共有する。
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
    /// `$XDG_STATE_HOME` (= 状態の root の第 2 候補)。
    XdgStateHome,
    /// `$HYOUI_STATE_DIR` (= 面を決める hyoui 専用の変数、状態の root の第 1 候補)。
    HyouiStateDir,
}

impl LocationVar {
    /// 全変数 (= unit に固定する一覧の正本)。
    pub const ALL: [LocationVar; 4] = [
        LocationVar::Home,
        LocationVar::XdgConfigHome,
        LocationVar::XdgStateHome,
        LocationVar::HyouiStateDir,
    ];

    /// env の名前。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LocationVar::Home => "HOME",
            LocationVar::XdgConfigHome => "XDG_CONFIG_HOME",
            LocationVar::XdgStateHome => "XDG_STATE_HOME",
            LocationVar::HyouiStateDir => "HYOUI_STATE_DIR",
        }
    }
}

/// 状態の root を決められない理由 (DR-0041 決定 6)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateRootError {
    /// `$HYOUI_STATE_DIR` が相対パス (= cwd 相対に倒さない)。
    RelativeStateDir(PathBuf),
    /// `$HYOUI_STATE_DIR` も、絶対パスの `$XDG_STATE_HOME` も、絶対パスの `$HOME` も無い。
    Unresolvable,
}

impl std::fmt::Display for StateRootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateRootError::RelativeStateDir(path) => write!(
                f,
                "HYOUI_STATE_DIR={} is a relative path; give an absolute path (hyoui does not resolve its state root against the current directory)",
                path.display()
            ),
            StateRootError::Unresolvable => f.write_str(
                "cannot decide hyoui's state root: set HYOUI_STATE_DIR, or an absolute XDG_STATE_HOME or HOME",
            ),
        }
    }
}

impl std::error::Error for StateRootError {}

impl From<StateRootError> for std::io::Error {
    fn from(error: StateRootError) -> Self {
        let kind = match error {
            StateRootError::RelativeStateDir(_) => std::io::ErrorKind::InvalidInput,
            StateRootError::Unresolvable => std::io::ErrorKind::NotFound,
        };
        std::io::Error::new(kind, error)
    }
}

/// [`LocationVar`] の値の snapshot。導出はすべてここを通す。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    home: Option<OsString>,
    xdg_config_home: Option<OsString>,
    xdg_state_home: Option<OsString>,
    hyoui_state_dir: Option<OsString>,
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
            hyoui_state_dir: read(LocationVar::HyouiStateDir),
        }
    }

    /// 変数 1 つの値 (= 空文字は `None`)。
    #[must_use]
    pub fn get(&self, var: LocationVar) -> Option<&OsStr> {
        match var {
            LocationVar::Home => self.home.as_deref(),
            LocationVar::XdgConfigHome => self.xdg_config_home.as_deref(),
            LocationVar::XdgStateHome => self.xdg_state_home.as_deref(),
            LocationVar::HyouiStateDir => self.hyoui_state_dir.as_deref(),
        }
    }

    /// `$HOME`。
    #[must_use]
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref().map(Path::new)
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

    /// `${XDG_STATE_HOME:-$HOME/.local/state}` (= XDG の値をそのまま使う)。どちらも
    /// 無ければ `None`。
    ///
    /// hyoui の状態の root は [`Env::state_root`] が決める。これは古い置き場
    /// (`$XDG_STATE_HOME/hyoui-web` 等) の検出のためだけにある。
    #[must_use]
    pub fn state_home(&self) -> Option<PathBuf> {
        match (&self.xdg_state_home, &self.home) {
            (Some(xdg), _) => Some(PathBuf::from(xdg)),
            (None, Some(home)) => Some(Path::new(home).join(".local/state")),
            (None, None) => None,
        }
    }

    /// hyoui の config dir (`<config_home>/hyoui`)。面で分けない (DR-0041 決定 6)。
    #[must_use]
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_home().map(|dir| dir.join("hyoui"))
    }

    /// hyoui の状態の root (= 面、DR-0041 決定 6)。
    ///
    /// 1. `$HYOUI_STATE_DIR` (空でなければそのまま使う。`hyoui/` を足さない)
    /// 2. `$XDG_STATE_HOME/hyoui` (絶対パスの時だけ)
    /// 3. `$HOME/.local/state/hyoui` (絶対パスの時だけ)
    ///
    /// どれも無ければエラーにし、cwd 相対の path に倒さない。`$HYOUI_STATE_DIR` が
    /// 相対パスの時も同じ理由でエラーにする (daemon は `chdir("/")` するので、相対の
    /// root は CLI と daemon で別の場所を指す)。監督者の OS 登録名の hash はこの root
    /// から作る (DR-0038 決定 4)。
    ///
    /// # Errors
    ///
    /// 上の 3 段のどれでも root を決められない時。
    pub fn state_root(&self) -> Result<PathBuf, StateRootError> {
        if let Some(dir) = &self.hyoui_state_dir {
            let dir = PathBuf::from(dir);
            if dir.is_relative() {
                return Err(StateRootError::RelativeStateDir(dir));
            }
            return Ok(dir);
        }
        if let Some(xdg) = self.xdg_state_home.as_deref().map(Path::new)
            && xdg.is_absolute()
        {
            return Ok(xdg.join("hyoui"));
        }
        match self.home() {
            Some(home) if home.is_absolute() => Ok(home.join(".local/state/hyoui")),
            _ => Err(StateRootError::Unresolvable),
        }
    }

    /// session の socket の置き場 (`<state_root>/sessions`、DR-0041 決定 4)。
    ///
    /// socket は `sessions/<uuid>.sock` にフラットに置き、name lock (`<uuid>.lock`) と
    /// dir lock (`.dir.lock`) も同じ dir に置く (決定 3)。
    ///
    /// # Errors
    ///
    /// [`Env::state_root`] と同じ。
    pub fn sessions_dir(&self) -> Result<PathBuf, StateRootError> {
        Ok(self.state_root()?.join("sessions"))
    }

    /// web の config の既定の置き場 (`<config_dir>/web`、DR-0038 決定 4)。
    #[must_use]
    pub fn web_config_dir(&self) -> Option<PathBuf> {
        self.config_dir().map(|dir| dir.join("web"))
    }

    /// web の状態の置き場 (`<state_root>/web`、DR-0038 決定 4)。
    ///
    /// 登録簿 `units/`、`logs/`、passkey の `auth.json` / `pending.json`、監督者の
    /// socket をここにまとめる。
    ///
    /// # Errors
    ///
    /// [`Env::state_root`] と同じ。
    pub fn web_state_dir(&self) -> Result<PathBuf, StateRootError> {
        Ok(self.state_root()?.join("web"))
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
            ("HYOUI_STATE_DIR", ""),
        ]);
        assert_eq!(e.config_dir(), Some(PathBuf::from("/c/hyoui")));
        assert_eq!(e.state_root(), Ok(PathBuf::from("/h/.local/state/hyoui")));
        assert_eq!(e.get(LocationVar::XdgStateHome), None);
        assert_eq!(e.get(LocationVar::HyouiStateDir), None);
    }

    /// 状態の root は `HYOUI_STATE_DIR` → `XDG_STATE_HOME/hyoui` → `HOME/.local/state/hyoui`
    /// の 3 段で決まる (DR-0041 決定 6)。
    #[test]
    fn state_root_takes_the_first_of_three_steps() {
        let all = env(&[
            ("HYOUI_STATE_DIR", "/face"),
            ("XDG_STATE_HOME", "/s"),
            ("HOME", "/h"),
        ]);
        // 1. 専用の変数はそのまま使う (= `hyoui/` を足さない)。
        assert_eq!(all.state_root(), Ok(PathBuf::from("/face")));
        // 2. XDG はその下に `hyoui/` を掘る。
        let xdg = env(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")]);
        assert_eq!(xdg.state_root(), Ok(PathBuf::from("/s/hyoui")));
        // 3. 既定。
        let home = env(&[("HOME", "/h")]);
        assert_eq!(
            home.state_root(),
            Ok(PathBuf::from("/h/.local/state/hyoui"))
        );
    }

    /// `XDG_STATE_HOME` が相対パスなら使わず次の段に進む (= XDG Base Directory の規定)。
    #[test]
    fn a_relative_xdg_state_home_is_skipped() {
        let e = env(&[("XDG_STATE_HOME", "rel/state"), ("HOME", "/h")]);
        assert_eq!(e.state_root(), Ok(PathBuf::from("/h/.local/state/hyoui")));
    }

    /// どの段でも決まらなければエラーにし、cwd 相対の path に倒さない。
    #[test]
    fn state_root_without_home_is_an_error() {
        assert_eq!(
            Env::default().state_root(),
            Err(StateRootError::Unresolvable)
        );
        let relative_only = env(&[("XDG_STATE_HOME", "rel"), ("HOME", "also-rel")]);
        assert_eq!(
            relative_only.state_root(),
            Err(StateRootError::Unresolvable)
        );
        assert_eq!(
            env(&[("HYOUI_STATE_DIR", "face"), ("HOME", "/h")]).state_root(),
            Err(StateRootError::RelativeStateDir(PathBuf::from("face")))
        );
        assert!(Env::default().sessions_dir().is_err());
        assert!(Env::default().web_state_dir().is_err());
        let message = StateRootError::Unresolvable.to_string();
        assert!(
            message.contains("HYOUI_STATE_DIR") && message.contains("HOME"),
            "{message}"
        );
    }

    /// session の socket は root 直下ではなく `sessions/` に置く (DR-0041 決定 4)。
    #[test]
    fn sessions_live_under_sessions_of_the_root() {
        let e = env(&[("HYOUI_STATE_DIR", "/face"), ("HOME", "/h")]);
        assert_eq!(e.sessions_dir(), Ok(PathBuf::from("/face/sessions")));
        assert_eq!(e.web_state_dir(), Ok(PathBuf::from("/face/web")));
    }

    /// config は面で分けない: `HYOUI_STATE_DIR` を変えても config の置き場は同じ
    /// (DR-0041 決定 6)。
    #[test]
    fn config_is_shared_across_faces() {
        let a = env(&[("HYOUI_STATE_DIR", "/a"), ("HOME", "/h")]);
        let b = env(&[("HYOUI_STATE_DIR", "/b"), ("HOME", "/h")]);
        assert_eq!(a.config_dir(), b.config_dir());
        assert_eq!(a.config_dir(), Some(PathBuf::from("/h/.config/hyoui")));
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
            Ok(PathBuf::from("/h/.local/state/hyoui/web"))
        );

        let e = env(&[
            ("HOME", "/h"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CONFIG_HOME", "/c"),
        ]);
        assert_eq!(e.web_config_dir(), Some(PathBuf::from("/c/hyoui/web")));
        assert_eq!(e.web_state_dir(), Ok(PathBuf::from("/s/hyoui/web")));

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
