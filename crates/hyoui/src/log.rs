//! daemon のログ (DR-0037 段 2、E-1)。
//!
//! 起動後の daemon は、fd 2 と logger の書き先を session ごとのログファイル
//! (`<状態の root>/sessions/logs/<session id>.log`、[`crate::paths::Env::session_log_path`])
//! にする。serve loop はログを bounded channel に待たない送信 (`try_send`) で積むだけで、
//! ファイルへの write は logger thread が行う (DR-0037 I-2)。channel が満杯なら捨てて件数を
//! 数え、次に書けた時に件数を 1 行残す。
//!
//! logger を立てていない process (= ready 通知の前の daemon、test が `Session` を直接
//! 動かす時) では [`emit`] は stderr にそのまま書く (= ready の前の失敗は呼び出し元の
//! stderr に出る、DR-0037 裁定 Q3)。

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

/// 1 つのログファイルに書く上限 (bytes)。超えたら印を 1 行書いて以後の行を捨てる。
pub const FILE_CAP_BYTES: u64 = 1024 * 1024;

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

/// process に 1 つの送り口。[`install`] で入れ、[`Logger::shutdown`] で外す。
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

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

/// session のログファイルを開く (無ければ作る)。dir は mode 0700、file は 0600 で作り、
/// 追記で開く。fd は CLOEXEC (= 子と upgrade の exec には fd 2 の複製だけが渡る)。
///
/// # Errors
///
/// dir を作れない、file を開けない時。
pub fn open_session_log(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// 立てた logger thread。[`Logger::shutdown`] で止める。
#[derive(Debug)]
pub struct Logger {
    done: Receiver<()>,
}

/// logger thread を立て、process の送り口にする。
///
/// `path` は `file` を開いた path。渡すと、logger の終了時にファイルが空 (= 何も書かれ
/// なかった) なら消す。消すのは `path` が今も `file` と同じ実体を指す時だけ。
///
/// # Errors
///
/// thread を立てられない時 (= 送り口は入れない。[`emit`] は stderr に書き続ける)。
pub fn install(file: File, path: Option<PathBuf>) -> std::io::Result<Logger> {
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
    *SINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Sink { tx, dropped });
    Ok(Logger { done })
}

impl Logger {
    /// 送り口を外し、logger が積まれた行を書き終えて終わるのを最大 `timeout` 待つ。
    /// 期限内に終われば `true`。
    ///
    /// daemon が serve loop を抜けた後、process を終える前に呼ぶ (= 終わり際のログを
    /// 書き切る)。fs が応答しない時も `timeout` で戻る。
    pub fn shutdown(self, timeout: Duration) -> bool {
        let sink = SINK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(sink);
        self.done.recv_timeout(timeout).is_ok()
    }
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

    fn write_line(&mut self, unix_ms: u64, text: &str) {
        if self.capped {
            return;
        }
        let stamp = crate::time::format_unix_ms_iso8601(unix_ms);
        let line = format!("{stamp} {}\n", text.trim_end_matches('\n'));
        if self.written + line.len() as u64 > self.cap {
            self.capped = true;
            let mark = format!(
                "{stamp} hyoui: log: reached the size cap ({} bytes); further lines are dropped\n",
                self.cap
            );
            self.put(&mark);
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

    /// 上限を超える行は書かず、印を 1 度だけ書いて以後を捨てる。
    #[test]
    fn size_cap_writes_one_mark_then_drops() {
        let mut buf = Vec::new();
        let mut w = LogWriter::new(&mut buf, 0, 40);
        w.write_line(0, "0123456789"); // 31 bytes
        w.write_line(0, "0123456789"); // 超える → 印
        w.write_line(0, "0123456789"); // 捨てる
        let out = text(&buf);
        assert_eq!(out.matches("0123456789").count(), 1, "{out}");
        assert_eq!(
            out.matches("reached the size cap (40 bytes)").count(),
            1,
            "{out}"
        );
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
}
