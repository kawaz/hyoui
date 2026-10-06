//! DR-0042 e2e: 非 tty の stdin は子の fd 0 にそのまま渡り、子は直接実行と同じく pipe を
//! 読んで pipe の EOF で終わる。`hyoui run` (attach) と `hyoui run --detached` で同じ結果に
//! なることを見る。`--pty-stdin` では子の stdin が PTY になり `hyoui input` が届く。
//!
//! 子は `sh -c <script> sh <out> <fifo>` で、受け取った bytes を `<out>` に書き、観測した
//! こと (exit code / 1 行目 / isatty) を FIFO に 1 行ずつ書く。完了は FIFO の読みで観測する
//! (= sleep で待たない)。FIFO は test 側が子の起動前に O_RDWR で開いて持ち続ける (= 子の
//! 書き込み側の open が読み手待ちで止まらない) ので、`poll` の期限で「子が終わらなかった」を
//! 判定できる。
//!
//! `hyoui run` は専用の PTY を制御端末にして起こす ([`spawn_in_private_ctty`])。非 detached の
//! attach client は stdin が tty でないと `/dev/tty` を開いてキーを読むので、端末から test を
//! 走らせた時に開発者の端末を奪わないようにするため。

mod common;

use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::pty::{PrivateCtty, spawn_in_private_ctty};
use common::session_dir::SessionDir;
use nix::poll::{PollFd, PollFlags, PollTimeout};
use pty_process::blocking::Command as PtyCommand;

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 子が受け取った bytes と exit code を書く script (`$1` = 出力 file、`$2` = FIFO)。
const CHILD_SCRIPT: &str = "cat > \"$1\"; printf '%s\\n' \"$?\" > \"$2\"";

/// 1 行目を FIFO に返してから残りを `CHILD_SCRIPT` と同じく読む script (= 入力が逐次
/// 届くことを、書き手を閉じる前に観測する)。
const ECHO_FIRST_LINE_SCRIPT: &str =
    "read l; printf '%s\\n' \"$l\" > \"$2\"; cat > \"$1\"; printf '%s\\n' \"$?\" > \"$2\"";

/// 子の fd 0 / fd 1 が tty か (`1` / `0`) を FIFO に 1 行で返し、続けて `CHILD_SCRIPT` と同じく
/// 読む script。
const ISATTY_SCRIPT: &str = "[ -t 0 ] && a=1 || a=0; [ -t 1 ] && b=1 || b=0; \
     printf '%s %s\\n' \"$a\" \"$b\" > \"$2\"; cat > \"$1\"; printf '%s\\n' \"$?\" > \"$2\"";

/// `/dev/tty` (= 子の PTY) から 1 行読んで FIFO に返し、続けて stdin (= pipe) を読み切って
/// exit 3 する script (= pipe を読みつつキーボードを使う TUI と同じ配線。exit code で attach
/// client が子の終了まで残ったかを見る)。
const TTY_KEY_SCRIPT: &str =
    "read k < /dev/tty; printf '%s\\n' \"$k\" > \"$2\"; cat > \"$1\"; exit 3";

/// stdin を閉じて走り続ける script (= 子が先に読み手でなくなる。書き手に EPIPE が届くかを
/// 見る)。
const CLOSE_STDIN_SCRIPT: &str = "exec 0<&-; printf 'closed\\n' > \"$2\"; sleep 30";

/// 子の終了や run の戻りを待つ上限 (= 超えたら「終わらなかった」と判定する)。
const DEADLINE: Duration = Duration::from_secs(10);

/// 1 セル分の作業場所 (runtime dir / 出力 file / FIFO)。後始末は `dir` の drop が
/// 持つ (= 配下の socket を走査して残った daemon を畳む)。
struct Cell {
    dir: SessionDir,
    session: String,
    fifo: std::fs::File,
}

/// spawn した `hyoui run` と、その制御端末 (= 子の終了まで持つ。書けば attach client が
/// `/dev/tty` から読むキーになる)。
struct Running {
    child: Child,
    ctty: PrivateCtty,
}

impl Cell {
    fn new(session: &str) -> Self {
        let dir = SessionDir::new("hyoui-pipe-");
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

    /// `hyoui run [flags] -- sh -c <script> sh <out> <fifo>` を専用の制御端末で起こす。
    /// stdout / stderr は捨てる。
    fn spawn(&self, flags: &[&str], script: &str, stdin: impl Into<Stdio>) -> Running {
        let mut args: Vec<String> = vec!["run".into(), format!("--session={}", self.session)];
        args.extend(flags.iter().map(|f| (*f).to_string()));
        args.extend(["--", "sh", "-c", script, "sh"].map(str::to_string));
        args.push(self.out_path().display().to_string());
        args.push(self.fifo_path().display().to_string());
        let cmd = PtyCommand::new(hyoui_bin())
            .args(args)
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("HYOUI_SESSION_ID")
            .env_remove("HYOUI_LOCK_TOKEN")
            .env_remove("HYOUI_NAMESPACE")
            .stdin(stdin)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (ctty, child) = spawn_in_private_ctty(cmd);
        Running { child, ctty }
    }

    /// `hyoui <args>` を同じ runtime dir で実行する (`input` / `kill` 等)。
    fn hyoui(&self, args: &[&str]) -> std::process::Output {
        common::pty::capture_with_deadline(
            Command::new(hyoui_bin())
                .args(args)
                .env("XDG_RUNTIME_DIR", self.dir.path())
                .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
                .env_remove("HYOUI_SESSION_ID")
                .env_remove("HYOUI_LOCK_TOKEN")
                .env_remove("HYOUI_NAMESPACE")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            DEADLINE,
        )
        .expect("run hyoui helper")
    }

    /// session の socket が現れるまで待つ (= 非 detached の run は daemon の起動を待たずに
    /// 戻らないので、外から操作する前に要る)。
    fn wait_socket(&self, deadline: Instant) -> bool {
        let sock = self
            .dir
            .path()
            .join("hyoui")
            .join(format!("{}.sock", self.session));
        while Instant::now() < deadline {
            if sock.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// 子が FIFO に書いた次の 1 件 (= exit code / 返した行) を読む。期限内に書かれなければ
    /// `None` (= 子が終わらない)。
    fn child_line(&self, deadline: Instant) -> Option<String> {
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

/// pipe に `input` を書いて閉じ、`hyoui run [flags] -- sh -c <script>` を走らせる。返り値は
/// (run の exit status、子が FIFO に書いた最初の行、子が受け取った bytes)。
fn run_with_pipe(
    session: &str,
    flags: &[&str],
    script: &str,
    input: &[u8],
) -> (Option<ExitStatus>, Option<String>, Vec<u8>) {
    let cell = Cell::new(session);
    let mut running = cell.spawn(flags, script, Stdio::piped());
    {
        let mut stdin = running.child.stdin.take().expect("stdin");
        stdin.write_all(input).expect("write stdin");
    }
    let deadline = Instant::now() + DEADLINE;
    let line = cell.child_line(deadline);
    let status = wait_with_deadline(running.child, deadline);
    (status, line, cell.received())
}

/// 改行で終わる 2 行の pipe: 子は全行を受け取り、EOF で exit 0。run も 0 で戻る。
#[test]
fn attach_pipe_two_lines_reaches_eof() {
    let (status, rc, got) = run_with_pipe("pipe-attach-2l", &[], CHILD_SCRIPT, b"l1\nl2\n");
    assert_eq!(rc.as_deref(), Some("0"), "子が EOF で終わること");
    assert_eq!(got, b"l1\nl2\n");
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
}

/// 改行で終わらない pipe (`printf hoge |`): pipe の EOF がそのまま子に届くので、子は
/// `hoge` を受け取って exit 0 する。
#[test]
fn attach_pipe_without_trailing_newline_reaches_eof() {
    let (status, rc, got) = run_with_pipe("pipe-attach-nonl", &[], CHILD_SCRIPT, b"hoge");
    assert_eq!(rc.as_deref(), Some("0"), "子が EOF で終わること");
    assert_eq!(got, b"hoge");
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
}

/// バイナリ (0x03 / 0x00 / 0x04 / CR) が化けずに届く (= ISIG / ICRNL / VEOF / echo が
/// かからない。`printf 'a\003b\000c\n' | hyoui run -- od -c` 相当)。attach と `--detached` で
/// 同じ。
#[test]
fn binary_pipe_reaches_child_unchanged() {
    let input: &[u8] = b"a\x03b\x00c\x04d\re\n";
    for (session, flags) in [("bin-attach", &[][..]), ("bin-det", &["--detached"][..])] {
        let (status, rc, got) = run_with_pipe(session, flags, CHILD_SCRIPT, input);
        assert_eq!(rc.as_deref(), Some("0"), "{flags:?}: 子が EOF で終わること");
        assert_eq!(got, input, "{flags:?}: bytes がそのまま届くこと");
        assert!(
            status.is_some_and(|s| s.success()),
            "{flags:?} run の exit: {status:?}"
        );
    }
}

/// 作業場所 helper の健全性 (= FIFO が期限で None を返す): 子を起動しない Cell では
/// 期限切れで `None` になる。これが壊れると上の test が「終わらない」を検出できない。
#[test]
fn fifo_deadline_reports_unfinished_child() {
    let cell = Cell::new("pipe-unused");
    let deadline = Instant::now() + Duration::from_millis(200);
    assert_eq!(cell.child_line(deadline), None);
}

/// `--detached` でも改行で終わる 2 行 / 改行で終わらない pipe が子に届き、EOF で子が
/// 終わる (= attach と同じ)。
#[test]
fn detached_pipe_reaches_eof() {
    for (session, input) in [
        ("pipe-det-2l", &b"l1\nl2\n"[..]),
        ("pipe-det-nonl", &b"hoge"[..]),
    ] {
        let (status, rc, got) = run_with_pipe(session, &["--detached"], CHILD_SCRIPT, input);
        assert_eq!(rc.as_deref(), Some("0"), "{session}: 子が EOF で終わること");
        assert_eq!(got, input, "{session}");
        assert!(
            status.is_some_and(|s| s.success()),
            "{session} run の exit: {status:?}"
        );
    }
}

/// `--detached` に file を stdin で渡しても子に届き、EOF で終わる。
#[test]
fn detached_file_reaches_eof() {
    let cell = Cell::new("pipe-det-file");
    let input = cell.dir.path().join("in.txt");
    std::fs::write(&input, b"f1\nf2\n").expect("write input");
    let running = cell.spawn(
        &["--detached"],
        CHILD_SCRIPT,
        std::fs::File::open(&input).expect("open input"),
    );
    let deadline = Instant::now() + DEADLINE;
    let status = wait_with_deadline(running.child, deadline);
    assert!(
        status.is_some_and(|s| s.success()),
        "run の exit: {status:?}"
    );
    assert_eq!(cell.child_line(deadline).as_deref(), Some("0"));
    assert_eq!(cell.received(), b"f1\nf2\n");
}

/// `/dev/null` も他の非 tty と同じく子の stdin になる: attach も `--detached` も子はすぐ
/// EOF を読んで終わる (= 直接実行の `prog </dev/null` と同じ)。
#[test]
fn dev_null_reaches_eof_in_attach_and_detached() {
    for (session, flags) in [("null-attach", &[][..]), ("null-det", &["--detached"][..])] {
        let cell = Cell::new(session);
        let running = cell.spawn(flags, CHILD_SCRIPT, Stdio::null());
        let deadline = Instant::now() + DEADLINE;
        let status = wait_with_deadline(running.child, deadline);
        assert!(
            status.is_some_and(|s| s.success()),
            "{flags:?} run の exit: {status:?}"
        );
        assert_eq!(cell.child_line(deadline).as_deref(), Some("0"), "{flags:?}");
        assert!(cell.received().is_empty(), "{flags:?}");
    }
}

/// 子の isatty: pipe を渡した時は fd 0 が偽・fd 1 が真 (= 直接実行と同じ)、`--pty-stdin` では
/// 両方真。attach と `--detached` の両方で見る。
#[test]
fn child_isatty_follows_stdin_kind() {
    let cases: [(&str, &[&str], &str); 4] = [
        ("tty-pipe-attach", &[], "0 1"),
        ("tty-pipe-det", &["--detached"], "0 1"),
        ("tty-pty-attach", &["--pty-stdin"], "1 1"),
        ("tty-pty-det", &["--detached", "--pty-stdin"], "1 1"),
    ];
    for (session, flags, want) in cases {
        let cell = Cell::new(session);
        let mut running = cell.spawn(flags, ISATTY_SCRIPT, Stdio::piped());
        let deadline = Instant::now() + DEADLINE;
        assert_eq!(
            cell.child_line(deadline).as_deref(),
            Some(want),
            "{flags:?}: 子の isatty(0) isatty(1)"
        );
        // 後始末: pipe を閉じ (= pipe を渡したセルの子はここで終わる)、PTY のセルは kill。
        drop(running.child.stdin.take());
        let _ = cell.hyoui(&["kill", &cell.session]);
        let _ = wait_with_deadline(running.child, Instant::now() + DEADLINE);
    }
}

/// `--pty-stdin` では呼び出し元の pipe は子に届かず、子の stdin (= PTY) に `hyoui input` が
/// 届く。`--detached` と非 detached の両方で見る (= 非 detached の attach client は stdin を
/// 読まない)。
#[test]
fn pty_stdin_receives_hyoui_input_not_the_pipe() {
    for (session, flags) in [
        ("ptyin-det", &["--detached", "--pty-stdin"][..]),
        ("ptyin-attach", &["--pty-stdin"][..]),
    ] {
        let cell = Cell::new(session);
        let mut running = cell.spawn(flags, ECHO_FIRST_LINE_SCRIPT, Stdio::piped());
        {
            let mut stdin = running.child.stdin.take().expect("stdin");
            stdin.write_all(b"from-pipe\n").expect("write stdin");
        }
        let deadline = Instant::now() + DEADLINE;
        assert!(cell.wait_socket(deadline), "{flags:?}: socket");
        let out = cell.hyoui(&["input", &cell.session, "text:from-input", "key:Enter"]);
        assert!(
            out.status.success(),
            "{flags:?}: hyoui input: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            cell.child_line(deadline).as_deref(),
            Some("from-input"),
            "{flags:?}: 子の 1 行目は hyoui input の行 (= pipe ではない)"
        );
        let _ = cell.hyoui(&["kill", &cell.session]);
        let _ = wait_with_deadline(running.child, Instant::now() + DEADLINE);
    }
}

/// stdin が pipe でも、attach client は制御端末 (`/dev/tty`) のキーを子の PTY に届け、子の
/// 終了まで残って exit code を伝える (DR-0042 決定 4)。子は `/dev/tty` からキーを、stdin から
/// pipe を読む。macOS の `/dev/tty` は poll できないので、attach client が制御端末の実体を
/// 開けていないと即座に離脱し、キーが届かず exit code も 0 になる。
#[test]
fn attach_reads_keys_from_ctty_while_pipe_goes_to_child() {
    let cell = Cell::new("ctty-keys");
    let mut running = cell.spawn(&[], TTY_KEY_SCRIPT, Stdio::piped());
    let mut stdin = running.child.stdin.take().expect("stdin");
    stdin.write_all(b"from-pipe\n").expect("write stdin");
    let deadline = Instant::now() + DEADLINE;
    assert!(cell.wait_socket(deadline), "socket");
    // attach client が raw に入って中継を始める前の打鍵も、制御端末の入力 queue に残って
    // 後で読まれる (= 待たずに書いてよい)。CR は子の PTY の ICRNL で改行になる。
    running
        .ctty
        .master
        .write_all(b"from-key\r")
        .expect("write key");
    assert_eq!(
        cell.child_line(deadline).as_deref(),
        Some("from-key"),
        "attach client のキーが子の /dev/tty に届くこと"
    );
    drop(stdin);
    let status = wait_with_deadline(running.child, deadline);
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(3),
        "attach client は子の終了まで残り exit code を伝えること"
    );
    assert_eq!(cell.received(), b"from-pipe\n", "pipe は子の stdin に届く");
}

/// 終わらない pipe でも `run --detached` はすぐ戻り、入力は逐次子に届き、書き手が閉じたら
/// 子は EOF で終わる (= daemon も run も pipe を持ち続けない)。
#[test]
fn detached_returns_while_pipe_stays_open() {
    let cell = Cell::new("pipe-det-open");
    let mut running = cell.spawn(&["--detached"], ECHO_FIRST_LINE_SCRIPT, Stdio::piped());
    let mut stdin = running.child.stdin.take().expect("stdin");
    let deadline = Instant::now() + DEADLINE;
    let status = wait_with_deadline(running.child, deadline);
    assert!(
        status.is_some_and(|s| s.success()),
        "書き手が開いたままでも run は戻ること: {status:?}"
    );
    stdin.write_all(b"one\n").expect("write one");
    assert_eq!(
        cell.child_line(deadline).as_deref(),
        Some("one"),
        "逐次届く"
    );
    stdin.write_all(b"two\n").expect("write two");
    drop(stdin);
    assert_eq!(
        cell.child_line(deadline).as_deref(),
        Some("0"),
        "EOF で終わる"
    );
    assert_eq!(cell.received(), b"two\n");
}

/// 子が stdin を閉じたら、書き手は EPIPE を受ける (= daemon も attach client も pipe の読み手
/// として残らない、DR-0042 決定 2 / 4)。attach と `--detached` の両方で見る。
#[test]
fn writer_gets_epipe_once_child_closes_stdin() {
    for (session, flags) in [
        ("epipe-det", &["--detached"][..]),
        ("epipe-attach", &[][..]),
    ] {
        let cell = Cell::new(session);
        let mut running = cell.spawn(flags, CLOSE_STDIN_SCRIPT, Stdio::piped());
        let stdin = running.child.stdin.take().expect("stdin");
        let deadline = Instant::now() + DEADLINE;
        assert_eq!(
            cell.child_line(deadline).as_deref(),
            Some("closed"),
            "{flags:?}: 子が stdin を閉じた"
        );
        let got = write_until_broken_pipe(stdin, deadline);
        assert!(
            got,
            "{flags:?}: 子が閉じた後も pipe の読み手が残っている (= EPIPE が来ない)"
        );
        let _ = cell.hyoui(&["kill", &cell.session]);
        let _ = wait_with_deadline(running.child, Instant::now() + DEADLINE);
    }
}

/// nonblocking で書き続け、期限内に EPIPE (BrokenPipe) を受けたら `true`。読み手が残って
/// いると pipe が詰まって EAGAIN が続くので、期限まで待って `false`。
fn write_until_broken_pipe(stdin: std::process::ChildStdin, deadline: Instant) -> bool {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let fd: std::os::fd::OwnedFd = stdin.into();
    let flags = fcntl(&fd, FcntlArg::F_GETFL).expect("F_GETFL");
    fcntl(
        &fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .expect("F_SETFL");
    let mut file = std::fs::File::from(fd);
    let chunk = [b'x'; 4096];
    while Instant::now() < deadline {
        match file.write(&chunk) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 読み手が残って詰まっている。読み手が消えれば次の write が EPIPE になる。
                let mut fds = [PollFd::new(file.as_fd(), PollFlags::POLLOUT)];
                let _ = nix::poll::poll(&mut fds, PollTimeout::from(100u16));
            }
            Err(e) => panic!("unexpected write error: {e}"),
        }
    }
    false
}
