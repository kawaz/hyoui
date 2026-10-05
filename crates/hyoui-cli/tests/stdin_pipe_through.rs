//! DR-0019 §5 e2e: 非 tty stdin が子に届き、EOF で子が終わること (pipe-through)。
//! attach (`hyoui run`) と `hyoui run --detached` で同じ結果になることを見る。
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

/// 1 行目を FIFO に返してから残りを `CHILD_SCRIPT` と同じく読む script (= 入力が逐次
/// 届くことを、書き手を閉じる前に観測する)。
const ECHO_FIRST_LINE_SCRIPT: &str =
    "read l; printf '%s\\n' \"$l\" > \"$2\"; cat > \"$1\"; printf '%s\\n' \"$?\" > \"$2\"";

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
        self.run_script(flags, CHILD_SCRIPT)
    }

    /// `hyoui run [flags] -- sh -c <script> sh <out> <fifo>` の Command。
    fn run_script(&self, flags: &[&str], script: &str) -> Command {
        let mut c = Command::new(hyoui_bin());
        c.args(["run", &format!("--session={}", self.session)])
            .args(flags)
            .args(["--", "sh", "-c", script, "sh"])
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

    /// 子が FIFO に書いた次の 1 件 (= exit code / 返した行) を読む。期限内に書かれなければ
    /// `None` (= 子が終わらない)。
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

/// `--detached` でも改行で終わる 2 行の pipe が子に届き、EOF で子が終わる (= attach と同じ)。
#[test]
fn detached_pipe_two_lines_reaches_eof() {
    let (status, rc, got) = run_with_pipe("pipe-det-2l", &["--detached"], b"l1\nl2\n");
    assert_eq!(rc.as_deref(), Some("0"), "子が EOF で終わること");
    assert_eq!(got, b"l1\nl2\n");
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
}

/// `--detached` でも改行で終わらない pipe は EOT 2 個で EOF になる (= attach と同じ)。
#[test]
fn detached_pipe_without_trailing_newline_reaches_eof() {
    let (status, rc, got) = run_with_pipe("pipe-det-nonl", &["--detached"], b"hoge");
    assert_eq!(rc.as_deref(), Some("0"), "子が EOF で終わること");
    assert_eq!(got, b"hoge");
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
}

/// `--detached` に file を stdin で渡しても子に届き、EOF で終わる。
#[test]
fn detached_file_reaches_eof() {
    let cell = Cell::new("pipe-det-file");
    let input = cell.dir.path().join("in.txt");
    std::fs::write(&input, b"f1\nf2\n").expect("write input");
    let child = cell
        .run_command(&["--detached"])
        .stdin(std::fs::File::open(&input).expect("open input"))
        .spawn()
        .expect("spawn hyoui run");
    let deadline = Instant::now() + DEADLINE;
    let status = wait_with_deadline(child, deadline);
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
    assert_eq!(cell.child_exit(deadline).as_deref(), Some("0"));
    assert_eq!(cell.received(), b"f1\nf2\n");
}

/// `/dev/null` も他の非 tty と同じく扱う: attach も `--detached` も 0 byte を流して EOF で
/// EOT を送り、子は EOF で終わる (= 両者で同じ結果)。
#[test]
fn dev_null_reaches_eof_in_attach_and_detached() {
    for (session, flags) in [("null-attach", &[][..]), ("null-det", &["--detached"][..])] {
        let cell = Cell::new(session);
        let child = cell
            .run_command(flags)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn hyoui run");
        let deadline = Instant::now() + DEADLINE;
        let status = wait_with_deadline(child, deadline);
        assert!(
            status.is_some_and(|s| s.success()),
            "{flags:?} run の exit: {status:?}"
        );
        assert_eq!(cell.child_exit(deadline).as_deref(), Some("0"), "{flags:?}");
        assert!(cell.received().is_empty(), "{flags:?}");
    }
}

/// `--detached --stdin-eof=detach` は入力を流すが EOT は送らない (= 子は残る)。
#[test]
fn detached_stdin_eof_detach_sends_no_eot() {
    let cell = Cell::new("pipe-det-noeot");
    let mut child = cell
        .run_script(
            &["--detached", "--stdin-eof=detach"],
            ECHO_FIRST_LINE_SCRIPT,
        )
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn hyoui run");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"l1\nl2\n").expect("write stdin");
    }
    let deadline = Instant::now() + DEADLINE;
    let status = wait_with_deadline(child, deadline);
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
    assert_eq!(
        cell.child_exit(deadline).as_deref(),
        Some("l1"),
        "入力は届く"
    );
    let window = Instant::now() + Duration::from_millis(1500);
    assert_eq!(cell.child_exit(window), None, "EOT が届いてはいけない");
}

/// 終わらない pipe でも `run --detached` はすぐ戻り、入力は逐次子に届き、書き手が閉じたら
/// 子は EOF で終わる。
#[test]
fn detached_returns_while_pipe_stays_open() {
    let cell = Cell::new("pipe-det-open");
    let mut child = cell
        .run_script(&["--detached"], ECHO_FIRST_LINE_SCRIPT)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn hyoui run");
    let mut stdin = child.stdin.take().expect("stdin");
    let deadline = Instant::now() + DEADLINE;
    let status = wait_with_deadline(child, deadline);
    assert!(
        status.is_some_and(|s| s.success()),
        "書き手が開いたままでも run は戻ること: {status:?}"
    );
    stdin.write_all(b"one\n").expect("write one");
    assert_eq!(
        cell.child_exit(deadline).as_deref(),
        Some("one"),
        "逐次届く"
    );
    stdin.write_all(b"two\n").expect("write two");
    drop(stdin);
    assert_eq!(
        cell.child_exit(deadline).as_deref(),
        Some("0"),
        "EOF で終わる"
    );
    assert_eq!(cell.received(), b"two\n");
}
