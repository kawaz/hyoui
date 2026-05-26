//! `hyoui run --detached` の daemonize 実装。
//!
//! 親 process は `current_exe` を `__daemonize-run --socket=PATH --session=ID --
//! CMD ARGS...` で spawn し、子の socket bind 完了を待ってから socket path を
//! stdout に 1 行出して exit する。
//!
//! 子 ([`run_daemon_child`]) は setsid で controlling tty を切り、stdio を
//! /dev/null に redirect、その後 `Session::start` → `Session::serve` を実行する。
//! Session::start 直後に「ready pipe」に 1 byte 書いて親に通知する。

use std::os::fd::IntoRawFd;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use hyoui::daemon::{DaemonConfig, Session};
use nix::sys::stat::Mode;

use crate::socket_path;

/// `hyoui run --detached` の parent path。
///
/// 子 daemon process を spawn し、socket bind 完了 (= ready pipe からの 1 byte)
/// を待ってから親は exit。stdout に socket path を 1 行出力する。
pub fn run_detached_parent(
    session_id_override: Option<String>,
    socket_override: Option<String>,
    cols: u16,
    rows: u16,
    until: Option<String>,
    cmd: Vec<String>,
) -> ExitCode {
    let session_id = session_id_override.unwrap_or_else(socket_path::auto_session_id);
    let sock = match socket_path::resolve(socket_override.as_deref(), &session_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: socket path 解決失敗: {e}");
            return ExitCode::from(1);
        }
    };

    // ready 通知用 pipe。子の write 端を渡す。
    let (rd, wr) = match nix::unistd::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hyoui: pipe 失敗: {e}");
            return ExitCode::from(1);
        }
    };

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: current_exe 失敗: {e}");
            return ExitCode::from(1);
        }
    };

    // 子に渡す ready pipe write 端の raw fd。`Command::spawn` 後、子側で
    // この fd が継承されている前提で読み書きする (POSIX 標準挙動)。
    let wr_raw = wr.into_raw_fd();

    let mut child = Command::new(exe);
    child.arg("__daemonize-run");
    child.arg(format!("--socket={}", sock.display()));
    child.arg(format!("--session={session_id}"));
    child.arg(format!("--cols={cols}"));
    child.arg(format!("--rows={rows}"));
    child.arg(format!("--ready-fd={wr_raw}"));
    // R5-FB1: --until PATTERN を daemon 子に伝搬。空 string は無効。
    if let Some(needle) = until.as_deref() {
        if !needle.is_empty() {
            child.arg(format!("--until={needle}"));
        }
    }
    child.arg("--");
    for c in cmd {
        child.arg(c);
    }
    // 子の stdio は /dev/null (= daemon らしく独立)。
    child.stdin(Stdio::null());
    child.stdout(Stdio::null());
    child.stderr(Stdio::null());

    let spawn_result = child.spawn();
    // 親側の write 端 fd は spawn 後 close (= 子が close したときに親側 read が
    // EOF を返せるように)。
    let _ = nix::unistd::close(wr_raw);

    if let Err(e) = spawn_result {
        eprintln!("hyoui: spawn 失敗: {e}");
        return ExitCode::from(1);
    }

    // ready pipe から 1 byte 読む (= 子が ready 通知)
    let mut buf = [0u8; 1];
    let n = nix::unistd::read(&rd, &mut buf);
    drop(rd);

    match n {
        Ok(1) => {
            // 子は live + bind 完了。親は exit。socket path を出力。
            println!("{}", sock.display());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("hyoui: daemon child failed to start");
            ExitCode::from(1)
        }
    }
}

/// `__daemonize-run` 隠し subcommand: daemon 子 process の本体。
///
/// 引数:
/// - `--socket=PATH`: bind 対象
/// - `--session=ID`: session id
/// - `--cols=N --rows=N`: 子 PTY サイズ
/// - `--ready-fd=N`: 親に ready 通知する pipe write 端 fd
/// - `--` 以降: 子 PTY が exec する command + args
pub fn run_daemon_child(args: &[String]) -> ExitCode {
    let mut socket: Option<String> = None;
    let mut session: Option<String> = None;
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    let mut ready_fd: Option<i32> = None;
    let mut until: Option<String> = None;
    let mut cmd: Vec<String> = Vec::new();
    let mut in_cmd = false;
    for arg in args {
        if in_cmd {
            cmd.push(arg.clone());
            continue;
        }
        if arg == "--" {
            in_cmd = true;
            continue;
        }
        if let Some(v) = arg.strip_prefix("--socket=") {
            socket = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--session=") {
            session = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--cols=") {
            cols = v.parse().unwrap_or(80);
        } else if let Some(v) = arg.strip_prefix("--rows=") {
            rows = v.parse().unwrap_or(24);
        } else if let Some(v) = arg.strip_prefix("--ready-fd=") {
            ready_fd = v.parse().ok();
        } else if let Some(v) = arg.strip_prefix("--until=") {
            // R5-FB1: 親から渡された needle pattern
            until = Some(v.to_string());
        }
    }

    let socket = match socket {
        Some(s) => PathBuf::from(s),
        None => {
            eprintln!("hyoui: __daemonize-run requires --socket");
            return ExitCode::from(2);
        }
    };
    let session_id = session.unwrap_or_else(|| format!("run-{}", std::process::id()));

    // setsid で新セッションリーダーになる (= controlling tty 切り離し)。
    // 既に session leader の場合は EPERM、無視。
    let _ = nix::unistd::setsid();
    // umask 077 で以降の file 作成を mode 0600 系にする
    nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
    // chdir / (= cwd を free 化、umount 妨げない慣習)
    let _ = nix::unistd::chdir("/");

    let mut dcfg = DaemonConfig::new(session_id, socket, cmd);
    dcfg.cols = cols;
    dcfg.rows = rows;
    // Round2 #2: HYOUI_LOCK_TOKEN env を daemon 側の expected_token に伝搬。
    // detached daemon child は親 process から env を受け継ぐ。
    if let Ok(token) = std::env::var("HYOUI_LOCK_TOKEN") {
        if !token.is_empty() {
            dcfg.expected_token = Some(token);
        }
    }
    // R5-FB1: --until pattern を daemon に配線。
    if let Some(needle) = until {
        if !needle.is_empty() {
            dcfg.until = Some(needle);
        }
    }

    let session = match Session::start(dcfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyoui (daemon child): Session::start 失敗: {e}");
            return ExitCode::from(1);
        }
    };

    // ready 通知: 親に 1 byte 書く。raw fd → OwnedFd 化は hyoui::sys 経由の
    // safe wrapper を使う (hyoui-cli は forbid(unsafe_code))。
    if let Some(fd) = ready_fd {
        let owned = hyoui::sys::raw::own_raw_fd(fd);
        let _ = nix::unistd::write(&owned, b"r");
        // owned は scope 末で drop = close される。
    }

    // session.serve() でブロック (= multi-attach accept + relay + finalize)
    match session.serve() {
        Ok(_code) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}
