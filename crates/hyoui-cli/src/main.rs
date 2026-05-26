//! `hyoui` binary entry point.
//!
//! Dispatches the parsed [`hyoui::cli::Command`] tree:
//!
//! * `Help`     — print usage to stdout (exit 0 for explicit help, exit 2 for
//!   unknown subcommands so callers can detect misuse from the status code).
//! * `Version`  — print `hyoui <VERSION>` and exit 0.
//! * `Run(cfg)` — daemon::Session::run 経由で子 PTY を foreground 実行。
//!   socket path は `--socket` 指定 / 未指定なら自動 (`socket_path::resolve`)。
//! * `Completion { shell }` — emit a hand-written completion script.
//! * `Error`    — print the diagnostic to stderr and exit 2.

#![forbid(unsafe_code)]

use std::io::Write;
use std::os::fd::AsFd;
use std::process::ExitCode;

use hyoui::cli::{
    AttachConfig, Command, HelpTopic, KillConfig, StatusConfig, TailConfig, WaitCliPredicate,
    WaitConfig, parse_args, usage,
};
use hyoui::client::{AttachOptions, ClientConnection};
use hyoui::daemon::{DaemonConfig, Session};
use hyoui::protocol::messages::{
    StatusQuery, TailRequest, WaitMatchOptions, WaitOutcome, WaitPredicate, WaitRequest,
};
use hyoui::protocol::{ControlMessage, Mode};
use hyoui::sys::{enter_raw, is_tty};

mod completion;
mod daemonize;
mod socket_path;

fn main() -> ExitCode {
    // Skip argv[0]: parse_args expects the trailing arguments only.
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // Hidden subcommand: 親 `hyoui run --detached ...` から self-exec 経由で
    // 起動される daemon 子 process の entry point。cli parser を汚さないため
    // ここで直接 dispatch する。
    if argv.first().map(String::as_str) == Some("__daemonize-run") {
        return daemonize::run_daemon_child(&argv[1..]);
    }

    let cmd = parse_args(&argv);

    match cmd {
        Command::Help { topic } => {
            // Explicit help (top/run/completion) is success; an unknown
            // subcommand renders top-level help but signals misuse via
            // exit 2, mirroring conventional CLI behavior.
            let is_unknown = matches!(topic, HelpTopic::UnknownSubcommand(_));
            let text = usage(&topic);
            if is_unknown {
                eprint!("{text}");
                ExitCode::from(2)
            } else {
                print!("{text}");
                ExitCode::SUCCESS
            }
        }

        Command::Version => {
            println!("hyoui {}", hyoui::VERSION);
            ExitCode::SUCCESS
        }

        Command::Run(cfg) => run_command(cfg),

        Command::Attach(cfg) => attach_command(cfg),

        Command::List => list_command(),

        Command::Kill(cfg) => kill_command(cfg),

        Command::Status(cfg) => status_command(cfg),

        Command::Tail(cfg) => tail_command(cfg),

        Command::Wait(cfg) => wait_command(cfg),

        Command::Completion { shell } => {
            print!("{}", completion::script(shell));
            ExitCode::SUCCESS
        }

        Command::Error(msg) => {
            eprintln!("hyoui: {msg}");
            eprintln!("Run `hyoui --help` for usage.");
            ExitCode::from(2)
        }
    }
}

/// `hyoui run` の主要ロジック。
///
/// 同 process 内で:
/// 1. 子 PTY 用 daemon session を起動 (= listener bind 完了)
/// 2. daemon thread を spawn (= accept + relay)
/// 3. main thread が attach client として接続、stdin/stdout を中継
/// 4. daemon thread を join、その exit code を返す
fn run_command(cfg: hyoui::cli::RunConfig) -> ExitCode {
    if cfg.detached {
        let cols = u16::try_from(cfg.cols).unwrap_or(80);
        let rows = u16::try_from(cfg.rows).unwrap_or(24);
        return daemonize::run_detached_parent(
            cfg.session.clone(),
            cfg.socket.clone(),
            cols,
            rows,
            cfg.command,
        );
    }

    let session_id = cfg
        .session
        .clone()
        .unwrap_or_else(socket_path::auto_session_id);
    let sock = match socket_path::resolve(cfg.socket.as_deref(), &session_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: socket path 解決失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let cols = u16::try_from(cfg.cols).unwrap_or(80);
    let rows = u16::try_from(cfg.rows).unwrap_or(24);
    let mut dcfg = DaemonConfig::new(session_id.clone(), sock.clone(), cfg.command);
    dcfg.cols = cols;
    dcfg.rows = rows;

    let session = match Session::start(dcfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyoui: daemon 起動失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let daemon_handle = std::thread::spawn(move || session.serve());

    // client side: connect + attach
    let opts = AttachOptions::default();
    let conn = match ClientConnection::connect(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: daemon への attach 失敗: {e}");
            // daemon thread を待って後始末 (= 何かしらの理由で待機状態のまま残らないように)
            let _ = daemon_handle.join();
            return ExitCode::from(1);
        }
    };

    // stdin が tty なら raw mode に切り替える (Drop で復元)
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let stdin_is_tty = is_tty(stdin.as_fd());
    let _raw_guard = if stdin_is_tty {
        match nix::unistd::dup(stdin.as_fd()) {
            Ok(dup_for_guard) => match enter_raw(dup_for_guard) {
                Ok(g) => Some(g),
                Err(e) => {
                    eprintln!("hyoui: raw mode 失敗: {e} (続行)");
                    None
                }
            },
            Err(e) => {
                eprintln!("hyoui: stdin dup 失敗: {e} (raw mode skip)");
                None
            }
        }
    } else {
        None
    };

    // ClientConnection::run に渡す stdin は dup したものを File として使う
    // (= raw mode 用 guard と read 用が別 fd なので close 順序を気にしなくて良い)
    let stdin_file_result = nix::unistd::dup(stdin.as_fd());
    let mut stdin_file = match stdin_file_result {
        Ok(fd) => std::fs::File::from(fd),
        Err(e) => {
            eprintln!("hyoui: stdin dup 失敗: {e}");
            let _ = daemon_handle.join();
            return ExitCode::from(1);
        }
    };

    // run の前に既に来ているかもしれない出力を流すため最初に flush
    let _ = stdout.flush();
    let run_result = conn.run(&mut stdin_file, &mut stdout);
    if let Err(e) = run_result {
        eprintln!("hyoui: client run エラー: {e}");
    }

    // daemon thread の終了を待って exit code を取る
    match daemon_handle.join() {
        Ok(Ok(code)) => {
            let masked = u8::try_from(code & 0xFF).unwrap_or(255);
            ExitCode::from(masked)
        }
        Ok(Err(e)) => {
            eprintln!("hyoui: daemon 実行エラー: {e}");
            ExitCode::from(1)
        }
        Err(_) => {
            eprintln!("hyoui: daemon thread panic");
            ExitCode::from(1)
        }
    }
}

/// `hyoui attach <session>` の主要ロジック。
///
/// 既存 daemon に socket connect し、stdin/stdout を中継する。
/// daemon は別 process / 別 hyoui run --detached 等で起動済みの想定。
fn attach_command(cfg: AttachConfig) -> ExitCode {
    let sock = if let Some(p) = cfg.socket.clone() {
        std::path::PathBuf::from(p)
    } else {
        let sid = match cfg.session_id.as_deref() {
            Some(s) => s,
            None => {
                eprintln!("hyoui: attach: session id or --socket required");
                return ExitCode::from(2);
            }
        };
        match socket_path::resolve(None, sid) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("hyoui: attach: socket path 解決失敗: {e}");
                return ExitCode::from(1);
            }
        }
    };

    let mode = match cfg.mode_str.as_deref() {
        None | Some("rw") => Mode::Rw,
        Some("ro") => Mode::Ro,
        Some("rw-no-leader") => Mode::RwNoLeader,
        Some(other) => {
            eprintln!("hyoui: attach: invalid --mode value: {other}");
            return ExitCode::from(2);
        }
    };

    let token = std::env::var("HYOUI_LOCK_TOKEN").ok();
    let opts = AttachOptions {
        mode,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token,
        exclusive: cfg.exclusive,
        detach_others: cfg.detach_others,
    };

    let conn = match ClientConnection::connect(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: attach: connect 失敗: {e}");
            return ExitCode::from(1);
        }
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let stdin_is_tty = is_tty(stdin.as_fd());
    let _raw_guard = if stdin_is_tty {
        match nix::unistd::dup(stdin.as_fd()) {
            Ok(dup_for_guard) => match enter_raw(dup_for_guard) {
                Ok(g) => Some(g),
                Err(e) => {
                    eprintln!("hyoui: raw mode 失敗: {e} (続行)");
                    None
                }
            },
            Err(e) => {
                eprintln!("hyoui: stdin dup 失敗: {e} (raw mode skip)");
                None
            }
        }
    } else {
        None
    };

    let mut stdin_file = match nix::unistd::dup(stdin.as_fd()) {
        Ok(fd) => std::fs::File::from(fd),
        Err(e) => {
            eprintln!("hyoui: stdin dup 失敗: {e}");
            return ExitCode::from(1);
        }
    };

    let _ = stdout.flush();
    match conn.run(&mut stdin_file, &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hyoui: attach 実行エラー: {e}");
            ExitCode::from(1)
        }
    }
}

/// `hyoui list` の主要ロジック。
///
/// socket dir 候補を全部 scan し、`*.sock` ファイルを 1 行ずつ出力する。
/// 出力形式: `<session>\t<socket-path>`。Phase 11 で `status.query` を併用して
/// child pid / clients 等の情報も付ける予定。
fn list_command() -> ExitCode {
    let dirs = list_candidate_dirs();
    let mut found = 0usize;
    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // dir 不存在は無視 (= 何も daemon 起動してない可能性)
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("sock") {
                continue;
            }
            let session = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            println!("{session}\t{}", path.display());
            found += 1;
        }
    }
    if found == 0 {
        // 0 件は stderr で明示 (script 用に stdout を汚さない)
        eprintln!("hyoui: no sessions found");
    }
    ExitCode::SUCCESS
}

/// `hyoui list` で scan する候補 dir を返す。
fn list_candidate_dirs() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            let p = std::path::PathBuf::from(xdg).join("hyoui");
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    let tmp = std::env::var_os("TMPDIR")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let uid = nix::unistd::geteuid().as_raw();
    let p = tmp.join(format!("hyoui-{uid}"));
    if p.is_dir() {
        out.push(p);
    }
    out
}

/// `hyoui kill <session>` の主要ロジック。
fn kill_command(cfg: KillConfig) -> ExitCode {
    let sock = if let Some(p) = cfg.socket.clone() {
        std::path::PathBuf::from(p)
    } else {
        let sid = match cfg.session_id.as_deref() {
            Some(s) => s,
            None => {
                eprintln!("hyoui: kill: session id or --socket required");
                return ExitCode::from(2);
            }
        };
        match socket_path::resolve(None, sid) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("hyoui: kill: socket path 解決失敗: {e}");
                return ExitCode::from(1);
            }
        }
    };

    let opts = AttachOptions {
        mode: Mode::Ro, // kill だけ送るので入力なし、ro で OK
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };

    let mut conn = match ClientConnection::connect(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: kill: connect 失敗: {e}");
            return ExitCode::from(1);
        }
    };

    let kill = hyoui::protocol::messages::Kill { signum: cfg.signum };
    if let Err(e) = conn.send_control(&hyoui::protocol::ControlMessage::Kill(kill)) {
        eprintln!("hyoui: kill: send 失敗: {e}");
        return ExitCode::from(1);
    }

    // daemon が close するのを待ってから exit。read で EOF を待つ。
    // ClientConnection::run は stdin が必要なので使えない。明示的に socket を
    // drop して exit。
    drop(conn);

    println!("hyoui: kill 送信完了: {}", sock.display());
    ExitCode::SUCCESS
}

/// session_id / socket オプションから target socket path を resolve するヘルパ。
fn resolve_target_socket(
    cmd: &str,
    socket: Option<&str>,
    session_id: Option<&str>,
) -> Result<std::path::PathBuf, ExitCode> {
    if let Some(p) = socket {
        return Ok(std::path::PathBuf::from(p));
    }
    let sid = match session_id {
        Some(s) => s,
        None => {
            eprintln!("hyoui: {cmd}: session id or --socket required");
            return Err(ExitCode::from(2));
        }
    };
    socket_path::resolve(None, sid).map_err(|e| {
        eprintln!("hyoui: {cmd}: socket path 解決失敗: {e}");
        ExitCode::from(1)
    })
}

/// `status` subcommand: connect → handshake → status.query → print response。
fn status_command(cfg: StatusConfig) -> ExitCode {
    let sock =
        match resolve_target_socket("status", cfg.socket.as_deref(), cfg.session_id.as_deref()) {
            Ok(p) => p,
            Err(code) => return code,
        };
    let opts = AttachOptions {
        mode: Mode::Ro,
        ..AttachOptions::default()
    };
    let mut conn = match ClientConnection::connect(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: status: connect 失敗: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = conn.send_control(&ControlMessage::StatusQuery(StatusQuery {})) {
        eprintln!("hyoui: status: send 失敗: {e}");
        return ExitCode::from(1);
    }
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: status: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::StatusResponse(sr) => {
                print_status_response(&sr);
                return ExitCode::SUCCESS;
            }
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: status: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}

fn print_status_response(sr: &hyoui::protocol::messages::StatusResponse) {
    println!("session-id: {}", sr.session_id);
    if let Some(pid) = sr.child_pid {
        println!("child-pid: {pid}");
    } else {
        println!("child-pid: (exited)");
    }
    println!("scrollback-bytes: {}", sr.scrollback_bytes);
    if let Some(holder) = sr.lock_holder {
        println!("lock-holder: client {holder}");
    } else {
        println!("lock-holder: (none)");
    }
    println!("clients:");
    for c in &sr.clients {
        let leader = if c.leader { " leader" } else { "" };
        println!("  - id={} mode={:?}{leader}", c.client_id, c.mode);
    }
}

/// `tail` subcommand: connect → handshake (ro) → tail.request → stdout に書き出す。
fn tail_command(cfg: TailConfig) -> ExitCode {
    let sock = match resolve_target_socket("tail", cfg.socket.as_deref(), cfg.session_id.as_deref())
    {
        Ok(p) => p,
        Err(code) => return code,
    };
    let opts = AttachOptions {
        mode: Mode::Ro,
        ..AttachOptions::default()
    };
    let mut conn = match ClientConnection::connect(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: tail: connect 失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let req = TailRequest {
        since_ms: cfg.since_ms,
        since_strict: false,
        follow: cfg.follow,
        strip_ansi: cfg.strip_ansi,
        last_bytes: cfg.last_bytes,
    };
    if let Err(e) = conn.send_control(&ControlMessage::TailRequest(req)) {
        eprintln!("hyoui: tail: send 失敗: {e}");
        return ExitCode::from(1);
    }
    let mut stdout = std::io::stdout().lock();
    loop {
        let frame = match conn.recv_frame() {
            Ok(f) => f,
            Err(_) => return ExitCode::SUCCESS, // EOF → socket close → 正常終了
        };
        match frame.ty {
            hyoui::protocol::TYPE_RAW_DATA => {
                // tail subscription 切替前の lingering raw_data。bytes は同じ
                // 内容なので stdout にそのまま流す。
                let _ = stdout.write_all(&frame.body);
                let _ = stdout.flush();
            }
            hyoui::protocol::TYPE_CBOR_CONTROL => {
                let msg = match ControlMessage::decode_from(frame.body.as_slice()) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                match msg {
                    ControlMessage::TailData(td) => {
                        let _ = stdout.write_all(&td.bytes);
                        let _ = stdout.flush();
                    }
                    ControlMessage::TailEnd(_) => return ExitCode::SUCCESS,
                    ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
                    _ => continue,
                }
            }
            _ => continue,
        }
    }
}

/// `wait` subcommand: connect → handshake (ro) → wait.request → outcome に応じた exit。
///
/// exit code:
/// - 0: Matched
/// - 1: Timeout
/// - 2: Cancelled (= client detach 等)
/// - 130: ChildExited (= 子 PTY が exit、shell convention)
fn wait_command(cfg: WaitConfig) -> ExitCode {
    let sock = match resolve_target_socket("wait", cfg.socket.as_deref(), cfg.session_id.as_deref())
    {
        Ok(p) => p,
        Err(code) => return code,
    };
    let opts = AttachOptions {
        mode: Mode::Ro,
        ..AttachOptions::default()
    };
    let mut conn = match ClientConnection::connect(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: wait: connect 失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let predicate = match cfg.predicate {
        WaitCliPredicate::Text(s) => WaitPredicate::Text { value: s },
        WaitCliPredicate::Pattern(r) => WaitPredicate::Pattern { regex: r },
        WaitCliPredicate::Idle(ms) => WaitPredicate::Idle { ms },
    };
    let req = WaitRequest {
        predicate,
        timeout_ms: cfg.timeout_ms,
        options: WaitMatchOptions {
            strip_escapes: cfg.strip_escapes,
            newline_convert_lf: cfg.newline_convert_lf,
        },
    };
    if let Err(e) = conn.send_control(&ControlMessage::WaitRequest(req)) {
        eprintln!("hyoui: wait: send 失敗: {e}");
        return ExitCode::from(1);
    }
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: wait: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::WaitResult(wr) => {
                return match wr.outcome {
                    WaitOutcome::Matched => ExitCode::SUCCESS,
                    WaitOutcome::Timeout => ExitCode::from(1),
                    WaitOutcome::Cancelled => ExitCode::from(2),
                    WaitOutcome::ChildExited => ExitCode::from(130),
                };
            }
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: wait: daemon error: code={} message={}",
                    e.code, e.message
                );
                return ExitCode::from(1);
            }
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: wait: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}
