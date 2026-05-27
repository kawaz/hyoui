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
    AttachConfig, Command, HelpTopic, InputCommand, InputSpec, KillConfig, ListConfig,
    LockAcquireConfig, LockCommand, LockMode, LockReleaseConfig, ScreenCommand,
    ScreenDumpCliFormat, ScreenDumpCliLayer, ScreenDumpConfig, ScreenSnapshotConfig,
    SnapshotCliComponent, StatusConfig, TailConfig, WaitConfig, parse_args, usage,
};
use hyoui::client::{AttachOptions, ClientConnection};
use hyoui::daemon::{DaemonConfig, Session};
use hyoui::protocol::messages::{
    DumpRect, ScreenDumpFormat, ScreenDumpLayer, ScreenDumpRequest, SnapshotComponent,
    StateSnapshotRequest, StatusQuery, TailRequest,
};
use hyoui::protocol::{ControlMessage, Mode};
use hyoui::sys::{enter_raw, is_tty};

mod completion;
mod daemonize;
mod input_handlers;
mod socket_path;
mod wait_core;

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

        Command::Input(cmd) => input_command(cmd),

        Command::Lock(sub) => match sub {
            LockCommand::Acquire(cfg) => lock_acquire_command(cfg),
            LockCommand::Release(cfg) => lock_release_command("lock release", cfg),
            // `LockCommand` is `#[non_exhaustive]`; future variants surface as
            // a generic skew error so older binaries report clearly.
            _ => {
                eprintln!(
                    "hyoui: lock: unsupported lock subcommand variant (binary/library version skew)"
                );
                ExitCode::from(2)
            }
        },

        Command::Unlock(cfg) => lock_release_command("unlock", cfg),

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
/// `--scrollback-rows` flag (CLI) と `HYOUI_SCROLLBACK_ROWS` env を解決する。
///
/// 優先順位 (高 → 低):
/// 1. `--scrollback-rows=<N>` flag (= `cfg.scrollback_rows` が `Some(N)`)
/// 2. `HYOUI_SCROLLBACK_ROWS=<N>` env
/// 3. `None` (= DaemonConfig の既定値 1000 行を維持)
///
/// env が空文字列 / parse 不能なら `None` 同等 (= 既定値維持)。
fn resolve_scrollback_rows(cfg_value: Option<usize>) -> Option<usize> {
    if let Some(n) = cfg_value {
        return Some(n);
    }
    match std::env::var("HYOUI_SCROLLBACK_ROWS") {
        Ok(v) if !v.is_empty() => v.parse::<usize>().ok(),
        _ => None,
    }
}

fn run_command(cfg: hyoui::cli::RunConfig) -> ExitCode {
    let scrollback_rows = resolve_scrollback_rows(cfg.scrollback_rows);
    if cfg.detached {
        let cols = u16::try_from(cfg.cols).unwrap_or(80);
        let rows = u16::try_from(cfg.rows).unwrap_or(24);
        return daemonize::run_detached_parent(
            cfg.session.clone(),
            cfg.socket.clone(),
            cols,
            rows,
            cfg.until.clone(),
            cfg.on_child_suspend,
            cfg.on_parent_suspend,
            scrollback_rows,
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
    // DR-0001 軸 1/2: cli で parse + preset 解決済の suspend policy を daemon に渡す。
    // (旧版は RunConfig 構造体に格納されるだけで daemon に伝わっていなかった)。
    dcfg.on_child_suspend = cfg.on_child_suspend;
    dcfg.on_parent_suspend = cfg.on_parent_suspend;
    // DR-0013 §8 + §8 Update: vt100 内蔵 scrollback ring の行数上限を CLI/env で
    // override。`resolve_scrollback_rows` が cfg / env 順で解決し、None なら
    // DaemonConfig の既定値 1000 行を維持する。
    if let Some(n) = scrollback_rows {
        dcfg.screen_vt100_scrollback_rows = n;
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
        since_strict: cfg.since_strict,
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
                        // どの理由で stream が止まったかを知れるようにする)。
                        // DR-0006 §11: `--since-strict` で BufferTruncated を受けたら
                        // exit 非 0 (= since 範囲が scrollback ring から evict 済)。
                        use hyoui::protocol::messages::TailEndReason;
                        let (reason_str, exit_code) = match te.reason {
                            TailEndReason::Eof => {
                                ("eof (= scrollback flush done)", ExitCode::SUCCESS)
                            }
                            TailEndReason::BufferTruncated => (
                                "buffer-truncated (= since range evicted from ring buffer)",
                                ExitCode::from(1),
                            ),
                            TailEndReason::ClientCancel => ("client-cancel", ExitCode::SUCCESS),
                            TailEndReason::ChildExited => ("child-exited", ExitCode::SUCCESS),
                            // `TailEndReason` is `#[non_exhaustive]`; future
                            // variants surface as "unknown" so logging stays
                            // readable on version skew.
                            _ => ("unknown", ExitCode::SUCCESS),
                        };
                        eprintln!("hyoui: tail: stream ended ({reason_str})");
                        return exit_code;
                    }
                    ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
                    _ => continue,
                }
            }
            _ => continue,
        }
    }
}

/// `wait` subcommand: state-based polling で visible cells の regex match を待つ
/// (DR-0006 §9 改訂後の実装)。
///
/// 旧 wait (= daemon に `WaitRequest` を送って scrollback bytes regex で判定する
/// 方式) は廃止。client 側で `StateSnapshotRequest` を polling し、cells を行 join
/// した text に対して match する形に置き換え済 ([[wait_core]] が共通実装)。
///
/// exit code:
/// - 0: Matched
/// - 1: Timeout / I/O error / connect error
/// - 2: Cancelled / invalid usage (= regex compile 失敗等)
/// - 3: daemon error (= `state-snapshot-v1` cap 未対応 / unexpected response 等)
fn wait_command(cfg: WaitConfig) -> ExitCode {
    let sock = match resolve_target_socket("wait", cfg.socket.as_deref(), cfg.session_id.as_deref())
    {
        Ok(p) => p,
        Err(code) => return code,
    };
    // wait は read-only attach で十分 (= snapshot 取得のみ、daemon に bytes を
    // 送らない)。MVP_CAPS を要求して `state-snapshot-v1` を intersect する。
    let opts = AttachOptions {
        mode: Mode::Ro,
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
            print_connect_failure("wait", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // poll interval: CLI > env > default。
    let interval = match cfg.poll_interval_ms {
        Some(ms) => std::time::Duration::from_millis(ms),
        None => wait_core::poll_interval_from_env().unwrap_or(wait_core::DEFAULT_POLL_INTERVAL),
    };
    let timeout = cfg.timeout_ms.map(std::time::Duration::from_millis);

    match wait_core::wait_for_pattern(&mut conn, &cfg.pattern, timeout, interval) {
        wait_core::WaitOutcome::Matched => ExitCode::SUCCESS,
        wait_core::WaitOutcome::Timeout => ExitCode::from(1),
        wait_core::WaitOutcome::IoError(msg) => {
            eprintln!("hyoui: wait: {msg}");
            ExitCode::from(1)
        }
        wait_core::WaitOutcome::InvalidPattern(msg) => {
            eprintln!("hyoui: wait: invalid pattern: {msg}");
            ExitCode::from(2)
        }
        wait_core::WaitOutcome::DaemonError(msg) => {
            eprintln!("hyoui: wait: daemon error: {msg}");
            if msg.contains("state-snapshot-v1") {
                eprintln!("       daemon が `state-snapshot-v1` cap をサポートしていません。");
                eprintln!("       daemon を新しいバージョンに更新してください。");
            }
            ExitCode::from(3)
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
        ScreenDumpCliFormat::TextPlain => ScreenDumpFormat::TextPlain,
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

/// `hyoui input <session> <spec>...` subcommand (= DR-0006 §8、task #16)。
///
/// 各 spec を出現順に評価し、handler が返した bytes を daemon の master PTY に
/// `raw_data` frame で流す。
///
/// 採用経路: **既存 raw bytes 経路** (= 新規 protocol message を増やさない)。
/// daemon 側の `TYPE_RAW_DATA` frame handler が body を master fd に write する
/// (= attach 中の Rw client が stdin に打鍵したのと完全同一の流路)。これにより:
/// - protocol level の変更なし (= cap negotiation 不要)
/// - daemon 側の追加 handler 不要
/// - 互換性問題なし
///
/// 失敗時の挙動: 最初に失敗した spec で即 abort + exit 1。後続 spec は実行しない
/// (= partial execution は MVP scope 外、DR-0006 §8.6 の atomicity 方針と整合)。
///
/// **wait / wait-idle は task #17 の scope**。本 task では到達したら明示 error。
fn input_command(cmd: InputCommand) -> ExitCode {
    // socket path 解決。session_id / --socket どちらかが必須 (= 通常 parser 段で
    // 確定済だが defense-in-depth)。
    if cmd.socket.is_none() && cmd.session_id.is_none() {
        print_session_required("input");
        return ExitCode::from(2);
    }
    if cmd.specs.is_empty() {
        eprintln!("hyoui: input: spec list が空です (内部 invariant 違反)");
        return ExitCode::from(2);
    }

    // 1. socket path resolve。
    let sock =
        match resolve_target_socket("input", cmd.socket.as_deref(), cmd.session_id.as_deref()) {
            Ok(p) => p,
            Err(code) => return code,
        };

    // 2. attach (= Rw mode で連結)。raw_data frame の書き込みは Rw client のみ
    //    daemon が master fd に流す (= Ro は silently drop される)。
    //    Lock token は DR-0006 §6 / §8.5 に従い flag 優先 + env fallback で解決:
    //    - `--lock-token=<T>` が指定されていれば、その値を handshake.token に流す
    //    - flag 未指定なら `HYOUI_LOCK_TOKEN` env を読む (= 既存 auto 継承挙動)
    //    flag 優先により「親 (= tx) が export した env を子 input が上書き指定したい」
    //    といったケースに対応する。空 string の flag は parser 段で reject 済。
    let token = cmd
        .lock_token
        .clone()
        .or_else(|| std::env::var("HYOUI_LOCK_TOKEN").ok());
    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token,
        exclusive: false,
        detach_others: false,
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("input", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // wait の per-spec timeout は `cmd.timeout` を使う (= input 全体 timeout
    // ではなく、各 wait/wait-idle spec それぞれに適用する)。default 5s なので
    // 永久 wait したい場合は `--timeout=<長め>` を明示する。
    let wait_timeout = Some(cmd.timeout);
    let poll_interval =
        wait_core::poll_interval_from_env().unwrap_or(wait_core::DEFAULT_POLL_INTERVAL);

    // 3. 各 spec を順に dispatch。最初の失敗で abort。
    //    `cmd.max_file_bytes` は CLI flag / env / default で解決済 (= parser 段)。
    for (idx, spec) in cmd.specs.iter().enumerate() {
        match dispatch_spec(
            spec,
            &mut conn,
            wait_timeout,
            poll_interval,
            cmd.max_file_bytes,
        ) {
            Ok(()) => {}
            Err(msg) => {
                eprintln!("hyoui: input: spec[{idx}]: {msg}");
                return ExitCode::from(1);
            }
        }
    }

    // 4. 接続 close (= drop で socket close、daemon は client 切断として処理)。
    drop(conn);
    ExitCode::SUCCESS
}

// =============================================================================
// lock subcommand dispatchers (DR-0006 §7、task #20)
// =============================================================================

/// `hyoui lock acquire <session>` の dispatcher (= DR-0006 §7)。
///
/// 取得シーケンス:
/// 1. socket connect + handshake (= Rw mode、lock cap)
/// 2. `LockAcquire { wait, ... }` を送る。`Acquired` 受信 → token を stdout に 1 行 print。
///    `Denied` 受信 → `--mode=fail` なら exit 1、`--mode=wait` なら短時間 sleep して
///    `--timeout` 内で再送 (polling)。MVP daemon は wait queue 未実装 (= wait=true でも
///    Denied) なので CLI 側 polling で擬似 wait semantics を実現する。
/// 3. 取得後は **connection を保持して block**: stdin / socket / self-pipe (SIGINT/SIGTERM)
///    を poll で並行監視し、いずれかが「終了 signal」を出すまで wait。
/// 4. 終了 signal: stdin EOF / SIGINT / SIGTERM / socket close / mode.change(Locked→Rw 等の異常)
///    を検知したら `LockRelease` を送って exit 0。socket がすでに切れていれば daemon が
///    auto-release するので best-effort で OK。
fn lock_acquire_command(cfg: LockAcquireConfig) -> ExitCode {
    use std::time::{Duration, Instant};

    let sock = match resolve_target_socket(
        "lock acquire",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };
    // lock 取得には Rw mode が必要 (= daemon::control::handle_lock_acquire の
    // `Mode::Ro` reject path)。lock cap は MVP_CAPS に含まれているのでそのまま使う。
    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        // 既存 lock holder の token を流すと自分も lock holder 扱いで認証されるが、
        // 本 subcommand 自身が **新たに lock を取る** 入口なので、env の HYOUI_LOCK_TOKEN
        // は handshake に流さない (= 新規 holder として認証されるため token=None 推奨)。
        // ただし daemon 側 expected_token が設定されている場合は handshake が落ちるので
        // その場合は env から token を取り込んで認証通過させる必要がある。両立のため
        // env がある時のみ流す (= holder 確定後の自分の token は別途取り直す)。
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("lock acquire", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // handshake 直後に daemon から flush される leader.notify などの broadcast 系を
    // 飲み込む必要は無い (= recv_control は frame 境界で stop するので、後段で
    // 「待っている response」を取りに行く時に紛れる)。実装簡略化のため、後段の
    // recv loop 側で「期待外の message は捨てる」方式を取る。

    // wait/timeout 戦略の polling 間隔。DR には明示が無いので 100ms 固定 (= 短すぎず
    // 長すぎず、ユーザの体感応答 < 0.1s)。将来は env で調整可能にする余地あり。
    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    let deadline: Option<Instant> = cfg
        .timeout_ms
        .map(|ms| Instant::now() + Duration::from_millis(ms));

    // 取得 loop。
    let acquired_token = loop {
        let body =
            hyoui::protocol::ControlMessage::LockAcquire(hyoui::protocol::messages::LockAcquire {
                // wait の意味は daemon に伝えるが、MVP daemon は wait=true を queue 化
                // していないので Denied が返ることを前提に CLI 側 polling で wait する。
                // wait=true は将来 daemon 側 queue 実装で意味を持つ前向き signal。
                wait: matches!(cfg.mode, LockMode::Wait),
                timeout_abs_ms: cfg.timeout_ms,
                timeout_idle_ms: None,
                process_bound: false,
            });
        if let Err(e) = conn.send_control(&body) {
            eprintln!("hyoui: lock acquire: send 失敗: {e}");
            return ExitCode::from(1);
        }
        // response 待ち (frame 1 つ)。期待外の broadcast (mode.change 等) は捨てる。
        let response = loop {
            match conn.recv_control(None) {
                Ok(hyoui::protocol::ControlMessage::LockResponse(lr)) => break lr,
                Ok(hyoui::protocol::ControlMessage::Error(em)) => {
                    eprintln!(
                        "hyoui: lock acquire: daemon error: {} ({:?})",
                        em.message, em.code
                    );
                    return ExitCode::from(1);
                }
                // mode.change / leader.notify 等は捨てて再受信
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("hyoui: lock acquire: recv 失敗: {e}");
                    return ExitCode::from(1);
                }
            }
        };
        match response.result {
            hyoui::protocol::messages::LockResult::Acquired => {
                break response.token.unwrap_or_default();
            }
            hyoui::protocol::messages::LockResult::Queued => {
                // 将来 daemon が queue 実装した場合に来る path。CLI は単に response が
                // 再送されてくるのを待つ (= 再 LockAcquire は送らない)。
                // 簡略化のため本 MVP では Queued が来たら polling と同じ扱いで sleep + 再送する。
                if let Some(dl) = deadline {
                    if Instant::now() >= dl {
                        eprintln!("hyoui: lock acquire: timeout (queued path)");
                        return ExitCode::from(1);
                    }
                }
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            hyoui::protocol::messages::LockResult::Denied => match cfg.mode {
                LockMode::Fail => {
                    eprintln!(
                        "hyoui: lock acquire: denied (= 他者が lock 保持中、--mode=fail で即時 fail)"
                    );
                    return ExitCode::from(1);
                }
                LockMode::Wait => {
                    if let Some(dl) = deadline {
                        if Instant::now() >= dl {
                            eprintln!("hyoui: lock acquire: timeout");
                            return ExitCode::from(1);
                        }
                    }
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
                // `LockMode` is `#[non_exhaustive]`; future variants surface as a
                // generic skew error so older binaries report clearly.
                _ => {
                    eprintln!("hyoui: lock acquire: unknown --mode variant (binary/library skew)");
                    return ExitCode::from(1);
                }
            },
            hyoui::protocol::messages::LockResult::Timeout => {
                eprintln!("hyoui: lock acquire: daemon timeout");
                return ExitCode::from(1);
            }
            // `LockResult` is `#[non_exhaustive]`; treat unknown as failure (skew)
            _ => {
                eprintln!("hyoui: lock acquire: unknown LockResult variant (binary/library skew)");
                return ExitCode::from(1);
            }
        }
    };

    // 取得済: token を stdout に 1 行 print + flush。shell capture (`$(hyoui lock acquire ...)`)
    // 用に確実に flush しておく。
    println!("{acquired_token}");
    if let Err(e) = std::io::stdout().flush() {
        eprintln!("hyoui: lock acquire: stdout flush 失敗: {e}");
        // flush 失敗は重大ではない (= token は出力済の可能性)、続行
        let _ = e;
    }
    eprintln!(
        "hyoui: lock acquire: lock を保持中。Ctrl-C / SIGTERM / stdin EOF で release します。"
    );

    // block phase: stdin / socket / self-pipe を poll で並行監視。
    // self-pipe を install して SIGINT/SIGTERM を捕る。
    let pipe: hyoui::sys::SelfPipe = match hyoui::sys::install_self_pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: lock acquire: self-pipe 作成失敗: {e} (続行、Ctrl-C は効きません)");
            // self-pipe が作れなくても lock 解放はしないと困るので、明示的に release を試みて exit。
            let _ = conn.send_control(&hyoui::protocol::ControlMessage::LockRelease(
                hyoui::protocol::messages::LockRelease {
                    token: acquired_token.clone(),
                },
            ));
            drop(conn);
            return ExitCode::from(1);
        }
    };
    for sig in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
        nix::sys::signal::Signal::SIGHUP,
    ] {
        if let Err(e) = hyoui::sys::register_self_pipe(sig) {
            eprintln!("hyoui: lock acquire: signal {sig:?} register 失敗: {e} (続行)");
        }
    }

    let exit_code = wait_until_release_signal(&mut conn, &pipe);

    // 解放: socket がまだ生きていれば LockRelease を best-effort で送る (= daemon は
    // socket disconnect でも auto-release するが、明示的に release した方が綺麗で速い)。
    let release_msg =
        hyoui::protocol::ControlMessage::LockRelease(hyoui::protocol::messages::LockRelease {
            token: acquired_token,
        });
    let _ = conn.send_control(&release_msg);
    drop(conn);
    let _ = pipe; // explicit drop (= 順序を明確にするだけ)

    exit_code
}

/// `lock_acquire_command` の block phase: signal / stdin EOF / socket close を待つ。
///
/// poll で socket / stdin / self-pipe を並行監視し、最初に「終了」を示した fd の
/// 種別に応じて `ExitCode` を返す:
/// - SIGINT / SIGTERM / SIGHUP: 通常終了 (0)
/// - stdin EOF (= POLLHUP): 通常終了 (0)
/// - socket close (= POLLHUP on socket): 通常終了 (0、daemon 側 close は abnormal でも
///   release は完了させた扱い)
/// - I/O error: 1
fn wait_until_release_signal(conn: &mut ClientConnection, pipe: &hyoui::sys::SelfPipe) -> ExitCode {
    use hyoui::sys::poll::{PollFlags, PollOutcome, poll};
    use nix::poll::{PollFd, PollTimeout};
    use std::io::Read;
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    loop {
        let socket_fd = conn.reader_fd();
        let stdin_fd = stdin.as_fd();
        let pipe_fd = pipe.read.as_fd();
        let mut fds = [
            PollFd::new(socket_fd, PollFlags::POLLIN),
            PollFd::new(stdin_fd, PollFlags::POLLIN),
            PollFd::new(pipe_fd, PollFlags::POLLIN),
        ];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(PollOutcome::Ready(_)) => {}
            Ok(PollOutcome::Interrupted) | Ok(PollOutcome::Timeout) => continue,
            // `PollOutcome` is `#[non_exhaustive]`; future variants treated as benign
            // continue (= 再 poll で確実な outcome を取り直す)。
            Ok(_) => continue,
            Err(e) => {
                eprintln!("hyoui: lock acquire: poll 失敗: {e}");
                return ExitCode::from(1);
            }
        }
        let sock_re = fds[0].revents().unwrap_or(PollFlags::empty());
        let stdin_re = fds[1].revents().unwrap_or(PollFlags::empty());
        let pipe_re = fds[2].revents().unwrap_or(PollFlags::empty());
        let _ = fds;

        // self-pipe: signal 受信。drain して exit。
        if pipe_re.contains(PollFlags::POLLIN) {
            let _ = pipe.drain(); // best-effort
            return ExitCode::SUCCESS;
        }
        // socket: daemon からの broadcast (mode.change 等) は飲み込む。close なら exit。
        if sock_re.contains(PollFlags::POLLIN) {
            match conn.recv_control(None) {
                Ok(_) => continue, // broadcast を 1 つ捨てる
                Err(_) => {
                    // socket close / decode error → daemon 側で session 終了
                    return ExitCode::SUCCESS;
                }
            }
        } else if sock_re.contains(PollFlags::POLLHUP) || sock_re.contains(PollFlags::POLLERR) {
            return ExitCode::SUCCESS;
        }
        // stdin: EOF を検知したら exit。pipe input の場合は stdin POLLIN が立つので
        // 1 byte 読んで EOF (= read 0) なら終了扱い。tty stdin の場合は普通 POLLIN は
        // 立たないので無視で OK。
        if stdin_re.contains(PollFlags::POLLIN) {
            let mut buf = [0u8; 64];
            match stdin.lock().read(&mut buf) {
                Ok(0) => return ExitCode::SUCCESS, // EOF
                Ok(_) => {
                    // stdin に何か届いたが lock acquire は中身を使わない (= 単に EOF 検知のため)。
                    // 続行して次の poll iteration で再度待つ。
                }
                Err(e) => {
                    eprintln!("hyoui: lock acquire: stdin read 失敗: {e}");
                    return ExitCode::SUCCESS; // 解放だけは確実に行うため 0 で抜ける
                }
            }
        } else if stdin_re.contains(PollFlags::POLLHUP) {
            return ExitCode::SUCCESS;
        }
    }
}

/// `hyoui lock release <session> --token=<T>` / `hyoui unlock <session> --token=<T>` の dispatcher。
///
/// 共通実装。`cmd_label` は error message 出力時に subcommand 名を出し分けるための文字列
/// (= `"lock release"` または `"unlock"`)。
///
/// **重要な制約** (= daemon-side semantics):
/// daemon は `holder client (= 取得時の同 connection) からの release のみ` accept する
/// (= [`crate::daemon::control::handle_lock_release`] が `state.lock_holder == Some(ch_id)`
/// と token 一致の両方を要求する)。本 CLI は新規 connection で release を送るため、
/// 別 process からの release は **必ず `LockNotHeld` で reject される**。
/// その場合は stderr に hint を出して exit 1 する。
fn lock_release_command(cmd_label: &str, cfg: LockReleaseConfig) -> ExitCode {
    // token の解決: --token flag > HYOUI_LOCK_TOKEN env。両方未指定なら exit 2 で reject。
    let token = match cfg
        .token
        .clone()
        .or_else(|| std::env::var("HYOUI_LOCK_TOKEN").ok())
    {
        Some(t) if !t.is_empty() => t,
        _ => {
            eprintln!("hyoui: {cmd_label}: --token=<T> または環境変数 HYOUI_LOCK_TOKEN が必要です");
            return ExitCode::from(2);
        }
    };
    let sock =
        match resolve_target_socket(cmd_label, cfg.socket.as_deref(), cfg.session_id.as_deref()) {
            Ok(p) => p,
            Err(code) => return code,
        };
    // release は新規 connection で送るため、daemon 側 holder 照合で必ず通らない。
    // それでも protocol 的に正しく `LockRelease` を送って `lock.not-held` error を観測し、
    // ユーザに明確な hint を返すのが現状の MVP 実装での誠実な挙動。
    // (= 将来 daemon が token-based release を実装したらこの hint は不要になる)
    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: Some(token.clone()),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure(cmd_label, &sock, &e);
            return ExitCode::from(1);
        }
    };
    let body =
        hyoui::protocol::ControlMessage::LockRelease(hyoui::protocol::messages::LockRelease {
            token: token.clone(),
        });
    if let Err(e) = conn.send_control(&body) {
        eprintln!("hyoui: {cmd_label}: send 失敗: {e}");
        return ExitCode::from(1);
    }
    // response 待ち。`LockRelease` への 1 件目 control message を見る:
    // - mode.change(Rw, lock_holder=None) なら成功 (= broadcast、release accept)
    // - Error(LockNotHeld) なら failed (= 新規 connection なので daemon は holder mismatch で reject)
    // - その他は不明 → 失敗扱い
    let result = loop {
        match conn.recv_control(None) {
            Ok(hyoui::protocol::ControlMessage::ModeChange(mc)) => {
                if matches!(mc.session_mode, hyoui::protocol::messages::SessionMode::Rw)
                    && mc.lock_holder.is_none()
                {
                    break Ok(());
                }
                continue; // 関係ない mode.change は捨てて再受信
            }
            Ok(hyoui::protocol::ControlMessage::Error(em)) => {
                break Err((em.code, em.message));
            }
            // leader.notify 等は捨てる
            Ok(_) => continue,
            Err(e) => {
                eprintln!("hyoui: {cmd_label}: recv 失敗: {e}");
                drop(conn);
                return ExitCode::from(1);
            }
        }
    };
    drop(conn);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("hyoui: {cmd_label}: daemon error: {message} ({code:?})");
            if matches!(code, hyoui::protocol::messages::ErrorCode::LockNotHeld) {
                eprintln!(
                    "       hint: lock は **取得時の同 connection** からのみ release できます。"
                );
                eprintln!(
                    "             別 process で `hyoui lock acquire` を実行中なら、その process に"
                );
                eprintln!("             SIGTERM / SIGINT を送って解放してください。");
            }
            ExitCode::from(1)
        }
    }
}

/// 1 つの [`InputSpec`] を実行する。bytes 系 spec (text/hex/file/paste/key) は
/// handler が bytes 化 → `send_raw_bytes`、wait 系 (wait/wait-idle) は
/// [`wait_core`] による client-side polling を行う (DR-0006 §9.2 input family)。
///
/// # Errors
///
/// - handler が validation で reject (= paste の終端 nest、key の不明名等)
/// - `send_raw_bytes` の I/O 失敗
/// - wait 系: regex compile / timeout / daemon error
fn dispatch_spec(
    spec: &InputSpec,
    conn: &mut ClientConnection,
    wait_timeout: Option<std::time::Duration>,
    poll_interval: std::time::Duration,
    max_file_bytes: u64,
) -> Result<(), String> {
    match spec {
        // bytes 系: handler が bytes 化、send_raw_bytes で daemon に流す。
        InputSpec::Text(s) => send_bytes(conn, input_handlers::handle_text(s)),
        InputSpec::Hex(b) => send_bytes(conn, input_handlers::handle_hex(b)),
        InputSpec::File(p) => send_bytes(conn, input_handlers::handle_file(p, max_file_bytes)?),
        InputSpec::Paste(s) => send_bytes(conn, input_handlers::handle_paste(s)?),
        InputSpec::Key(name) => send_bytes(conn, input_handlers::handle_key(name)?),
        // wait 系: client-side state polling。bytes 送信は伴わない。
        InputSpec::Wait(pattern) => {
            match wait_core::wait_for_pattern(conn, pattern, wait_timeout, poll_interval) {
                wait_core::WaitOutcome::Matched => Ok(()),
                wait_core::WaitOutcome::Timeout => {
                    Err(format!("wait: timeout (pattern={pattern:?})"))
                }
                wait_core::WaitOutcome::IoError(m) => Err(format!("wait: I/O error: {m}")),
                wait_core::WaitOutcome::InvalidPattern(m) => {
                    Err(format!("wait: invalid pattern: {m}"))
                }
                wait_core::WaitOutcome::DaemonError(m) => Err(format!("wait: daemon error: {m}")),
            }
        }
        InputSpec::WaitIdle(duration) => {
            match wait_core::wait_for_idle(conn, *duration, wait_timeout, poll_interval) {
                wait_core::WaitOutcome::Matched => Ok(()),
                wait_core::WaitOutcome::Timeout => {
                    Err(format!("wait-idle: timeout (idle_for={duration:?})"))
                }
                wait_core::WaitOutcome::IoError(m) => Err(format!("wait-idle: I/O error: {m}")),
                wait_core::WaitOutcome::InvalidPattern(m) => {
                    // wait-idle 経路では regex compile は走らないので到達しない想定。
                    Err(format!("wait-idle: invalid pattern (unexpected): {m}"))
                }
                wait_core::WaitOutcome::DaemonError(m) => {
                    Err(format!("wait-idle: daemon error: {m}"))
                }
            }
        }
        // `InputSpec` is `#[non_exhaustive]`; future variants surface as a generic
        // skew error so older binaries report clearly.
        _ => Err("unsupported InputSpec variant (binary/library version skew)".to_string()),
    }
}

/// bytes 系 spec の共通 helper (= raw_data frame で送信)。
fn send_bytes(conn: &mut ClientConnection, bytes: Vec<u8>) -> Result<(), String> {
    conn.send_raw_bytes(&bytes)
        .map_err(|e| format!("daemon への bytes 送信失敗: {e}"))
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

    /// daemon を起動して `screen dump --format=text/plain` を実行、payload に
    /// 子の marker 出力が含まれ、ANSI escape (= 0x1b バイト) が一切含まれない
    /// ことを確認するスモークテスト (= CLI 層の TextPlain wiring + daemon 側
    /// build_text_plain dispatch の retrogression 検知)。
    #[test]
    fn screen_dump_command_text_plain_returns_visible_chars_without_ansi() {
        use hyoui::daemon::{DaemonConfig, Session};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("dump-text-plain.sock");
        let out_path = sock_dir.path().join("dump-text-plain.out");

        // daemon spawn (= 子は marker "MARKER" を出して sleep 待機)。
        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "screen-dump-text-plain-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'MARKER'; sleep 30".into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // daemon の listener + 子の最初の出力が screen state に反映されるまで待つ
        std::thread::sleep(std::time::Duration::from_millis(200));

        let cfg = ScreenDumpConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            format: ScreenDumpCliFormat::TextPlain,
            layer: ScreenDumpCliLayer::Visible,
            rect: None,
            output: Some(out_path.to_string_lossy().into_owned()),
            timeout_ms: 5_000,
        };
        let _exit = screen_dump_command(cfg);

        let got = std::fs::read(&out_path).expect("read output");
        assert!(!got.is_empty(), "text/plain dump payload must not be empty");
        // marker 文字列が含まれる (= 子の出力が cell 化されて TextPlain に乗っている)
        assert!(
            got.windows(b"MARKER".len()).any(|w| w == b"MARKER"),
            "text/plain payload should contain MARKER: {:?}",
            std::str::from_utf8(&got).unwrap_or("<invalid utf8>")
        );
        // ANSI escape (= 0x1b) は一切含まれない (= 装飾 strip 済)
        assert!(
            !got.contains(&0x1b),
            "text/plain payload must not contain ANSI escape: {:?}",
            std::str::from_utf8(&got).unwrap_or("<invalid utf8>")
        );

        // cleanup
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

    /// daemon を起動して大量出力させ、`screen dump --layer=scrollback --format=text/plain`
    /// で visible からスクロールアウトした marker が取り出せることを CLI 経路で確認する。
    ///
    /// daemon-level の同等 test (= `serve_screen_dump_scrollback_text_plain_returns_old_marker`)
    /// で protocol レベルの検証は済んでいるため、ここでは CLI dispatch が
    /// `--layer=scrollback` を正しく protocol に渡し、payload が file 経由で取り出せる
    /// 経路の retrogression を防ぐ。
    #[test]
    fn screen_dump_command_layer_scrollback_returns_scrolled_out_marker() {
        use hyoui::daemon::{DaemonConfig, Session};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("dump-scrollback.sock");
        let out_path = sock_dir.path().join("dump-scrollback.out");

        // SCROLLED_OUT_HEAD → 50 行ダミー → VISIBLE_TAIL 構成 (= viewport 24 行を超えて
        // 確実に SCROLLED_OUT_HEAD が scrollback に押し出される)。
        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "screen-dump-scrollback-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'SCROLLED_OUT_HEAD\\n'; for i in $(seq 1 50); do printf 'L%d\\n' $i; done; \
                     printf 'VISIBLE_TAIL\\n'; sleep 30"
                        .into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // 子の全出力が screen state に反映されるまで少し長めに待つ (= 50 行 + 2 marker)
        std::thread::sleep(std::time::Duration::from_millis(400));

        let cfg = ScreenDumpConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            format: ScreenDumpCliFormat::TextPlain,
            layer: ScreenDumpCliLayer::Scrollback,
            rect: None,
            output: Some(out_path.to_string_lossy().into_owned()),
            timeout_ms: 5_000,
        };
        let _exit = screen_dump_command(cfg);

        let got = std::fs::read(&out_path).expect("read output");
        let text = std::str::from_utf8(&got).expect("utf8");
        assert!(
            text.contains("SCROLLED_OUT_HEAD"),
            "scrollback dump should contain SCROLLED_OUT_HEAD (= スクロールアウトした marker): {text:?}"
        );
        assert!(
            !text.contains("VISIBLE_TAIL"),
            "scrollback dump should NOT contain VISIBLE_TAIL (= まだ visible): {text:?}"
        );

        // cleanup
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

    /// daemon spawn + `ClientConnection::send_raw_bytes` の smoke test。
    ///
    /// handler の bytes 化検証は `input_handlers::tests` (= 24 件) で済んでおり、
    /// 本 test は CLI 層の wiring (= `dispatch_spec` → `send_raw_bytes` →
    /// daemon の `TYPE_RAW_DATA` 経路 → master PTY) が壊れていないことを
    /// 「send 後に daemon が disconnect せず、screen.dump も response を返す」
    /// で間接確認する。
    ///
    /// 子が echo する内容を screen から読み戻す形の test は別途必要だが、
    /// 既存 daemon test (`crates/hyoui/src/daemon/session.rs` の
    /// `serve_screen_dump_*` 群) で raw_data 経路の検証は protocol レベルで
    /// 済んでおり、CLI 側からは「送信 API が成立 + 後続 control が動く」で
    /// 十分。
    #[test]
    fn send_raw_bytes_does_not_disconnect_daemon() {
        use hyoui::daemon::{DaemonConfig, Session};
        use hyoui::protocol::messages::{
            ScreenDumpFormat as ProtoDumpFormat, ScreenDumpLayer as ProtoDumpLayer,
            ScreenDumpRequest,
        };
        use std::time::Duration;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("input-int.sock");

        // 子: 30 秒スリープする静かな子 (= 既存 screen_dump test と同じ pattern)。
        // bytes を送っても echo は出ないが、daemon が disconnect しないことを
        // 確認するには十分。
        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "input-int-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'READY'; sleep 30".into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // listener bind + 子の "READY" が screen state に反映されるまで少し待つ。
        std::thread::sleep(Duration::from_millis(200));

        // Rw client として attach。MVP_CAPS を要求して handshake 通す。
        let opts = AttachOptions {
            mode: Mode::Rw,
            caps: hyoui::protocol::MVP_CAPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..AttachOptions::default()
        };
        let mut conn = connect_with_retry(&sock_path, opts).expect("attach Rw");

        // 1. text bytes 送信 (= input_handlers::handle_text 経路と同等)
        conn.send_raw_bytes(b"HELLO_TXT")
            .expect("send_raw_bytes text");
        // 2. paste wrap (= handle_paste 経路と同等)
        conn.send_raw_bytes(b"\x1b[200~PASTE\x1b[201~")
            .expect("send_raw_bytes paste");
        // 3. key sequence (= handle_key("Enter") = "\r")
        conn.send_raw_bytes(b"\r").expect("send_raw_bytes enter");
        // 4. hex bytes (= handle_hex)
        conn.send_raw_bytes(&[0x1b, 0x5b, 0x41])
            .expect("send_raw_bytes hex");

        // raw_data 送信後でも screen.dump が response を返せる (= daemon が
        // disconnect していない、protocol violation を起こしていない)。
        let req = ScreenDumpRequest {
            format: ProtoDumpFormat::Ansi,
            layer: ProtoDumpLayer::Visible,
            rect: None,
            serial: Some(1),
        };
        conn.send_control(&ControlMessage::ScreenDumpRequest(req))
            .expect("send screen.dump request");

        // ModeChange/LeaderNotify を skip しつつ ScreenDumpResponse を待つ。
        let mut got_response = false;
        for _ in 0..10 {
            match conn.recv_control(None) {
                Ok(ControlMessage::ScreenDumpResponse(r)) => {
                    assert!(
                        !r.payload.is_empty(),
                        "screen.dump response payload must not be empty after raw bytes"
                    );
                    got_response = true;
                    break;
                }
                Ok(_) => continue,
                Err(e) => panic!("daemon disconnected after raw bytes: {e}"),
            }
        }
        assert!(
            got_response,
            "expected ScreenDumpResponse after sending raw bytes via CLI handler path"
        );

        // cleanup: kill 送って thread を終わらせる。
        let kill = hyoui::protocol::messages::Kill { signal: None };
        let _ = conn.send_control(&ControlMessage::Kill(kill));
        drop(conn);
        let _ = daemon_handle.join();
    }

    /// state-based wait の integration test (DR-0006 §9)。
    ///
    /// 子が "READY" を画面出力するまで `wait_command` が block し、出現後に
    /// 成功 (= ExitCode::SUCCESS) で返ることを確認する。子は sleep 中に出力
    /// するので polling が実際に動いていることも合わせて検証される。
    #[test]
    fn wait_command_matches_visible_state_pattern() {
        use hyoui::cli::WaitConfig;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("wait-match.sock");
        let sock_for_daemon = sock_path.clone();

        // 子: 200ms 待ってから "READY" を 1 行出力、その後 30 秒 sleep。
        // wait 起動時点 (= sleep 待ちより前) では READY がまだ無いので、
        // polling が回って 200ms 経過後に match する経路を踏む。
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "wait-match-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "sleep 0.2; printf 'READY\\n'; sleep 30".into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // listener bind 完了 + handshake 受付待ち
        std::thread::sleep(std::time::Duration::from_millis(150));

        let cfg = WaitConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            pattern: "READY".into(),
            timeout_ms: Some(5_000),
            poll_interval_ms: Some(50),
        };
        let start = std::time::Instant::now();
        let exit = wait_command(cfg);
        let elapsed = start.elapsed();

        // ExitCode::SUCCESS (0) を期待。
        let exit_dbg = format!("{exit:?}");
        assert!(
            exit_dbg.contains("ExitCode(unix_exit_status(0))")
                || exit_dbg.contains("SUCCESS")
                || exit_dbg.contains("0"),
            "wait_command should succeed when pattern appears; got {exit_dbg}, elapsed={elapsed:?}"
        );
        // 5s timeout より十分前に成立しているはず。
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "wait should match quickly after READY appears, but took {elapsed:?}"
        );

        // cleanup: kill して daemon thread を終わらせる
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

    /// state-based wait の timeout 経路。pattern が永遠に出ない子に対して
    /// `--timeout=300ms` で起動 → 300ms ちょい後に exit code 1 (= Timeout)。
    #[test]
    fn wait_command_times_out_when_pattern_never_appears() {
        use hyoui::cli::WaitConfig;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("wait-timeout.sock");
        let sock_for_daemon = sock_path.clone();

        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "wait-timeout-test",
                sock_for_daemon,
                // 完全に静かな子: 何も出力せずに sleep
                vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });
        std::thread::sleep(std::time::Duration::from_millis(150));

        let cfg = WaitConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            pattern: "NEVER_SHOWS_UP".into(),
            timeout_ms: Some(300),
            poll_interval_ms: Some(50),
        };
        let start = std::time::Instant::now();
        let exit = wait_command(cfg);
        let elapsed = start.elapsed();

        let exit_dbg = format!("{exit:?}");
        // exit code 1 (Timeout) を期待。
        assert!(
            exit_dbg.contains("(1)") || exit_dbg.contains("ExitCode(unix_exit_status(1))"),
            "wait_command should time out with exit 1; got {exit_dbg}"
        );
        // 300ms timeout を尊重しつつ、polling の最終 sleep 分のずれを許容 (= 1.5s 程度の余裕)。
        assert!(
            elapsed >= std::time::Duration::from_millis(280)
                && elapsed < std::time::Duration::from_millis(2_500),
            "expected timeout near 300ms, got {elapsed:?}"
        );

        // cleanup
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

    /// input family の `wait:<pattern>` spec 経路 (DR-0006 §9.2)。
    /// `hyoui input` の dispatch_spec から `wait_core::wait_for_pattern` が
    /// 呼ばれ、子が pattern を出力したら次 spec に進むことを確認する。
    #[test]
    fn input_dispatch_wait_spec_proceeds_after_match() {
        use hyoui::cli::{InputCommand, InputSpec};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("input-wait.sock");
        let sock_for_daemon = sock_path.clone();

        // 子: すぐに "GO" を出力 → wait:GO は即 match
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "input-wait-test",
                sock_for_daemon,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'GO'; sleep 30".into(),
                ],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });
        std::thread::sleep(std::time::Duration::from_millis(150));

        let cmd = InputCommand {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            specs: vec![InputSpec::Wait("GO".into())],
            timeout: std::time::Duration::from_secs(3),
            lock_token: None,
            max_file_bytes: hyoui::cli::DEFAULT_INPUT_MAX_FILE_BYTES,
        };
        let start = std::time::Instant::now();
        let exit = input_command(cmd);
        let elapsed = start.elapsed();
        let exit_dbg = format!("{exit:?}");
        assert!(
            exit_dbg.contains("SUCCESS") || exit_dbg.contains("(0)"),
            "input wait spec should succeed; got {exit_dbg}, elapsed={elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "wait:GO should match quickly; took {elapsed:?}"
        );

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

    /// input family の `wait-idle:<duration>` spec 経路 (DR-0006 §9.2)。
    /// 静かな子に対し short idle 期間で成立することを確認 (= Phase A1 で
    /// SequenceNo 観察により実装)。
    #[test]
    fn input_dispatch_wait_idle_spec_succeeds_on_quiet_child() {
        use hyoui::cli::{InputCommand, InputSpec};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("input-wait-idle.sock");
        let sock_for_daemon = sock_path.clone();

        // 完全に静かな子。最初の output 反映後は seqno が動かない。
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "input-wait-idle-test",
                sock_for_daemon,
                vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
            );
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });
        std::thread::sleep(std::time::Duration::from_millis(200));

        let cmd = InputCommand {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            specs: vec![InputSpec::WaitIdle(std::time::Duration::from_millis(200))],
            timeout: std::time::Duration::from_secs(3),
            lock_token: None,
            max_file_bytes: hyoui::cli::DEFAULT_INPUT_MAX_FILE_BYTES,
        };
        let exit = input_command(cmd);
        let exit_dbg = format!("{exit:?}");
        assert!(
            exit_dbg.contains("SUCCESS") || exit_dbg.contains("(0)"),
            "wait-idle should succeed on quiet child; got {exit_dbg}"
        );

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
