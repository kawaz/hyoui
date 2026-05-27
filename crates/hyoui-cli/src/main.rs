//! `hyoui` binary entry point.
//!
//! Dispatches the parsed [`hyoui::cli::Command`] tree:
//!
//! * `Help`     — print usage to stdout (exit 0 for explicit help, exit 2 for
//!   unknown subcommands so callers can detect misuse from the status code).
//! * `Version`  — print `hyoui <VERSION>` and exit 0.
//! * `Run(cfg)` — daemon::Session::serve 経由で子 PTY を foreground 実行。
//!   socket path は `--socket` 指定 / 未指定なら自動 (`socket_path::resolve`)。
//! * `Completion { shell }` — emit a hand-written completion script.
//! * `Error`    — print the diagnostic to stderr and exit 2.

#![forbid(unsafe_code)]

use std::io::Write;
use std::os::fd::AsFd;
use std::process::ExitCode;

use hyoui::cli::{
    AttachConfig, Command, HelpTopic, KillConfig, ListConfig, ScreenCommand, ScreenDumpCliFormat,
    ScreenDumpCliLayer, ScreenDumpConfig, ScreenSnapshotConfig, SnapshotCliComponent, StatusConfig,
    TailConfig, WaitCliPredicate, WaitConfig, parse_args, usage,
};
use hyoui::client::{AttachOptions, ClientConnection};
use hyoui::daemon::{DaemonConfig, Session};
use hyoui::protocol::messages::{
    DumpRect, ScreenDumpFormat, ScreenDumpLayer, ScreenDumpRequest, SnapshotComponent,
    StateSnapshotRequest, StatusQuery, TailRequest, WaitMatchOptions, WaitOutcome, WaitPredicate,
    WaitRequest,
};
use hyoui::protocol::{ControlMessage, Mode};
use hyoui::sys::{enter_raw, is_tty};

mod completion;
mod daemonize;
mod socket_path;

/// R5-FB4: socket connect の短時間 retry。
///
/// `hyoui run --detached -- <cmd> &` の直後に `hyoui wait <session>` を叩く
/// pattern で、daemon の listener bind が間に合わずに ENOENT で即 fail する
/// 現象が頻発していた (= 実機検証で kawaz 指摘)。
///
/// 同 process 内 (= `hyoui run` non-detached) の場合は `Session::start` で
/// listen 完了を保証しているため retry 不要だが、別 process との競合では
/// kernel listen が成立する瞬間まで待つ必要がある。
///
/// 戦略:
/// - socket 不存在系の errno (ENOENT / ECONNREFUSED) のみ retry
/// - 100ms × 20 attempts = 2s budget (= `hyoui run --detached` の典型起動時間
///   100-500ms に対し十分なマージン)
/// - 認証エラー (= AuthTokenMismatch 等) や protocol error は retry しない
///   (= retry しても同じエラーで終わる、即 fail で hint を出す方が良い)
fn connect_with_retry(
    sock: &std::path::Path,
    opts: AttachOptions,
) -> Result<ClientConnection, hyoui::sys::Error> {
    use hyoui::sys::Error;
    use nix::errno::Errno;
    const MAX_ATTEMPTS: u32 = 20;
    const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
    let mut attempt = 0u32;
    loop {
        match ClientConnection::connect(sock, opts.clone()) {
            Ok(c) => return Ok(c),
            Err(e) => {
                let retryable = match &e {
                    Error::Errno(Errno::ENOENT) | Error::Errno(Errno::ECONNREFUSED) => true,
                    // io::Error 経路 (= UnixStream::connect 由来) は kind() で判定。
                    // sys::socket::connect は nix の Errno を返すので通常こちらには
                    // 来ないが、defensive coding として両方を見る。
                    Error::Io(io_err) => matches!(
                        io_err.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ),
                    _ => false,
                };
                attempt += 1;
                if !retryable || attempt >= MAX_ATTEMPTS {
                    return Err(e);
                }
                std::thread::sleep(RETRY_INTERVAL);
            }
        }
    }
}

/// `connect 失敗` 系の error メッセージに next-action hint を足す共通 helper。
///
/// R4-H2: 旧版は `hyoui: attach: connect 失敗: <io error>` だけで止まっていて
/// 「次に何をすればいいか」が分からなかった (= 新規ユーザがハマる)。
///
/// `socket_path` を渡すと `(socket: <path>)` を表示し、socket が存在しない場合は
/// `hyoui list` で確認するよう促す。
fn print_connect_failure(cmd: &str, socket_path: &std::path::Path, err: &dyn std::fmt::Display) {
    let exists = socket_path.exists();
    eprintln!(
        "hyoui: {cmd}: connect 失敗: {err} (socket: {})",
        socket_path.display()
    );
    if !exists {
        eprintln!(
            "       socket file が見つかりません。`hyoui list` で起動中の session を確認してください。"
        );
        eprintln!("       session が無い場合は `hyoui run --detached -- <cmd>` で起動できます。");
    } else {
        eprintln!(
            "       socket は存在するが connect できません。daemon process が応答していない可能性があります。"
        );
        eprintln!("       `hyoui list` / `hyoui status <session>` で状態を確認してください。");
    }
}

/// `session id or --socket required` 系のエラーに hint を足す共通 helper。
fn print_session_required(cmd: &str) {
    eprintln!("hyoui: {cmd}: session id (positional) または --socket=<path> が必要です");
    eprintln!("       例: `hyoui {cmd} <session-id>` / `hyoui {cmd} --socket=/tmp/x.sock`");
    eprintln!("       起動中の session 一覧は `hyoui list` で確認できます。");
}

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

        Command::List(cfg) => list_command(cfg),

        Command::Kill(cfg) => kill_command(cfg),

        Command::Status(cfg) => status_command(cfg),

        Command::Tail(cfg) => tail_command(cfg),

        Command::Wait(cfg) => wait_command(cfg),

        Command::Screen(sub) => match sub {
            ScreenCommand::Dump(cfg) => screen_dump_command(cfg),
            ScreenCommand::Snapshot(cfg) => screen_snapshot_command(cfg),
            // `ScreenCommand` is `#[non_exhaustive]`; future variants surface as
            // a generic skew error so older binaries report clearly.
            _ => {
                eprintln!(
                    "hyoui: screen: unsupported screen subcommand variant (binary/library version skew)"
                );
                ExitCode::from(2)
            }
        },

        Command::Completion { shell } => {
            print!("{}", completion::script(shell));
            ExitCode::SUCCESS
        }

        Command::Error(msg) => {
            eprintln!("hyoui: {msg}");
            eprintln!("Run `hyoui --help` for usage.");
            ExitCode::from(2)
        }

        // `Command` is `#[non_exhaustive]`; a newer hyoui library may add
        // variants not yet handled by this binary version.
        _ => {
            eprintln!("hyoui: unsupported command variant (binary/library version skew)");
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
            cfg.until.clone(),
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
    // Round2 #2: HYOUI_LOCK_TOKEN env を daemon の expected_token に配線。
    // 値が空文字列の場合は Some("") にせず None 扱い (= 認証無効化) として扱う
    // (= `expected_token = Some("")` で全 client 通過してしまう問題の二重防御)。
    if let Ok(token) = std::env::var("HYOUI_LOCK_TOKEN") {
        if !token.is_empty() {
            dcfg.expected_token = Some(token);
        }
    }
    // R5-FB1: `--until PATTERN` を daemon 側に配線 (旧版は cli で parse される
    // だけで daemon に渡っていなかった)。空 string は無効として扱う。
    if let Some(needle) = cfg.until.clone() {
        if !needle.is_empty() {
            dcfg.until = Some(needle);
        }
    }

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
    // R5-FB2: stdin が pipe / file の場合 (= `echo "1+2" | hyoui run -- bc`
    // のような pattern) は stdin EOF を子 PTY に EOT (0x04) として伝える。
    // tty の場合は通常 EOF が来ないし、来ても detach 同等が望ましいので
    // 既定の Detach のまま。`--mode=headless` の有無で分岐する選択肢もあるが、
    // 「stdin が pipe か」の方が本質 (= tty なのに headless mode、tty で
    // pipe ではないなど multi-axis に対応)。
    let eof_action = if stdin_is_tty {
        hyoui::client::StdinEofAction::Detach
    } else {
        hyoui::client::StdinEofAction::SendEof
    };
    let conn = conn.with_stdin_eof_action(eof_action);
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
    // H3: HYOUI_DETACH_PREFIX を raw mode 入る **前** に validate。invalid なら
    // 通常 terminal で stderr に出してから exit (= 旧 silent fallback で warning が
    // raw mode 後の scrollback に流される罠を回避)。
    if let Err(e) = hyoui::client::resolve_detach_prefix_from_env() {
        eprintln!("hyoui: attach: {e}");
        return ExitCode::from(2);
    }

    let sock = if let Some(p) = cfg.socket.clone() {
        std::path::PathBuf::from(p)
    } else {
        let sid = match cfg.session_id.as_deref() {
            Some(s) => s,
            None => {
                print_session_required("attach");
                return ExitCode::from(2);
            }
        };
        match socket_path::resolve(None, sid) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("hyoui: attach: socket path 解決失敗: {e} (session: {sid})");
                eprintln!("       起動中の session 一覧は `hyoui list` で確認してください。");
                return ExitCode::from(1);
            }
        }
    };

    let mode = match cfg.mode_str.as_deref() {
        None | Some("rw") => Mode::Rw,
        Some("ro") => Mode::Ro,
        Some("rw-no-leader") => Mode::RwNoLeader,
        Some(other) => {
            eprintln!("hyoui: attach: invalid --mode value: {other:?}");
            eprintln!("       valid values: `rw` | `ro` | `rw-no-leader`");
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

    // R5-FB4: socket 不存在系 errno は短時間 retry (= 別 process の daemon が
    // listen するまでの window 対策)。詳細は `connect_with_retry` doc を参照。
    let conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("attach", &sock, &e);
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

/// R5-H3: socket liveness probe (= daemon が応答するか確認)。
///
/// daemon が panic / SIGKILL で異常終了すると `UnixSock::drop` が走らず
/// socket file が残留する (= stale socket)。socket file の存在だけで live と
/// 判定する旧 `hyoui list` では stale と区別不能で、ユーザは `hyoui status` で
/// connect 失敗を見るまで気付けなかった。
///
/// 本関数は best-effort connect 試行で生死判定する:
/// - `connect_timeout` で 100ms 以内に成功すれば `live`
/// - ECONNREFUSED / timeout / その他 IO error なら `stale`
///
/// **handshake は実施しない** (= token が必要な daemon もあるため)。
/// kernel が SOCK_STREAM の listen backlog にいる場合は connect 成功するので、
/// daemon process が alive かつ accept できる状態であることを確認できる。
fn probe_socket_liveness(path: &std::path::Path) -> bool {
    use std::os::unix::net::UnixStream;
    // Unix domain socket の connect は kernel level で即座に結果が返る (= TCP の
    // SYN 待ちのような network delay は存在しない)。listener が居なければ即
    // ECONNREFUSED、居れば即 success。timeout は listener queue が一杯で
    // 待たされるケースだけ意味を持つが、stale 判定用途では「即 success or
    // 即 fail」だけ見えれば十分なので blocking `connect` で OK。
    UnixStream::connect(path).is_ok()
}

/// `hyoui list` の主要ロジック (R5-H3 対応)。
///
/// socket dir 候補を全部 scan し、`*.sock` ファイルを 1 行ずつ出力する。
/// 各 socket は `probe_socket_liveness` で死活確認し、`live` / `stale` を 2 列目に
/// 出す (= 旧形式 `<session>\t<path>` から 3 列目構造に拡張)。
///
/// `--prune-stale` 指定時、stale と判定された socket は `unlink(2)` で削除する。
/// この削除は best-effort (= 失敗してもユーザに warning するだけで exit code は 0)。
fn list_command(cfg: ListConfig) -> ExitCode {
    list_command_with_dirs(cfg, list_candidate_dirs())
}

/// `list_command` の testable な内部実装。dir 一覧を引数で受けることで
/// env (`XDG_RUNTIME_DIR` / `TMPDIR`) 依存を切り離し、unit test 可能にする。
fn list_command_with_dirs(cfg: ListConfig, dirs: Vec<std::path::PathBuf>) -> ExitCode {
    let mut found = 0usize;
    let mut stale_count = 0usize;
    let mut pruned_count = 0usize;
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
            let live = probe_socket_liveness(&path);
            let status = if live { "live" } else { "stale" };
            if !live {
                stale_count += 1;
            }
            println!("{session}\t{status}\t{}", path.display());
            found += 1;

            // R5-H3: --prune-stale で stale socket を unlink。
            if !live && cfg.prune_stale {
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        pruned_count += 1;
                        eprintln!("hyoui: pruned stale socket: {}", path.display());
                    }
                    Err(e) => {
                        eprintln!("hyoui: warning: failed to prune {}: {e}", path.display());
                    }
                }
            }
        }
    }
    if found == 0 {
        // 0 件は stderr で明示 (script 用に stdout を汚さない)
        eprintln!("hyoui: no sessions found");
        eprintln!("       新しい session を始めるには: `hyoui run --detached -- <cmd>`");
        eprintln!(
            "       socket 候補 dir: $XDG_RUNTIME_DIR/hyoui または ${{TMPDIR:-/tmp}}/hyoui-<uid>"
        );
    } else if stale_count > 0 && !cfg.prune_stale {
        eprintln!(
            "hyoui: {stale_count} stale socket(s) found. \
             Run `hyoui list --prune-stale` to remove them."
        );
    } else if cfg.prune_stale && pruned_count > 0 {
        eprintln!("hyoui: pruned {pruned_count} stale socket(s)");
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
                print_session_required("kill");
                return ExitCode::from(2);
            }
        };
        match socket_path::resolve(None, sid) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("hyoui: kill: socket path 解決失敗: {e} (session: {sid})");
                eprintln!("       起動中の session 一覧は `hyoui list` で確認してください。");
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

    // R5-FB4: socket 不存在系 errno は短時間 retry。
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("kill", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // DR-0012: wire は signal name string。CLI 段で正規表記 (SIG-prefix 大文字)
    // を強制済 (= cli.rs::parse_kill の `--signal` validate) なので、ここでは
    // そのまま wire に流す。
    let kill = hyoui::protocol::messages::Kill {
        signal: cfg.signal.clone(),
    };
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
            print_session_required(cmd);
            return Err(ExitCode::from(2));
        }
    };
    socket_path::resolve(None, sid).map_err(|e| {
        eprintln!("hyoui: {cmd}: socket path 解決失敗: {e} (session: {sid})");
        eprintln!("       起動中の session 一覧は `hyoui list` で確認してください。");
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
    // R5-FB4: socket 不存在系 errno は短時間 retry。
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("status", &sock, &e);
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
                match cfg.format {
                    hyoui::cli::StatusFormat::Plain => print_status_plain(&sr),
                    hyoui::cli::StatusFormat::Json => print_status_json(&sr),
                    // `StatusFormat` is `#[non_exhaustive]`; fall back to plain
                    // for unknown future variants.
                    _ => print_status_plain(&sr),
                }
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

fn print_status_plain(sr: &hyoui::protocol::messages::StatusResponse) {
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

/// H5: scripting / jq 用に 1 行 JSON object で StatusResponse を出力。
/// 依存なしで手書き (= serde_json を入れるよりも軽い、status はフィールド限定)。
fn print_status_json(sr: &hyoui::protocol::messages::StatusResponse) {
    use std::fmt::Write as _;
    let mut out = String::new();
    out.push('{');
    write!(&mut out, "\"session_id\":{}", json_string(&sr.session_id)).ok();
    match sr.child_pid {
        Some(pid) => write!(&mut out, ",\"child_pid\":{pid}").ok(),
        None => write!(&mut out, ",\"child_pid\":null").ok(),
    };
    write!(&mut out, ",\"scrollback_bytes\":{}", sr.scrollback_bytes).ok();
    match sr.lock_holder {
        Some(h) => write!(&mut out, ",\"lock_holder\":{h}").ok(),
        None => write!(&mut out, ",\"lock_holder\":null").ok(),
    };
    out.push_str(",\"clients\":[");
    for (i, c) in sr.clients.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let mode_str = match c.mode {
            hyoui::protocol::Mode::Rw => "rw",
            hyoui::protocol::Mode::Ro => "ro",
            hyoui::protocol::Mode::RwNoLeader => "rw-no-leader",
            // `Mode` is `#[non_exhaustive]`; surface unknown variants as
            // "unknown" so JSON output stays well-formed for older clients.
            _ => "unknown",
        };
        write!(
            &mut out,
            "{{\"client_id\":{},\"mode\":\"{}\",\"leader\":{}}}",
            c.client_id, mode_str, c.leader
        )
        .ok();
    }
    out.push_str("]}");
    println!("{out}");
}

/// JSON 文字列エスケープ (= " / \ / control char)。
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                write!(&mut out, "\\u{:04x}", c as u32).ok();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    // R5-FB4: socket 不存在系 errno は短時間 retry。
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("tail", &sock, &e);
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
                    ControlMessage::TailEnd(te) => {
                        // H4: 終了理由を stderr に明示 (= `| tee log` 等でログから
                        // どの理由で stream が止まったかを知れるようにする)
                        use hyoui::protocol::messages::TailEndReason;
                        let reason_str = match te.reason {
                            TailEndReason::Eof => "eof (= scrollback flush done)",
                            TailEndReason::BufferTruncated => {
                                "buffer-truncated (= since range evicted from ring buffer)"
                            }
                            TailEndReason::ClientCancel => "client-cancel",
                            TailEndReason::ChildExited => "child-exited",
                            // `TailEndReason` is `#[non_exhaustive]`; future
                            // variants surface as "unknown" so logging stays
                            // readable on version skew.
                            _ => "unknown",
                        };
                        eprintln!("hyoui: tail: stream ended ({reason_str})");
                        return ExitCode::SUCCESS;
                    }
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
/// - 3: ChildExited (= 子 PTY が exit、condition 未達のまま session 終了)
///
/// 注: 旧版は 130 (= 128 + SIGINT) を ChildExited に割り当てていたが、shell
/// 慣例で 130 は「Ctrl-C で中断」を意味するため scripting で `$? -eq 130` を
/// 「interrupt」と読む既存パターンと衝突する。3 は POSIX application-defined
/// 範囲なので副作用なし。
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
    // R5-FB4: socket 不存在系 errno は短時間 retry。
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("wait", &sock, &e);
            return ExitCode::from(1);
        }
    };
    let predicate = match cfg.predicate {
        WaitCliPredicate::Text(s) => WaitPredicate::Text { value: s },
        WaitCliPredicate::Pattern(r) => WaitPredicate::Pattern { regex: r },
        WaitCliPredicate::Idle(ms) => WaitPredicate::Idle { ms },
        // `WaitCliPredicate` is `#[non_exhaustive]`; the parser only emits
        // known variants today, so an unknown one indicates a library/binary
        // version skew.
        _ => {
            eprintln!("hyoui: wait: unsupported predicate variant (binary/library version skew)");
            return ExitCode::from(2);
        }
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
                    WaitOutcome::ChildExited => ExitCode::from(3),
                    // `WaitOutcome` is `#[non_exhaustive]`; treat unknown
                    // future variants as a generic failure.
                    _ => {
                        eprintln!(
                            "hyoui: wait: unknown outcome variant (binary/library version skew)"
                        );
                        ExitCode::from(1)
                    }
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

/// `screen dump <session>` subcommand (= DR-0013 §9 + DR-0006 §10.2)。
///
/// connect → handshake (cap=`screen-dump-v1`) → `screen.dump.request` 送信 →
/// `screen.dump.response` を受信して payload を stdout (or `--output` file) に
/// 書き出す。
///
/// daemon が `screen-dump-v1` を intersect しない場合、`screen.dump.request` を
/// 送ると daemon が `error` (= `unsupported-capability`) を返す。これは
/// `ControlMessage::Error` 経路で受け取り、stderr に表示して exit 1。
fn screen_dump_command(cfg: ScreenDumpConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "screen dump",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // protocol 側 enum へ変換 (= CLI 表現と wire 表現の境界)。
    let format = match cfg.format {
        ScreenDumpCliFormat::Ansi => ScreenDumpFormat::Ansi,
        ScreenDumpCliFormat::Binary => ScreenDumpFormat::Binary,
        ScreenDumpCliFormat::Cbor => ScreenDumpFormat::Cbor,
        // `ScreenDumpCliFormat` is `#[non_exhaustive]`; treat unknown variants as
        // a binary/library version skew rather than silently degrading.
        _ => {
            eprintln!(
                "hyoui: screen dump: unsupported format variant (binary/library version skew)"
            );
            return ExitCode::from(2);
        }
    };
    let layer = match cfg.layer {
        ScreenDumpCliLayer::Visible => ScreenDumpLayer::Visible,
        ScreenDumpCliLayer::Scrollback => ScreenDumpLayer::Scrollback,
        ScreenDumpCliLayer::Both => ScreenDumpLayer::Both,
        _ => {
            eprintln!(
                "hyoui: screen dump: unsupported layer variant (binary/library version skew)"
            );
            return ExitCode::from(2);
        }
    };
    let rect = cfg.rect.map(|r| DumpRect {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
    });

    // handshake では MVP_CAPS を全部要求する (= 既存の status/tail/wait と同じ pattern)。
    // `screen-dump-v1` も MVP_CAPS に含まれているため daemon と intersect する。
    // token は env (HYOUI_LOCK_TOKEN) から取る (= 認証付き daemon にも attach 可能)。
    let opts = AttachOptions {
        mode: Mode::Ro,
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        ..AttachOptions::default()
    };
    // R5-FB4: socket 不存在系 errno は短時間 retry。
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("screen dump", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // PDU serial は client 側で採番 (= response 対応付け確認用)。固定値で良い (= 1).
    let req = ScreenDumpRequest {
        format,
        layer,
        rect,
        serial: Some(1),
    };
    if let Err(e) = conn.send_control(&ControlMessage::ScreenDumpRequest(req)) {
        eprintln!("hyoui: screen dump: send 失敗: {e}");
        return ExitCode::from(1);
    }

    // NOTE: `--timeout=<ms>` (= cfg.timeout_ms) は CLI 層で parse 済だが、現状の
    // `ClientConnection::recv_control` は blocking で socket-level read timeout を
    // 露出していない。daemon は `screen.dump.request` を受けたら即同期で
    // `screen.dump.response` を返す設計 (= DR-0013 §9) のため、実害はほぼ無い。
    // timeout 配線は別 task で `ClientConnection` 側に `set_read_timeout` を
    // 露出させてから対応する (= 残懸念として明示)。
    let _ = cfg.timeout_ms;
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: screen dump: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::ScreenDumpResponse(resp) => {
                // serial echo は debug 用、本実装では body だけ書き出せば十分。
                return write_screen_dump_payload(&resp.payload, cfg.output.as_deref());
            }
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: screen dump: daemon error: code={:?} message={}",
                    e.code, e.message
                );
                // daemon が `screen-dump-v1` を持たない場合は `unsupported-capability`
                // が message に入ってくる。next-action hint を出す:
                if e.message.contains("screen-dump-v1") {
                    eprintln!("       daemon が `screen-dump-v1` cap をサポートしていません。");
                    eprintln!("       daemon を新しいバージョンに更新してください。");
                }
                return ExitCode::from(1);
            }
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: screen dump: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}

/// `screen dump` の payload を stdout または `--output=<path>` に書き出す。
///
/// stdout 書き出しは `lock` を取って raw bytes を直接流す (= terminal 再生用の
/// ANSI sequence を decode せず通す)。file 書き出しは `truncate` で上書き。
fn write_screen_dump_payload(payload: &[u8], output: Option<&str>) -> ExitCode {
    match output {
        Some(path) => match std::fs::File::create(path) {
            Ok(mut f) => match f.write_all(payload).and_then(|()| f.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hyoui: screen dump: 書き出し失敗 ({path}): {e}");
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("hyoui: screen dump: file open 失敗 ({path}): {e}");
                ExitCode::from(1)
            }
        },
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            match lock.write_all(payload).and_then(|()| lock.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hyoui: screen dump: stdout 書き出し失敗: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// `screen snapshot <session>` subcommand (= DR-0013 §9 + DR-0006 §10.3)。
///
/// connect → handshake (cap=`state-snapshot-v1`) → `screen.snapshot.request` 送信
/// → `screen.snapshot.response` を受信して CBOR encoded bytes を stdout
/// (or `--output` file) に書き出す。
///
/// daemon が `state-snapshot-v1` を intersect しない場合、`screen.snapshot.request`
/// を送ると daemon が `error` (= `unsupported-capability`) を返す。これは
/// `ControlMessage::Error` 経路で受け取り、stderr に表示して exit 1。
///
/// `--format=json` は MVP scope 外 (= daemon 未実装)。CLI 段では cli.rs で
/// 受理するが、wire 上は cbor で送って payload をそのまま流す
/// (= json encoder は後段 task で実装)。
fn screen_snapshot_command(cfg: ScreenSnapshotConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "screen snapshot",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // CLI 表現 → protocol 表現の変換 (= 1:1 mapping、`Style` のみ protocol に
    // 対応 variant が無いので skip)。
    let mut include: Vec<SnapshotComponent> = Vec::with_capacity(cfg.include.len());
    for c in &cfg.include {
        let mapped = match *c {
            SnapshotCliComponent::Cells => Some(SnapshotComponent::Cells),
            SnapshotCliComponent::Cursor => Some(SnapshotComponent::Cursor),
            SnapshotCliComponent::Mode => Some(SnapshotComponent::Mode),
            SnapshotCliComponent::Scrollback => Some(SnapshotComponent::Scrollback),
            SnapshotCliComponent::WindowSize => Some(SnapshotComponent::WindowSize),
            SnapshotCliComponent::Buffer => Some(SnapshotComponent::Buffer),
            SnapshotCliComponent::SequenceNo => Some(SnapshotComponent::SequenceNo),
            // `Style` は protocol layer に variant が無い (= MVP scope 外)。CLI 段で
            // 受理しても wire には送らず無視する (= 早期 fail させず forward-compat)。
            SnapshotCliComponent::Style => None,
            // `SnapshotCliComponent` is `#[non_exhaustive]`; future variants surface
            // as a binary/library version skew rather than silent fallback.
            _ => {
                eprintln!(
                    "hyoui: screen snapshot: unsupported include variant (binary/library version skew)"
                );
                return ExitCode::from(2);
            }
        };
        if let Some(m) = mapped {
            include.push(m);
        }
    }
    if include.is_empty() {
        eprintln!(
            "hyoui: screen snapshot: --include に protocol で送信可能な component が 1 つも含まれていません"
        );
        eprintln!(
            "       valid: Cells / Cursor / Mode / Scrollback / WindowSize / Buffer / SequenceNo"
        );
        return ExitCode::from(2);
    }

    // handshake では MVP_CAPS を全部要求する (= 既存 screen dump と同じ pattern)。
    // `state-snapshot-v1` も MVP_CAPS に含まれているため daemon と intersect する。
    let opts = AttachOptions {
        mode: Mode::Ro,
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        ..AttachOptions::default()
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("screen snapshot", &sock, &e);
            return ExitCode::from(1);
        }
    };

    let req = StateSnapshotRequest {
        include,
        serial: Some(1),
    };
    if let Err(e) = conn.send_control(&ControlMessage::StateSnapshotRequest(req)) {
        eprintln!("hyoui: screen snapshot: send 失敗: {e}");
        return ExitCode::from(1);
    }

    // NOTE: `--timeout=<ms>` (= cfg.timeout_ms) は CLI 層で parse 済だが、現状の
    // `ClientConnection::recv_control` は blocking で socket-level read timeout を
    // 露出していない (= screen dump と同じ事情)。daemon は `screen.snapshot.request`
    // を受けたら即同期で `screen.snapshot.response` を返すため実害はほぼ無い。
    // timeout 配線は別 task で `ClientConnection::set_read_timeout` を生やす。
    let _ = cfg.timeout_ms;
    // `--format=json` は受理するが現状 daemon は CBOR しか返さないため、CLI は
    // 受信した response を CBOR で再 encode して書き出す (= forward-compat、別 task
    // で json encoder を入れたらここで分岐する)。
    let _ = cfg.format;
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: screen snapshot: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::StateSnapshotResponse(resp) => {
                // response 全体を CBOR で再 encode して payload として書き出す
                // (= 各 component の値が partial で入っているので、構造ごと
                // 別 tool に渡せる形が便利、ControlMessage 全体ではなく中身の
                // `StateSnapshotResponse` を独立した CBOR root として出力)。
                let mut buf = Vec::new();
                if let Err(e) = ciborium::ser::into_writer(&resp, &mut buf) {
                    eprintln!("hyoui: screen snapshot: response の再 encode 失敗: {e}");
                    return ExitCode::from(1);
                }
                return write_screen_snapshot_payload(&buf, cfg.output.as_deref());
            }
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: screen snapshot: daemon error: code={:?} message={}",
                    e.code, e.message
                );
                if e.message.contains("state-snapshot-v1") {
                    eprintln!("       daemon が `state-snapshot-v1` cap をサポートしていません。");
                    eprintln!("       daemon を新しいバージョンに更新してください。");
                }
                if e.message.contains("scrollback") {
                    eprintln!(
                        "       scrollback component は Phase B では未実装です (= Phase C 配線予定)。"
                    );
                    eprintln!("       `--include` から `Scrollback` を外して再実行してください。");
                }
                return ExitCode::from(1);
            }
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: screen snapshot: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}

/// `screen snapshot` の payload を stdout または `--output=<path>` に書き出す。
/// 構造は `write_screen_dump_payload` と同じだが、stderr メッセージで command 名を
/// 区別するため別関数にしている。
fn write_screen_snapshot_payload(payload: &[u8], output: Option<&str>) -> ExitCode {
    match output {
        Some(path) => match std::fs::File::create(path) {
            Ok(mut f) => match f.write_all(payload).and_then(|()| f.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hyoui: screen snapshot: 書き出し失敗 ({path}): {e}");
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("hyoui: screen snapshot: file open 失敗 ({path}): {e}");
                ExitCode::from(1)
            }
        },
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            match lock.write_all(payload).and_then(|()| lock.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("hyoui: screen snapshot: stdout 書き出し失敗: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use tempfile::TempDir;

    fn make_0700_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod 0700");
        dir
    }

    /// R5-H3: live socket (= listener が bind されている) は `probe_socket_liveness`
    /// で true を返す。
    #[test]
    fn probe_returns_true_for_live_socket() {
        let dir = make_0700_dir();
        let path = dir.path().join("live.sock");
        let _listener = UnixListener::bind(&path).expect("bind");
        assert!(
            probe_socket_liveness(&path),
            "live socket should probe as alive"
        );
    }

    /// R5-H3: stale socket (= file は残っているが listener が居ない) は
    /// `probe_socket_liveness` で false を返す。`hyoui list` がこの判定で
    /// `live` / `stale` を出し分ける。
    #[test]
    fn list_marks_stale_socket_when_no_ping_response() {
        let dir = make_0700_dir();
        let path = dir.path().join("stale.sock");
        // listener を bind して即 drop → file は残るが accept する process がない
        {
            let _listener = UnixListener::bind(&path).expect("bind");
            // drop here would unlink (`UnixListener::drop` doesn't unlink, but std fn doesn't either);
            // we manually keep the file by creating it separately if needed.
        }
        // 上の scope exit で listener は close されたが、Rust の std UnixListener は
        // unlink しないので file はそのまま残る (= まさに daemon panic 後の状態)。
        assert!(path.exists(), "stale socket file should still exist");
        assert!(
            !probe_socket_liveness(&path),
            "stale socket should probe as dead"
        );
    }

    /// R5-H3: 存在しない socket path は connect 失敗 → false。
    #[test]
    fn probe_returns_false_for_missing_socket() {
        let dir = make_0700_dir();
        let path = dir.path().join("nonexistent.sock");
        assert!(
            !probe_socket_liveness(&path),
            "missing socket should probe as dead"
        );
    }

    /// R5-H3: `--prune-stale` flag 付きで `list_command_with_dirs` を呼ぶと、
    /// stale な socket file が unlink される。live socket は触らない。
    ///
    /// `list_command` は env (`XDG_RUNTIME_DIR` / `TMPDIR`) で dir を解決するが、
    /// edition 2024 では `env::set_var` が unsafe であり、`#![forbid(unsafe_code)]`
    /// と衝突する。代わりに dir 一覧を直接渡す内部関数 `list_command_with_dirs`
    /// を介してテストする。
    #[test]
    fn list_prune_stale_removes_dead_sockets() {
        let sock_dir = make_0700_dir();

        // stale socket: bind して即 close、file だけ残す。std の UnixListener::drop は
        // unlink しないので file 残留 (= まさに daemon panic 後の状態)。
        let stale_path = sock_dir.path().join("stale-sess.sock");
        {
            let _l = UnixListener::bind(&stale_path).expect("bind stale");
        }
        assert!(stale_path.exists(), "stale socket file should exist");

        // live socket: bind して listener を保持する (= test 中 alive)
        let live_path = sock_dir.path().join("live-sess.sock");
        let _live_listener = UnixListener::bind(&live_path).expect("bind live");

        // dir 一覧を直接渡して env mutation を回避
        let cfg = ListConfig { prune_stale: true };
        let _exit = list_command_with_dirs(cfg, vec![sock_dir.path().to_path_buf()]);

        // 確認: stale は unlink された、live はまだ残っている
        assert!(
            !stale_path.exists(),
            "--prune-stale should unlink stale socket"
        );
        assert!(
            live_path.exists(),
            "--prune-stale must not unlink live socket"
        );
    }

    /// R5-H3: `--prune-stale` を指定しない時は stale でも socket file は削除しない。
    #[test]
    fn list_without_prune_keeps_stale_sockets() {
        let sock_dir = make_0700_dir();
        let stale_path = sock_dir.path().join("stale.sock");
        {
            let _l = UnixListener::bind(&stale_path).expect("bind");
        }
        assert!(stale_path.exists());

        let cfg = ListConfig { prune_stale: false };
        let _exit = list_command_with_dirs(cfg, vec![sock_dir.path().to_path_buf()]);

        assert!(
            stale_path.exists(),
            "list without --prune-stale must not remove sockets"
        );
    }

    /// R5-FB4: socket がまだ存在しない時点で connect_with_retry を呼んでも、
    /// 別 thread で daemon が立ち上がるまで retry し、成功すること。
    ///
    /// `hyoui run --detached -- <cmd> &` の直後に `hyoui wait <session>` を
    /// 叩く実機 pattern を模擬する。
    #[test]
    fn wait_retries_until_socket_appears() {
        use hyoui::daemon::{DaemonConfig, Session};
        use std::sync::{Arc, Barrier};
        use std::time::Duration;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("retry.sock");

        // socket は最初存在しない (= ENOENT)。daemon thread を別途立ち上げる前に
        // connect_with_retry を呼んで、retry 経路を実際に通すことを保証する。
        assert!(
            !sock_path.exists(),
            "precondition: socket must not exist yet"
        );

        let barrier = Arc::new(Barrier::new(2));
        let barrier_daemon = Arc::clone(&barrier);
        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            // client が connect_with_retry の最初の attempt を済ませた後に
            // daemon を立ち上げる (= retry path を確実に通すため)。
            barrier_daemon.wait();
            // 数 retry interval 分待ってから listener bind する (= ENOENT を
            // 数回返してから socket を作る)
            std::thread::sleep(Duration::from_millis(300));
            let cfg = DaemonConfig::new(
                "retry-test",
                sock_for_daemon,
                vec!["/bin/sleep".into(), "30".into()],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        barrier.wait();
        let opts = AttachOptions {
            mode: Mode::Ro,
            ..AttachOptions::default()
        };
        let result = connect_with_retry(&sock_path, opts);
        assert!(
            result.is_ok(),
            "connect_with_retry must succeed once daemon starts; got: {:?}",
            result.err()
        );
        drop(result);

        // daemon を kill して thread を終わらせる
        let kill_opts = AttachOptions {
            mode: Mode::Ro,
            ..AttachOptions::default()
        };
        if let Ok(mut conn) = connect_with_retry(&sock_path, kill_opts) {
            let kill = hyoui::protocol::messages::Kill { signal: None };
            let _ = conn.send_control(&ControlMessage::Kill(kill));
            drop(conn);
        }
        let _ = daemon_handle.join();
    }

    /// `write_screen_dump_payload` (= --output 指定なし) は stdout に流す。
    /// stdout を直接乗っ取るのは難しいため、ここでは `--output=<path>` 経路
    /// (= file 書き出し) を確認する。
    #[test]
    fn write_screen_dump_payload_writes_to_file() {
        let dir = make_0700_dir();
        let out = dir.path().join("dump.bin");
        let payload = b"\x1b[1;1HHELLO\r\n";
        let out_str = out.to_str().expect("path utf8").to_string();
        let code = write_screen_dump_payload(payload, Some(&out_str));
        // ExitCode の比較はできないが、文字化けせず作成されていることを確認。
        // exit code が SUCCESS の場合はファイル一致を見れば十分。
        let _ = code;
        let got = std::fs::read(&out).expect("read back");
        assert_eq!(got, payload);
    }

    /// daemon を起動して `screen dump --format=ansi` を実行、payload が
    /// stdout (= file 経路で代用) に書かれるところまで通すスモークテスト。
    ///
    /// 既存 daemon test (`crates/hyoui/src/daemon/session.rs::serve_screen_dump_ansi_returns_state_formatted`)
    /// で protocol レベルの確認はあるので、ここでは CLI dispatch が
    /// `screen.dump.request` を正しく送って response の payload が file 経由で
    /// 取り出せることを確認する (= CLI 層の wiring の retrogression 検知)。
    #[test]
    fn screen_dump_command_writes_response_payload_to_file() {
        use hyoui::daemon::{DaemonConfig, Session};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("dump-test.sock");
        let out_path = sock_dir.path().join("dump.out");

        // daemon spawn (= 子プロセスは 1 回 "SMOKE" を出して sleep 待機)。
        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "screen-dump-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'SMOKE'; sleep 30".into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // daemon の listener が立ち上がるまで retry connect で待つ + 子の最初の
        // 出力 "SMOKE" が screen state に反映されるまで少し待つ (= 50ms × 数回)。
        // 既存 daemon test と同様に read_until_contains を本気で実装してもよいが、
        // ここでは smoke レベルの assertion (= payload が空でない) で十分。
        std::thread::sleep(std::time::Duration::from_millis(200));

        let cfg = ScreenDumpConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            format: ScreenDumpCliFormat::Ansi,
            layer: ScreenDumpCliLayer::Visible,
            rect: None,
            output: Some(out_path.to_string_lossy().into_owned()),
            timeout_ms: 5_000,
        };
        let _exit = screen_dump_command(cfg);

        // payload が file に書かれているはず (= ANSI prefix `\x1b` で始まる)。
        let got = std::fs::read(&out_path).expect("read output");
        assert!(!got.is_empty(), "dump payload must not be empty");
        assert!(
            got.starts_with(b"\x1b"),
            "ANSI dump should start with ESC, got first byte: {:?}",
            got.first()
        );

        // cleanup: daemon に kill 送って thread を終わらせる
        let kill_opts = AttachOptions {
            mode: Mode::Ro,
            ..AttachOptions::default()
        };
        if let Ok(mut conn) = connect_with_retry(&sock_path, kill_opts) {
            let kill = hyoui::protocol::messages::Kill { signal: None };
            let _ = conn.send_control(&ControlMessage::Kill(kill));
            drop(conn);
        }
        let _ = daemon_handle.join();
    }

    /// `write_screen_snapshot_payload` の file 出力経路を確認。
    /// stdout 経路はそのままだと captured 出来ないため file 経由で代用 (= dump と同じ pattern)。
    #[test]
    fn write_screen_snapshot_payload_writes_to_file() {
        let dir = make_0700_dir();
        let out = dir.path().join("snap.cbor");
        let payload = b"\xa1\x66cursor\xa3\x63row\x03\x63col\x05\x67visible\xf5"; // arbitrary CBOR bytes
        let out_str = out.to_str().expect("path utf8").to_string();
        let code = write_screen_snapshot_payload(payload, Some(&out_str));
        let _ = code;
        let got = std::fs::read(&out).expect("read back");
        assert_eq!(got, payload);
    }

    /// daemon を起動して `screen snapshot --include=Cursor,Mode,WindowSize` を
    /// 実行、CBOR encoded `StateSnapshotResponse` が file に書かれるところまで
    /// 通すスモークテスト。
    ///
    /// protocol レベルの確認は daemon test (= `handle_state_snapshot_request`)
    /// で済んでいるので、ここでは CLI dispatch が正しく `StateSnapshotRequest` を
    /// 送って、response の payload が file 経由で取り出せて、しかも CBOR として
    /// decode 可能 (= 構造が壊れていない) を確認する。
    #[test]
    fn screen_snapshot_command_writes_response_payload_to_file() {
        use hyoui::daemon::{DaemonConfig, Session};
        use hyoui::protocol::messages::StateSnapshotResponse;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("snap-test.sock");
        let out_path = sock_dir.path().join("snap.cbor");

        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "screen-snapshot-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'SMOKE'; sleep 30".into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // daemon listener bind + 子の "SMOKE" 出力が反映されるまで少し待つ。
        std::thread::sleep(std::time::Duration::from_millis(200));

        let cfg = ScreenSnapshotConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            include: vec![
                SnapshotCliComponent::Cursor,
                SnapshotCliComponent::Mode,
                SnapshotCliComponent::WindowSize,
                SnapshotCliComponent::Buffer,
                SnapshotCliComponent::SequenceNo,
            ],
            format: hyoui::cli::ScreenSnapshotCliFormat::Cbor,
            output: Some(out_path.to_string_lossy().into_owned()),
            timeout_ms: 5_000,
        };
        let _exit = screen_snapshot_command(cfg);

        let got = std::fs::read(&out_path).expect("read output");
        assert!(!got.is_empty(), "snapshot payload must not be empty");
        // CBOR として decode 可能か検証 (= ciborium 経由で StateSnapshotResponse に復元)
        let decoded: StateSnapshotResponse =
            ciborium::de::from_reader(got.as_slice()).expect("decode response");
        // 要求した component が乗ってきているかを確認 (= Cells は要求していないので None)
        assert!(decoded.cursor.is_some(), "cursor should be included");
        assert!(decoded.mode.is_some(), "mode should be included");
        assert!(
            decoded.window_size.is_some(),
            "window_size should be included"
        );
        assert!(decoded.buffer.is_some(), "buffer should be included");
        assert!(
            decoded.sequence_no.is_some(),
            "sequence_no should be included"
        );
        assert!(decoded.cells.is_none(), "cells must not be included");

        // cleanup: kill 送って thread を終わらせる
        let kill_opts = AttachOptions {
            mode: Mode::Ro,
            ..AttachOptions::default()
        };
        if let Ok(mut conn) = connect_with_retry(&sock_path, kill_opts) {
            let kill = hyoui::protocol::messages::Kill { signal: None };
            let _ = conn.send_control(&ControlMessage::Kill(kill));
            drop(conn);
        }
        let _ = daemon_handle.join();
    }

    /// R5-FB4: socket が永遠に作られない場合、retry 期限切れで Err 返却。
    /// retry budget (= 約 2s) を超えたら諦めて caller に error を返す。
    #[test]
    fn connect_with_retry_gives_up_when_socket_never_appears() {
        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("never.sock");
        let start = std::time::Instant::now();
        let opts = AttachOptions {
            mode: Mode::Ro,
            ..AttachOptions::default()
        };
        let result = connect_with_retry(&sock_path, opts);
        let elapsed = start.elapsed();
        assert!(
            result.is_err(),
            "expected ENOENT to bubble up after retry budget"
        );
        // 20 attempts × 100ms ≈ 2s。short-circuit で 100ms 未満で fail したら
        // retry 経路を通っていないことになるので fail させる。
        assert!(
            elapsed >= std::time::Duration::from_millis(1500),
            "retry budget should be ~2s, but failed in {elapsed:?}"
        );
    }
}
