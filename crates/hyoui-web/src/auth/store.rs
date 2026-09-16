//! `auth.json` / `pending.json` の read-modify-write (DR-0036 決定 4)。
//!
//! 置き場は `$XDG_STATE_HOME/hyoui-web/` で、DR-0034 決定 2 が作った
//! `units/` / `logs/` / `supervisor.sock` にこの 2 file を並べる。root を
//! `hyoui/` にしないのは、そちらが session discovery の走査 base だからである。
//!
//! ## 書き手が複数いる
//!
//! 2 つの unit (stable / unstable) と CLI が同じ file を書く。gateway 間で同期する
//! protocol は持たず、file を正本にして各々が読む (決定 4)。writer の直列化は
//! `flock` に委ね、読み手が半端な内容を見ないことは tmp + rename で担保する。
//!
//! ## lock は別 file に取る
//!
//! **`flock` は `<name>.lock` に取る。data file 自身には取らない。** data file は
//! tmp + rename で差し替えるので、rename の瞬間に inode が入れ替わる。data file に
//! lock を取る形だと、後から来た writer が**既に置き換えられた古い inode** の lock を
//! 掴んで「自分だけが書いている」と信じ、read-modify-write が衝突して lost update に
//! なる (rename は lock の状態を引き継がない)。lock 対象を差し替えられない別 file に
//! 固定すれば、lock の identity が writer 全員で一致する。
//!
//! この性質は `tests::lock_identity_survives_rename_only_for_a_separate_lock_file` が
//! 「data file に取る形だと成立しないこと」を含めて固定している。

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// state file の読み書きで起きる失敗。
#[derive(Debug)]
pub enum StoreError {
    /// file / dir の I/O。
    Io {
        /// 対象 path。
        path: PathBuf,
        /// 理由。
        reason: String,
    },
    /// JSON の解釈に失敗した (= 手で壊した file)。
    Corrupt {
        /// 対象 path。
        path: PathBuf,
        /// 理由。
        reason: String,
    },
    /// lock を取れなかった。
    Lock {
        /// lock file の path。
        path: PathBuf,
        /// 理由。
        reason: String,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io { path, reason } => write!(f, "{}: {reason}", path.display()),
            StoreError::Corrupt { path, reason } => {
                write!(f, "{} is not valid JSON: {reason}", path.display())
            }
            StoreError::Lock { path, reason } => {
                write!(f, "could not lock {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {}

type Result<T> = std::result::Result<T, StoreError>;

/// `auth.json` / `pending.json` を置く dir。
///
/// `root` を引数で持つのは、test が `XDG_STATE_HOME` を隔離するため (決定 9)。
/// **認証に無認証 mode が無いので、state dir の隔離が test の前提になる。**
#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

impl StateDir {
    /// `$XDG_STATE_HOME/hyoui-web/` (DR-0034 決定 2 と同じ root)。
    pub fn default_root() -> Self {
        Self::from_env(
            std::env::var_os("XDG_STATE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }

    /// env を引数で受ける形 (= env を触らずに test する口)。
    fn from_env(state_home: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> Self {
        let root = if let Some(state_home) = state_home.filter(|value| !value.is_empty()) {
            PathBuf::from(state_home).join("hyoui-web")
        } else if let Some(home) = home.filter(|value| !value.is_empty()) {
            PathBuf::from(home).join(".local/state/hyoui-web")
        } else {
            PathBuf::from(".local/state/hyoui-web")
        };
        Self { root }
    }

    /// root を明示して開く (= test / 隔離 `XDG_STATE_HOME`)。
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// root の path。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// credential record と token family (決定 4)。
    pub fn auth(&self) -> StateFile {
        StateFile::new(self.root.join("auth.json"))
    }

    /// 登録 jwt の HMAC secret と challenge の在庫 (決定 4)。
    pub fn pending(&self) -> StateFile {
        StateFile::new(self.root.join("pending.json"))
    }
}

/// 1 つの state file。読みは lock 無し、書きは lock 下の read-modify-write。
#[derive(Debug, Clone)]
pub struct StateFile {
    path: PathBuf,
}

impl StateFile {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// data file の path。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// lock file の path (= data file とは別 file、決定 4)。
    pub fn lock_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        self.path.with_file_name(name)
    }

    /// 現在の内容を読む。file が無ければ既定値。
    ///
    /// **lock を取らない。** tmp + rename で差し替えるので、読み手は古いか新しいかの
    /// どちらかを見る。ただし **family の検証はこの読みを cache してはならない**
    /// (決定 4) — rotate の直後に他 unit が古い値で判定すると再利用検知が誤発火する。
    pub fn read<T: DeserializeOwned + Default>(&self) -> Result<T> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
            Err(e) => {
                return Err(StoreError::Io {
                    path: self.path.clone(),
                    reason: e.to_string(),
                });
            }
        };
        if text.trim().is_empty() {
            return Ok(T::default());
        }
        serde_json::from_str(&text).map_err(|e| StoreError::Corrupt {
            path: self.path.clone(),
            reason: e.to_string(),
        })
    }

    /// lock を取って読み、変更し、tmp + rename で差し替える。
    ///
    /// 「lock → 読む → 変更 → tmp に書く → rename → unlock」が 1 単位である
    /// (決定 4)。challenge の消費と 6 桁コードの試行回数の加算をこの外で行うと、
    /// 2 unit に同時に来た要求が同じ challenge を 2 回消費でき、試行回数も数え
    /// 落とす (= 総当たりの回数上限が unit の数だけ緩む)。
    pub fn update<T, R, F>(&self, mutate: F) -> Result<R>
    where
        T: DeserializeOwned + Default + Serialize,
        F: FnOnce(&mut T) -> R,
    {
        let guard = self.lock()?;
        let mut value: T = self.read()?;
        let outcome = mutate(&mut value);
        self.write_atomic(&value)?;
        drop(guard);
        Ok(outcome)
    }

    /// 排他 lock を取る。`Drop` で外れる。
    fn lock(&self) -> Result<nix::fcntl::Flock<std::fs::File>> {
        self.ensure_dir()?;
        let path = self.lock_path();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| StoreError::Lock {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive).map_err(|(_, errno)| {
            StoreError::Lock {
                path,
                reason: errno.to_string(),
            }
        })
    }

    fn ensure_dir(&self) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| StoreError::Io {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?;
        // dir も 0700 に寄せる (= record は同 uid 以外に見せない)。
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        Ok(())
    }

    /// 同じ dir に書いて rename する。mode 0600 (決定 4)。
    fn write_atomic<T: Serialize>(&self, value: &T) -> Result<()> {
        self.ensure_dir()?;
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let text = serde_json::to_vec_pretty(value).map_err(|e| StoreError::Io {
            path: self.path.clone(),
            reason: e.to_string(),
        })?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".tmp-")
            .permissions(std::fs::Permissions::from_mode(0o600))
            .tempfile_in(dir)
            .map_err(|e| StoreError::Io {
                path: dir.to_path_buf(),
                reason: e.to_string(),
            })?;
        temporary.write_all(&text).map_err(|e| StoreError::Io {
            path: temporary.path().to_path_buf(),
            reason: e.to_string(),
        })?;
        // record を失いたくないので、rename 前に中身を落とす。
        temporary.as_file().sync_all().map_err(|e| StoreError::Io {
            path: temporary.path().to_path_buf(),
            reason: e.to_string(),
        })?;
        temporary.persist(&self.path).map_err(|e| StoreError::Io {
            path: self.path.clone(),
            reason: e.error.to_string(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// test 用の最小 file 形。
    #[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
    struct Counters {
        counters: BTreeMap<String, u64>,
    }

    #[test]
    fn state_dir_follows_dr0034_root() {
        let dir = StateDir::from_env(Some("/tmp/state".as_ref()), None);
        assert_eq!(dir.root(), Path::new("/tmp/state/hyoui-web"));
        assert_eq!(
            dir.auth().path(),
            Path::new("/tmp/state/hyoui-web/auth.json")
        );
        assert_eq!(
            dir.pending().path(),
            Path::new("/tmp/state/hyoui-web/pending.json")
        );
        let from_home = StateDir::from_env(None, Some("/home/someone".as_ref()));
        assert_eq!(
            from_home.root(),
            Path::new("/home/someone/.local/state/hyoui-web")
        );
    }

    #[test]
    fn lock_path_is_a_separate_file() {
        let dir = StateDir::at("/tmp/x");
        assert_eq!(
            dir.auth().lock_path(),
            Path::new("/tmp/x/auth.json.lock"),
            "lock は data file とは別 file に取る (決定 4)"
        );
    }

    #[test]
    fn missing_file_reads_as_default() {
        let tmp = tempfile::tempdir().unwrap();
        let file = StateDir::at(tmp.path()).auth();
        let value: Counters = file.read().unwrap();
        assert!(value.counters.is_empty());
    }

    #[test]
    fn written_file_is_owner_only_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let file = StateDir::at(tmp.path()).auth();
        file.update::<Counters, _, _>(|value| {
            value.counters.insert("a".to_string(), 1);
        })
        .unwrap();
        let read: Counters = file.read().unwrap();
        assert_eq!(read.counters.get("a"), Some(&1));
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "record は同 uid 以外に見せない (決定 4)");
    }

    /// DR-0036 決定 4 の要点そのもの。
    ///
    /// **data file に lock を取る形では、rename を跨いだ writer 同士が別の inode の
    /// lock を掴む。** その状態を実測して、別 file に取る形との差を固定する。
    #[test]
    fn lock_identity_survives_rename_only_for_a_separate_lock_file() {
        use nix::fcntl::{Flock, FlockArg};

        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("auth.json");
        std::fs::write(&data, b"{}").unwrap();

        // (1) data file 自身に lock を取り、保持したまま tmp + rename で差し替える。
        let held = Flock::lock(
            std::fs::File::open(&data).unwrap(),
            FlockArg::LockExclusiveNonblock,
        )
        .expect("data file の lock");
        let replacement = tmp.path().join("auth.json.new");
        std::fs::write(&replacement, b"{}").unwrap();
        std::fs::rename(&replacement, &data).unwrap();

        // 差し替え後の path を開いて lock を試すと**取れてしまう** (= 別 inode)。
        // これが lost update の入口である。
        let second = Flock::lock(
            std::fs::File::open(&data).unwrap(),
            FlockArg::LockExclusiveNonblock,
        );
        assert!(
            second.is_ok(),
            "rename 後の data file は別 inode なので lock が同時に取れてしまう \
             (= data file に lock を取る実装は排他にならない)"
        );
        drop(second);
        drop(held);

        // (2) 別 file に取る形では、rename しても lock の identity が変わらない。
        let file = StateDir::at(tmp.path()).auth();
        let lock_path = file.lock_path();
        std::fs::write(&lock_path, b"").unwrap();
        let held = Flock::lock(
            std::fs::File::open(&lock_path).unwrap(),
            FlockArg::LockExclusiveNonblock,
        )
        .expect("lock file の lock");
        let replacement = tmp.path().join("auth.json.new2");
        std::fs::write(&replacement, b"{}").unwrap();
        std::fs::rename(&replacement, &data).unwrap();
        let second = Flock::lock(
            std::fs::File::open(&lock_path).unwrap(),
            FlockArg::LockExclusiveNonblock,
        );
        assert!(
            second.is_err(),
            "lock file は差し替えないので、data file を rename しても排他が保たれる"
        );
        drop(held);
    }

    /// 決定 4: 複数の writer が同時に read-modify-write しても数を落とさない。
    ///
    /// thread で回すのは lock が **同一プロセス内でも** 効くことを見るため。
    /// プロセス跨ぎは `tests/auth_store_concurrency.rs` が実際の 2 プロセスで見る。
    #[test]
    fn concurrent_increments_do_not_lose_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let file = StateDir::at(tmp.path()).auth();
        const THREADS: usize = 8;
        const PER_THREAD: u64 = 25;
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let file = file.clone();
                scope.spawn(move || {
                    for _ in 0..PER_THREAD {
                        file.update::<Counters, _, _>(|value| {
                            *value.counters.entry("hits".to_string()).or_insert(0) += 1;
                        })
                        .unwrap();
                    }
                });
            }
        });
        let read: Counters = file.read().unwrap();
        assert_eq!(
            read.counters.get("hits"),
            Some(&(THREADS as u64 * PER_THREAD)),
            "lock 下の read-modify-write なので加算は落ちない"
        );
    }

    #[test]
    fn corrupt_file_is_reported_not_silently_reset() {
        // 黙って既定値に戻すと、壊れた record を「登録が無い」と読んで
        // 登録し直させることになる。壊れていることを言う。
        let tmp = tempfile::tempdir().unwrap();
        let file = StateDir::at(tmp.path()).auth();
        std::fs::write(file.path(), b"{ not json").unwrap();
        let err = file.read::<Counters>().unwrap_err();
        assert!(
            matches!(err, StoreError::Corrupt { .. }),
            "壊れた file は Corrupt で返す: {err}"
        );
    }
}
