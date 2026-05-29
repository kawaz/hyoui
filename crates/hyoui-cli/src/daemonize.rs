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
#[allow(clippy::too_many_arguments)]
pub fn run_detached_parent(
    session_id_override: Option<String>,
    socket_override: Option<String>,
    initial_size: Option<(u16, u16)>,
    until: Option<String>,
    scrollback_rows: Option<usize>,
    debug_dump: Option<String>,
    cmd: Vec<String>,
) -> ExitCode {
    match spawn_detached_daemon_and_wait_ready(
        session_id_override,
        socket_override,
        initial_size,
        until,
        scrollback_rows,
        debug_dump,
        cmd,
    ) {
        Ok((_session_id, sock)) => {
            // 子は live + bind 完了。親は exit。socket path を出力。
            println!("{}", sock.display());
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

/// DR-0015 §1 (exec attach pattern): detached daemon を spawn して ready 通知を待ち、
/// 成功時に `(session_id, sock_path)` を返す。失敗時は stderr を吐いて `ExitCode` を返す。
///
/// `hyoui run --detached` の path (= ready 通知後に親が exit) と、
/// `hyoui run` 非 detached の path (= ready 通知後に親が `hyoui attach` に exec で
/// 自プロセスを置換) で共通利用される spawn + wait helper。
#[allow(clippy::too_many_arguments)]
pub fn spawn_detached_daemon_and_wait_ready(
    session_id_override: Option<String>,
    socket_override: Option<String>,
    initial_size: Option<(u16, u16)>,
    until: Option<String>,
    scrollback_rows: Option<usize>,
    debug_dump: Option<String>,
    cmd: Vec<String>,
) -> Result<(String, PathBuf), ExitCode> {
    let session_id = session_id_override.unwrap_or_else(socket_path::auto_session_id);
    let sock = match socket_path::resolve(socket_override.as_deref(), &session_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: socket path 解決失敗: {e}");
            return Err(ExitCode::from(1));
        }
    };

    // ready 通知用 pipe。子の write 端を渡す。
    let (rd, wr) = match nix::unistd::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hyoui: pipe 失敗: {e}");
            return Err(ExitCode::from(1));
        }
    };

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: current_exe 失敗: {e}");
            return Err(ExitCode::from(1));
        }
    };

    // 子に渡す ready pipe write 端の raw fd。`Command::spawn` 後、子側で
    // この fd が継承されている前提で読み書きする (POSIX 標準挙動)。
    let wr_raw = wr.into_raw_fd();

    let mut child = Command::new(exe);
    child.arg("__daemonize-run");
    child.arg(format!("--socket={}", sock.display()));
    child.arg(format!("--session={session_id}"));
    // DR-0015 Task #N (2026-05-29 kawaz 指示): 初期 PTY size は **stdin pipe 経由**で
    // daemon に渡す (= 明示 `--cols/--rows` も同経路、ps から数値完全クリーン化)。
    // 旧 `--cols=N --rows=M` arg は廃止 (= ps cleanup の主目的)。
    child.arg(format!("--ready-fd={wr_raw}"));
    if let Some(needle) = until.as_deref() {
        if !needle.is_empty() {
            child.arg(format!("--until={needle}"));
        }
    }
    if let Some(n) = scrollback_rows {
        child.arg(format!("--scrollback-rows={n}"));
    }
    if let Some(path) = debug_dump.as_deref() {
        if !path.is_empty() {
            child.arg(format!("--debug-dump={path}"));
        }
    }
    child.arg("--");
    for c in cmd {
        child.arg(c);
    }
    // 子の stdio: stdin は **piped** で initial size 通信用 (= 1 行読み終わったら
    // daemon 側で /dev/null に redirect)。stdout は /dev/null、stderr は inherit
    // (= §2.3.5 採用パターン、daemon 起動失敗時の error 文字列を parent / ユーザに伝える)。
    child.stdin(Stdio::piped());
    child.stdout(Stdio::null());
    child.stderr(Stdio::inherit());

    let spawn_result = child.spawn();
    // 親側の write 端 fd は spawn 後 close (= 子が close したときに親側 read が
    // EOF を返せるように)。
    let _ = nix::unistd::close(wr_raw);

    let mut spawned = match spawn_result {
        Ok(child) => child,
        Err(e) => {
            eprintln!("hyoui: spawn 失敗: {e}");
            return Err(ExitCode::from(1));
        }
    };

    // initial size を stdin pipe に書く (= optional、None なら何も書かない、
    // daemon 側 default 80x24 で起動)。書き終わったら stdin handle を drop して
    // EOF 通知 (= daemon 側 read_line が return)。
    if let Some((cols, rows)) = initial_size {
        if let Some(stdin) = spawned.stdin.as_mut() {
            use std::io::Write as _;
            let _ = writeln!(stdin, "size {cols} {rows}");
        }
    }
    drop(spawned.stdin.take());

    // ready pipe から 1 byte 読む (= 子が ready 通知)
    let mut buf = [0u8; 1];
    let n = nix::unistd::read(&rd, &mut buf);
    drop(rd);

    match n {
        Ok(1) => Ok((session_id, sock)),
        _ => {
            eprintln!("hyoui: daemon child failed to start");
            Err(ExitCode::from(1))
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
    // DR-0013 §8: parent から伝搬される scrollback rows。None なら DaemonConfig 既定値を維持。
    let mut scrollback_rows: Option<usize> = None;
    let mut debug_dump: Option<String> = None;
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
        } else if let Some(v) = arg.strip_prefix("--scrollback-rows=") {
            // DR-0013 §8: 親 RunConfig から伝搬。parse 失敗時は既定値維持。
            scrollback_rows = v.parse::<usize>().ok();
        } else if let Some(v) = arg.strip_prefix("--debug-dump=") {
            if !v.is_empty() {
                debug_dump = Some(v.to_string());
            }
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

    // DR-0015 Task #N (2026-05-29 kawaz 指示): stdin pipe 経由で initial size を
    // 受け取る (= ps から `--cols/--rows` 数値を消す目的)。parent が `"size COLS ROWS"`
    // を 1 行書いて drop、daemon は read_line で受け取る。read 後は stdin を /dev/null
    // に redirect (= daemonize 慣例)。pipe が無い (= 旧経路 / 単体起動) なら read EOF
    // で何もせず continue。
    {
        use std::io::BufRead as _;
        let stdin = std::io::stdin();
        let mut line = String::new();
        // pipe からの 1 行 read (parent が drop で EOF 通知)。pipe 不在なら即 EOF。
        let _ = stdin.lock().read_line(&mut line);
        // split_whitespace は trailing whitespace を skip するため trim 不要 (clippy)
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[0] == "size" {
            if let Ok(c) = parts[1].parse::<u16>() {
                cols = c;
            }
            if let Ok(r) = parts[2].parse::<u16>() {
                rows = r;
            }
        }
    }
    // stdin を /dev/null に redirect (= daemon 化慣例、後続処理が誤って stdin から
    // read しないように)。pipe handle は既に EOF 状態、drop で close。
    // hyoui-cli は forbid(unsafe_code) のため hyoui::sys::raw 経由で safe wrap を使う。
    let _ = hyoui::sys::raw::redirect_stdin_to_devnull();

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
    // DR-0015: daemon は jobcontrol policy を持たない (= client 側発動)。
    // DR-0013 §8 + §8 Update: scrollback rows 上限を daemon に配線。
    if let Some(n) = scrollback_rows {
        dcfg.screen_vt100_scrollback_rows = n;
    }
    // `--debug-dump=<path>` を daemon に配線 (= 子 PTY raw bytes を file に append)。
    if let Some(p) = debug_dump {
        dcfg.debug_dump_path = Some(PathBuf::from(p));
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

// DR-0015: suspend policy 値 round-trip helper (= child_suspend_str /
// parent_suspend_str / parse_*) は廃止。policy は client 側で発動するため
// daemon child に伝搬する必要なし。
