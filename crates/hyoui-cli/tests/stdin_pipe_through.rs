//! DR-0019 §5 e2e: 非 tty stdin が子に届き、EOF で子が終わること (pipe-through)。
//!
//! 子は `cat` で、受け取った bytes を file に書き、終わったら exit code を FIFO に書く。
//! 完了は FIFO の読みで観測する (= sleep で待たない)。FIFO は test 側が子の起動前に
//! O_RDWR で開いて持ち続ける (= 子の書き込み側の open が読み手待ちで止まらない) ので、
//! `poll` の期限で「子が終わらなかった」を判定できる。

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 子が受け取った bytes と exit code を書く script (`$1` = 出力 file、`$2` = FIFO)。
const CHILD_SCRIPT: &str = "cat > \"$1\"; printf '%s\\n' \"$?\" > \"$2\"";

/// 子の終了や run の戻りを待つ上限 (= 超えたら「終わらなかった」と判定する)。
const DEADLINE: Duration = Duration::from_secs(10);

/// 1 セル分の作業場所 (runtime dir / 出力 file / FIFO) と、後始末。
struct Cell {
    dir: tempfile::TempDir,
    session: String,
    fifo: std::fs::File,
}

impl Cell {
    fn new(session: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("hyoui-pipe-")
            .tempdir()
            .expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        nix::unistd::mkfifo(
            &dir.path().join("rc.fifo"),
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .expect("mkfifo");
        let fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("rc.fifo"))
            .expect("open fifo");
        Self {
            dir,
            session: session.to_string(),
            fifo,
        }
    }

    fn out_path(&self) -> PathBuf {
        self.dir.path().join("out.bin")
    }

    fn fifo_path(&self) -> PathBuf {
        self.dir.path().join("rc.fifo")
    }

    /// `hyoui run [flags] -- sh -c CHILD_SCRIPT` の Command (stdin は呼び出し側が決める)。
    fn run_command(&self, flags: &[&str]) -> Command {
        let mut c = Command::new(hyoui_bin());
        c.args(["run", &format!("--session={}", self.session)])
            .args(flags)
            .args(["--", "sh", "-c", CHILD_SCRIPT, "sh"])
            .arg(self.out_path())
            .arg(self.fifo_path())
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("HYOUI_SESSION_ID")
            .env_remove("HYOUI_LOCK_TOKEN")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    }

    /// 子の exit code を FIFO から読む。期限内に書かれなければ `None` (= 子が終わらない)。
    fn child_exit(&self, deadline: Instant) -> Option<String> {
        let fifo = &self.fifo;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
        let mut fds = [PollFd::new(fifo.as_fd(), PollFlags::POLLIN)];
        match nix::poll::poll(&mut fds, timeout) {
            Ok(n) if n > 0 => {
                let mut buf = [0u8; 64];
                let n = nix::unistd::read(fifo, &mut buf).expect("read fifo");
                Some(String::from_utf8_lossy(&buf[..n]).trim().to_string())
            }
            _ => None,
        }
    }

    /// 子が受け取った bytes。
    fn received(&self) -> Vec<u8> {
        std::fs::read(self.out_path()).unwrap_or_default()
    }
}

impl Drop for Cell {
    fn drop(&mut self) {
        let _ = Command::new(hyoui_bin())
            .args(["kill", &self.session, "--signal=KILL"])
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env_remove("HYOUI_SESSION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `child` の終了を別 thread で待ち、期限内に終われば status を返す。
fn wait_with_deadline(child: Child, deadline: Instant) -> Option<ExitStatus> {
    let (tx, rx) = mpsc::channel();
    let mut child = child;
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
        .and_then(Result::ok)
}

/// pipe に `input` を書いて閉じ、`hyoui run [flags]` を走らせる。返り値は
/// (run の exit status、子の exit code、子が受け取った bytes)。
fn run_with_pipe(
    session: &str,
    flags: &[&str],
    input: &[u8],
) -> (Option<ExitStatus>, Option<String>, Vec<u8>) {
    let cell = Cell::new(session);
    let mut child = cell
        .run_command(flags)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn hyoui run");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(input).expect("write stdin");
    }
    let deadline = Instant::now() + DEADLINE;
    let status = wait_with_deadline(child, deadline);
    let rc = cell.child_exit(deadline);
    (status, rc, cell.received())
}

/// 改行で終わる 2 行の pipe: 子は全行を受け取り、EOF で exit 0。run も 0 で戻る。
#[test]
fn attach_pipe_two_lines_reaches_eof() {
    let (status, rc, got) = run_with_pipe("pipe-attach-2l", &[], b"l1\nl2\n");
    assert_eq!(rc.as_deref(), Some("0"), "子が EOF で終わること");
    assert_eq!(got, b"l1\nl2\n");
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
}

/// 改行で終わらない pipe (`printf hoge |`): 0x04 を 2 個送るので、途中の行が確定した
/// 後に行頭の EOF が届き、子は `hoge` を受け取って exit 0 する。
#[test]
fn attach_pipe_without_trailing_newline_reaches_eof() {
    let (status, rc, got) = run_with_pipe("pipe-attach-nonl", &[], b"hoge");
    assert_eq!(rc.as_deref(), Some("0"), "子が EOF で終わること");
    assert_eq!(got, b"hoge");
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
}

/// 作業場所 helper の健全性 (= FIFO が期限で None を返す): 子を起動しない Cell では
/// 期限切れで `None` になる。これが壊れると上の test が「終わらない」を検出できない。
#[test]
fn fifo_deadline_reports_unfinished_child() {
    let cell = Cell::new("pipe-unused");
    let deadline = Instant::now() + Duration::from_millis(200);
    assert_eq!(cell.child_exit(deadline), None);
}
