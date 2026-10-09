//! daemon のログ (DR-0037 段 2、E-1)。
//!
//! 起動後の daemon は、fd 2 と logger の書き先を session ごとのログファイル
//! ([`crate::paths::Env::session_log_path_for`]: 既定の socket なら
//! `<状態の root>/sessions/logs/<session id>.log`、明示した socket なら socket の隣) にする。serve loop はログを bounded channel に待たない送信 (`try_send`) で積むだけで、
//! ファイルへの write は logger thread が行う (DR-0037 I-2)。channel が満杯なら捨てて件数を
//! 数え、次に書けた時に件数を 1 行残す。
//!
//! logger を立てていない process (= ready 通知の前の daemon、test が `Session` を直接
//! 動かす時) では [`emit`] は stderr にそのまま書く (= ready の前の失敗は呼び出し元の
//! stderr に出る、DR-0037 裁定 Q3)。
//!
//! 1 ファイルの上限 ([`FILE_CAP_BYTES`]) が掛かるのは logger を通した行だけで、panic の文言
//! など fd 2 に直接書かれる分は上限の外。

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::Duration;

/// logger の channel に積める行数 (DR-0037「標準エラーとログ」)。
pub const CHANNEL_CAPACITY: usize = 1024;

/// 1 つのログファイルに書く上限 (bytes)。超える行は書かず、印を 1 行書いて以後の行を
/// 捨てる。印の分も上限の内に収める。
pub const FILE_CAP_BYTES: u64 = 1024 * 1024;

/// daemon の終了時に、logger が積まれた行を書き終えるのを待つ上限。fs が応答しない時も
/// これを過ぎたら戻る (= 残りの行は失われる)。
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// logger に渡す 1 行。時刻は積んだ時点で取る (= logger の遅れで時刻がずれない)。
#[derive(Debug)]
struct Line {
    unix_ms: u64,
    text: String,
}

/// serve loop 側が持つ送り口。
#[derive(Debug)]
struct Sink {
    tx: SyncSender<Line>,
    dropped: Arc<AtomicU64>,
}

impl Sink {
    /// 待たずに積む。満杯なら捨てて件数を数える。logger が終わっていたら黙って捨てる
    /// (= shutdown の後に届いた行。書き先がもう無い)。
    fn push(&self, line: Line) {
        match self.tx.try_send(line) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// process に 1 つの送り口。[`install`] で入れ、[`shutdown`] で外す。
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// logger thread の終了の知らせ。[`install`] で入れ、[`shutdown`] が受ける。
static DONE: Mutex<Option<Receiver<()>>> = Mutex::new(None);

/// daemon のログを 1 行出す。logger があれば channel に積み、無ければ stderr に書く。
pub fn emit(text: String) {
    let guard = SINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_ref() {
        Some(sink) => sink.push(Line {
            unix_ms: crate::time::now_unix_ms(),
            text,
        }),
        None => {
            drop(guard);
            eprintln!("{text}");
        }
    }
}

/// `format!` の書式で [`emit`] する。
#[macro_export]
macro_rules! daemon_log {
    ($($arg:tt)*) => {
        $crate::log::emit(::std::format!($($arg)*))
    };
}

/// session のログファイルを開く (無ければ作る)。
///
/// 置き場の dir は無ければ mode 0700 で作る。開く前に dir を、開いた後に file を確かめ、
/// 次のどれかに当たれば開かずに (開いた fd は閉じて) エラーを返す。
///
/// - dir: symlink、dir でない、持ち主が自分 (euid) でない、mode が 0700 でない
/// - file: path が symlink (`O_NOFOLLOW`)、普通のファイルでない (FIFO / socket / device。
///   `O_NONBLOCK` で開くので、読み手の無い FIFO でも open で止まらない)、持ち主が自分で
///   ない、group / other に権限がある
///
/// 新しく作る file は 0600。追記で開き、確かめた後に `O_NONBLOCK` を外す (= 書き込みは
/// blocking)。fd は CLOEXEC (= 子と upgrade の exec には fd 2 の複製だけが渡る)。
///
/// # Errors
///
/// dir を作れない・確かめられない、file を開けない・確かめられない時。
pub fn open_session_log(path: &Path) -> std::io::Result<File> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    check_private_dir(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    check_private_file(&file, path)?;
    clear_nonblock(&file)?;
    Ok(file)
}

fn unusable(what: &Path, why: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("{}: {why}", what.display()),
    )
}

/// ログの dir が「symlink でない dir、持ち主が自分、mode 0700」か。
fn check_private_dir(dir: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(unusable(dir, "the log directory is a symlink"));
    }
    if !meta.is_dir() {
        return Err(unusable(dir, "the log directory is not a directory"));
    }
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err(unusable(dir, "the log directory is owned by another user"));
    }
    if meta.mode() & 0o777 != 0o700 {
        return Err(unusable(
            dir,
            &format!(
                "the log directory has mode {:o} (want 700)",
                meta.mode() & 0o777
            ),
        ));
    }
    Ok(())
}

/// 開いたログが「普通のファイル、持ち主が自分、group / other に権限が無い」か (fstat)。
fn check_private_file(file: &File, path: &Path) -> std::io::Result<()> {
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(unusable(path, "the log is not a regular file"));
    }
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err(unusable(path, "the log is owned by another user"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(unusable(
            path,
            &format!(
                "the log has mode {:o} (group / other must have no access)",
                meta.mode() & 0o777
            ),
        ));
    }
    Ok(())
}

/// `O_NONBLOCK` を外す (= 開く時だけ止まらないようにし、追記は blocking に戻す)。
fn clear_nonblock(file: &File) -> std::io::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = fcntl(file, FcntlArg::F_GETFL).map_err(std::io::Error::from)?;
    let flags = OFlag::from_bits_truncate(flags) & !OFlag::O_NONBLOCK;
    fcntl(file, FcntlArg::F_SETFL(flags)).map_err(std::io::Error::from)?;
    Ok(())
}

/// logger thread を立て、process の送り口にする。
///
/// `path` は `file` を開いた path。渡すと、logger の終了時にファイルが空 (= 何も書かれ
/// なかった) なら消す。消すのは `path` が今も `file` と同じ実体を指す時だけ。
///
/// # Errors
///
/// thread を立てられない時 (= 送り口は入れない。[`emit`] は stderr に書き続ける)。
pub fn install(file: File, path: Option<PathBuf>) -> std::io::Result<()> {
    let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_CAPACITY);
    let (done_tx, done) = std::sync::mpsc::sync_channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_in_thread = Arc::clone(&dropped);
    std::thread::Builder::new()
        .name("hyoui-logger".to_string())
        .spawn(move || {
            run(rx, file, path.as_deref(), &dropped_in_thread);
            let _ = done_tx.send(());
        })?;
    *DONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(done);
    *SINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Sink { tx, dropped });
    Ok(())
}

/// 送り口を外し、logger が積まれた行を書き終えて終わる (空のログの削除を含む) のを最大
/// `timeout` 待つ。期限内に終われば `true`。logger が無ければ何もせず `true`。
///
/// daemon は serve を抜けた後、listener と name lock を手放す **前** に呼ぶ (= `hyoui kill
/// --wait` が daemon の終わりを見届けた時には、ログは書き終わり、空のログは消えている)。
/// 2 回目以降の呼び出しは何もしない。以後の [`emit`] は stderr (= fd 2) に直接書く。
pub fn shutdown(timeout: Duration) -> bool {
    let sink = SINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    drop(sink);
    let done = DONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    done.is_none_or(|d| d.recv_timeout(timeout).is_ok())
}

/// logger thread の本体。送り口が全部外れるまで書き、最後に捨てた件数を残し、空なら
/// ファイルを消す。
fn run(rx: Receiver<Line>, file: File, path: Option<&Path>, dropped: &AtomicU64) {
    let written = file.metadata().map_or(0, |m| m.len());
    let mut writer = LogWriter::new(&file, written, FILE_CAP_BYTES);
    for line in rx {
        writer.note_dropped(line.unix_ms, dropped.swap(0, Ordering::Relaxed));
        writer.write_line(line.unix_ms, &line.text);
    }
    writer.note_dropped(
        crate::time::now_unix_ms(),
        dropped.swap(0, Ordering::Relaxed),
    );
    if let Some(path) = path {
        remove_if_empty(&file, path);
    }
}

/// `file` が空で、`path` が今も同じ実体を指していれば `path` を消す。
fn remove_if_empty(file: &File, path: &Path) {
    let (Ok(own), Ok(named)) = (file.metadata(), std::fs::metadata(path)) else {
        return;
    };
    if own.len() == 0 && own.dev() == named.dev() && own.ino() == named.ino() {
        let _ = std::fs::remove_file(path);
    }
}

/// 時刻付きの行をファイルに書く。上限を超えたら印を 1 行書いて以後を捨てる。
struct LogWriter<W: Write> {
    out: W,
    written: u64,
    cap: u64,
    capped: bool,
}

impl<W: Write> LogWriter<W> {
    fn new(out: W, written: u64, cap: u64) -> Self {
        Self {
            out,
            written,
            cap,
            capped: written >= cap,
        }
    }

    /// 上限の内に、この行と印の両方が収まる時だけ行を書く。収まらなければ印だけを書いて
    /// (= 印の分は先に取ってあるので、合計は上限を超えない) 以後を捨てる。
    fn write_line(&mut self, unix_ms: u64, text: &str) {
        if self.capped {
            return;
        }
        let stamp = crate::time::format_unix_ms_iso8601(unix_ms);
        let line = format!("{stamp} {}\n", text.trim_end_matches('\n'));
        let mark = format!(
            "{stamp} hyoui: log: reached the size cap ({} bytes); further lines are dropped\n",
            self.cap
        );
        if self.written + line.len() as u64 + mark.len() as u64 > self.cap {
            self.capped = true;
            if self.written + mark.len() as u64 <= self.cap {
                self.put(&mark);
            }
            return;
        }
        self.put(&line);
    }

    /// channel が満杯で捨てた行があれば、その件数を 1 行書く。
    fn note_dropped(&mut self, unix_ms: u64, count: u64) {
        if count > 0 {
            self.write_line(
                unix_ms,
                &format!("hyoui: log: dropped {count} line(s) because the log queue was full"),
            );
        }
    }

    fn put(&mut self, s: &str) {
        if self.out.write_all(s.as_bytes()).is_ok() {
            self.written += s.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buf: &[u8]) -> String {
        String::from_utf8(buf.to_vec()).expect("utf8")
    }

    /// 行は時刻 (UTC の ISO 8601) と本文で 1 行になる。末尾の改行は 1 つにまとめる。
    #[test]
    fn lines_are_stamped_and_newline_terminated() {
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, 0, FILE_CAP_BYTES);
        w.write_line(0, "hello");
        w.write_line(1_000, "world\n");
        assert_eq!(
            text(&buf),
            "1970-01-01T00:00:00Z hello\n1970-01-01T00:00:01Z world\n"
        );
    }

    /// 上限を超える行は書かず、印を 1 度だけ書いて以後を捨てる。印を含めても上限を超えない。
    #[test]
    fn size_cap_writes_one_mark_then_drops() {
        // 1 行 = 20 (時刻) + 1 + 10 + 1 = 32 bytes、印 = 20 + 1 + 72 = 93 bytes 前後。
        let cap = 32 * 2 + 100;
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, 0, cap);
        for _ in 0..5 {
            w.write_line(0, "0123456789");
        }
        let out = text(&buf);
        assert!(out.len() as u64 <= cap, "{} > {cap}: {out}", out.len());
        assert_eq!(out.matches("reached the size cap").count(), 1, "{out}");
        assert!(out.ends_with("further lines are dropped\n"), "{out}");
        assert!(out.matches("0123456789").count() >= 1, "{out}");
    }

    /// 印すら入らない残りしか無い時は、印も書かない (= 上限を超えない)。
    #[test]
    fn no_room_even_for_the_mark_writes_nothing() {
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, 30, 40);
        w.write_line(0, "x");
        assert!(buf.is_empty(), "{}", text(&buf));
    }

    /// 既に上限まで書かれたファイル (= 同じ id の session の続き) には何も足さない。
    #[test]
    fn a_file_already_at_the_cap_gets_nothing() {
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, 40, 40);
        w.write_line(0, "x");
        assert!(buf.is_empty());
    }

    /// channel が満杯の時は捨てて数え、logger は次に書く時に件数を 1 行残す。
    #[test]
    fn a_full_channel_counts_drops_and_the_logger_reports_them() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = Sink {
            tx,
            dropped: Arc::clone(&dropped),
        };
        for i in 0..3 {
            sink.push(Line {
                unix_ms: 0,
                text: format!("line {i}"),
            });
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        drop(sink);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.log");
        let file = open_session_log(&path).expect("open");
        run(rx, file, Some(&path), &dropped);
        let out = std::fs::read_to_string(&path).expect("read log");
        assert_eq!(
            out,
            "1970-01-01T00:00:00Z hyoui: log: dropped 2 line(s) because the log queue was full\n\
             1970-01-01T00:00:00Z line 0\n"
        );
    }

    /// 何も書かれなかったログは logger の終了時に消え、書かれたログは残る。
    #[test]
    fn an_empty_log_is_removed_and_a_written_one_is_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("logs/empty.log");
        let (tx, rx) = std::sync::mpsc::sync_channel::<Line>(1);
        drop(tx);
        run(
            rx,
            open_session_log(&empty).expect("open"),
            Some(&empty),
            &AtomicU64::new(0),
        );
        assert!(!empty.exists());

        let kept = dir.path().join("logs/kept.log");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(Line {
            unix_ms: 0,
            text: "x".into(),
        })
        .unwrap();
        drop(tx);
        run(
            rx,
            open_session_log(&kept).expect("open"),
            Some(&kept),
            &AtomicU64::new(0),
        );
        assert!(kept.exists());
    }

    /// path が別の実体に差し替わっていたら、空でも消さない。
    #[test]
    fn a_replaced_path_is_not_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.log");
        let file = open_session_log(&path).expect("open");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel::<Line>(1);
        drop(tx);
        run(rx, file, Some(&path), &AtomicU64::new(0));
        assert!(path.exists());
    }

    /// dir は 0700、file は 0600 で作る (= 同じ面の他ユーザに読ませない)。
    #[test]
    fn the_log_dir_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sessions/logs/s.log");
        let _file = open_session_log(&path).expect("open");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
    }

    /// 読み手の無い FIFO は open で止まらず、普通のファイルでないとして断る。
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.log");
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        assert!(open_session_log(&path).is_err());
        // 読み手が居る FIFO も、開けた後の fstat で断る。
        let _reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let err = open_session_log(&path).expect_err("fifo with a reader");
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    /// symlink はたどらない (= 先のファイルに追記しない)。
    #[test]
    fn a_symlink_is_refused_and_its_target_is_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("other");
        std::fs::write(&target, b"keep").unwrap();
        let path = dir.path().join("s.log");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(open_session_log(&path).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }

    /// group / other に権限のある既存ファイルは断る (mode は変えない)。
    #[test]
    fn a_file_readable_by_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.log");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = open_session_log(&path).expect_err("0644");
        assert!(err.to_string().contains("mode 644"), "{err}");
    }

    /// dir が symlink、または 0700 でなければ断る。
    #[test]
    fn a_symlinked_or_open_dir_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.path().join("logs");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = open_session_log(&link.join("s.log")).expect_err("symlinked dir");
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!real.join("s.log").exists());

        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = open_session_log(&open.join("s.log")).expect_err("0755 dir");
        assert!(err.to_string().contains("mode 755"), "{err}");
        assert!(!open.join("s.log").exists());
    }

    /// 開いた fd は blocking (= `O_NONBLOCK` は開く時だけ)。
    #[test]
    fn the_opened_log_is_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = open_session_log(&dir.path().join("s.log")).expect("open");
        let flags = nix::fcntl::fcntl(&file, nix::fcntl::FcntlArg::F_GETFL).unwrap();
        assert_eq!(flags & libc::O_NONBLOCK, 0);
    }
}
