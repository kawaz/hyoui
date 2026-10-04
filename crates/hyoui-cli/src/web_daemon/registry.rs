//! Web gateway unit registry (= DR-0038 決定 2)。
//!
//! `<web の状態の置き場>/units/<name>.toml` が 1 unit 1 ファイル。unit = 任意 path の
//! config ファイル 1 つで、登録簿が持つのは**その path への参照**と、起動する実行
//! ファイル、desired state だけ。`listen` / `assets_dir` は config の `[web]` が正本で、
//! 読み手 (= 子の `web daemon run <name>`、監督者、`list`) が読むたびに config から引く。

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 登録済みの web gateway インスタンス。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    /// この unit が読む config ファイル (= 絶対パス)。
    pub config: PathBuf,
    /// この unit を起動する実行ファイル (= config の `[web].binary_path`、無ければ
    /// `add` した時点の自分自身)。
    pub binary_path: PathBuf,
    /// 監督者に起こしていてほしいか (= desired state)。
    pub enabled: bool,
    /// 登録時刻 (ISO 8601)。
    pub added_at: String,
}

impl Unit {
    /// この unit の config を読む (= `extends` を畳んだ後の値)。
    pub fn load_config(
        &self,
    ) -> std::result::Result<hyoui::config::WebConfig, hyoui::config::ConfigError> {
        hyoui::config::load_web(&self.config).map(|file| file.web)
    }
}

/// 登録簿操作の失敗。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// unit ファイル 1 つを一意に指せない名前。
    #[error("`{0}` is not a valid unit name; use 1-32 characters from A-Z, a-z, 0-9, `_`, `-`")]
    BadName(String),
    /// その名前の unit が無い。
    #[error("there is no web gateway unit named `{name}`")]
    UnknownUnit {
        /// 要求された名前。
        name: String,
    },
    /// その名前の unit が既にある。
    #[error("web gateway unit `{name}` is already registered")]
    AlreadyRegistered {
        /// 既存の名前。
        name: String,
    },
    /// 登録簿を読み書きできなかった。
    #[error("could not use the web gateway unit registry at {}: {reason}", path.display())]
    Storage {
        /// 失敗に関与した path。
        path: PathBuf,
        /// 原因。
        reason: String,
    },
}

/// 登録簿操作の結果。
pub type Result<T> = std::result::Result<T, Error>;

/// ファイルに載った unit 登録簿。
#[derive(Debug, Clone)]
pub struct Registry {
    dir: PathBuf,
}

impl Registry {
    /// unit dir を明示して開く (= test / 隔離 `XDG_STATE_HOME`)。
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// 既定の unit dir を開く。
    pub fn open() -> Self {
        Self::at(default_units_dir())
    }

    /// unit dir を返す。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 既存の登録を置き換えずに unit を足す。
    pub fn add(&self, name: &str, unit: &Unit) -> Result<()> {
        validate_name(name)?;
        let path = self.path_of(name);
        if self.read(&path)?.is_some() {
            return Err(Error::AlreadyRegistered {
                name: name.to_owned(),
            });
        }
        self.write_atomic(&path, unit)
    }

    /// 登録を外す。
    pub fn remove(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let path = self.path_of(name);
        if self.read(&path)?.is_none() {
            return Err(Error::UnknownUnit {
                name: name.to_owned(),
            });
        }
        std::fs::remove_file(&path).map_err(|error| self.storage(&path, &error))
    }

    /// unit 1 つを読む。
    pub fn get(&self, name: &str) -> Result<Unit> {
        validate_name(name)?;
        self.read(&self.path_of(name))?
            .ok_or_else(|| Error::UnknownUnit {
                name: name.to_owned(),
            })
    }

    /// 登録済み unit を名前の昇順で読む。
    pub fn list(&self) -> Result<Vec<(String, Unit)>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(self.storage(&self.dir, &error)),
        };
        let mut units = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| self.storage(&self.dir, &error))?
                .path();
            if path.extension().is_none_or(|extension| extension != "toml") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            // 名前として使えないファイルは登録簿の一部ではない (= tmp の残骸や人が
            // 置いたもの)。list 全体を失敗させず読み飛ばす。
            if validate_name(name).is_err() {
                continue;
            }
            if let Some(unit) = self.read(&path)? {
                units.push((name.to_owned(), unit));
            }
        }
        units.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(units)
    }

    /// 他の属性を保ったまま desired state を書き換える。
    ///
    /// これを書くのは監督者で、`add` / `remove` は CLI が書く。書き手が 2 者いる
    /// ことが tmp + rename の要件になっている (DR-0034 決定 2 / 4)。
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<Unit> {
        let mut unit = self.get(name)?;
        if unit.enabled != enabled {
            unit.enabled = enabled;
            self.write_atomic(&self.path_of(name), &unit)?;
        }
        Ok(unit)
    }

    fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.toml"))
    }

    fn read(&self, path: &Path) -> Result<Option<Unit>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.storage(path, &error)),
        };
        toml::from_str(&text)
            .map(Some)
            .map_err(|error| self.storage(path, &error))
    }

    /// 同じ dir に書いて rename する (= 読み手は古いか新しいかのどちらかを見る)。
    ///
    /// `enabled` を書き換えるのは監督者、`add` / `remove` は CLI で書き手が 2 者
    /// いるため、途中の半端な内容が読まれない形が要件 (DR-0034 決定 2)。
    fn write_atomic(&self, path: &Path, unit: &Unit) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|error| self.storage(&self.dir, &error))?;
        let text = toml::to_string_pretty(unit).map_err(|error| self.storage(path, &error))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.dir)
            .map_err(|error| self.storage(&self.dir, &error))?;
        temporary
            .write_all(text.as_bytes())
            .map_err(|error| self.storage(temporary.path(), &error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| self.storage(temporary.path(), &error))?;
        temporary
            .persist(path)
            .map_err(|error| self.storage(path, &error.error))?;
        Ok(())
    }

    fn storage(&self, path: &Path, reason: &dyn std::fmt::Display) -> Error {
        Error::Storage {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        }
    }
}

/// units / logs / 監督者 socket をまとめる root (= web の状態の置き場、DR-0038 決定 4)。
pub fn default_root() -> PathBuf {
    hyoui::paths::Env::current().web_state_dir()
}

/// 既定の unit 登録簿 dir。
pub fn default_units_dir() -> PathBuf {
    default_root().join("units")
}

/// root からの監督者の制御 socket の位置 (DR-0038 決定 4)。
///
/// root 直下ではなく `run/` に 1 段下げる。root (`hyoui/web/`) は session socket の
/// base (`hyoui/`) の直下にあり、discovery はその直下の dir を namespace とみなして
/// 中の `*.sock` に hyoui protocol で問い合わせる (DR-0018)。root 直下に置くと、
/// その問い合わせと監督者の 1 行読みが互いの応答を待ち合い、監督者の event loop が
/// 1 回 5 秒止まる (実測)。discovery は 1 段しか潜らないので、`run/` の中は見ない。
pub fn supervisor_socket_in(root: &Path) -> PathBuf {
    root.join("run").join("supervisor.sock")
}

/// 監督者の制御 socket (= DR-0034 決定 4)。
pub fn supervisor_socket_path() -> PathBuf {
    supervisor_socket_in(&default_root())
}

/// DR-0034 決定 2 の unit 名文法 (`[A-Za-z0-9_-]{1,32}`)。
///
/// ファイル名に使うため、path separator や `.` を含む名前は拒否する。
pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(Error::BadName(name.to_owned()))
    }
}

/// 今の時刻を unit の `added_at` として書ける形で返す。
pub fn now_iso8601() -> String {
    hyoui::time::now_iso8601()
}

/// listen が既存 unit とぶつかる形。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenConflict {
    /// 同じ宛先を指している (= `add` を断る)。
    Same {
        /// 既にその宛先を持つ unit。
        name: String,
        /// その unit の listen 表記。
        listen: String,
    },
    /// 宛先が重なりうるが一致とは言えない (= warning に留める)。
    Overlapping {
        /// 重なりうる unit。
        name: String,
        /// その unit の listen 表記。
        listen: String,
        /// 何が判断できなかったか。
        reason: String,
    },
}

/// `SocketAddr` に解決してから宛先を比べる。
///
/// `units` は `(unit 名, その unit の listen)`。`localhost:43690` と `127.0.0.1:43690`
/// を同じと見なすため、文字列比較ではなく解決後の値で比べる。解決できない値は
/// 文字列で比べ、`add` を止めずに warning にする (DR-0034 決定 2)。
///
/// port 0 (= kernel に任せる) はどれとも衝突しない — 同じ宛先を取り合うことが無い。
pub fn find_listen_conflict(units: &[(String, String)], listen: &str) -> Option<ListenConflict> {
    if listen_is_ephemeral(listen) {
        return None;
    }
    let wanted = resolve_socket_addrs(listen);
    let mut overlapping = None;

    for (name, unit_listen) in units {
        if listen_is_ephemeral(unit_listen) {
            continue;
        }
        let existing = resolve_socket_addrs(unit_listen);
        match (&wanted, &existing) {
            (Some(wanted), Some(existing)) => {
                if wanted.iter().any(|addr| existing.contains(addr)) {
                    return Some(ListenConflict::Same {
                        name: name.clone(),
                        listen: unit_listen.clone(),
                    });
                }
                // `0.0.0.0:43690` と `127.0.0.1:43690` のような包含関係は一致とは
                // 言えない。port が同じで片方が wildcard なら重なりうると伝える。
                if overlapping.is_none() && ports_overlap_on_wildcard(wanted, existing) {
                    overlapping = Some(ListenConflict::Overlapping {
                        name: name.clone(),
                        listen: unit_listen.clone(),
                        reason: "one of the two addresses is a wildcard on the same port"
                            .to_string(),
                    });
                }
            }
            _ => {
                if unit_listen == listen {
                    return Some(ListenConflict::Same {
                        name: name.clone(),
                        listen: unit_listen.clone(),
                    });
                }
                if overlapping.is_none() {
                    overlapping = Some(ListenConflict::Overlapping {
                        name: name.clone(),
                        listen: unit_listen.clone(),
                        reason: "at least one of the two addresses could not be resolved"
                            .to_string(),
                    });
                }
            }
        }
    }
    overlapping
}

fn listen_is_ephemeral(listen: &str) -> bool {
    listen
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>() == Ok(0))
}

fn resolve_socket_addrs(listen: &str) -> Option<Vec<SocketAddr>> {
    let resolved: Vec<SocketAddr> = listen.to_socket_addrs().ok()?.collect();
    (!resolved.is_empty()).then_some(resolved)
}

fn ports_overlap_on_wildcard(left: &[SocketAddr], right: &[SocketAddr]) -> bool {
    left.iter().any(|l| {
        right
            .iter()
            .any(|r| l.port() == r.port() && (l.ip().is_unspecified() || r.ip().is_unspecified()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(config: &str) -> Unit {
        Unit {
            config: PathBuf::from(config),
            binary_path: PathBuf::from("/opt/homebrew/bin/hyoui"),
            enabled: true,
            added_at: "2026-09-15T00:00:00+09:00".to_owned(),
        }
    }

    fn registry() -> (tempfile::TempDir, Registry) {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("units"));
        (directory, registry)
    }

    fn listens(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, l)| ((*n).to_string(), (*l).to_string()))
            .collect()
    }

    #[test]
    fn unit_round_trips_and_list_is_sorted() {
        let (_directory, registry) = registry();
        registry.add("unstable", &unit("/c/unstable.toml")).unwrap();
        registry.add("stable", &unit("/c/stable.toml")).unwrap();

        assert_eq!(registry.get("stable").unwrap(), unit("/c/stable.toml"));
        assert_eq!(
            registry
                .list()
                .unwrap()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["stable", "unstable"]
        );
    }

    /// 登録簿の 1 ファイルが持つのは config への参照・実行ファイル・desired state・
    /// 登録時刻だけ。設定値 (`listen` 等) は持たない (DR-0038 決定 2)。
    #[test]
    fn a_unit_file_holds_a_reference_not_settings() {
        let (_directory, registry) = registry();
        registry.add("stable", &unit("/c/stable.toml")).unwrap();
        let text = std::fs::read_to_string(registry.dir().join("stable.toml")).unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["added_at", "binary_path", "config", "enabled"]);

        // 設定値を書いた登録簿は読まない (= 黙って一部だけ使わない)。
        std::fs::write(
            registry.dir().join("old.toml"),
            "listen = \"127.0.0.1:1\"\nbinary = \"/x\"\nenabled = true\nadded_at = \"t\"\n",
        )
        .unwrap();
        assert!(matches!(registry.get("old"), Err(Error::Storage { .. })));
    }

    #[test]
    fn names_follow_the_exact_file_name_grammar() {
        for valid in ["a", "stable", "unstable_2", "A-9", &"x".repeat(32)] {
            assert!(validate_name(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "white space",
            "é",
            &"x".repeat(33),
        ] {
            assert!(
                matches!(validate_name(invalid), Err(Error::BadName(_))),
                "{invalid}"
            );
        }
    }

    /// tmp + rename で書くので、書き終わった dir に残骸が残らない。
    #[test]
    fn writing_a_unit_leaves_no_temporary_file() {
        let (_directory, registry) = registry();
        registry.add("stable", &unit("/c/stable.toml")).unwrap();

        let entries: Vec<_> = std::fs::read_dir(registry.dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from("stable.toml")]);

        // 上書き (= 監督者による desired state の書き換え) でも残骸は増えない。
        registry.set_enabled("stable", false).unwrap();
        assert_eq!(std::fs::read_dir(registry.dir()).unwrap().count(), 1);

        // 消して同じ名前で入れ直しても増えない。
        registry.remove("stable").unwrap();
        registry.add("stable", &unit("/c/stable.toml")).unwrap();
        assert_eq!(std::fs::read_dir(registry.dir()).unwrap().count(), 1);
    }

    /// desired state だけが変わり、他の属性は保たれる。
    #[test]
    fn changing_the_desired_state_preserves_every_other_property() {
        let (_directory, registry) = registry();
        let original = unit("/c/stable.toml");
        registry.add("stable", &original).unwrap();

        let changed = registry.set_enabled("stable", false).unwrap();
        assert!(!changed.enabled);
        assert_eq!(changed.config, original.config);
        assert_eq!(changed.binary_path, original.binary_path);
        assert_eq!(changed.added_at, original.added_at);
        assert_eq!(registry.get("stable").unwrap(), changed);

        // 同じ値の書き込みは no-op で、読み戻しても変わらない。
        assert_eq!(registry.set_enabled("stable", false).unwrap(), changed);
        assert!(matches!(
            registry.set_enabled("ghost", true),
            Err(Error::UnknownUnit { .. })
        ));
    }

    #[test]
    fn duplicate_and_unknown_units_are_distinct_errors() {
        let (_directory, registry) = registry();
        registry.add("stable", &unit("/c/stable.toml")).unwrap();
        assert!(matches!(
            registry.add("stable", &unit("/c/other.toml")),
            Err(Error::AlreadyRegistered { .. })
        ));
        assert!(matches!(
            registry.get("missing"),
            Err(Error::UnknownUnit { .. })
        ));
    }

    #[test]
    fn missing_registry_dir_lists_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("absent/units"));
        assert!(registry.list().unwrap().is_empty());
    }

    #[test]
    fn files_that_are_not_units_are_skipped() {
        let (_directory, registry) = registry();
        registry.add("stable", &unit("/c/stable.toml")).unwrap();
        std::fs::write(registry.dir().join("notes.txt"), "ignored").unwrap();
        std::fs::write(registry.dir().join("has space.toml"), "config = 'x'").unwrap();

        assert_eq!(registry.list().unwrap().len(), 1);
    }

    /// unit の config を読み、`extends` を畳んだ値を返す。
    #[test]
    fn a_unit_reads_its_config_file() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("base.toml"),
            "[web]\nlisten = \"127.0.0.1:43690\"\n",
        )
        .unwrap();
        let config = directory.path().join("unstable.toml");
        std::fs::write(
            &config,
            "extends = \"base.toml\"\n[web]\nlisten = \"127.0.0.1:43691\"\n",
        )
        .unwrap();
        let unit = Unit {
            config,
            ..unit("/unused")
        };
        assert_eq!(unit.load_config().unwrap().listen, "127.0.0.1:43691");
    }

    #[test]
    fn the_same_destination_spelled_differently_is_a_conflict() {
        let units = listens(&[("stable", "127.0.0.1:43690")]);

        assert_eq!(
            find_listen_conflict(&units, "localhost:43690"),
            Some(ListenConflict::Same {
                name: "stable".to_string(),
                listen: "127.0.0.1:43690".to_string(),
            })
        );
        assert_eq!(find_listen_conflict(&units, "127.0.0.1:43691"), None);
    }

    #[test]
    fn a_wildcard_on_the_same_port_only_warns() {
        let units = listens(&[("stable", "0.0.0.0:43690")]);

        assert!(matches!(
            find_listen_conflict(&units, "127.0.0.1:43690"),
            Some(ListenConflict::Overlapping { .. })
        ));
    }

    /// port 0 は kernel が空きを選ぶので、何度並べても宛先を取り合わない。
    #[test]
    fn port_zero_never_conflicts() {
        let units = listens(&[("a", "127.0.0.1:0")]);
        assert_eq!(find_listen_conflict(&units, "127.0.0.1:0"), None);
        assert_eq!(find_listen_conflict(&units, "localhost:0"), None);
        let units = listens(&[("a", "127.0.0.1:43690")]);
        assert_eq!(find_listen_conflict(&units, "127.0.0.1:0"), None);
    }

    #[test]
    fn unresolvable_addresses_fall_back_to_text_comparison() {
        let units = listens(&[("stable", "no-such-host.invalid.example:43690")]);

        assert_eq!(
            find_listen_conflict(&units, "no-such-host.invalid.example:43690"),
            Some(ListenConflict::Same {
                name: "stable".to_string(),
                listen: "no-such-host.invalid.example:43690".to_string(),
            })
        );
        assert!(matches!(
            find_listen_conflict(&units, "127.0.0.1:43690"),
            Some(ListenConflict::Overlapping { .. })
        ));
    }

    #[test]
    fn registered_at_is_written_in_the_shared_iso8601_form() {
        // 暦の開き方そのものは `hyoui::time` が固定する。ここで見るのは
        // 登録簿が書く形が読み戻せる表記であること。
        assert!(now_iso8601().ends_with('Z'));
        assert_eq!(now_iso8601().len(), "1970-01-01T00:00:00Z".len());
    }
}
