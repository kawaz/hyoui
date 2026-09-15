//! Web gateway unit registry (= DR-0034 決定 2)。
//!
//! `${XDG_STATE_HOME:-~/.local/state}/hyoui-web/units/<name>.toml` が 1 unit 1
//! ファイルの正本。unit の属性は `add` が解決し切って書くので、読み手 (= 子の
//! `web daemon run <name>`、監督者、`list`) は config を読み直さない。
//!
//! root を `hyoui/` ではなく `hyoui-web/` にするのは、`${XDG_STATE_HOME}/hyoui/`
//! が session discovery の走査 base で、そのサブ dir が namespace として扱われる
//! ため (`crates/hyoui/src/discovery.rs`)。gateway の運用状態を session の名前空間
//! と同じ木に置かない。

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 登録済みの web gateway インスタンス。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    /// gateway が bind する host:port (= `add` 時点で解決済み)。
    pub listen: String,
    /// この unit を起動する実行ファイル。
    pub binary: PathBuf,
    /// 静的 assets の差し替え先。無ければ埋め込み assets を使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_assets_dir: Option<PathBuf>,
    /// 監督者に起こしていてほしいか (= desired state)。
    pub enabled: bool,
    /// 登録時刻 (ISO 8601)。
    pub added_at: String,
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

/// units / logs / 監督者 socket をまとめる root。
pub fn default_root() -> PathBuf {
    root_from(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// [`default_root`] の env を引数で受ける形 (= env を触らずに test する口)。
fn root_from(state_home: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> PathBuf {
    if let Some(state_home) = state_home.filter(|value| !value.is_empty()) {
        PathBuf::from(state_home).join("hyoui-web")
    } else if let Some(home) = home.filter(|value| !value.is_empty()) {
        PathBuf::from(home).join(".local/state/hyoui-web")
    } else {
        PathBuf::from(".local/state/hyoui-web")
    }
}

/// 既定の unit 登録簿 dir。
pub fn default_units_dir() -> PathBuf {
    default_root().join("units")
}

/// 監督者の制御 socket (= DR-0034 決定 4、protocol は P3)。
pub fn supervisor_socket_path() -> PathBuf {
    default_root().join("supervisor.sock")
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
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    format_iso8601_utc(seconds)
}

/// epoch 秒を UTC の ISO 8601 に整形する。
///
/// Design rationale: 時刻 crate を足さずに自前で持つ。offset は UTC (`Z`) 固定で
/// local offset を解決しない — 登録時刻は絶対時刻として読めれば足り、tz database
/// を引くために依存を増やす理由が無い。
fn format_iso8601_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// epoch からの日数を暦の (年, 月, 日) に開く。
///
/// Howard Hinnant の `civil_from_days` (public domain) と同じ式で、3 月を年の
/// 起点に取り直してうるう年の分岐を無くしている。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
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
/// `localhost:43690` と `127.0.0.1:43690` を同じと見なすため、文字列比較では
/// なく解決後の値で比べる。解決できない値は文字列で比べ、`add` を止めずに
/// warning にする (DR-0034 決定 2)。
pub fn find_listen_conflict(units: &[(String, Unit)], listen: &str) -> Option<ListenConflict> {
    let wanted = resolve_socket_addrs(listen);
    let mut overlapping = None;

    for (name, unit) in units {
        let existing = resolve_socket_addrs(&unit.listen);
        match (&wanted, &existing) {
            (Some(wanted), Some(existing)) => {
                if wanted.iter().any(|addr| existing.contains(addr)) {
                    return Some(ListenConflict::Same {
                        name: name.clone(),
                        listen: unit.listen.clone(),
                    });
                }
                // `0.0.0.0:43690` と `127.0.0.1:43690` のような包含関係は一致とは
                // 言えない。port が同じで片方が wildcard なら重なりうると伝える。
                if overlapping.is_none() && ports_overlap_on_wildcard(wanted, existing) {
                    overlapping = Some(ListenConflict::Overlapping {
                        name: name.clone(),
                        listen: unit.listen.clone(),
                        reason: "one of the two addresses is a wildcard on the same port"
                            .to_string(),
                    });
                }
            }
            _ => {
                if unit.listen == listen {
                    return Some(ListenConflict::Same {
                        name: name.clone(),
                        listen: unit.listen.clone(),
                    });
                }
                if overlapping.is_none() {
                    overlapping = Some(ListenConflict::Overlapping {
                        name: name.clone(),
                        listen: unit.listen.clone(),
                        reason: "at least one of the two addresses could not be resolved"
                            .to_string(),
                    });
                }
            }
        }
    }
    overlapping
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

    fn unit(listen: &str) -> Unit {
        Unit {
            listen: listen.to_owned(),
            binary: PathBuf::from("/opt/homebrew/bin/hyoui"),
            web_assets_dir: Some(PathBuf::from("/tmp/assets")),
            enabled: true,
            added_at: "2026-09-15T00:00:00+09:00".to_owned(),
        }
    }

    fn registry() -> (tempfile::TempDir, Registry) {
        let directory = tempfile::tempdir().unwrap();
        let registry = Registry::at(directory.path().join("units"));
        (directory, registry)
    }

    #[test]
    fn unit_round_trips_and_list_is_sorted() {
        let (_directory, registry) = registry();
        registry.add("unstable", &unit("127.0.0.1:43691")).unwrap();
        registry.add("stable", &unit("127.0.0.1:43690")).unwrap();

        assert_eq!(registry.get("stable").unwrap(), unit("127.0.0.1:43690"));
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

    #[test]
    fn unit_without_assets_dir_round_trips() {
        let (_directory, registry) = registry();
        let mut expected = unit("127.0.0.1:43690");
        expected.web_assets_dir = None;
        registry.add("stable", &expected).unwrap();

        assert_eq!(registry.get("stable").unwrap(), expected);
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
        registry.add("stable", &unit("127.0.0.1:43690")).unwrap();

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
        registry.add("stable", &unit("127.0.0.1:43690")).unwrap();
        assert_eq!(std::fs::read_dir(registry.dir()).unwrap().count(), 1);
    }

    /// desired state だけが変わり、他の属性は保たれる。
    #[test]
    fn changing_the_desired_state_preserves_every_other_property() {
        let (_directory, registry) = registry();
        let original = unit("127.0.0.1:43690");
        registry.add("stable", &original).unwrap();

        let changed = registry.set_enabled("stable", false).unwrap();
        assert!(!changed.enabled);
        assert_eq!(changed.listen, original.listen);
        assert_eq!(changed.binary, original.binary);
        assert_eq!(changed.web_assets_dir, original.web_assets_dir);
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
        registry.add("stable", &unit("127.0.0.1:43690")).unwrap();
        assert!(matches!(
            registry.add("stable", &unit("127.0.0.1:43691")),
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
        registry.add("stable", &unit("127.0.0.1:43690")).unwrap();
        std::fs::write(registry.dir().join("notes.txt"), "ignored").unwrap();
        std::fs::write(registry.dir().join("has space.toml"), "listen = 'x'").unwrap();

        assert_eq!(registry.list().unwrap().len(), 1);
    }

    #[test]
    fn the_same_destination_spelled_differently_is_a_conflict() {
        let units = vec![("stable".to_string(), unit("127.0.0.1:43690"))];

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
        let units = vec![("stable".to_string(), unit("0.0.0.0:43690"))];

        assert!(matches!(
            find_listen_conflict(&units, "127.0.0.1:43690"),
            Some(ListenConflict::Overlapping { .. })
        ));
    }

    #[test]
    fn unresolvable_addresses_fall_back_to_text_comparison() {
        let units = vec![(
            "stable".to_string(),
            unit("no-such-host.invalid.example:43690"),
        )];

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
    fn epoch_seconds_format_as_iso8601() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601_utc(1), "1970-01-01T00:00:01Z");
        // うるう年の 2 月 29 日と、その翌日。
        assert_eq!(format_iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_iso8601_utc(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(format_iso8601_utc(1_709_251_200), "2024-03-01T00:00:00Z");
        // うるう年でない年の 3 月 1 日 (2100 は 400 で割れないので平年)。
        assert_eq!(format_iso8601_utc(4_107_542_400), "2100-03-01T00:00:00Z");
        // epoch より前も暦として開ける。
        assert_eq!(format_iso8601_utc(-1), "1969-12-31T23:59:59Z");
        // 登録時刻は登録簿に書いた形のまま読み戻せる。
        assert!(now_iso8601().ends_with('Z'));
        assert_eq!(now_iso8601().len(), "1970-01-01T00:00:00Z".len());
    }

    #[test]
    fn the_registry_root_is_outside_the_session_discovery_base() {
        // `${XDG_STATE_HOME}/hyoui/` は discovery の走査 base で、そのサブ dir が
        // namespace として扱われる。root を分けていることを固定する (決定 2)。
        let state_home = std::ffi::OsString::from("/tmp/state");
        let home = std::ffi::OsString::from("/home/someone");

        assert_eq!(
            root_from(Some(&state_home), Some(&home)),
            PathBuf::from("/tmp/state/hyoui-web")
        );
        assert_eq!(
            root_from(None, Some(&home)),
            PathBuf::from("/home/someone/.local/state/hyoui-web")
        );
        assert_eq!(
            root_from(Some(&std::ffi::OsString::new()), Some(&home)),
            PathBuf::from("/home/someone/.local/state/hyoui-web")
        );
    }
}
