//! DR-0043 e2e: daemon (anchor 経路) が起こした子は、SIGINT / SIGQUIT / SIGTSTP / SIGTTIN /
//! SIGTTOU / SIGPIPE が既定 (SIG_DFL) で、signal mask が空の状態で始まる。呼び出し元が無視・
//! block していた設定も、hyoui 自身 (Rust の runtime が SIGPIPE を無視にする) の設定も子に
//! 届かない。一覧の外 (SIGHUP) の無視は直接実行と同じく引き継ぐ。
//!
//! 子は perl で、自分の `%SIG` と `sigprocmask` を 1 行にして FIFO に書く (= `/proc` の無い
//! macOS でも子の中から見える)。FIFO は test 側が子の起動前に O_RDWR で開いて持つので、子の
//! open は読み手待ちで止まらず、`poll` の期限で「報告が来なかった」を判定できる。

mod common;

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use nix::poll::{PollFd, PollFlags, PollTimeout};

/// 子の報告と `hyoui run` の戻りを待つ上限。
const DEADLINE: Duration = Duration::from_secs(10);

/// 子に自分の signal の扱いと mask を `$ARGV[0]` (FIFO) へ 1 行で報告させる perl。
const PROBE: &str = r#"use POSIX qw(:signal_h);
my @r = map { my $h = $SIG{$_}; "$_=" . ((!defined $h || $h eq '' || $h eq 'DEFAULT') ? 'DFL' : $h) } qw(INT QUIT TSTP TTIN TTOU PIPE HUP);
my $old = POSIX::SigSet->new; sigprocmask(SIG_BLOCK, POSIX::SigSet->new, $old);
open(my $f, '>', $ARGV[0]) or die "open: $!";
print $f join(' ', @r), ' mask=', join(',', grep { $old->ismember($_) } 1..31), "\n";
close $f;"#;

/// 呼び出し元の役: 6 つ + SIGHUP を無視にし、SIGTERM / SIGUSR2 を block して `@ARGV` を exec
/// する perl (= `$(...)` の中や非対話 shell の `cmd &` から起動された hyoui を作る)。
const IGNORING_CALLER: &str = r#"use POSIX qw(:signal_h);
$SIG{$_} = 'IGNORE' for qw(INT QUIT TSTP TTIN TTOU PIPE HUP);
sigprocmask(SIG_BLOCK, POSIX::SigSet->new(SIGTERM, SIGUSR2)) or die "sigprocmask: $!";
exec { $ARGV[0] } @ARGV or die "exec: $!";"#;

/// 呼び出し元の役: 7 つを既定にし、mask を空にして `@ARGV` を exec する perl (= test を
/// 走らせる環境の設定に依らず、何も変えていない呼び出し元を作る)。
const DEFAULT_CALLER: &str = r#"use POSIX qw(:signal_h);
$SIG{$_} = 'DEFAULT' for qw(INT QUIT TSTP TTIN TTOU PIPE HUP);
sigprocmask(SIG_SETMASK, POSIX::SigSet->new) or die "sigprocmask: $!";
exec { $ARGV[0] } @ARGV or die "exec: $!";"#;

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 1 セル分の作業場所 (状態の root と報告用の FIFO)。後始末は `dir` の drop が持つ。
struct Cell {
    dir: SessionDir,
    fifo: std::fs::File,
}

impl Cell {
    fn new(name: &str) -> Self {
        let dir = SessionDir::new(&format!("hyoui-sig-{name}-"));
        let path = dir.path().join("report.fifo");
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600))
            .expect("mkfifo");
        let fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open fifo");
        Self { dir, fifo }
    }

    /// `[caller...] hyoui run --detached --pty-stdin -- perl -e PROBE <fifo>` を実行し、
    /// `hyoui run` の終了 (= ready 通知後の親の exit) を待つ。
    fn run_detached(&self, caller: &[&str]) {
        let fifo = self.dir.path().join("report.fifo");
        let mut argv: Vec<String> = caller.iter().map(|s| (*s).to_string()).collect();
        argv.push(hyoui_bin().display().to_string());
        argv.extend(
            [
                "run",
                "--detached",
                "--pty-stdin",
                "--",
                "perl",
                "-e",
                PROBE,
            ]
            .map(str::to_string),
        );
        argv.push(fifo.display().to_string());
        let out = common::pty::capture_with_deadline(
            Command::new(&argv[0])
                .args(&argv[1..])
                .env("HYOUI_STATE_DIR", self.dir.path())
                .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
                .env_remove("HYOUI_SESSION_ID")
                .env_remove("HYOUI_LOCK_TOKEN")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            DEADLINE,
        )
        .expect("run hyoui");
        assert!(out.status.success(), "hyoui run --detached failed: {out:?}");
    }

    /// 子が FIFO に書いた報告の 1 行。期限内に来なければ `None`。
    fn report(&self) -> Option<String> {
        let deadline = Instant::now() + DEADLINE;
        let mut acc = Vec::new();
        while !acc.contains(&b'\n') {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
            let mut fds = [PollFd::new(self.fifo.as_fd(), PollFlags::POLLIN)];
            match nix::poll::poll(&mut fds, timeout) {
                Ok(n) if n > 0 => {
                    let mut buf = [0u8; 256];
                    let n = nix::unistd::read(&self.fifo, &mut buf).expect("read fifo");
                    acc.extend_from_slice(&buf[..n]);
                }
                _ => return None,
            }
        }
        Some(String::from_utf8_lossy(&acc).trim().to_string())
    }
}

/// 呼び出し元が 6 つを無視し、SIGTERM / SIGUSR2 を block していても、子は既定の扱いと空の
/// mask で始まる。SIGHUP (一覧の外) の無視は引き継ぐ。
#[test]
fn child_resets_signals_ignored_and_blocked_by_the_caller() {
    let cell = Cell::new("ignoring");
    cell.run_detached(&["perl", "-e", IGNORING_CALLER]);
    assert_eq!(
        cell.report().as_deref(),
        Some("INT=DFL QUIT=DFL TSTP=DFL TTIN=DFL TTOU=DFL PIPE=DFL HUP=IGNORE mask=")
    );
}

/// 呼び出し元が何も変えていなくても、hyoui (Rust の runtime) が無視にした SIGPIPE は子に
/// 届かない。
#[test]
fn child_does_not_inherit_hyoui_own_sigpipe_ignore() {
    let cell = Cell::new("plain");
    cell.run_detached(&["perl", "-e", DEFAULT_CALLER]);
    assert_eq!(
        cell.report().as_deref(),
        Some("INT=DFL QUIT=DFL TSTP=DFL TTIN=DFL TTOU=DFL PIPE=DFL HUP=DFL mask=")
    );
}
