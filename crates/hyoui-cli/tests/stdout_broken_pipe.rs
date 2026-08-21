//! stdout の読み手が先に去ったとき (= `hyoui status | head -5`) に panic しないことの
//! 回帰 test (issue 2026-07-30)。
//!
//! Rust runtime は起動時に SIGPIPE を SIG_IGN にするため、素の `println!` は EPIPE で
//! `failed printing to stdout: Broken pipe` panic (= exit 101) になる。hyoui は
//! 「print して終了する」subcommand に限って SIGPIPE を SIG_DFL に戻し、他の UNIX CLI
//! と同じ「読み手が去ったら SIGPIPE で静かに終わる」挙動にしている。
//!
//! test は読み端を即 close した pipe を stdout に渡して EPIPE を **決定的に** 起こす
//! (= 出力量や head の timing に依存しない)。

use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 引数の subcommand を「読み手のいない pipe」に向けて実行し、(exit code, signal, stderr)
/// を返す。
fn run_with_dead_stdout(args: &[&str]) -> (Option<i32>, Option<i32>, String) {
    use std::os::unix::process::ExitStatusExt;

    let (rd, wr) = nix::unistd::pipe().expect("pipe");
    // 読み端の FD_CLOEXEC 必須: 素の `pipe(2)` の fd は子に継承されるため、子自身が
    // 読み手として残って EPIPE が起きない (= test が常に green になる偽陽性。macOS の
    // nix には `pipe2` が無いので fcntl で立てる)。
    nix::fcntl::fcntl(
        &rd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .expect("set FD_CLOEXEC on read end");
    let wr: OwnedFd = wr;
    let mut child = Command::new(hyoui_bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(wr))
        .stderr(Stdio::piped())
        // test 自身が hyoui 配下で動いている場合 (= dogfooding) の干渉を避ける。
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_NAMESPACE")
        .spawn()
        .expect("spawn hyoui");
    // 読み端を即 close する = 以降の stdout write は必ず EPIPE / SIGPIPE。
    drop(rd);

    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("wait");
    (status.code(), status.signal(), stderr)
}

/// 読み手が去った stdout に書いても panic しない (= exit 101 + "panicked" にならない)。
#[test]
fn print_and_exit_commands_do_not_panic_on_broken_pipe() {
    for args in [
        vec!["--version"],
        vec!["--help"],
        vec!["completion", "zsh"],
        vec!["list"],
    ] {
        let (code, signal, stderr) = run_with_dead_stdout(&args);
        assert!(
            !stderr.contains("panicked"),
            "{args:?}: panic してはならない: stderr={stderr:?}"
        );
        assert_ne!(
            code,
            Some(101),
            "{args:?}: panic 由来の exit 101 になってはならない: stderr={stderr:?}"
        );
        // 読み手が居ない stdout に書けば必ず SIGPIPE 死 (= signal 13、shell から
        // 見ると 141)。他の UNIX CLI (`ls | head` 等) と同じ終わり方。
        assert_eq!(
            signal,
            Some(nix::libc::SIGPIPE),
            "{args:?}: SIGPIPE で終了すべき (code={code:?}) stderr={stderr:?}"
        );
    }
}
