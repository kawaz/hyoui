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
    AttachConfig, Command, HelpTopic, InputCommand, InputSpec, KillConfig, ListConfig, ListFormat,
    LockAcquireConfig, LockCommand, LockMode, LockReleaseConfig, RecordCommand, RecordDirectionArg,
    RecordFormatArg, RecordInputSecrecyArg, RecordListConfig, RecordListFormatArg,
    RecordStartConfig, RecordStopConfig, ScreenCommand, ScreenDumpCliFormat, ScreenDumpCliLayer,
    ScreenDumpConfig, ScreenSnapshotConfig, SnapshotCliComponent, StatusConfig, TailConfig,
    WaitConfig, parse_args, usage,
};
use hyoui::client::{AttachOptions, ClientConnection, RunOutcome};
use hyoui::protocol::messages::{
    DumpRect, InputSecrecy, RecordDirection, RecordFormat, RecordInfo, RecordListRequest,
    RecordStartRequest, RecordStopAllRequest, RecordStopRequest, ScreenDumpFormat, ScreenDumpLayer,
    ScreenDumpRequest, SnapshotComponent, StateSnapshotRequest, StatusQuery, TailRequest,
};
use hyoui::protocol::{ControlMessage, Mode};
use hyoui::sys::{enter_raw, is_tty};

mod completion;
mod daemonize;
use hyoui::input_bytes as input_handlers;
mod socket_path;
mod wait_core;

/// `hyoui attach` / `hyoui run` が daemon との接続を予期せず失った
/// (= `RunOutcome::ConnectionLost`、daemon 消滅の疑い) ときの exit code
/// (issue 2026-06-11 優先1)。
///
/// Design rationale: 子 PTY の exit code (= 0..=255、signal 死は 128+signum、
/// よく見る 130/137/143) や hyoui の既存自己エラー慣習 (1=一般 / 2=usage・parse /
/// 3=kill 系特殊) のいずれとも重ならない値として 9 を専用予約する。これにより
/// 「子が正常終了した (exit 0)」と「daemon が落ちて attach が切れた」を呼び出し側
/// (= スクリプト) が status code で区別できる。
const EXIT_CONNECTION_LOST: u8 = 9;

/// `--debug-dump-client=<path>` 経路で stdout に書く bytes を file にも複製する
/// **Tee writer**。
///
/// - `a` (= stdout) を主、`b` (= file) は best-effort。`b` のエラーで stdout 中継を
///   止めない (= debug dump の I/O error は session 継続を妨げない)。
/// - `write()` の返値は a の write 量で確定 (= caller の partial write 判定が
///   a の状態のみに依存)、b には同じ範囲を `write_all` で書く
struct TeeWriter<A: std::io::Write, B: std::io::Write> {
    primary: A,
    dump: B,
}

impl<A: std::io::Write, B: std::io::Write> std::io::Write for TeeWriter<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.primary.write(buf)?;
        // dump 側のエラーは飲み込む (= best-effort、stdout 中継を止めない)
        let _ = self.dump.write_all(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let r = self.primary.flush();
        let _ = self.dump.flush();
        r
    }
}

/// DR-0015 §2.3: attach client process が外部 SIGTSTP / SIGCONT を受けた時の
/// termios 復元 / 再 raw 化を専任する **signal monitor thread** を起動する。
///
/// `Arc<Mutex<TtyGuard>>` で TtyGuard を main thread と共有 (= `Termios` 内の
/// `RefCell` が `!Sync` のため Arc 直接共有不可、Mutex で run-time sync)。
/// `Arc<AtomicBool>` の shutdown flag で main 終了時に signal_thread を畳む
/// (= 畳まないと Arc::drop で TtyGuard が drop されず termios 復元されない)。
///
/// signal handler 自体は self-pipe に signum を 1 byte 書くだけ (= async-signal-safe)。
/// 本 thread が drain で取り出して同期 path で termios 操作 + `raise(SIGTSTP)`。
fn install_attach_signal_thread(
    guard: std::sync::Arc<std::sync::Mutex<hyoui::sys::TtyGuard>>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    winch_notify_wr: Option<std::os::fd::OwnedFd>,
) -> Result<std::thread::JoinHandle<()>, hyoui::sys::Error> {
    use hyoui::sys::signal::{install_default, install_self_pipe, raise, register_self_pipe};
    use nix::sys::signal::Signal;
    use std::sync::atomic::Ordering;

    let pipe = install_self_pipe()?;
    register_self_pipe(Signal::SIGTSTP)?;
    register_self_pipe(Signal::SIGCONT)?;
    // DR-0019 §6: SIGWINCH も self-pipe に集約 (= SELFPIPE_WRITE_FD は process
    // グローバルなので別 self-pipe は作れない)。WINCH を観測したら winch_notify
    // pipe へ 1 byte 書いて run loop を起こす (= signal handler は async-signal-safe な
    // write のみ、TIOCGWINSZ 取得 + Resize 送信は run loop の責務)。
    if winch_notify_wr.is_some() {
        register_self_pipe(Signal::SIGWINCH)?;
    }

    let handle = std::thread::spawn(move || {
        loop {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            let drained = match pipe.drain() {
                Ok(v) => v,
                Err(_) => return,
            };
            for &sig in &drained {
                let signum = sig as i32;
                if signum == Signal::SIGTSTP as i32 {
                    // 外側 TTY を pre-raw に戻す → SIGTSTP を kernel default で処理させて
                    // STOPPED へ。復帰時 (= SIGCONT 受信時) に raw 再設定。
                    if let Ok(g) = guard.lock() {
                        g.suspend();
                    }
                    let _ = install_default(Signal::SIGTSTP);
                    let _ = raise(Signal::SIGTSTP);
                    // STOPPED → SIGCONT で復帰。disposition を self-pipe に戻して raw 再設定。
                    let _ = register_self_pipe(Signal::SIGTSTP);
                    if let Ok(g) = guard.lock() {
                        g.resume();
                    }
                } else if signum == Signal::SIGWINCH as i32 {
                    // DR-0019 §6: WINCH を run loop に中継 (= notify pipe へ 1 byte)。
                    // best-effort、EAGAIN (= 既に未読 byte あり) は無視。
                    if let Some(wr) = winch_notify_wr.as_ref() {
                        let _ = nix::unistd::write(wr, &[1u8]);
                    }
                }
                // SIGCONT byte: process は既に kernel で起きてる、thread 内では no-op。
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    Ok(handle)
}

/// `--debug-dump-client=<path>` を open する helper。失敗時は stderr に warn を
/// 出して `None` を返し、dump を諦める (= session は止めない)。
fn open_debug_dump(path: &str, role: &str) -> Option<std::fs::File> {
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("hyoui: --debug-dump-{role} open 失敗 (= path: {path}): {e} (dump 無効化)");
            None
        }
    }
}

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

/// daemon の応答 1 frame を待つ受信タイムアウト (= half-open 接続での永久 hang 防止)。
///
/// daemon は同期 request/response 設計なので健全時はこの上限に届かない。daemon
/// process が消えたが socket FIN が流れていない half-open ケースで、blocking
/// `recv_control` が永久に固まるのを防ぐ。
const LOCK_RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `conn` の reader fd を poll して recv 可能になるまで最大 `timeout` 待つ。
///
/// 戻り値:
/// - `Ok(true)`: POLLIN、caller は `recv_control` で frame を読める。
/// - `Ok(false)`: timeout 超過 / POLLHUP / POLLERR (= daemon 無応答 or 消失)。
///   caller は「daemon 消失」として error 終了すべき。
/// - `Err`: poll(2) 自体の失敗。
///
/// EINTR は signal 割り込みなので通算 deadline で re-poll する。
fn poll_recv_ready(
    conn: &ClientConnection,
    timeout: std::time::Duration,
) -> Result<bool, hyoui::sys::Error> {
    use hyoui::sys::poll::{PollFlags, PollOutcome, poll};
    use nix::poll::{PollFd, PollTimeout};
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let fd = conn.reader_fd();
        let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
        let to = PollTimeout::try_from(remaining.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(PollTimeout::NONE);
        match poll(&mut fds, to) {
            Ok(PollOutcome::Ready(_)) => {
                let re = fds[0].revents().unwrap_or(PollFlags::empty());
                if re.contains(PollFlags::POLLIN) {
                    return Ok(true);
                }
                if re.contains(PollFlags::POLLHUP) || re.contains(PollFlags::POLLERR) {
                    return Ok(false);
                }
                // 想定外 revents は re-poll。
            }
            Ok(PollOutcome::Timeout) => return Ok(false),
            Ok(PollOutcome::Interrupted) => continue,
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }
}

fn main() -> ExitCode {
    // Skip argv[0]: parse_args expects the trailing arguments only.
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // DR-0015 Task #N (2026-05-29 kawaz 指示): daemon 子 process は **env で識別**する
    // (= 旧 `__daemonize-run` hidden subcommand 廃止)。`ps` から見ると通常の
    // `hyoui run --detached --socket=... --session=... -- cmd` に見える形に。
    //
    // 起動直後に env を unset することで daemon が spawn する孫 process (= 子 PTY 経由
    // の cmd) には漏れない。
    // env `HYOUI_DAEMONIZE_INIT` 存在で daemon child 経路に分岐 (= JSON serialize で
    // 初期化情報を全部受け取る)。run_daemon_child 内で env parse + unset する。
    // 旧 `__daemonize-run` hidden subcommand + 旧 `--socket=` 等 flag は **全廃**。
    if std::env::var("HYOUI_DAEMONIZE_INIT").is_ok() {
        return daemonize::run_daemon_child();
    }

    // DR-0028 Phase 1: self-exec upgrade で継承した fd / init env を検知したら
    // upgrade-resume child 経路に分岐する (= HYOUI_DAEMONIZE_INIT と同じ pattern)。
    // 検出 env は `HYOUI_UPGRADE_RESUME=1` のみ、他の値は無視 (= 誤誘導防止)。
    if std::env::var("HYOUI_UPGRADE_RESUME").as_deref() == Ok("1") {
        return daemonize::run_upgrade_resume_child();
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

        Command::Set(cfg) => set_command(cfg),

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

        Command::Detach(cfg) => detach_command(cfg),

        Command::Record(sub) => match sub {
            RecordCommand::Start(cfg) => record_start_command(cfg),
            RecordCommand::Stop(cfg) => record_stop_command(cfg),
            RecordCommand::List(cfg) => record_list_command(cfg),
            // `RecordCommand` is `#[non_exhaustive]`; future variants surface as
            // a generic skew error so older binaries report clearly.
            _ => {
                eprintln!(
                    "hyoui: record: unsupported record subcommand variant (binary/library version skew)"
                );
                ExitCode::from(2)
            }
        },

        Command::Web(cfg) => web_command(cfg),

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

/// `--scrollback-rows` flag (CLI) と `HYOUI_SCROLLBACK_ROWS` env を解決する。
///
/// 優先順位 (高 → 低):
///
/// - `--scrollback-rows=<N>` flag (= `cfg.scrollback_rows` が `Some(N)`)
/// - `HYOUI_SCROLLBACK_ROWS=<N>` env
/// - `None` (= DaemonConfig の既定値 1000 行を維持)
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

/// `hyoui run` の主要ロジック。
///
/// 同 process 内で:
/// 1. 子 PTY 用 daemon session を起動 (= listener bind 完了)
/// 2. daemon thread を spawn (= accept + relay)
/// 3. main thread が attach client として接続、stdin/stdout を中継
/// 4. daemon thread を join、その exit code を返す
fn run_command(cfg: hyoui::cli::RunConfig) -> ExitCode {
    let scrollback_rows = resolve_scrollback_rows(cfg.scrollback_rows);
    // DR-0018: namespace を解決 (= --namespace flag > HYOUI_NAMESPACE env > default)。
    // 解決済 namespace は (1) socket 配置 dir の決定、(2) 子プロセスへの常時 env 注入、
    // (3) 非 detached 経路で exec する `hyoui attach` への明示伝搬、に使う。
    let namespace = socket_path::resolve_namespace(cfg.namespace.as_deref());
    // DR-0024: 子 PTY env scrub。config (= ~/.config/hyoui/config.toml) を load し、
    // target = argv basename で builtin + user 設定を merge して ScrubPlan を解決する。
    // `None` で完全 disable (= --no-scrub-env or config の scrub_env_enabled=false)、
    // `Some(plan)` で daemon child の `env_scrub::apply` に渡す。
    //
    // `--no-scrub-env` 指定時は config load 自体を skip する (= 完全 escape hatch、
    // config 不在/不正の影響を受けず起動できる)。config パースエラーは exit non-zero
    // で起動を拒否 (= 意図しない設定での起動は親 host Internal Context 漏洩リスク)。
    let scrub_env_plan = if cfg.no_scrub_env {
        None
    } else {
        let config = match hyoui::config::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("hyoui: {e}");
                eprintln!(
                    "hyoui: config ファイルを修正してから再実行してください。または \
                     `--no-scrub-env` で起動して config の読み込みをスキップできます"
                );
                return ExitCode::from(2);
            }
        };
        hyoui::sys::env_scrub::resolve_plan(false, &config, &cfg.command)
    };
    // size 解決 (= ユーザ指示 2026-05-29、stdin pipe 経由):
    // - 明示指定 (= --cols/--rows/--size) があればそれを使う
    // - 非 detached + 明示なし → 外側 TTY size (= stdin) を継承
    // - detached + 明示なし → None (= daemon 側 default 80x24 で起動、後で attach resize)
    // - 非 TTY (= pipe) + 明示なし → None (= daemon 側 default)
    //
    // Some なら parent が daemon spawn 時に stdin pipe で送る (= ps から数値消す目的)、
    // None なら何も送らない (= daemon 側 default 80x24)。
    let initial_size: Option<(u16, u16)> = {
        let explicit_c = cfg.cols.and_then(|c| u16::try_from(c).ok());
        let explicit_r = cfg.rows.and_then(|r| u16::try_from(r).ok());
        match (explicit_c, explicit_r) {
            (Some(c), Some(r)) => Some((c, r)),
            _ if !cfg.detached => {
                let stdin = std::io::stdin();
                if let Ok(Some(ws)) = hyoui::sys::tty_size(stdin.as_fd()) {
                    Some((explicit_c.unwrap_or(ws.cols), explicit_r.unwrap_or(ws.rows)))
                } else {
                    // 部分指定は補完して送る、両方なしは None で daemon default
                    match (explicit_c, explicit_r) {
                        (Some(c), None) => Some((c, 24)),
                        (None, Some(r)) => Some((80, r)),
                        _ => None,
                    }
                }
            }
            _ => match (explicit_c, explicit_r) {
                (Some(c), None) => Some((c, 24)),
                (None, Some(r)) => Some((80, r)),
                _ => None, // detached + 全部未指定 → daemon default
            },
        }
    };
    if cfg.detached {
        if cfg.debug_dump_client.is_some() {
            // detached parent は client role を担わない (= 即 exit) ので
            // `--debug-dump-client` は意味を成さない。silent ignore せず明示 reject。
            eprintln!(
                "hyoui: --debug-dump-client は --detached と併用できません \
                 (= detached daemon に後から `hyoui attach --debug-dump-client=...` で接続する形を取ってください)"
            );
            return ExitCode::from(2);
        }
        return daemonize::run_detached_parent(
            cfg.session.clone(),
            cfg.socket.clone(),
            initial_size,
            cfg.until.clone(),
            scrollback_rows,
            cfg.debug_dump_server.clone(),
            namespace,
            cfg.on_child_suspend,
            cfg.timeout_ms,
            cfg.idle_timeout_ms,
            scrub_env_plan,
            cfg.command,
        );
    }

    // DR-0015 §1 exec attach pattern:
    // 1. detached daemon を spawn して ready 通知を待つ
    // 2. 親 process 自身を `hyoui attach <session>` に exec で置換
    // これにより:
    // - `ps` で常に "hyoui run --detached --session=..." (daemon) + "hyoui attach <session>"
    //   (= 親) が並ぶ = role 一目了然
    // - memory image が完全に置換されるので、fork+thread の global static 競合事故ゼロ
    // - `hyoui attach` の既存 attach_command 実装をそのまま流用 (= コード重複ゼロ)
    let (session_id, _sock) = match daemonize::spawn_detached_daemon_and_wait_ready(
        cfg.session.clone(),
        cfg.socket.clone(),
        initial_size,
        cfg.until.clone(),
        scrollback_rows,
        cfg.debug_dump_server.clone(),
        namespace.clone(),
        cfg.on_child_suspend,
        cfg.timeout_ms,
        cfg.idle_timeout_ms,
        scrub_env_plan,
        cfg.command,
    ) {
        Ok(pair) => pair,
        Err(code) => return code,
    };

    // exec で自プロセスを `hyoui attach <session>` に置換。
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: current_exe 失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let mut attach_cmd = std::process::Command::new(exe);
    attach_cmd.arg("attach").arg(&session_id);
    if let Some(socket) = cfg.socket.as_deref() {
        attach_cmd.arg(format!("--socket={socket}"));
    } else {
        // DR-0018: socket 明示なしのときは namespace を attach に伝える。run が
        // --namespace flag で解決した場合 env に値が無いので、明示渡しが必須
        // (= flag 経路と env 経路で attach の socket 解決を一致させる)。
        attach_cmd.arg(format!("--namespace={namespace}"));
    }
    if let Some(p) = cfg.debug_dump_client.as_deref() {
        attach_cmd.arg(format!("--debug-dump-client={p}"));
    }
    // DR-0019 §5: pipe-through。run --stdin-eof を明示指定時のみ exec attach に
    // 伝搬する (= 未指定なら attach 側が stdin tty 判定で解決する)。
    if let Some(eof) = cfg.stdin_eof {
        let v = match eof {
            hyoui::cli::StdinEofArg::SendEof => "send-eof",
            // Detach + 将来追加 variant は detach 扱い (= 未指定時の安全側 fallback と
            // 整合。新値が増えたら明示 arm を足す)。
            _ => "detach",
        };
        attach_cmd.arg(format!("--stdin-eof={v}"));
    }
    // CommandExt::exec で自プロセスを置換。成功時は戻らない、失敗時は io::Error を返す。
    use std::os::unix::process::CommandExt;
    let err = attach_cmd.exec();
    eprintln!("hyoui: exec hyoui attach 失敗: {err}");
    ExitCode::from(1)
}

/// `hyoui attach <session>` の主要ロジック。
///
/// 既存 daemon に socket connect し、stdin/stdout を中継する。
/// daemon は別 process / 別 hyoui run --detached 等で起動済みの想定。
/// `--index=N` (or 位置引数の数字) から session id を解決する。
///
/// `hyoui list` と同様に socket dir を scan、`*.sock` の live 一覧を mtime 昇順 sort、
/// 以下の index 規約で 1 件選ぶ:
/// - `1` → 最古、`2` → 2 番目に古い、...
/// - `-1` → 最新、`-2` → 2 番目に新しい、...
///
/// stale socket は除外 (= attach 失敗確実のため index に含めない)。
/// 範囲外 / 0 件 → `Err`。
fn resolve_session_by_index(index: i32, namespace: &str) -> Result<String, String> {
    if index == 0 {
        return Err("index 0 は不正です (= 1-based、1 が最古、-1 が最新)".to_string());
    }
    let dirs = list_candidate_dirs(namespace);
    let mut entries: Vec<(String, std::time::SystemTime)> = Vec::new();
    for dir in dirs {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("sock") {
                continue;
            }
            if !probe_socket_liveness(&path) {
                continue;
            }
            let session = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.push((session, mtime));
        }
    }
    if entries.is_empty() {
        return Err("no live sessions found".to_string());
    }
    entries.sort_by_key(|e| e.1);
    let len = entries.len();
    let resolved = if index > 0 {
        // 1-based 古い順: 1 → 最古
        let idx = (index - 1) as usize;
        entries.get(idx).map(|e| e.0.clone())
    } else {
        // -1 → 最新、-2 → 2 番目に新しい
        let abs = (-index) as usize;
        if abs > len {
            None
        } else {
            Some(entries[len - abs].0.clone())
        }
    };
    resolved.ok_or_else(|| {
        format!(
            "attach: index {index} は範囲外 (= {len} live session(s) 検出、\
             `hyoui list` で一覧確認可)"
        )
    })
}

fn attach_command(cfg: AttachConfig) -> ExitCode {
    // H3: HYOUI_DETACH_PREFIX を raw mode 入る **前** に validate。invalid なら
    // 通常 terminal で stderr に出してから exit (= 旧 silent fallback で warning が
    // raw mode 後の scrollback に流される罠を回避)。解決値は §5 の発見性ヒント
    // (= 実 prefix を文言に反映) でも使う。
    let detach_prefix = match hyoui::client::resolve_detach_prefix_from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: attach: {e}");
            return ExitCode::from(2);
        }
    };
    // DR-0026: attach UX 設定は既存 config.toml 読み込み経路を共有する。
    // 不正 config は raw mode / handshake より前に拒否し、意図しない介入設定で続行しない。
    let app_config = match hyoui::config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("hyoui: attach: {e}");
            eprintln!("hyoui: config ファイルを修正してから再実行してください");
            return ExitCode::from(2);
        }
    };

    // DR-0018: namespace を解決 (= --namespace flag > HYOUI_NAMESPACE env > default)。
    let namespace = socket_path::resolve_namespace(cfg.namespace.as_deref());
    let sock = if let Some(p) = cfg.socket.clone() {
        let p = std::path::PathBuf::from(p);
        // DR-0020 §3 (codex review 2026-06-12): `--socket` 明示経路でも self-attach
        // ガードを効かせる (= sid 経路だけだと `--socket=<自分の sock>` で迂回できた)。
        // $HYOUI_SESSION_ID が指す自セッションの socket path と canonical 比較する。
        if socket_path_is_self(&p) {
            eprintln!(
                "hyoui: attach: 自セッションの socket への attach はできません \
                 (= hyoui in hyoui のネスト防止、DR-0020 §3)。"
            );
            eprintln!(
                "       中から脱出したい場合は `hyoui detach`、終了したい場合は `hyoui kill` を使ってください。"
            );
            return ExitCode::from(2);
        }
        p
    } else {
        // index 指定なら resolve_session_by_index で session-id を確定、
        // それ以外は cfg.session_id を使う (= parse_attach で同時指定は弾かれている)。
        let sid_owned: String;
        let sid = if let Some(index) = cfg.index {
            match resolve_session_by_index(index, &namespace) {
                Ok(s) => {
                    sid_owned = s;
                    sid_owned.as_str()
                }
                Err(e) => {
                    eprintln!("hyoui: attach: {e}");
                    return ExitCode::from(1);
                }
            }
        } else {
            match cfg.session_id.as_deref() {
                Some(s) => s,
                None => {
                    print_session_required("attach");
                    return ExitCode::from(2);
                }
            }
        };
        // DR-0020 §3: attach 対象が自セッションならネスト防止エラー (= tmux $TMUX
        // ネスト防止と同型)。明示引数でも index 解決結果でも、解決した sid + namespace
        // が子へ注入された $HYOUI_SESSION_ID + $HYOUI_NAMESPACE と一致したら拒否する。
        // self default は作らない (= session 省略は上の required エラー)、ro 観戦の
        // 例外も設けない (= DR-0020 §3、需要が出たら再検討)。
        if attach_target_is_self(sid, &namespace) {
            eprintln!(
                "hyoui: attach: 自セッション ({sid}) への attach はできません \
                 (= hyoui in hyoui のネスト防止、DR-0020 §3)。"
            );
            eprintln!(
                "       中から脱出したい場合は `hyoui detach`、終了したい場合は `hyoui kill` を使ってください。"
            );
            return ExitCode::from(2);
        }
        match socket_path::resolve_in_namespace(None, sid, &namespace) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "hyoui: attach: socket path 解決失敗: {e} (session: {sid}, namespace: {namespace})"
                );
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
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("attach", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // DR-0026 §2: rw attach は stopped child を操作する意思表明とみなし、handshake
    // snapshot が stopped の場合だけ既存 resume.request を即送る。ro / rw-no-leader は
    // 観察または非 leader 接続なので、設定値に関係なく子を起こさない。
    if conn.response.child_stopped
        && conn.response.mode == Mode::Rw
        && app_config.attach.resume.on_reattach
        && let Err(e) = conn.send_child_resume()
    {
        eprintln!("hyoui: attach: stopped child の resume 要求送信失敗: {e}");
        return ExitCode::from(1);
    }

    // DR-0020 §5: attach 成立時に detach / peek の発見性ヒントを stderr へ 1 行出す。
    // 子の出力経路 (PTY/stdout) ではなく client の stderr なので透過性を壊さない
    // (= screen 慣行)。`--quiet` で抑止、非 tty stderr (= pipe) では出さない。
    //
    // 出力位置は raw mode に入る **前** (= 外側端末がまだ cooked のうち)。これで
    // ヒントは外側端末の scrollback に残り、attach 後の redraw (= 全画面クリア) で
    // 消されない。raw 前の stderr 出力が detach key を取りこぼす旧回帰は、root cause
    // (= `enter_raw` の TCSAFLUSH が cooked 窓の入力 queue を破棄していた) を
    // TCSANOW で解消済み (Fable review M4、`sys/tty.rs` の regression test 参照)。
    //
    // 文言は `HYOUI_DETACH_PREFIX` の解決値から生成 (= custom prefix を反映)。
    // `none` (= detach key 無効化) ならヒント自体出さない (= 嘘の脱出方法を教えない)。
    if !cfg.quiet
        && is_tty(std::io::stderr().as_fd())
        && let Some(prefix) = detach_prefix
    {
        let prefix_disp = match prefix {
            // Ctrl byte (0x01..=0x1a) は "Ctrl-X" 表記。
            1..=26 => format!("Ctrl-{}", (b'A' + prefix - 1) as char),
            other => format!("0x{other:02x}"),
        };
        eprintln!(
            "[hyoui] detach: {prefix_disp} d  |  peek (read-only): hyoui attach <session> --mode=ro"
        );
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let stdin_is_tty = is_tty(stdin.as_fd());
    let raw_guard: Option<std::sync::Arc<std::sync::Mutex<hyoui::sys::TtyGuard>>> = if stdin_is_tty
    {
        match nix::unistd::dup(stdin.as_fd()) {
            Ok(dup_for_guard) => match enter_raw(dup_for_guard) {
                Ok(g) => Some(std::sync::Arc::new(std::sync::Mutex::new(g))),
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

    // DR-0019 §6: SIGWINCH → Resize 配線。tty stdin (= raw_guard あり) のとき、
    // signal thread → run loop の連絡用 notify pipe を用意する。signal thread が
    // WINCH を受けて write 端に 1 byte、run loop が read 端 (= WinchSource) を poll。
    // read 端は non-blocking にして run loop の drain ループが EAGAIN で抜けられる
    // ようにする。size 取得用に stdin を dup した独立 fd を closure に持たせる。
    let mut winch_notify_wr: Option<std::os::fd::OwnedFd> = None;
    let winch_source: Option<hyoui::client::WinchSource> = if raw_guard.is_some() {
        match nix::unistd::pipe() {
            Ok((rd, wr)) => {
                use nix::fcntl::{FcntlArg, OFlag, fcntl};
                // 両端を non-blocking に:
                // - read 端: run loop の drain で block しない。
                // - write 端: signal thread が pipe full でも block しない (= WINCH 連打で
                //   run loop が drain する前に書ききると buffer full になり得る。EAGAIN は
                //   「既に未読 byte あり = WINCH 通知は届く」ので無視してよい。この設定が
                //   無いと write が block して signal thread が固まる)。これで `main.rs` の
                //   WINCH 中継コメント「EAGAIN は無視」と実装が一致する。
                for fd in [rd.as_fd(), wr.as_fd()] {
                    if let Ok(flags) = fcntl(fd, FcntlArg::F_GETFL) {
                        let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
                        let _ = fcntl(fd, FcntlArg::F_SETFL(flags));
                    }
                }
                // size closure 用に stdin を dup (= run / signal と fd 所有を分離)。
                let size_fd = nix::unistd::dup(stdin.as_fd()).ok();
                let size_fn: Box<dyn FnMut() -> Option<(u16, u16)> + Send> = Box::new(move || {
                    let fd = size_fd.as_ref()?;
                    match hyoui::sys::tty_size(fd.as_fd()) {
                        Ok(Some(ws)) => Some((ws.cols, ws.rows)),
                        _ => None,
                    }
                });
                // write 端は signal thread に渡す。
                winch_notify_wr = Some(wr);
                Some(hyoui::client::WinchSource::new(rd, size_fn))
            }
            Err(e) => {
                eprintln!("hyoui: SIGWINCH notify pipe 作成失敗: {e} (続行、resize 追従なし)");
                None
            }
        }
    } else {
        None
    };

    // DR-0015 §2.3: attach client が外部 SIGTSTP を受けたら、自プロセスの termios を
    // 復元 → STOPPED に入り、SIGCONT 復帰時に raw mode 再設定。daemon には何も
    // 送らない (= 軸 2 廃止、子プロセスとは無関係)。
    let signal_shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // signal thread の起動に成功したか。失敗時は winch notify pipe の write 端が
    // この match 内で drop され、read 端 (= winch_source) だけが残る。その read 端を
    // conn に注入すると、誰も write しない pipe を run loop が poll し、Linux では
    // POLLHUP が revents に立ち続けて busy loop になる (= codex 指摘)。なので失敗時は
    // winch_source を無効化する (= 下の注入で None に倒す)。
    let mut winch_enabled = false;
    let signal_thread = if let Some(guard) = raw_guard.as_ref() {
        let guard_for_thread = std::sync::Arc::clone(guard);
        let shutdown_for_thread = std::sync::Arc::clone(&signal_shutdown);
        match install_attach_signal_thread(guard_for_thread, shutdown_for_thread, winch_notify_wr) {
            Ok(handle) => {
                winch_enabled = true;
                Some(handle)
            }
            Err(e) => {
                eprintln!(
                    "hyoui: SIGTSTP/WINCH handler 設定失敗: {e} (続行、suspend / resize 追従なし)"
                );
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

    // 子 self-stop follow 時 (= ^Z で claude/vim 等を suspend) の外側端末状態管理
    // (= issue 2026-06-11)。raw guard を保持しているとき (= stdin が tty)、`run` が
    // SIGSTOP 直前に termios を cooked へ戻し、SIGCONT 復帰後に raw 再設定するよう
    // suspend hook を渡す。reset escape の出力と daemon redraw 要求は `run` 内が担う。
    // この hook は `install_attach_signal_thread` (= client 自身が外部 SIGTSTP を
    // 受けた経路) とは別経路: こちらは「子が止まったので client も追従して止まる」経路。
    let conn = if let Some(guard) = raw_guard.as_ref() {
        let guard_suspend = std::sync::Arc::clone(guard);
        let guard_resume = std::sync::Arc::clone(guard);
        let hooks = hyoui::client::SuspendHooks::new(
            Box::new(move || {
                if let Ok(g) = guard_suspend.lock() {
                    g.suspend();
                }
            }),
            Box::new(move || {
                if let Ok(g) = guard_resume.lock() {
                    g.resume();
                }
            }),
        );
        conn.with_suspend_hooks(hooks)
    } else {
        conn
    };

    // DR-0019 §5: pipe-through stdin EOF policy を解決して配線する。
    // - 明示 `--stdin-eof=detach|send-eof` があればそれを使う
    // - 未指定なら stdin が tty でない場合 SendEof (= pipe-through の透過性回復、
    //   `echo "1+2" | hyoui run -- bc` で bc が自然 exit)、tty なら従来の Detach
    //   (tty では EOF が通常来ないので実質影響なし)
    let eof_action = match cfg.stdin_eof {
        Some(hyoui::cli::StdinEofArg::SendEof) => hyoui::client::StdinEofAction::SendEof,
        // 明示 `--stdin-eof=detach` (+ 将来 variant) は Detach。
        Some(_) => hyoui::client::StdinEofAction::Detach,
        // 未指定: 非 tty なら SendEof (= pipe-through 透過性回復)、tty なら Detach。
        None if !stdin_is_tty => hyoui::client::StdinEofAction::SendEof,
        None => hyoui::client::StdinEofAction::Detach,
    };
    let conn = conn
        .with_stdin_eof_action(eof_action)
        .with_tstp_config(app_config.attach.tstp.clone());

    // DR-0019 §6: SIGWINCH → Resize 配線。winch_source を注入し、attach 成立直後に
    // 初回 Resize を送る (= 別端末から attach した時のサイズ不一致を解消、leader 限定)。
    // signal thread 起動に失敗した場合 (= winch_enabled=false) は、誰も write しない
    // notify pipe を poll させないため winch_source を注入しない (= busy loop 回避)。
    let mut conn = match winch_source {
        Some(src) if winch_enabled => conn.with_winch_source(src),
        _ => conn,
    };
    if let Err(e) = conn.send_initial_resize() {
        eprintln!("hyoui: 初回 Resize 送信失敗: {e} (続行)");
    }

    let _ = stdout.flush();

    let client_dump = cfg
        .debug_dump_client
        .as_deref()
        .and_then(|p| open_debug_dump(p, "client"));
    let run_result = match client_dump {
        Some(dump_file) => {
            let mut tee = TeeWriter {
                primary: stdout,
                dump: dump_file,
            };
            conn.run(&mut stdin_file, &mut tee)
        }
        None => conn.run(&mut stdin_file, &mut stdout),
    };
    let exit_code = match run_result {
        // DR-0015 §2.1: session.exit.notify 受信時は exit-status をそのまま伝搬。
        Ok(RunOutcome::ChildExited { exit_status }) => {
            let masked = u8::try_from(exit_status & 0xFF).unwrap_or(255);
            ExitCode::from(masked)
        }
        // 自発 detach (= detach key / `--stdin-eof=detach` の EOF) は正常離脱。子は
        // daemon 配下に残る。スクリプトから見ても「意図通り離れた」なので exit 0。
        Ok(RunOutcome::Detached) => ExitCode::SUCCESS,
        // issue 2026-06-11 優先1: 予期しない接続喪失 (= daemon 消滅の疑い) は非 0 で
        // 終わり、stderr に 1 行出す。旧実装はこれを exit 0 にしていたため、スクリプト
        // から「子が exit 0 で正常終了した」と「daemon が落ちて attach が切れた」を
        // 区別できなかった。
        //
        // Design rationale: exit code を `EXIT_CONNECTION_LOST` (= 9) に固定する。
        // 子 PTY の exit code (= 0..=255、signal 死は 128+signum、よく見る 130=SIGINT /
        // 137=SIGKILL / 143=SIGTERM) や、hyoui の既存自己エラー慣習 (1=一般エラー /
        // 2=usage・parse error / 3=kill 系の特殊) のいずれとも重ならない値を選んだ。
        // 完全な衝突回避は exit code が u8 である以上不可能だが、9 を daemon 接続喪失
        // 専用に予約することで「子の正常 exit と取り違える」最頻ケースを排除する。
        Ok(RunOutcome::ConnectionLost) => {
            eprintln!("hyoui: daemon との接続が失われました (daemon が終了した可能性があります)");
            ExitCode::from(EXIT_CONNECTION_LOST)
        }
        // daemon が backpressure で client を切断した。daemon 自体は健在で、原因は
        // client の出力 buffer 超過 (= suspend 中の出力過多等)。exit code は
        // ConnectionLost と同じ 9 だが、原因が異なるので stderr を出し分ける。
        Ok(RunOutcome::BackpressureDisconnected) => {
            eprintln!(
                "hyoui: daemon が出力 buffer 超過で client を切断しました (suspend 中の出力過多等)"
            );
            ExitCode::from(EXIT_CONNECTION_LOST)
        }
        // 出力先 (= stdout) への書き込みに失敗した。daemon は健在で自分側の出力経路が
        // 壊れただけなので、daemon 消滅 (= exit 9) とは区別して一般エラー exit 1。
        Ok(RunOutcome::StdoutWriteFailed) => {
            eprintln!(
                "hyoui: 出力先への書き込みに失敗しました (端末 / pipe が閉じた可能性があります)"
            );
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("hyoui: attach 実行エラー: {e}");
            ExitCode::from(1)
        }
    };

    // signal_thread を畳む (= Arc 解放 → main の `raw_guard` Arc が drop されたら
    // TtyGuard::Drop が走り termios 復元される)。
    signal_shutdown.store(true, std::sync::atomic::Ordering::Release);
    if let Some(handle) = signal_thread {
        // join は signal_thread の drain ループ次の iteration で shutdown 検知し終了
        // (= 最大 50ms の sleep + drain 1 周)。
        let _ = handle.join();
    }
    drop(raw_guard); // 明示的に Arc<Mutex<TtyGuard>> を drop して termios 復元
    exit_code
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
    // DR-0018: scan 対象 dir を namespace スコープで決める。
    // - --all-namespaces → 全 namespace の (ns, dir) を列挙、NS 列を表示
    // - それ以外 → 解決した単一 namespace の dir のみ (= 従来互換)
    let dirs: Vec<(String, std::path::PathBuf)> = if cfg.all_namespaces {
        list_candidate_dirs_all_namespaces()
    } else {
        let ns = socket_path::resolve_namespace(cfg.namespace.as_deref());
        list_candidate_dirs(&ns)
            .into_iter()
            .map(|d| (ns.clone(), d))
            .collect()
    };
    list_command_with_dirs(cfg, dirs)
}

/// `hyoui list` で 1 session を表す internal 構造体。
///
/// mtime 順 sort のために `started_unix_ms` を保持する。`hyoui attach --index=N`
/// (= [`docs/issue/2026-05-30-feature-attach-index-shortcut.md`]) も本順序を前提に
/// 「1=最古 / -1=最新」と解釈する。
struct ListEntry {
    session: String,
    /// DR-0018: この entry が属する namespace (= NS 列表示用)。
    namespace: String,
    socket_path: std::path::PathBuf,
    /// socket file mtime を epoch ms に換算した値 (= sort key + jsonl 出力用)。
    started_unix_ms: u64,
    /// 起動からの経過時間 (= `now - mtime`)。stale の場合は 0 とする (= 表示時は `-`)。
    dur: std::time::Duration,
    /// daemon 状態 (= live なら status.response の field を必ず持つ、stale なら無し)。
    status: ListEntryStatus,
}

/// daemon 状態 (= `hyoui list` の 1 entry が live か stale か、live なら status.response の値)。
///
/// **設計判断**: live は `cwd` / `argv` / `clients` を必ず持つ (= v1.0 breaking OK 方針、
/// `status.response` を required field 化したので「live なのに値なし」は protocol 違反)。
/// 旧実装は live でも `Option<String>` で graceful degradation していたが、daemon が
/// 一時的に slow なだけで `cwd: -` の誤情報を出す経路ができていた (= kawaz の指摘 #2)。
enum ListEntryStatus {
    /// daemon が socket 上で応答した。`cwd` / `argv` / `clients` は status.response から。
    Live {
        cwd: String,
        argv: Vec<String>,
        clients: usize,
        /// DR-0017 §柱2: 子が stopped (= ^Z / SIGSTOP) のまま残っているか。
        /// STATUS 列を "stopped" 表示にして放置 stopped child を可観測にする。
        child_stopped: bool,
        /// 子 PTY の PID (= exited なら None)。`list` の PID 列 / jsonl 用。
        child_pid: Option<u32>,
        /// 子 PTY の pgid (= exited なら None)。jsonl 用。
        child_pgid: Option<u32>,
        /// DR-0019 Update: 現在の on-child-suspend policy (= SUSPEND 列)。
        on_child_suspend: Option<hyoui::protocol::messages::OnChildSuspendPolicy>,
        /// DR-0019 Update: daemon バイナリ version (= VERSION 列、
        /// 空文字なら旧 daemon で `-` 表示)。
        daemon_version: String,
    },
    /// socket file は存在するが connect / handshake / status.query が失敗。daemon が
    /// 既に死亡 (panic / SIGKILL) で stale socket が残留しているケース。
    Stale,
}

/// `list_command` の testable な内部実装。`(namespace, dir)` 一覧を引数で受けることで
/// env (`XDG_RUNTIME_DIR` / `TMPDIR`) 依存を切り離し、unit test 可能にする (= DR-0018)。
fn list_command_with_dirs(cfg: ListConfig, dirs: Vec<(String, std::path::PathBuf)>) -> ExitCode {
    let now = std::time::SystemTime::now();
    let mut entries: Vec<ListEntry> = Vec::new();
    let mut pruned_count = 0usize;
    for (ns, dir) in dirs {
        let read_entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // dir 不存在は無視 (= 何も daemon 起動してない可能性)
        };
        for entry in read_entries.flatten() {
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
            let (started_unix_ms, dur) = match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(mtime) => {
                    let started_ms = mtime
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let dur = now.duration_since(mtime).unwrap_or_default();
                    (started_ms, dur)
                }
                Err(_) => (0, std::time::Duration::ZERO),
            };
            // live 暫定判定。enrich で status.query が成功すれば Live に格上げ、
            // 失敗 (= connect / handshake / decode error) なら Stale のまま。
            // 初期 status は probe 結果に基づき仮置きで、後段の enrich が確定値を入れる。
            let status = if live {
                // placeholder。enrich で必ず上書きされる (= live → Live or Stale 格下げ)。
                ListEntryStatus::Live {
                    cwd: String::new(),
                    argv: Vec::new(),
                    clients: 0,
                    child_stopped: false,
                    child_pid: None,
                    child_pgid: None,
                    on_child_suspend: None,
                    daemon_version: String::new(),
                }
            } else {
                ListEntryStatus::Stale
            };
            entries.push(ListEntry {
                session,
                namespace: ns.clone(),
                socket_path: path.clone(),
                started_unix_ms,
                dur,
                status,
            });

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

    // mtime ascending (= 古い session が先頭、新しい session が末尾)。
    // attach --index=1 が最古、--index=-1 が最新を指す前提。
    entries.sort_by_key(|e| e.started_unix_ms);

    // probe で live と判定された entry に status.query を投げて Live に格上げする
    // (= cwd / argv / clients を確定値で埋める)。本物の hyoui daemon は local Unix
    // socket 経由で必ず即応答するため、ここで timeout / graceful degradation は使わず
    // blocking で query する。失敗 (= connect / handshake / decode error) は probe で
    // live と判定したのに応答できない状態 = stale 格下げ + error log (= 異常を明示)。
    // 並列化は wall-clock 短縮目的でのみ残す (= 5 session あっても接続コストは max 1 つ分)。
    enrich_entries_with_status(&mut entries);

    match cfg.format {
        ListFormat::Plain => print_list_plain(&entries, cfg.all_namespaces),
        ListFormat::Jsonl => print_list_jsonl(&entries),
        // `ListFormat` is `#[non_exhaustive]`; fall back to plain
        // for unknown future variants.
        _ => print_list_plain(&entries, cfg.all_namespaces),
    }

    let found = entries.len();
    let stale_count = entries
        .iter()
        .filter(|e| matches!(e.status, ListEntryStatus::Stale))
        .count();
    if found == 0 {
        // 0 件は stderr で明示 (script 用に stdout を汚さない)。
        // 詳細な誘導 (= 起動例 / socket dir) は冗長で「エラー?」と誤認させるため
        // 1 行のみとし、context が必要なら `hyoui list --help` を参照させる。
        eprintln!("hyoui: no sessions found");
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

/// `std::time::Duration` を human readable な短い表記に整形 (= `1h2m` / `15m` / `3d4h`)。
///
/// `hyoui list` の DUR 列で固定長を維持しやすくするため、cap は 10 chars 程度に
/// 収まる範囲で表示する。
fn fmt_dur(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// session id を最大 `max` chars に切り詰める。超過分は末尾を `…` で示す。
fn truncate_to(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// `enrich_entries_with_status` の per-entry 戻り値。`query_status_for_list` の結果を
/// `ListEntryStatus` への mutation 指示として持ち回る。
enum StatusFetchOutcome {
    /// daemon が status.response を返した。required field の cwd / argv と clients 数。
    Live {
        cwd: String,
        argv: Vec<String>,
        clients: usize,
        /// DR-0017 §柱2: 子が stopped のまま残っているか。
        child_stopped: bool,
        /// 子 PTY の PID / pgid (= exited なら None)。`list` の PID 列 / jsonl 用。
        child_pid: Option<u32>,
        child_pgid: Option<u32>,
        /// DR-0019 Update: 現在の on-child-suspend policy。
        on_child_suspend: Option<hyoui::protocol::messages::OnChildSuspendPolicy>,
        /// DR-0019 Update: daemon バイナリ version (= 空文字なら旧 daemon)。
        daemon_version: String,
    },
    /// connect / handshake / decode / I/O error。probe では live だったので「異常状態」を
    /// log に出すために理由を保持する (= silent 格下げを避ける)。
    Failed(String),
}

/// probe で live と判定された `ListEntry` に status.query を投げて cwd / argv / clients
/// を埋める。
///
/// **設計判断 (kawaz 指摘 #2 対応)**: timeout は **使わない**。本物の hyoui daemon は local
/// Unix socket 経由で必ず即応答するため、自前 timeout を埋め込むと「daemon が GC で 300ms
/// 応答しなかっただけ」で誤情報 (= `cwd: -`) を出す経路が出来てしまう。timeout したら
/// daemon-side の問題として log + stale 格下げで明示する方が筋。
///
/// 並列化は wall-clock 短縮目的で残す (= N 個の daemon を逐次 query すると N × handshake
/// RTT がかかる、並列化で max(per-query) に抑える)。各 thread は blocking で query し、
/// 完了したら mpsc で結果を回収する。
fn enrich_entries_with_status(entries: &mut [ListEntry]) {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<(usize, StatusFetchOutcome)>();
    let mut spawned = 0usize;
    for (idx, e) in entries.iter().enumerate() {
        if !matches!(e.status, ListEntryStatus::Live { .. }) {
            continue;
        }
        spawned += 1;
        let tx = tx.clone();
        let sock = e.socket_path.clone();
        std::thread::spawn(move || {
            let outcome = query_status_for_list(&sock);
            let _ = tx.send((idx, outcome));
        });
    }
    drop(tx);
    for _ in 0..spawned {
        let (idx, outcome) = match rx.recv() {
            Ok(v) => v,
            Err(_) => break,
        };
        match outcome {
            StatusFetchOutcome::Live {
                cwd,
                argv,
                clients,
                child_stopped,
                child_pid,
                child_pgid,
                on_child_suspend,
                daemon_version,
            } => {
                entries[idx].status = ListEntryStatus::Live {
                    cwd,
                    argv,
                    clients,
                    child_stopped,
                    child_pid,
                    child_pgid,
                    on_child_suspend,
                    daemon_version,
                };
            }
            StatusFetchOutcome::Failed(reason) => {
                // probe では live だったが status.query で fail = 異常状態を明示。
                // silent に Stale に落とすと「kawaz 指摘 #2」の誤情報経路に近い症状に
                // なるので、必ず stderr に出す。
                eprintln!(
                    "hyoui: warning: session {} (socket: {}) probed live but status.query failed: {reason} (格下げして stale 扱い)",
                    entries[idx].session,
                    entries[idx].socket_path.display(),
                );
                entries[idx].status = ListEntryStatus::Stale;
            }
        }
    }
}

/// 1 socket に対し `ClientConnection` 経由で status.query を投げて結果を返す。
///
/// **設計判断 (kawaz 指摘 #2 対応)**: timeout / 自前 handshake を一切使わず、`ClientConnection::
/// connect` の標準経路で blocking query する。本物の hyoui daemon は local Unix socket 経由で
/// 必ず即応答するので、ここで block しても実害なし。timeout / graceful `-` 表示 (= 旧実装)
/// は「daemon が GC で slow なだけ」を「壊れた」と誤判定する経路を作っていたので廃止。
///
/// **scope (= 対象外を明示)**: daemon が `expected_token` を持つのに caller の env
/// `HYOUI_LOCK_TOKEN` が不一致 / 未設定の場合、`ClientConnection::connect` が
/// `AuthTokenMismatch` で `Err` を返すため、本関数は `Stale` 格下げと同等の扱いになる
/// (= 実体は live、status query 不能)。MVP は token-less 既定で運用しており、token-aware
/// に拡張する場合は `ListEntryStatus::LiveUnknown` のような第3 variant を導入する必要がある
/// (= 別 task)。「live なのに `cwd: -`」を主訴とする kawaz 指摘 #2 の本筋は同一 env 下の
/// 「daemon が一時 slow なだけ」を弾くこと、これは timeout 撤廃で達成済。
fn query_status_for_list(sock: &std::path::Path) -> StatusFetchOutcome {
    let opts = AttachOptions {
        // Ro mode + MVP_CAPS (= status.query は cap 不要だが、handshake で intersect される)。
        mode: Mode::Ro,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };
    let mut conn = match ClientConnection::connect(sock, opts) {
        Ok(c) => c,
        Err(e) => return StatusFetchOutcome::Failed(format!("connect/handshake: {e}")),
    };
    if let Err(e) = conn.send_control(&ControlMessage::StatusQuery(StatusQuery {})) {
        return StatusFetchOutcome::Failed(format!("send status.query: {e}"));
    }
    // status.response を待つ。ModeChange / LeaderNotify / TailData 等の interrupt
    // message は無視して次の control を受け取る (= `hyoui status` と同 pattern)。
    loop {
        match conn.recv_control(None) {
            Ok(ControlMessage::StatusResponse(sr)) => {
                return StatusFetchOutcome::Live {
                    cwd: sr.cwd,
                    argv: sr.argv,
                    clients: sr.clients.len(),
                    child_stopped: sr.child_stopped,
                    child_pid: sr.child_pid,
                    child_pgid: sr.child_pgid,
                    on_child_suspend: sr.on_child_suspend,
                    daemon_version: sr.daemon_version,
                };
            }
            Ok(ControlMessage::ModeChange(_)) | Ok(ControlMessage::LeaderNotify(_)) => continue,
            Ok(ControlMessage::Error(e)) => {
                return StatusFetchOutcome::Failed(format!(
                    "daemon error: {:?} ({})",
                    e.code, e.message
                ));
            }
            Ok(other) => {
                return StatusFetchOutcome::Failed(format!(
                    "unexpected response kind: {:?}",
                    std::mem::discriminant(&other)
                ));
            }
            Err(e) => return StatusFetchOutcome::Failed(format!("recv: {e}")),
        }
    }
}

/// cwd を `hyoui list` 表示用に短縮する。
///
/// rule:
/// - `<...>/repos/<host>/<owner>/<repo>/<sub...>` を含む path は `<owner>/<repo>/<sub...>`
///   に短縮 (= `git-repo-management.md` の規約に沿った形)
/// - host は `github.com` 限定ではなく汎用に効かせる (= `bitbucket.org` 等の他 host も
///   同じ階層構造で運用する想定、`identifiers-*.md` 規約のサニタイズと衝突しない)
/// - rule 不一致なら `$HOME` 前カット (= `~/foo` 形式) のみ適用、それ以外は無変更
fn shorten_cwd(cwd: &str) -> String {
    if let Some((_, rest)) = cwd.split_once("/repos/") {
        // rest = "<host>/<owner>/<repo>/<sub...>" 形式を仮定。
        // host 部分だけ落として `<owner>/<repo>/<sub...>` を返す。
        if let Some((_, after_host)) = rest.split_once('/') {
            return after_host.to_string();
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = cwd.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    cwd.to_string()
}

/// argv を 1 行表示用に整形する (= shell-escape は最小、`'` を含む arg のみ quote)。
///
/// 完全な shell-escape は `--format=jsonl` 側を使ってもらう想定。plain 側は人間が読める
/// 一覧として十分な近似で OK。
fn fmt_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let needs_quote = a.is_empty()
                || a.contains(' ')
                || a.contains('\t')
                || a.contains('\n')
                || a.contains('\r')
                || a.contains('"');
            let escaped = a
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            if needs_quote {
                format!("\"{escaped}\"")
            } else {
                escaped
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Plain format (= 固定長 column) で `entries` を stdout に出力する。
///
/// header: `SESSION (20ch) STATUS (7ch) DUR (10ch) CLIENTS (8ch) CWD (32ch) ARGV (残り)`。
/// SOCKET 列は plain には出さない (= kawaz の「socket 名だけ出されても分からん」要件、
/// 機械可読が要るなら `--format=jsonl` を使う)。entries 0 件なら header も出さない。
///
/// **設計判断 (kawaz 指摘 #2 対応)**: live entry は cwd / argv / clients を **必ず**
/// concrete value で出す (= `-` 表示は stale entry に限定)。timeout で `-` 表示する
/// graceful degradation 経路は廃止 (= `enrich_entries_with_status` が timeout を
/// 持たないので、live なら必ず status.response を取れている)。
///
/// DR-0018: `show_ns = true` (= `--all-namespaces`) のとき先頭に NS 列を追加する。
/// 単一 namespace 表示 (= default) では NS 列を出さず、従来の見え方を保つ。
fn print_list_plain(entries: &[ListEntry], show_ns: bool) {
    if entries.is_empty() {
        return;
    }
    if show_ns {
        println!(
            "{:<16} {:<20} {:<7} {:<8} {:<11} {:<8} {:<10} {:<8} {:<32} ARGV",
            "NS", "SESSION", "STATUS", "PID", "SUSPEND", "VERSION", "DUR", "CLIENTS", "CWD"
        );
    } else {
        println!(
            "{:<20} {:<7} {:<8} {:<11} {:<8} {:<10} {:<8} {:<32} ARGV",
            "SESSION", "STATUS", "PID", "SUSPEND", "VERSION", "DUR", "CLIENTS", "CWD"
        );
    }
    for e in entries {
        let session = truncate_to(&e.session, 20);
        let ns_prefix = if show_ns {
            format!("{:<16} ", truncate_to(&e.namespace, 16))
        } else {
            String::new()
        };
        match &e.status {
            ListEntryStatus::Live {
                cwd,
                argv,
                clients,
                child_stopped,
                child_pid,
                on_child_suspend,
                daemon_version,
                ..
            } => {
                let dur = fmt_dur(e.dur);
                let cwd_disp = truncate_to(&shorten_cwd(cwd), 32);
                let argv_disp = if argv.is_empty() {
                    "-".to_string()
                } else {
                    fmt_argv(argv)
                };
                // DR-0017 §柱2: 子が stopped のまま残っていれば STATUS を "stopped" に。
                let status = if *child_stopped { "stopped" } else { "live" };
                let pid_disp = child_pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_string());
                // DR-0019 Update: SUSPEND 列 (= on-child-suspend policy)。
                // None (= field を送らない旧 daemon) は `-` (unknown) に倒す —
                // 既定値 notify を断定表示すると実際と逆の値を出しうる (M3)。
                let suspend = on_child_suspend.map(|p| p.as_str()).unwrap_or("-");
                // DR-0019 Update: VERSION 列。空 (= 旧 daemon) なら `-`。
                let ver = if daemon_version.is_empty() {
                    "-"
                } else {
                    daemon_version.as_str()
                };
                println!(
                    "{ns_prefix}{session:<20} {status:<7} {pid_disp:<8} {suspend:<11} {ver:<8} {dur:<10} {clients:<8} {cwd_disp:<32} {argv_disp}"
                );
            }
            ListEntryStatus::Stale => {
                // stale は cwd/argv/clients 取得不能なので `-` 統一。
                println!(
                    "{ns_prefix}{session:<20} {:<7} {:<8} {:<11} {:<8} {:<10} {:<8} {:<32} -",
                    "stale", "-", "-", "-", "-", "-", "-"
                );
            }
        }
    }
}

/// JSON Lines format で `entries` を stdout に出力する。
///
/// 1 session = 1 行の JSON object。field: `session` / `status` / `started_unix_ms` /
/// `dur_ms` / `socket` / `cwd` / `argv` / `clients`。
///
/// **設計判断 (kawaz 指摘 #2 対応)**: live entry の `cwd` / `argv` / `clients` は **必ず**
/// concrete value (= null にならない、required field)。stale entry は 3 fields とも null。
fn print_list_jsonl(entries: &[ListEntry]) {
    for e in entries {
        let obj = match &e.status {
            ListEntryStatus::Live {
                cwd,
                argv,
                clients,
                child_stopped,
                child_pid,
                child_pgid,
                on_child_suspend,
                daemon_version,
            } => serde_json::json!({
                "session": e.session,
                // DR-0018: namespace を常時出力 (= default ns は "default")。
                "namespace": e.namespace,
                // DR-0017 §柱2: stopped child は status を "stopped" にして可観測化。
                "status": if *child_stopped { "stopped" } else { "live" },
                "child_stopped": child_stopped,
                // 子の実行時状態 (= running / stopped。list の live は exited を含まない)。
                "child_state": if *child_stopped { "stopped" } else { "running" },
                "child_pid": child_pid,
                "child_pgid": child_pgid,
                // DR-0019 Update: policy + daemon version (旧 daemon = 不明 → null)。
                "on_child_suspend": on_child_suspend.map(|p| p.as_str()),
                "daemon_version": if daemon_version.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(daemon_version.clone())
                },
                "started_unix_ms": e.started_unix_ms,
                "dur_ms": e.dur.as_millis() as u64,
                "socket": e.socket_path.display().to_string(),
                "cwd": cwd,
                "argv": argv,
                "clients": clients,
            }),
            ListEntryStatus::Stale => serde_json::json!({
                "session": e.session,
                "namespace": e.namespace,
                "status": "stale",
                "child_pid": serde_json::Value::Null,
                "child_pgid": serde_json::Value::Null,
                "started_unix_ms": e.started_unix_ms,
                "dur_ms": e.dur.as_millis() as u64,
                "socket": e.socket_path.display().to_string(),
                "cwd": serde_json::Value::Null,
                "argv": serde_json::Value::Null,
                "clients": serde_json::Value::Null,
            }),
        };
        println!("{obj}");
    }
}

/// hyoui base socket dir 候補 (= namespace を含めない `hyoui` まで) を返す。
///
/// resolver と同じ優先順位 (`XDG_RUNTIME_DIR` → XDG state fallback) で実在する
/// dir だけを列挙する。起動 path と `hyoui list` の探索 path は必ず同期させる。
fn base_socket_dirs() -> Vec<std::path::PathBuf> {
    socket_path::existing_base_dirs()
}

/// `hyoui list` 等で scan する候補 dir を **namespace スコープ**で返す (= DR-0018)。
///
/// - `default` namespace → base dir をそのまま (= 互換、`<base>/*.sock` を直接 scan)
/// - それ以外 → `<base>/<namespace>` (= 実在する場合のみ)
fn list_candidate_dirs(namespace: &str) -> Vec<std::path::PathBuf> {
    let bases = base_socket_dirs();
    if namespace == hyoui::cli::DEFAULT_NAMESPACE {
        return bases;
    }
    bases
        .into_iter()
        .map(|b| b.join(namespace))
        .filter(|p| p.is_dir())
        .collect()
}

/// 全 namespace 横断で scan する候補 dir を `(namespace, dir)` ペアで返す (= DR-0018)。
///
/// 各 base dir 直下を 1 段 read_dir し、`*.sock` (= default ns の socket) と
/// サブ dir (= 非 default ns) を区別して列挙する:
/// - base dir 自身 → namespace = `default`
/// - base dir 配下の各サブ dir `<ns>` → namespace = `<ns>`
fn list_candidate_dirs_all_namespaces() -> Vec<(String, std::path::PathBuf)> {
    let mut out: Vec<(String, std::path::PathBuf)> = Vec::new();
    for base in base_socket_dirs() {
        // base dir 自身 = default namespace。
        out.push((hyoui::cli::DEFAULT_NAMESPACE.to_string(), base.clone()));
        // base 配下のサブ dir = 各 namespace。
        let read = match std::fs::read_dir(&base) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(ns) = path.file_name().and_then(|s| s.to_str()) {
                // namespace 名として妥当なものだけ採用 (= 不正名の dir は無視)。
                if hyoui::cli::validate_namespace(ns).is_ok() {
                    out.push((ns.to_string(), path));
                }
            }
        }
    }
    out
}

/// 全 live session の id を mtime 昇順で列挙する (= `--all` 用)。
///
/// `resolve_session_by_index` と同じ scan logic だが index 解決ではなく全件返す。
fn list_all_live_sessions(namespace: &str) -> Vec<String> {
    let dirs = list_candidate_dirs(namespace);
    let mut entries: Vec<(String, std::time::SystemTime)> = Vec::new();
    for dir in dirs {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("sock") {
                continue;
            }
            if !probe_socket_liveness(&path) {
                continue;
            }
            let session = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.push((session, mtime));
        }
    }
    entries.sort_by_key(|e| e.1);
    entries.into_iter().map(|(s, _)| s).collect()
}

/// `hyoui kill <session>` の主要ロジック。
fn kill_command(cfg: KillConfig) -> ExitCode {
    // DR-0018: namespace を解決。--all の列挙と各 session の socket 解決を同一
    // namespace スコープに揃える。
    let namespace = socket_path::resolve_namespace(cfg.namespace.as_deref());
    // --all は全 live session を順次 kill (= killall 相当)。
    if cfg.all {
        let sessions = list_all_live_sessions(&namespace);
        if sessions.is_empty() {
            eprintln!("hyoui: kill --all: no live sessions found");
            return ExitCode::SUCCESS;
        }
        let mut failures = 0usize;
        let total = sessions.len();
        for sid in sessions {
            let sub_cfg = KillConfig {
                socket: None,
                session_id: Some(sid.clone()),
                signal: cfg.signal.clone(),
                index: None,
                all: false,
                // --all + --no-terminate は parse 段で reject 済 (= ここは常に false)。
                no_terminate: false,
                // --wait は各 session の terminate 完了を順に見届ける (= killall で
                // 1 件ずつ確実に畳んでから次へ)。
                wait: cfg.wait,
                wait_timeout_ms: cfg.wait_timeout_ms,
                kill_on_timeout: cfg.kill_on_timeout,
                namespace: cfg.namespace.clone(),
            };
            let exit = kill_command_single(sub_cfg);
            if exit != ExitCode::SUCCESS {
                failures += 1;
            }
        }
        if failures > 0 {
            eprintln!("hyoui: kill --all: {failures}/{total} session(s) failed");
            return ExitCode::from(1);
        }
        eprintln!("hyoui: kill --all: {total}/{total} session(s) terminated");
        return ExitCode::SUCCESS;
    }

    kill_command_single(cfg)
}

/// 単一 session の kill 実行 (= `--all` で 1 件ずつ呼び出すための内部 helper)。
fn kill_command_single(cfg: KillConfig) -> ExitCode {
    let namespace = socket_path::resolve_namespace(cfg.namespace.as_deref());
    let sock = if let Some(p) = cfg.socket.clone() {
        std::path::PathBuf::from(p)
    } else {
        // index 指定なら resolve_session_by_index で session-id を確定、
        // それ以外は cfg.session_id を使う (= parse_kill で同時指定は弾かれている)。
        let sid_owned: String;
        let sid = if let Some(index) = cfg.index {
            match resolve_session_by_index(index, &namespace) {
                Ok(s) => {
                    sid_owned = s;
                    sid_owned.as_str()
                }
                Err(e) => {
                    eprintln!("hyoui: kill: {e}");
                    return ExitCode::from(1);
                }
            }
        } else {
            match cfg.session_id.as_deref() {
                // 明示 session id (= 外から使う既存挙動、env より優先、DR-0020 §2)。
                Some(s) => s,
                // 省略時: $HYOUI_SESSION_ID を解決 (DR-0020 §2)。kill の self
                // default は許容 (= `exit` 相当の意図的用途、DR-0020 §3)。
                None => match resolve_session_from_env("kill", &namespace) {
                    Ok(Some(s)) => {
                        sid_owned = s;
                        sid_owned.as_str()
                    }
                    Ok(None) => {
                        print_session_required("kill");
                        return ExitCode::from(2);
                    }
                    Err(code) => return code,
                },
            }
        };
        match socket_path::resolve_in_namespace(None, sid, &namespace) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "hyoui: kill: socket path 解決失敗: {e} (session: {sid}, namespace: {namespace})"
                );
                eprintln!("       起動中の session 一覧は `hyoui list` で確認してください。");
                return ExitCode::from(1);
            }
        }
    };

    let opts = AttachOptions {
        // kill は daemon 側 `handle_kill` で `ensure_rw_mode` 必須 (= Round2 #6 で
        // 厳格化、`!Ro` → `Rw` のみに)。Ro で attach すると daemon が黙って
        // ErrorMessage 返して session terminate しない (= 旧実装の regression、
        // 「送信完了」表示で exit 0 してた致命 bug)。`hyoui kill` は session
        // terminate 操作なので Rw + detach_others で leader 確保が筋。
        mode: Mode::Rw,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        // kill は破壊操作なので既存 leader を蹴ってでも実行する (= UX 上「kill が
        // leader busy で失敗」より「kill は確実に効く」が期待される)。
        detach_others: true,
    };

    // R5-FB4: socket 不存在系 errno は短時間 retry。
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("kill", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // DR-0017 §柱2: `--no-terminate` 指定時は session を畳まず signal だけ送る
    // (= `ControlMessage::Signal` 経路、stopped child を CONT で起こす用途等)。
    if cfg.no_terminate {
        return signal_no_terminate(conn, &cfg, &sock);
    }

    // DR-0012: wire は signal name string。CLI 段で正規表記 (SIG-prefix 大文字)
    // を強制済 (= cli.rs::parse_kill の `--signal` validate)。
    //
    // `--wait` でも **`wait: false` (= 即時 ack mode) を送る**。daemon は signal
    // 送信 + KillAck 返送だけして serve を継続し、「見届け」は client 側が担う:
    //
    // - 子が死ねば daemon は PTY-EOF → ChildExited 経路で `SessionExitNotify` を
    //   broadcast してから session を畳む → client はそれを成功 (= 見届け完了) と判定
    // - deadline 超過なら client が timeout 判定。**session は無傷で残る** (= daemon
    //   は kill を「送った」以外何も変わっていない)。`--kill-on-timeout` なら
    //   SIGKILL の Kill をもう 1 発送って見届けを継続する
    //
    // daemon 側に blocking 見届けが無いので、TERM を ignore する子を `--wait` で
    // 撃っても daemon が無限 waitpid で孤児化しない (= 2026-06-11 の孤児 daemon の
    // 根源対策)。protocol 変更も不要 (= 既存 Kill{wait:false} + SessionExitNotify
    // の組合せだけで成立)。
    let kill = hyoui::protocol::messages::Kill {
        signal: cfg.signal.clone(),
        wait: false,
    };
    if let Err(e) = conn.send_control(&hyoui::protocol::ControlMessage::Kill(kill)) {
        eprintln!("hyoui: kill: send 失敗: {e}");
        return ExitCode::from(1);
    }

    if !cfg.wait {
        // [default = 即時応答] KillAck (新 daemon) or EOF (旧 daemon 互換) で成功。
        // 失敗 path: ensure_rw_mode 失敗 / invalid signal name 等で daemon が
        // `ControlMessage::Error` を返す。LeaderNotify / ModeChange 等の broadcast
        // は skip。
        loop {
            match conn.recv_control(None) {
                Ok(hyoui::protocol::ControlMessage::Error(err)) => {
                    eprintln!(
                        "hyoui: kill: daemon rejected ({:?}): {}",
                        err.code, err.message
                    );
                    return ExitCode::from(1);
                }
                Ok(hyoui::protocol::ControlMessage::KillAck(_)) => break,
                Ok(_) => continue,
                // EOF (= 旧 daemon が session terminate して socket close)。成功扱い。
                Err(_) => break,
            }
        }
        drop(conn);
        println!("hyoui: kill 送信完了: {}", sock.display());
        return ExitCode::SUCCESS;
    }

    // [--wait] 子 exit 見届け mode。timeout は parse 段で必ず Some (= 裸 --wait は
    // default 10s)。通算 deadline で管理し、broadcast 受信で wait が延びないように
    // 毎 iteration 残り時間を read timeout に設定し直す。
    let timeout_ms = cfg
        .wait_timeout_ms
        .unwrap_or(hyoui::cli::KILL_WAIT_DEFAULT_TIMEOUT_MS);
    // SIGKILL 昇格後の見届け上限。SIGKILL は catch / ignore 不能なので、子が
    // D-state (= uninterruptible sleep) 等の異常でない限りここに届く前に死ぬ。
    const ESCALATE_REAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
    let mut deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut escalated = false;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            if cfg.kill_on_timeout && !escalated {
                // SIGKILL 昇格: もう 1 発 Kill (wait:false) を送って見届けを継続。
                let kill9 = hyoui::protocol::messages::Kill {
                    signal: Some("SIGKILL".to_string()),
                    wait: false,
                };
                if let Err(e) = conn.send_control(&hyoui::protocol::ControlMessage::Kill(kill9)) {
                    eprintln!("hyoui: kill --kill-on-timeout: SIGKILL 送信失敗: {e}");
                    return ExitCode::from(1);
                }
                eprintln!(
                    "hyoui: kill --wait: timeout ({timeout_ms}ms)、SIGKILL に昇格して見届けます"
                );
                escalated = true;
                deadline = std::time::Instant::now() + ESCALATE_REAP_BUDGET;
                continue;
            }
            if escalated {
                // SIGKILL 後も exit を確認できない = D-state 等の異常。
                eprintln!(
                    "hyoui: kill --wait: SIGKILL 昇格後も子の終了を確認できませんでした (= uninterruptible sleep 等の異常の可能性): {}",
                    sock.display()
                );
                return ExitCode::from(1);
            }
            eprintln!(
                "hyoui: kill --wait: timeout ({timeout_ms}ms 以内に子が終了しませんでした)。子と session はそのまま残っています。確実に終わらせるには --kill-on-timeout を付けて再実行してください: {}",
                sock.display()
            );
            return ExitCode::from(3);
        }
        // 残り時間だけ recv を block する (= 0 は「無限」を意味するので 1ms に clamp)。
        let to = remaining.max(std::time::Duration::from_millis(1));
        if let Err(e) = conn.set_read_timeout(Some(to)) {
            eprintln!("hyoui: kill: set_read_timeout 失敗: {e}");
            return ExitCode::from(1);
        }
        match conn.recv_control(None) {
            Ok(hyoui::protocol::ControlMessage::Error(err)) => {
                eprintln!(
                    "hyoui: kill: daemon rejected ({:?}): {}",
                    err.code, err.message
                );
                return ExitCode::from(1);
            }
            // 子 exit の確定通知 (= ChildExited 経路の broadcast)。見届け完了。
            Ok(hyoui::protocol::ControlMessage::SessionExitNotify(_)) => break,
            // KillAck / LeaderNotify / ModeChange 等は skip して次 frame。
            Ok(_) => continue,
            Err(e) => {
                if is_timeout_error(&e) {
                    // read timeout → loop 先頭で deadline 判定 (= escalation / exit 3)。
                    continue;
                }
                // EOF。`wait: false` kill では daemon は子が死んだ時しか session を
                // 畳まないので、notify を取りこぼした EOF も「子 exit」と判定して
                // 成功扱い (= 旧 daemon 互換も同じ理由で成功)。
                break;
            }
        }
    }
    drop(conn);
    println!(
        "hyoui: kill 完了 (子 exit + session 終了を見届け): {}",
        sock.display()
    );
    ExitCode::SUCCESS
}

/// `recv_control` の `Error` が read timeout 由来 (= WouldBlock / TimedOut) か。
///
/// `--wait` の client 側 deadline 超過を EOF (= daemon 都合の socket close) と
/// 区別するために使う。
fn is_timeout_error(e: &hyoui::Error) -> bool {
    match e {
        hyoui::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        // EAGAIN == EWOULDBLOCK な platform もある (= macOS) ため EAGAIN のみ判定
        // (= 両方 or で書くと unreachable pattern。別値 platform でも socket recv
        // timeout は EAGAIN で来る)。
        hyoui::Error::Errno(errno) => *errno == nix::errno::Errno::EAGAIN,
        _ => false,
    }
}

/// DR-0017 §柱2: `hyoui kill <session> --signal=<SIG> --no-terminate` の実体。
///
/// session を畳まず、子 PTY に signal だけ送る (= `ControlMessage::Signal`、
/// daemon 側 `handle_signal` は非 terminate)。主用途は **stopped child を CONT で
/// 起こす** (= auto-resume 廃止後の外側 resume API、DR-0017 §柱2)。
///
/// `handle_signal` は成功時に ack を返さない (= `Continue`)。よって短い read
/// timeout を被せ、その間に `Error` frame が来なければ成功と判定する。
fn signal_no_terminate(
    mut conn: hyoui::client::ClientConnection,
    cfg: &KillConfig,
    sock: &std::path::Path,
) -> ExitCode {
    // 非 terminate な signal 送信は signal 名が必須 (= 送るべき signal が無いと無意味)。
    // 既定 (= `--signal` 未指定) は SIGTERM だが、session を畳まず子に SIGTERM を
    // 送る用途は稀。`--signal` 明示を促しつつ、未指定なら SIGTERM で送る。
    let sig = cfg.signal.clone().unwrap_or_else(|| "SIGTERM".to_string());
    let msg = hyoui::protocol::ControlMessage::Signal(hyoui::protocol::messages::Signal {
        signal: sig.clone(),
    });
    if let Err(e) = conn.send_control(&msg) {
        eprintln!("hyoui: kill --no-terminate: send 失敗: {e}");
        return ExitCode::from(1);
    }

    // 成功時 ack は来ないので、短時間だけ Error frame を待つ。timeout = 成功。
    if let Err(e) = conn.set_read_timeout(Some(std::time::Duration::from_millis(300))) {
        eprintln!("hyoui: kill --no-terminate: set_read_timeout 失敗: {e}");
        return ExitCode::from(1);
    }
    match conn.recv_control(None) {
        Ok(hyoui::protocol::ControlMessage::Error(err)) => {
            eprintln!(
                "hyoui: kill --no-terminate: daemon rejected ({:?}): {}",
                err.code, err.message
            );
            ExitCode::from(1)
        }
        // broadcast (LeaderNotify / ModeChange 等) が来たら無視して成功扱い
        // (= Error でなければ signal は受理されている)。
        Ok(_) => {
            drop(conn);
            println!(
                "hyoui: signal {sig} 送信完了 (session 継続): {}",
                sock.display()
            );
            ExitCode::SUCCESS
        }
        // timeout (= WouldBlock / TimedOut) or EOF。Error が来なかった = 成功。
        Err(_) => {
            drop(conn);
            println!(
                "hyoui: signal {sig} 送信完了 (session 継続): {}",
                sock.display()
            );
            ExitCode::SUCCESS
        }
    }
}

/// session_id / socket / index オプションから target socket path を resolve するヘルパ。
///
/// 優先順:
/// 1. `socket` が `Some` → そのまま PathBuf
/// 2. `index` が `Some` → `resolve_session_by_index` で session-id を確定 → socket_path::resolve
/// 3. `session_id` が `Some` → socket_path::resolve
/// 4. 全て None → `print_session_required` で error
///
/// session selector の共通化 (= kawaz 方針 2026-05-30) のため `index` 引数を取る。
/// status / tail / wait / screen / lock 系の全 caller で同一の選択ロジックを共有する。
fn resolve_target_socket(
    cmd: &str,
    socket: Option<&str>,
    session_id: Option<&str>,
    index: Option<i32>,
    namespace: &str,
) -> Result<std::path::PathBuf, ExitCode> {
    if let Some(p) = socket {
        return Ok(std::path::PathBuf::from(p));
    }
    let sid_owned: String;
    let sid: &str = if let Some(idx) = index {
        // 明示 index 解決 (= 外から使う既存挙動、env より優先、DR-0020 §2)。
        match resolve_session_by_index(idx, namespace) {
            Ok(s) => {
                sid_owned = s;
                sid_owned.as_str()
            }
            Err(e) => {
                eprintln!("hyoui: {cmd}: {e}");
                return Err(ExitCode::from(1));
            }
        }
    } else {
        match session_id {
            // 明示 session id (= 外から使う既存挙動、env より優先、DR-0020 §2)。
            Some(s) => s,
            // 省略時: $HYOUI_SESSION_ID (= 中から実行) を解決 (DR-0020 §2)。
            None => match resolve_session_from_env(cmd, namespace)? {
                Some(s) => {
                    sid_owned = s;
                    sid_owned.as_str()
                }
                None => {
                    print_session_required(cmd);
                    return Err(ExitCode::from(2));
                }
            },
        }
    };
    socket_path::resolve_in_namespace(None, sid, namespace).map_err(|e| {
        eprintln!(
            "hyoui: {cmd}: socket path 解決失敗: {e} (session: {sid}, namespace: {namespace})"
        );
        eprintln!("       起動中の session 一覧は `hyoui list` で確認してください。");
        ExitCode::from(1)
    })
}

/// `$HYOUI_SESSION_ID` 由来の session 省略時解決 (DR-0020 §2)。
///
/// daemon が子プロセスへ常時注入する `HYOUI_SESSION_ID` (DR-0020 §1) を読み、
/// 「中から session を省略実行した」ケースを自セッションに解決する。
///
/// 戻り値:
/// - `Ok(Some(sid))`: env が set + 非空 + その session の socket が live。
/// - `Ok(None)`: env が unset / 空 (= 中から実行ではない)。caller は既存 fallback
///   (= `print_session_required` 等) に進む。
/// - `Err(ExitCode)`: env は set だが socket が存在しない (= stale env)。既存
///   fallback に **落とさず明示エラー** にする (= 「自分を指したつもりが別 session」
///   の誤爆防止、DR-0020 §2)。
fn resolve_session_from_env(cmd: &str, namespace: &str) -> Result<Option<String>, ExitCode> {
    let sid = match std::env::var("HYOUI_SESSION_ID") {
        Ok(v) if !v.is_empty() => v,
        // unset / 空 → 中から実行ではない。caller の既存 fallback に委ねる。
        _ => return Ok(None),
    };
    // stale env 検出のため socket path を解決して liveness を確認する。
    let sock = match socket_path::resolve_in_namespace(None, &sid, namespace) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "hyoui: {cmd}: $HYOUI_SESSION_ID={sid:?} の socket path 解決失敗: {e} \
                 (namespace: {namespace})"
            );
            return Err(ExitCode::from(1));
        }
    };
    if !probe_socket_liveness(&sock) {
        // stale env: env は set だが session が居ない。既存 fallback に落とすと
        // 「自分を指したつもりが別 session」になり得るので明示エラー (DR-0020 §2)。
        eprintln!(
            "hyoui: {cmd}: $HYOUI_SESSION_ID={sid:?} が指す session が見つかりません \
             (= stale env、daemon が既に終了した可能性)。"
        );
        eprintln!(
            "       明示的に session を指定するか、`hyoui list` で起動中の session を確認してください。"
        );
        return Err(ExitCode::from(1));
    }
    Ok(Some(sid))
}

/// attach 対象 (`sid` + `namespace`) が自セッションかを判定する (DR-0020 §3)。
///
/// daemon が子へ注入する `$HYOUI_SESSION_ID` (DR-0020 §1) + `$HYOUI_NAMESPACE`
/// (DR-0018) と照合する。両方が一致したときだけ self (= ネスト) と判定する。
/// 子が別 namespace を明示して attach する場合は self ではない (= namespace まで
/// 突き合わせる)。env が未 set (= 外から実行) なら常に false。
fn attach_target_is_self(sid: &str, namespace: &str) -> bool {
    let env_sid = match std::env::var("HYOUI_SESSION_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    if env_sid != sid {
        return false;
    }
    // namespace も一致して初めて self。子の namespace は resolve_namespace と同じ
    // 解決順 (= HYOUI_NAMESPACE env > default) で引く。
    let env_ns = socket_path::resolve_namespace(None);
    env_ns == namespace
}

/// `--socket` 明示 path が自セッションの socket を指しているかを判定する
/// (DR-0020 §3、codex review 2026-06-12)。
///
/// `$HYOUI_SESSION_ID` + 子の namespace (= `$HYOUI_NAMESPACE` 解決順) から自セッション
/// の socket path を解決し、与えられた path と canonical 比較する。
///
/// **best-effort の UX ガード** であってセキュリティ境界ではない: symlink farm /
/// bind mount / hardlink 等での path 偽装までは追わない (= `canonicalize` が解決できる
/// 範囲のみ)。canonicalize 失敗時 (= path 不存在等) は素の path 文字列比較に
/// fallback する。env 未 set (= 外から実行) なら常に false。
fn socket_path_is_self(p: &std::path::Path) -> bool {
    let env_sid = match std::env::var("HYOUI_SESSION_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    let env_ns = socket_path::resolve_namespace(None);
    let self_sock = match socket_path::resolve_in_namespace(None, &env_sid, &env_ns) {
        Ok(s) => s,
        // 解決失敗 (= sid 不正等) は self 判定不能 → ガードしない (= 過剰 block 回避)。
        Err(_) => return false,
    };
    // canonicalize で symlink / `..` を解決して比較。失敗したら素の path で比較。
    let canon = |path: &std::path::Path| -> std::path::PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    };
    canon(p) == canon(&self_sock)
}

/// `detach [session]` subcommand (DR-0020 §4)。常に all (= 全 attach client を切断)。
///
/// 指定 session に一時接続して `Detach{All}` を送り、全 attach client を引き剥がす。
/// daemon と子 PTY は影響を受けず継続する (= DR-0015 §2.3.1)。session 省略時は
/// `$HYOUI_SESSION_ID` で自セッションに解決 (= self default 許容、TUI 脱出用途)。
/// target 指定は持たない (= `DetachConfig` doc 参照、Fable review M1 2026-06-12)。
/// `hyoui web` subcommand (= DR-0027 Phase 1)。
///
/// tokio multi-thread runtime を新規に起こし、`hyoui_web::serve` を回す。
/// listen アドレスの解決順は CLI flag `--listen` > config.toml `[web].listen` >
/// hardcoded default (`127.0.0.1:43690` = 0xAAAA)。
fn web_command(cfg: hyoui::cli::WebConfig) -> ExitCode {
    let config = match hyoui::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: web: config 読み込み失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let listen = cfg.listen.unwrap_or_else(|| config.web.listen.clone());
    // assets_dir: CLI flag > config `[web].assets_dir` > None (embedded)。
    let assets_dir = cfg
        .assets_dir
        .clone()
        .or_else(|| config.web.assets_dir.clone());
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("hyoui: web: tokio runtime 構築失敗: {e}");
            return ExitCode::from(1);
        }
    };
    let result = runtime.block_on(hyoui_web::serve(&listen, config, assets_dir));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hyoui: web: serve 失敗 (listen={listen}): {e}");
            ExitCode::from(1)
        }
    }
}

fn detach_command(cfg: hyoui::cli::DetachConfig) -> ExitCode {
    use hyoui::protocol::messages::{Detach, DetachTarget};

    let namespace = socket_path::resolve_namespace(cfg.namespace.as_deref());
    // session 解決: 明示 socket > 明示 session/index > $HYOUI_SESSION_ID (= 中から)。
    // detach の self default は許容 (= 中から TUI 脱出)。stale env は明示エラー。
    let sock = match resolve_target_socket(
        "detach",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        &namespace,
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // detach は他 client を引き剥がす操作。leader は取らない (= RwNoLeader)。
    // detach CLI 自身が leader を奪うと cascade が歪むため。
    let opts = AttachOptions {
        mode: Mode::RwNoLeader,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        exclusive: false,
        detach_others: false,
    };

    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("detach", &sock, &e);
            return ExitCode::from(1);
        }
    };

    if let Err(e) = conn.send_control(&ControlMessage::Detach(Detach {
        target: DetachTarget::All,
    })) {
        eprintln!("hyoui: detach: send 失敗: {e}");
        return ExitCode::from(1);
    }

    // DR-0020 §4 / Fable Minor3: daemon は drop 実行前に DetachAck{dropped_count} を
    // 返す (= socket buffer に乗ってから自分も drop されるので All でも読める)。
    // ack を short deadline で待って成功報告を出す。0 件でも exit 0 (= 冪等)。
    // ack が届かない場合 (= 旧 daemon / 途中切断) も要求は送信済なので成功扱い。
    let deadline = std::time::Duration::from_secs(2);
    while matches!(poll_recv_ready(&conn, deadline), Ok(true)) {
        match conn.recv_control(None) {
            Ok(ControlMessage::DetachAck(ack)) => {
                println!("{} client(s) detached", ack.dropped_count);
                break;
            }
            Ok(ControlMessage::Error(e)) => {
                eprintln!(
                    "hyoui: detach: daemon rejected ({:?}): {}",
                    e.code, e.message
                );
                return ExitCode::from(1);
            }
            // broadcast (LeaderNotify / ModeChange 等) は読み飛ばして次 frame へ。
            Ok(_) => continue,
            // EOF / decode error = 自分が drop された (= 処理完了)。ack を
            // 取りこぼしても成功扱い。
            Err(_) => break,
        }
    }
    ExitCode::SUCCESS
}

/// `status` subcommand: connect → handshake → status.query → print response。
fn status_command(cfg: StatusConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "status",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
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

/// `set` subcommand: connect (rw) → handshake → cap check (`set-v1`) → set.request →
/// set.ack 受信 (DR-0019 Update)。
///
/// exit code: 0=成功 / 1=connect or daemon reject / 2=引数不正 (parse 段で済) /
/// 4=daemon が `set-v1` 未対応 (= 古い daemon)。
fn set_command(cfg: hyoui::cli::SetConfig) -> ExitCode {
    use hyoui::protocol::messages::{SetAck, SetRequest};

    let sock = match resolve_target_socket(
        "set",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };
    // set は書き込み操作なので Rw で接続する (= leader は取らなくてよい、kill と同じ)。
    let opts = AttachOptions {
        mode: Mode::Rw,
        ..AttachOptions::default()
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("set", &sock, &e);
            return ExitCode::from(1);
        }
    };
    // cap negotiation: daemon が `set-v1` を advertise していなければ set 不可。
    // 新 client → 旧 daemon のケースを明示エラーにする (= silent no-op を避ける)。
    if !conn.response.caps.iter().any(|c| c == "set-v1") {
        eprintln!(
            "hyoui: set: この daemon は `set-v1` をサポートしていません (= 古い hyoui で起動された session)。\n       \
             新しい hyoui で session を起動し直すと set が使えます。"
        );
        return ExitCode::from(4);
    }
    let req = SetRequest {
        key: cfg.key.clone(),
        value: cfg.value.clone(),
    };
    if let Err(e) = conn.send_control(&ControlMessage::SetRequest(req)) {
        eprintln!("hyoui: set: send 失敗: {e}");
        return ExitCode::from(1);
    }
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: set: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::SetAck(SetAck { key, value }) => {
                println!("set ok: {key}={value}");
                return ExitCode::SUCCESS;
            }
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: set: daemon が reject しました: {:?} ({})",
                    e.code, e.message
                );
                return ExitCode::from(1);
            }
            // broadcast 系 (mode.change / leader.notify) は無視して ack を待つ。
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: set: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}

fn print_status_plain(sr: &hyoui::protocol::messages::StatusResponse) {
    use hyoui::protocol::messages::ChildLiveState;
    println!("session-id: {}", sr.session_id);
    println!("daemon-pid: {}", sr.daemon_pid);
    // child_state を正本に表示する (= child_pid / child_stopped を包含)。
    match ChildLiveState::from_legacy(sr.child_pid, sr.child_stopped) {
        ChildLiveState::Running | ChildLiveState::Stopped => {
            let pid = sr.child_pid.map(|p| p.to_string()).unwrap_or_default();
            let pgid = sr
                .child_pgid
                .map(|p| format!(" pgid={p}"))
                .unwrap_or_default();
            println!("child-pid: {pid}{pgid}");
            let st = if matches!(
                ChildLiveState::from_legacy(sr.child_pid, sr.child_stopped),
                ChildLiveState::Stopped
            ) {
                "stopped"
            } else {
                "running"
            };
            println!("child-state: {st}");
        }
        ChildLiveState::Exited { code } => {
            println!("child-pid: (exited)");
            match code {
                Some(c) => println!("child-state: exited (code {c})"),
                None => println!("child-state: exited"),
            }
        }
    }
    // DR-0019 Update: on-child-suspend policy (= `hyoui set` で変更可) を可視化。
    // None (= field を送らない旧 daemon) は `-` (unknown) — 既定値を断定表示しない。
    println!(
        "on-child-suspend: {}",
        sr.on_child_suspend.map(|p| p.as_str()).unwrap_or("-")
    );
    // DR-0019 Update: daemon バイナリ version。field 無しの旧 daemon は
    // 空文字なので `-` 表示に倒す (= それ自体が「古い daemon」のシグナル)。
    let ver = if sr.daemon_version.is_empty() {
        "-"
    } else {
        sr.daemon_version.as_str()
    };
    println!("daemon-version: {ver}");
    println!("scrollback-bytes: {}", sr.scrollback_bytes);
    if let Some(holder) = sr.lock_holder {
        println!("lock-holder: client {holder}");
    } else {
        println!("lock-holder: (none)");
    }
    println!("clients:");
    let now = hyoui::sys::clock::now_unix_ms();
    for c in &sr.clients {
        let leader = if c.leader { " leader" } else { "" };
        // DR-0020 §5: 接続時刻を「接続からの経過時間」で表示する (= どの端末が
        // 古株か把握しやすい)。connected_at が 0 (= field 無しの旧 daemon) は不明。
        let conn = if c.connected_at_unix_ms == 0 {
            "connected=?".to_string()
        } else {
            let elapsed = now.saturating_sub(c.connected_at_unix_ms);
            format!(
                "connected={} ago",
                fmt_dur(std::time::Duration::from_millis(elapsed))
            )
        };
        println!("  - id={} mode={:?}{leader} {conn}", c.client_id, c.mode);
    }
}

/// H5: scripting / jq 用に 1 行 JSON object で StatusResponse を出力。
/// 依存なしで手書き (= serde_json を入れるよりも軽い、status はフィールド限定)。
fn print_status_json(sr: &hyoui::protocol::messages::StatusResponse) {
    use std::fmt::Write as _;
    let mut out = String::new();
    out.push('{');
    write!(&mut out, "\"session_id\":{}", json_string(&sr.session_id)).ok();
    write!(&mut out, ",\"daemon_pid\":{}", sr.daemon_pid).ok();
    match sr.child_pid {
        Some(pid) => write!(&mut out, ",\"child_pid\":{pid}").ok(),
        None => write!(&mut out, ",\"child_pid\":null").ok(),
    };
    match sr.child_pgid {
        Some(pgid) => write!(&mut out, ",\"child_pgid\":{pgid}").ok(),
        None => write!(&mut out, ",\"child_pgid\":null").ok(),
    };
    // DR-0017 §柱2: stopped child の可観測性 (= jq で `.child_stopped` を拾える)。
    write!(&mut out, ",\"child_stopped\":{}", sr.child_stopped).ok();
    // child_state: running / stopped / exited(code) の正本 (= 旧 2 field を包含)。
    {
        use hyoui::protocol::messages::ChildLiveState;
        let cs = match ChildLiveState::from_legacy(sr.child_pid, sr.child_stopped) {
            ChildLiveState::Running => "running".to_string(),
            ChildLiveState::Stopped => "stopped".to_string(),
            ChildLiveState::Exited { code } => match code {
                Some(c) => format!("exited:{c}"),
                None => "exited".to_string(),
            },
        };
        write!(&mut out, ",\"child_state\":{}", json_string(&cs)).ok();
    }
    // DR-0019 Update: policy + daemon version。旧 daemon (= field 無し) は null —
    // 既定値を断定表示しない (jq 側で null = unknown を判別できる)。
    match sr.on_child_suspend {
        Some(p) => write!(
            &mut out,
            ",\"on_child_suspend\":{}",
            json_string(p.as_str())
        )
        .ok(),
        None => write!(&mut out, ",\"on_child_suspend\":null").ok(),
    };
    if sr.daemon_version.is_empty() {
        out.push_str(",\"daemon_version\":null");
    } else {
        write!(
            &mut out,
            ",\"daemon_version\":{}",
            json_string(&sr.daemon_version)
        )
        .ok();
    }
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
            "{{\"client_id\":{},\"mode\":\"{}\",\"leader\":{},\"connected_at_unix_ms\":{}}}",
            c.client_id, mode_str, c.leader, c.connected_at_unix_ms
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
    let sock = match resolve_target_socket(
        "tail",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
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
    let sock = match resolve_target_socket(
        "wait",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
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
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
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
/// `--format=json` は CLI 段で `StateSnapshotResponse` を `serde_json` で
/// human-readable JSON に変換して出力する (= daemon は CBOR が正本、JSON は
/// CLI 段変換)。`cells` / `scrollback` の bytes は serde default で number
/// array に展開される (= 量が増えるので `--include` で skip 推奨)。
fn screen_snapshot_command(cfg: ScreenSnapshotConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "screen snapshot",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
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
                // ControlMessage 全体ではなく中身の `StateSnapshotResponse` を独立 root と
                // して出力 (= 各 component の値が partial で入っているので、構造ごと別 tool
                // に渡せる形が便利)。format 分岐は CLI 段で行う (= daemon は CBOR が正本)。
                let payload = match cfg.format {
                    hyoui::cli::ScreenSnapshotCliFormat::Cbor => {
                        let mut buf = Vec::new();
                        if let Err(e) = ciborium::ser::into_writer(&resp, &mut buf) {
                            eprintln!(
                                "hyoui: screen snapshot: response の CBOR 再 encode 失敗: {e}"
                            );
                            return ExitCode::from(1);
                        }
                        buf
                    }
                    hyoui::cli::ScreenSnapshotCliFormat::Json => {
                        match serde_json::to_string_pretty(&resp) {
                            Ok(mut s) => {
                                s.push('\n');
                                s.into_bytes()
                            }
                            Err(e) => {
                                eprintln!("hyoui: screen snapshot: response の JSON 変換失敗: {e}");
                                return ExitCode::from(1);
                            }
                        }
                    }
                    // `ScreenSnapshotCliFormat` is `#[non_exhaustive]`; future variants surface
                    // as a binary/library version skew rather than silent fallback.
                    _ => {
                        eprintln!(
                            "hyoui: screen snapshot: unsupported --format variant (binary/library version skew)"
                        );
                        return ExitCode::from(2);
                    }
                };
                return write_screen_snapshot_payload(&payload, cfg.output.as_deref());
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
    // session 必須判定は `resolve_target_socket` に一元化されている (= 明示 >
    // $HYOUI_SESSION_ID > required エラー、DR-0020 §2)。ここで socket/session/index
    // 不在を先に弾くと env (= 中から実行) の self 解決経路を遮断してしまうため、
    // 先行チェックは置かない (Fable review C1 2026-06-12)。
    if cmd.specs.is_empty() {
        eprintln!("hyoui: input: spec list が空です (内部 invariant 違反)");
        return ExitCode::from(2);
    }

    // 1. socket path resolve。
    let sock = match resolve_target_socket(
        "input",
        cmd.socket.as_deref(),
        cmd.session_id.as_deref(),
        cmd.index,
        socket_path::resolve_namespace(cmd.namespace.as_deref()).as_str(),
    ) {
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
    let inherited_token = cmd
        .lock_token
        .clone()
        .or_else(|| std::env::var("HYOUI_LOCK_TOKEN").ok());
    let opts = AttachOptions {
        mode: Mode::Rw,
        caps: hyoui::protocol::MVP_CAPS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        token: inherited_token.clone(),
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

    // DR-0022: invocation 全体で auto-lock を 1 本取得。
    //
    // 外側 token を継承している場合 (= --lock-token か HYOUI_LOCK_TOKEN がある場合) は
    // skip。外側 wrapper (= hyoui lock tx 等) が既に holder を確立しているため、
    // 内側で再 acquire / release すると外側の lock を壊す恐れがある。
    //
    // 取得済 token は `owned_lock_token: Option<String>` に保持し、最後に明示 release
    // するかどうかを decide する。途中 error / early return 時の保険は daemon 側
    // process-bound GC (= 接続切断で auto-release) に任せる。
    let owned_lock_token: Option<String> = if inherited_token.is_none() {
        match hyoui::client::acquire_auto_lock(&mut conn, cmd.auto_lock_timeout_acquire) {
            Ok(tok) => Some(tok),
            Err(e) => {
                eprintln!("hyoui: input: auto-lock acquire 失敗: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        None
    };

    // wait の per-spec timeout は `cmd.timeout` を使う (= input 全体 timeout
    // ではなく、各 wait/wait-idle spec それぞれに適用する)。default 5s なので
    // 永久 wait したい場合は `--timeout=<長め>` を明示する。
    let wait_timeout = Some(cmd.timeout);
    let poll_interval =
        wait_core::poll_interval_from_env().unwrap_or(wait_core::DEFAULT_POLL_INTERVAL);

    // 3. 各 spec を順に dispatch。最初の失敗で abort。
    //    `cmd.max_file_bytes` は CLI flag / env / default で解決済 (= parser 段)。
    let mut dispatch_failed = false;
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
                dispatch_failed = true;
                break;
            }
        }
    }

    // 4. auto-lock を明示 release (= daemon に LockReleased lifecycle event を残し、
    //    mode.change を broadcast させる)。失敗しても daemon の process-bound GC が
    //    2 重保険として接続切断時に release する。
    //
    //    release は dispatch 成否に関わらず実行する (= 取った lock は必ず返す)。
    if let Some(tok) = owned_lock_token.as_ref()
        && let Err(e) = hyoui::client::release_auto_lock(&mut conn, tok)
    {
        eprintln!("hyoui: input: auto-lock release 失敗 (best-effort、daemon GC で回収): {e}");
    }

    // 5. 接続 close (= drop で socket close、daemon は client 切断として処理)。
    drop(conn);
    if dispatch_failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

// DR-0022 の auto-lock helper (`acquire_auto_lock` / `release_auto_lock`) は
// `hyoui::client::auto_lock` に移動 (= CLI + web gateway で共有)。

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
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
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
        // recv_control は blocking で read timeout を持たないため、daemon が half-open
        // (= process 消失だが FIN 未着) で固まると永久 hang する。reader fd を poll で
        // 監視して LOCK_RECV_TIMEOUT / POLLHUP を検知し、daemon 消失なら error 終了する。
        let response = loop {
            match poll_recv_ready(&conn, LOCK_RECV_TIMEOUT) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!(
                        "hyoui: lock acquire: daemon が応答しません (= recv timeout、daemon が消失/停止した可能性)"
                    );
                    return ExitCode::from(1);
                }
                Err(e) => {
                    eprintln!("hyoui: lock acquire: poll 失敗: {e}");
                    return ExitCode::from(1);
                }
            }
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
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
                    eprintln!("hyoui: lock acquire: timeout (queued path)");
                    return ExitCode::from(1);
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
                    if let Some(dl) = deadline
                        && Instant::now() >= dl
                    {
                        eprintln!("hyoui: lock acquire: timeout");
                        return ExitCode::from(1);
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

    // block phase 準備: stdin / socket / self-pipe を poll で並行監視するための
    // self-pipe を install して SIGINT/SIGTERM を捕る。
    //
    // **token print より前に install すること** (= issue 2026-06-11): token 行は
    // 「graceful release 可能になった」合図として shell / test が SIGTERM を送る
    // トリガーに使う。print 後に install する旧順序では、print〜install の窓に
    // SIGTERM が刺さると default action で process が signal 死し、lock release が
    // 走らない (= lock_cli の SIGTERM flaky の根因)。
    let pipe: hyoui::sys::SelfPipe = match hyoui::sys::install_self_pipe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: lock acquire: self-pipe 作成失敗: {e} (続行、Ctrl-C は効きません)");
            // self-pipe が作れなくても lock 解放はしないと困るので、明示的に release を試みて exit。
            // token 未出力なので caller は lock を見ていない (= release して exit 1 が安全)。
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

    // 取得済 + handler 設置済: token を stdout に 1 行 print + flush。shell capture
    // (`$(hyoui lock acquire ...)`) 用に確実に flush しておく。
    println!("{acquired_token}");
    if let Err(e) = std::io::stdout().flush() {
        eprintln!("hyoui: lock acquire: stdout flush 失敗: {e}");
        // flush 失敗は重大ではない (= token は出力済の可能性)、続行
        let _ = e;
    }
    eprintln!(
        "hyoui: lock acquire: lock を保持中。Ctrl-C / SIGTERM / stdin EOF で release します。"
    );

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
    let sock = match resolve_target_socket(
        cmd_label,
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
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

// =============================================================================
// DR-0016 Phase 7: `record` subcommand executors
// =============================================================================
//
// CLI 表現 enum (= `RecordDirectionArg` 等) → protocol 表現 enum (= `RecordDirection`
// 等) の写像、connect + handshake + send/recv 処理、出力整形を本セクションで行う。
//
// daemon 側 hook 配線 (Phase 4) 完了前は record 系 message を送ると daemon が
// `unsupported-capability` 系 error を返す前提。CLI は `ControlMessage::Error`
// を受けて hint を出して exit 1 する。

/// CLI 表現の direction を protocol 表現に写像。
fn map_record_direction(d: RecordDirectionArg) -> RecordDirection {
    match d {
        RecordDirectionArg::Stdin => RecordDirection::Stdin,
        RecordDirectionArg::Stdout => RecordDirection::Stdout,
        RecordDirectionArg::Both => RecordDirection::Both,
        // `RecordDirectionArg` は `#[non_exhaustive]`; future variants は Both で
        // 落とす (= 録画範囲を絞らず誤動作回避)。
        _ => RecordDirection::Both,
    }
}

/// CLI 表現の format を protocol 表現に写像。
fn map_record_format(f: RecordFormatArg) -> RecordFormat {
    match f {
        RecordFormatArg::Jsonl => RecordFormat::Jsonl,
        RecordFormatArg::Raw => RecordFormat::Raw,
        _ => RecordFormat::Jsonl,
    }
}

/// CLI 表現の input secrecy を protocol 表現に写像。
fn map_record_input_secrecy(s: RecordInputSecrecyArg) -> InputSecrecy {
    match s {
        // redact-after-prompt は parse 段で reject 済 (= ここには到達しない) が、
        // protocol enum との 1:1 写像として arm を残す (DR-0016 §6 interim)。
        RecordInputSecrecyArg::RedactAfterPrompt => InputSecrecy::RedactAfterPrompt,
        RecordInputSecrecyArg::RecordAll => InputSecrecy::RecordAll,
        RecordInputSecrecyArg::NeverRecordStdin => InputSecrecy::NeverRecordStdin,
        _ => InputSecrecy::RecordAll,
    }
}

/// daemon error が `record-v1` cap 未対応を示すかを heuristics で判定し、stderr
/// に hint を出す共通 helper。
fn print_record_cap_hint(err_msg: &str) {
    if err_msg.contains("record-v1") || err_msg.contains("unsupported-capability") {
        eprintln!(
            "       daemon が `record-v1` cap をサポートしていません (= Phase 4 未配線 or 旧 daemon)。"
        );
        eprintln!("       daemon を新しいバージョンに更新してください。");
    }
}

/// `record start` 実行時に stderr へ出す 3 行 loud warning (DR-0016 §2)。
///
/// `--input-secrecy` の値と出力 path を埋め込み、record file が機密情報を含む
/// 可能性を毎回明示する (= ユーザが「うっかり共有」を防ぐ最終防壁)。
fn print_record_start_warning(output_path: &std::path::Path, secrecy: RecordInputSecrecyArg) {
    eprintln!("WARNING: record file contains ALL bytes including potential secrets");
    eprintln!("  (passwords typed at prompts, OTP, API tokens).");
    eprintln!(
        "  Output: {} (mode 0600, only readable by your user).",
        output_path.display()
    );
    match secrecy {
        RecordInputSecrecyArg::NeverRecordStdin => {
            eprintln!("  Input secrecy: --input-secrecy=never-record-stdin.");
            eprintln!("  stdin is NOT recorded (dropped entirely); only stdout/lifecycle stored.");
        }
        // record-all (= interim default)。redact-after-prompt は parse 段で reject 済
        // のため到達しないが、non_exhaustive の受け皿として record-all 扱い (= 安全側:
        // 「stdin が残る」警告を出す、DR-0016 §6 interim)。
        _ => {
            eprintln!("  Input secrecy: --input-secrecy=record-all (default).");
            eprintln!(
                "  stdin is recorded VERBATIM (no redaction); secrets typed during\n  \
                 recording WILL be stored in the record file."
            );
        }
    }
    eprintln!("  Do NOT share record files outside your authentication boundary.");
}

/// `record start` 実行ロジック。
///
/// 1. session selector を socket path に解決
/// 2. stderr に 3 行 loud warning (DR-0016 §2)
/// 3. `--max-bytes 0` / `--max-duration 0` で disable された場合の追加 warning
/// 4. connect + handshake (cap negotiate で `record-v1` を要求)
/// 5. `RecordStartRequest` を送信
/// 6. `RecordStartResponse` を受信して `record_id` を stdout に出す
fn record_start_command(cfg: RecordStartConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "record start",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // Loud warning は **必ず** record 開始の意思表示直前に出す (= 接続前に出して
    // ユーザが Ctrl-C で中断する余地を残す)。
    print_record_start_warning(&cfg.output_path, cfg.input_secrecy);
    if cfg.max_bytes_disabled {
        eprintln!(
            "WARNING: --max-bytes=0 で size 上限が無効化されています。disk full で他 record /\n         \
             session に影響する前に手動 stop を確実に実行してください。"
        );
    }
    if cfg.max_duration_disabled {
        eprintln!(
            "WARNING: --max-duration=0 で duration 上限が無効化されています。session 終了まで\n         \
             record が継続するため、record file size を別途監視してください。"
        );
    }

    // handshake では MVP_CAPS を要求 (= `record-v1` 含む、既存 status/screen/tail と同流儀)。
    // token は env (HYOUI_LOCK_TOKEN) から取る (= 認証付き daemon にも対応)。
    let opts = AttachOptions {
        mode: Mode::Ro,
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        ..AttachOptions::default()
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("record start", &sock, &e);
            return ExitCode::from(1);
        }
    };

    let req = RecordStartRequest {
        direction: map_record_direction(cfg.direction),
        format: map_record_format(cfg.format),
        // protocol 上は String、CLI 側は PathBuf。display 経由でなく to_string_lossy
        // で wire 文字列化 (= 絶対 path 保証済なので lossy 化の loss なし、UTF-8 path 想定)。
        output_path: cfg.output_path.to_string_lossy().into_owned(),
        max_bytes: cfg.max_bytes,
        max_duration_ms: cfg.max_duration_ms,
        input_secrecy: map_record_input_secrecy(cfg.input_secrecy),
        prompt_pattern: cfg.prompt_pattern.clone(),
    };
    if let Err(e) = conn.send_control(&ControlMessage::RecordStartRequest(req)) {
        eprintln!("hyoui: record start: send 失敗: {e}");
        return ExitCode::from(1);
    }

    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: record start: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::RecordStartResponse(resp) => {
                println!(
                    "Started record #{} -> {}",
                    resp.record_id,
                    cfg.output_path.display()
                );
                return ExitCode::SUCCESS;
            }
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: record start: daemon error: code={:?} message={}",
                    e.code, e.message
                );
                print_record_cap_hint(&e.message);
                return ExitCode::from(1);
            }
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: record start: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}

/// `record stop` 実行ロジック。
///
/// - `--all` → `RecordStopAllRequest` を送信
/// - `--id N` → `RecordStopRequest { record_id: N }` を送信
/// - 両省略 → 先に `RecordListRequest` を query して single active なら自動採用、
///   複数 active なら error、none なら error (= ambiguity を CLI 側で吸収)
fn record_stop_command(cfg: RecordStopConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "record stop",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let opts = AttachOptions {
        mode: Mode::Ro,
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        ..AttachOptions::default()
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("record stop", &sock, &e);
            return ExitCode::from(1);
        }
    };

    // --all → 全停止 message
    if cfg.all {
        if let Err(e) = conn.send_control(&ControlMessage::RecordStopAllRequest(
            RecordStopAllRequest {},
        )) {
            eprintln!("hyoui: record stop: send 失敗 (--all): {e}");
            return ExitCode::from(1);
        }
        return record_stop_wait_response(&mut conn, "record stop (--all)");
    }

    // --id 明示 → 単一停止
    if let Some(id) = cfg.record_id {
        if let Err(e) = conn.send_control(&ControlMessage::RecordStopRequest(RecordStopRequest {
            record_id: id,
        })) {
            eprintln!("hyoui: record stop: send 失敗 (--id={id}): {e}");
            return ExitCode::from(1);
        }
        return record_stop_wait_response(&mut conn, "record stop");
    }

    // --id も --all も無し → 先に list query で single active を確認。
    if let Err(e) = conn.send_control(&ControlMessage::RecordListRequest(RecordListRequest {})) {
        eprintln!("hyoui: record stop: list query send 失敗: {e}");
        return ExitCode::from(1);
    }
    let records = match wait_record_list_response(&mut conn, "record stop") {
        Ok(r) => r,
        Err(code) => return code,
    };
    match records.len() {
        0 => {
            eprintln!("hyoui: record stop: 停止対象の record がありません (active 0 件)");
            ExitCode::from(1)
        }
        1 => {
            let id = records[0].record_id;
            if let Err(e) =
                conn.send_control(&ControlMessage::RecordStopRequest(RecordStopRequest {
                    record_id: id,
                }))
            {
                eprintln!("hyoui: record stop: auto-select send 失敗 (--id={id}): {e}");
                return ExitCode::from(1);
            }
            eprintln!("hyoui: record stop: auto-selected record_id={id}");
            record_stop_wait_response(&mut conn, "record stop")
        }
        n => {
            eprintln!(
                "hyoui: record stop: 複数の active record が存在します ({n} 件)。\
                 `--id <N>` で対象を指定するか `--all` で一括停止してください。"
            );
            for r in &records {
                eprintln!("       record_id={} -> {}", r.record_id, r.output_path);
            }
            ExitCode::from(1)
        }
    }
}

/// `record stop` の response (= success / error) を待って exit code を決める helper。
///
/// transient な broadcast (= `ModeChange` / `LeaderNotify`) は skip + 次 message を
/// 待つ。daemon は成功時 `RecordStopResponse { stopped }` を返す (= 無音だと client
/// が永久 hang する、DR-0016 §7)。失敗は `record-not-found` error。
fn record_stop_wait_response(conn: &mut ClientConnection, label: &str) -> ExitCode {
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: {label}: recv 失敗: {e}");
                return ExitCode::from(1);
            }
        };
        match msg {
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            ControlMessage::RecordStopResponse(resp) => {
                println!("hyoui: {label}: stopped {} record(s)", resp.stopped);
                return ExitCode::SUCCESS;
            }
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: {label}: daemon error: code={:?} message={}",
                    e.code, e.message
                );
                print_record_cap_hint(&e.message);
                return ExitCode::from(1);
            }
            other => {
                eprintln!("hyoui: {label}: unexpected response: {other:?}");
                return ExitCode::from(1);
            }
        }
    }
}

/// `record.list.response` を待って records を返す helper。error は exit code に変換。
fn wait_record_list_response(
    conn: &mut ClientConnection,
    label: &str,
) -> Result<Vec<RecordInfo>, ExitCode> {
    loop {
        let msg = match conn.recv_control(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("hyoui: {label}: recv 失敗: {e}");
                return Err(ExitCode::from(1));
            }
        };
        match msg {
            ControlMessage::RecordListResponse(resp) => return Ok(resp.records),
            ControlMessage::Error(e) => {
                eprintln!(
                    "hyoui: {label}: daemon error: code={:?} message={}",
                    e.code, e.message
                );
                print_record_cap_hint(&e.message);
                return Err(ExitCode::from(1));
            }
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                eprintln!("hyoui: {label}: unexpected response: {other:?}");
                return Err(ExitCode::from(1));
            }
        }
    }
}

/// `record list` 実行ロジック。
fn record_list_command(cfg: RecordListConfig) -> ExitCode {
    let sock = match resolve_target_socket(
        "record list",
        cfg.socket.as_deref(),
        cfg.session_id.as_deref(),
        cfg.index,
        socket_path::resolve_namespace(cfg.namespace.as_deref()).as_str(),
    ) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let opts = AttachOptions {
        mode: Mode::Ro,
        token: std::env::var("HYOUI_LOCK_TOKEN").ok(),
        ..AttachOptions::default()
    };
    let mut conn = match connect_with_retry(&sock, opts) {
        Ok(c) => c,
        Err(e) => {
            print_connect_failure("record list", &sock, &e);
            return ExitCode::from(1);
        }
    };
    if let Err(e) = conn.send_control(&ControlMessage::RecordListRequest(RecordListRequest {})) {
        eprintln!("hyoui: record list: send 失敗: {e}");
        return ExitCode::from(1);
    }
    let records = match wait_record_list_response(&mut conn, "record list") {
        Ok(r) => r,
        Err(code) => return code,
    };
    match cfg.format {
        RecordListFormatArg::Table => print_record_list_table(&records),
        RecordListFormatArg::Jsonl => print_record_list_jsonl(&records),
        _ => print_record_list_table(&records),
    }
    ExitCode::SUCCESS
}

/// `record list` の table format 出力 (= 人間可読、固定長 column)。
///
/// 0 件なら header 行のみ出す (= scripting でも空集合と判別可能)。
fn print_record_list_table(records: &[RecordInfo]) {
    println!(
        "{:>4}  {:<6}  {:<6}  {:>20}  {:>10}  {:>10}  OUTPUT",
        "ID", "DIR", "FORMAT", "STARTED_MS", "RAW_BYTES", "FILE_BYTES",
    );
    for r in records {
        let dir = match r.direction {
            RecordDirection::Stdin => "stdin",
            RecordDirection::Stdout => "stdout",
            RecordDirection::Both => "both",
            _ => "?",
        };
        let fmt = match r.format {
            RecordFormat::Jsonl => "jsonl",
            RecordFormat::Raw => "raw",
            _ => "?",
        };
        println!(
            "{:>4}  {:<6}  {:<6}  {:>20}  {:>10}  {:>10}  {}",
            r.record_id,
            dir,
            fmt,
            r.started_unix_ms,
            r.raw_bytes_recorded,
            r.file_bytes_written,
            r.output_path,
        );
    }
}

/// `record list` の jsonl format 出力 (= scripting / jq 用)。
///
/// `RecordInfo` を 1 record 1 行に整形。serde_json への依存を避け、手書きで
/// JSON encode する (= `print_status_json` と同流儀、kebab-case wire 名は使わず
/// snake_case で出す = field 名 一貫性)。
fn print_record_list_jsonl(records: &[RecordInfo]) {
    use std::fmt::Write as _;
    for r in records {
        let dir = match r.direction {
            RecordDirection::Stdin => "stdin",
            RecordDirection::Stdout => "stdout",
            RecordDirection::Both => "both",
            _ => "unknown",
        };
        let fmt = match r.format {
            RecordFormat::Jsonl => "jsonl",
            RecordFormat::Raw => "raw",
            _ => "unknown",
        };
        let mut out = String::new();
        out.push('{');
        write!(&mut out, "\"record_id\":{}", r.record_id).ok();
        write!(&mut out, ",\"direction\":\"{dir}\"").ok();
        write!(&mut out, ",\"format\":\"{fmt}\"").ok();
        write!(&mut out, ",\"output_path\":{}", json_string(&r.output_path)).ok();
        write!(&mut out, ",\"started_unix_ms\":{}", r.started_unix_ms).ok();
        write!(
            &mut out,
            ",\"started_by_client_id\":{}",
            r.started_by_client_id
        )
        .ok();
        write!(&mut out, ",\"raw_bytes_recorded\":{}", r.raw_bytes_recorded).ok();
        write!(&mut out, ",\"file_bytes_written\":{}", r.file_bytes_written).ok();
        write!(
            &mut out,
            ",\"last_flushed_unix_ms\":{}",
            r.last_flushed_unix_ms
        )
        .ok();
        out.push('}');
        println!("{out}");
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

    /// `shorten_cwd`: `repos/github.com/...` 配下は `<owner>/<repo>/<sub>` に短縮。
    #[test]
    fn shorten_cwd_strips_repos_host_prefix() {
        let cwd = "/Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main";
        assert_eq!(shorten_cwd(cwd), "kawaz/hyoui/main");
    }

    /// `shorten_cwd`: `repos/<other-host>/...` も同じ規則で前カット。
    #[test]
    fn shorten_cwd_handles_other_host() {
        let cwd = "/home/u/.local/share/repos/bitbucket.org/team/proj";
        assert_eq!(shorten_cwd(cwd), "team/proj");
    }

    /// `shorten_cwd`: `repos/` 配下でない path は `$HOME` 前カットのみ。
    #[test]
    fn shorten_cwd_falls_back_to_home_prefix() {
        // HOME が test 環境で seed されている前提 (= cargo test 由来の env)。
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().into_owned();
            let cwd = format!("{home}/projects/foo");
            assert_eq!(shorten_cwd(&cwd), "~/projects/foo");
        }
    }

    /// `shorten_cwd`: 該当しない path は無変更。
    #[test]
    fn shorten_cwd_passthrough_unmatched() {
        let cwd = "/tmp/some/where";
        // HOME と異なる前提 (= test runner 環境では HOME = /Users/... 等)。
        // HOME prefix とぶつかる場合は passthrough にならないので、明確に外す path を選ぶ。
        let result = shorten_cwd(cwd);
        // /tmp が HOME 配下になることは通常ない (= 安全な assert)
        assert!(
            result == cwd || result.starts_with("~/"),
            "expected unchanged or HOME-prefixed, got: {result}"
        );
    }

    /// `fmt_argv`: space を含む arg は quote される。
    #[test]
    fn fmt_argv_quotes_args_with_spaces() {
        let argv = vec!["echo".to_string(), "hello world".to_string()];
        assert_eq!(fmt_argv(&argv), "echo \"hello world\"");
    }

    /// `fmt_argv`: 空白なし arg はそのまま。
    #[test]
    fn fmt_argv_plain_args_no_quote() {
        let argv = vec!["bash".to_string(), "-l".to_string()];
        assert_eq!(fmt_argv(&argv), "bash -l");
    }

    /// `fmt_argv`: 改行 / CR / TAB を含む arg は quote + `\n` `\r` `\t` escape
    /// (= plain list 出力が縦に崩れないため。JSONL 側は serde_json が escape 済)。
    #[test]
    fn fmt_argv_escapes_control_chars() {
        let argv = vec![
            "bash".to_string(),
            "-c".to_string(),
            "echo foo\nbar\tbaz\r".to_string(),
        ];
        assert_eq!(fmt_argv(&argv), "bash -c \"echo foo\\nbar\\tbaz\\r\"");
    }

    /// `fmt_argv`: 既存の `\` はそのままの literal として escape される
    /// (= 復元可能性のため。表示専用なので厳密な shell 互換 quoting は不要)。
    #[test]
    fn fmt_argv_escapes_backslash() {
        let argv = vec!["a\\b".to_string()];
        assert_eq!(fmt_argv(&argv), "a\\\\b");
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
    ///
    /// CI flaky: Linux (Ubuntu runner) + macOS の両方で偶発失敗が確認されている
    /// (= UnixListener Drop 後 OS が短時間 connect を accept してしまう挙動の差)。
    /// 私の DR-0015 改修と無関係な既存問題。Task 21 (test fixture 改修) で恒久対応。
    #[ignore = "既存 flaky test、Task 21 で恒久対応"]
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
        use hyoui::daemon::{DaemonConfig, Session};

        let sock_dir = make_0700_dir();

        // stale socket: bind して即 close、file だけ残す。std の UnixListener::drop は
        // unlink しないので file 残留 (= まさに daemon panic 後の状態)。
        let stale_path = sock_dir.path().join("stale-sess.sock");
        {
            let _l = UnixListener::bind(&stale_path).expect("bind stale");
        }
        assert!(stale_path.exists(), "stale socket file should exist");

        // live socket: **本物の hyoui daemon を起動** する。
        // kawaz 指摘 #2 対応で `enrich_entries_with_status` が timeout 無し blocking
        // になったため、bind-only listener (= accept しても handshake 返さない) では
        // 永遠に hang する。本物 daemon に置き換えて「live は必ず即応答」を担保。
        let live_path = sock_dir.path().join("live-sess.sock");
        let mut cfg_live = DaemonConfig::new(
            "live-sess",
            live_path.clone(),
            vec!["/bin/sleep".into(), "30".into()],
        );
        cfg_live.cwd = Some(std::path::PathBuf::from("/tmp"));
        let session_live = Session::start(cfg_live).expect("live daemon start");
        let daemon_handle = std::thread::spawn(move || session_live.serve());

        // dir 一覧を直接渡して env mutation を回避
        let cfg = ListConfig {
            prune_stale: true,
            ..Default::default()
        };
        let _exit = list_command_with_dirs(
            cfg,
            vec![(
                hyoui::cli::DEFAULT_NAMESPACE.to_string(),
                sock_dir.path().to_path_buf(),
            )],
        );

        // 確認: stale は unlink された、live はまだ残っている
        assert!(
            !stale_path.exists(),
            "--prune-stale should unlink stale socket"
        );
        assert!(
            live_path.exists(),
            "--prune-stale must not unlink live socket"
        );

        // cleanup: live daemon を kill して thread を畳む
        let opts = AttachOptions {
            mode: Mode::Rw,
            ..AttachOptions::default()
        };
        if let Ok(mut conn) = connect_with_retry(&live_path, opts) {
            let _ = conn.send_control(&ControlMessage::Kill(hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            }));
            drop(conn);
        }
        let _ = daemon_handle.join();
    }

    /// R5-H3: `--prune-stale` を指定しない時は stale でも socket file は削除しない。
    #[test]
    fn list_without_prune_keeps_stale_sockets() {
        let sock_dir = make_0700_dir();
        let stale_path = sock_dir.path().join("stale.sock");
        // kawaz 指摘 #2 対応で `enrich_entries_with_status` が timeout 廃止になったため、
        // bind-then-drop fixture は OS race (= drop 後も accept が成立する瞬間) で
        // `probe_socket_liveness` が稀に true を返し、enrich が永遠に hang する。
        // **regular file** を `*.sock` 名で置く方が確実: connect(2) は ENOTSOCK で
        // 即 fail → probe false → enrich skip → 旧 stale 経路で扱われる。
        std::fs::write(&stale_path, b"").expect("create regular file as stale fixture");
        assert!(stale_path.exists());

        let cfg = ListConfig {
            prune_stale: false,
            ..Default::default()
        };
        let _exit = list_command_with_dirs(
            cfg,
            vec![(
                hyoui::cli::DEFAULT_NAMESPACE.to_string(),
                sock_dir.path().to_path_buf(),
            )],
        );

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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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
            index: None,
            namespace: None,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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
            index: None,
            namespace: None,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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
            index: None,
            namespace: None,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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
            index: None,
            namespace: None,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
            let _ = conn.send_control(&ControlMessage::Kill(kill));
            drop(conn);
        }
        let _ = daemon_handle.join();
    }

    /// `screen snapshot --format=json` 経路の検証。daemon が CBOR で返した
    /// `StateSnapshotResponse` を CLI 段で `serde_json::to_string_pretty` に変換し、
    /// file に書かれた payload が valid JSON で要求 component を持つことを確認する。
    #[test]
    fn screen_snapshot_command_writes_json_response_payload_to_file() {
        use hyoui::daemon::{DaemonConfig, Session};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("snap-json-test.sock");
        let out_path = sock_dir.path().join("snap.json");

        let sock_for_daemon = sock_path.clone();
        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "screen-snapshot-json-test",
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

        std::thread::sleep(std::time::Duration::from_millis(200));

        let cfg = ScreenSnapshotConfig {
            socket: Some(sock_path.to_string_lossy().into_owned()),
            session_id: None,
            index: None,
            namespace: None,
            include: vec![
                SnapshotCliComponent::Cursor,
                SnapshotCliComponent::Mode,
                SnapshotCliComponent::WindowSize,
                SnapshotCliComponent::Buffer,
                SnapshotCliComponent::SequenceNo,
            ],
            format: hyoui::cli::ScreenSnapshotCliFormat::Json,
            output: Some(out_path.to_string_lossy().into_owned()),
            timeout_ms: 5_000,
        };
        let _exit = screen_snapshot_command(cfg);

        let got = std::fs::read_to_string(&out_path).expect("read output");
        assert!(!got.is_empty(), "json payload must not be empty");
        assert!(got.ends_with('\n'), "json payload must end with newline");
        let v: serde_json::Value = serde_json::from_str(&got).expect("valid JSON");
        let obj = v.as_object().expect("top-level must be object");
        assert!(obj.contains_key("cursor"), "cursor key must be present");
        assert!(obj.contains_key("mode"), "mode key must be present");
        assert!(
            obj.contains_key("window-size"),
            "window-size key must be present (kebab-case from rename_all)"
        );
        assert!(obj.contains_key("buffer"), "buffer key must be present");
        assert!(
            obj.contains_key("sequence-no"),
            "sequence-no key must be present"
        );
        assert!(
            !obj.contains_key("cells"),
            "cells must be omitted (skip_serializing_if = None)"
        );

        let kill_opts = AttachOptions {
            mode: Mode::Ro,
            ..AttachOptions::default()
        };
        if let Ok(mut conn) = connect_with_retry(&sock_path, kill_opts) {
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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
        let kill = hyoui::protocol::messages::Kill {
            signal: None,
            wait: true,
        };
        let _ = conn.send_control(&ControlMessage::Kill(kill));
        drop(conn);
        let _ = daemon_handle.join();
    }

    /// DR-0021 PTY drain ack の race regression test。
    ///
    /// 子に `/bin/cat` を起動 (= stdin → stdout echo の line-oriented 系)。
    /// `text:長文` + `key:Enter` を 1 connection で順次送り、cat が echo した結果
    /// (= screen state に反映される primary buffer の visible bytes) で「text bytes
    /// の末尾に Enter (= 改行) が来ている」を確認する。
    ///
    /// 旧実装 (= socket flush で完了扱い) では、長文 text の master fd write が
    /// chunked になっている途中で次の frame (= Enter) が pipeline で届くと、
    /// daemon の serve_loop は同一 frame の完了後に次 frame を処理するため通常は
    /// 順序が保たれるが、子の TUI が ICRNL を切ったり line discipline buffer の
    /// flush タイミングによって取りこぼされる race が報告されていた (= issue
    /// `2026-06-16-bug-input-text-key-enter-not-sent`)。新実装は ack 待ちで client
    /// 側が確実に同期するため、本 test は「ack 機構が確実に動作する」regression
    /// guard として 30 回繰り返して全 pass を要求する。
    #[test]
    fn send_raw_bytes_text_then_enter_arrives_in_order() {
        use hyoui::daemon::{DaemonConfig, Session};
        use hyoui::protocol::messages::{
            ScreenDumpFormat as ProtoDumpFormat, ScreenDumpLayer as ProtoDumpLayer,
            ScreenDumpRequest,
        };
        use std::time::Duration;

        // 反復回数: race を 100% で踏むなら 1 回でも十分だが、ack 機構の安定性
        // (= timeout / pending_frames buffer / 順序保証) を 30 回連続で確認する。
        const ITERATIONS: usize = 30;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("ack-race.sock");
        let sock_for_daemon = sock_path.clone();

        let daemon_handle = std::thread::spawn(move || {
            // 子: cat は stdin の行 (= \n 区切り) を echo する。30 秒 sleep を後段に
            // 入れずに cat だけだと EOF まで生きるので、cat を直起動。
            let cfg = DaemonConfig::new("ack-race-test", sock_for_daemon, vec!["/bin/cat".into()]);
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        // listener bind + cat 起動完了を待つ。
        std::thread::sleep(Duration::from_millis(200));

        let opts = AttachOptions {
            mode: Mode::Rw,
            caps: hyoui::protocol::MVP_CAPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..AttachOptions::default()
        };
        let mut conn = connect_with_retry(&sock_path, opts).expect("attach Rw");

        // 各反復で text + Enter を送り、screen に "MARK<i>\n" が反映されるのを
        // 確認する。各反復のラベルを変えることで「前の反復の echo を見ている」
        // 誤判定を避ける。
        for i in 0..ITERATIONS {
            // 識別子付き label。echo されると "MARK<i>\r\n" が screen に乗る。
            let label = format!("MARK{i}");
            conn.send_raw_bytes(label.as_bytes())
                .expect("send_raw_bytes text");
            // key:Enter (= "\r"、`input_handlers::handle_key("Enter")` と同じ)。
            conn.send_raw_bytes(b"\r").expect("send_raw_bytes enter");

            // cat の echo が screen state に反映されるまで polling。
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let mut seen = false;
            while std::time::Instant::now() < deadline {
                let req = ScreenDumpRequest {
                    format: ProtoDumpFormat::Ansi,
                    layer: ProtoDumpLayer::Visible,
                    rect: None,
                    serial: Some(i as u64 + 1),
                };
                conn.send_control(&ControlMessage::ScreenDumpRequest(req))
                    .expect("send screen.dump request");
                // ModeChange/LeaderNotify/RawAck 等を skip しつつ
                // ScreenDumpResponse を取り出す。
                let payload_opt = loop {
                    match conn.recv_control(None) {
                        Ok(ControlMessage::ScreenDumpResponse(r)) => break Some(r.payload),
                        Ok(_) => continue,
                        Err(_) => break None,
                    }
                };
                let Some(payload) = payload_opt else { break };
                let s = String::from_utf8_lossy(&payload);
                if s.contains(&label) {
                    seen = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                seen,
                "iteration {i}: label {label} should appear in cat echo (= Enter must \
                 fire after text, screen.dump never observed the label). PTY drain ack \
                 should have synchronized text→Enter ordering."
            );
        }

        // cleanup。
        let kill = hyoui::protocol::messages::Kill {
            signal: None,
            wait: true,
        };
        let _ = conn.send_control(&ControlMessage::Kill(kill));
        drop(conn);
        let _ = daemon_handle.join();
    }

    /// DR-0021 改訂: `recv_raw_ack_inner` の partial-byte discard race regression test。
    ///
    /// 旧実装は `set_read_timeout(Some(RAW_ACK_TIMEOUT))` で `read(2)` 自体に timeout を
    /// 入れていたため、`Frame::decode_from` が body 読み出し途中で `TimedOut` を踏むと
    /// `read_exact` の partial-bytes 破棄仕様で socket に body 残骸が居残り、次 iteration で
    /// 残骸の先頭 4 B を size として誤解読し `frame decode failed while waiting raw_ack`
    /// (= Error::Invalid) が発生した。実機検証では `python -i` に 1038 B 以上の text + Enter
    /// で再現 (= line discipline buffer ≤ 1024 の echo タイミングと一致)。
    ///
    /// 本 test は子に `/bin/cat` を起動し、毎反復で 2000 B 級の text + Enter を送って ack を
    /// 待つ。新実装 (= poll-based readiness + blocking decode) では partial-byte race が
    /// 構造的に起きない (= frame 境界でのみ deadline 判定が発火する)。20 反復で全 pass を
    /// 要求する。
    #[test]
    fn send_raw_bytes_partial_byte_race_regression() {
        use hyoui::daemon::{DaemonConfig, Session};
        use std::time::Duration;

        const ITERATIONS: usize = 20;
        const TEXT_LEN: usize = 2000;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("ack-partial.sock");
        let sock_for_daemon = sock_path.clone();

        let daemon_handle = std::thread::spawn(move || {
            let cfg =
                DaemonConfig::new("ack-partial-test", sock_for_daemon, vec!["/bin/cat".into()]);
            let session = Session::start(cfg).expect("daemon start");
            session.serve()
        });

        std::thread::sleep(Duration::from_millis(200));

        let opts = AttachOptions {
            mode: Mode::Rw,
            caps: hyoui::protocol::MVP_CAPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..AttachOptions::default()
        };
        let mut conn = connect_with_retry(&sock_path, opts).expect("attach Rw");

        // 各反復: ascii 2000 B + Enter。ack が来れば Ok(()), race を踏むと
        // Err(Error::Invalid("frame decode failed while waiting raw_ack")) が出る。
        for i in 0..ITERATIONS {
            let big = vec![b'A'; TEXT_LEN];
            conn.send_raw_bytes(&big).unwrap_or_else(|e| {
                panic!(
                    "iteration {i}: send_raw_bytes text (2000 B) failed: {e:?} \
                       (= partial-byte race regression)"
                );
            });
            conn.send_raw_bytes(b"\r").unwrap_or_else(|e| {
                panic!("iteration {i}: send_raw_bytes Enter failed: {e:?}");
            });
        }

        let kill = hyoui::protocol::messages::Kill {
            signal: None,
            wait: true,
        };
        let _ = conn.send_control(&ControlMessage::Kill(kill));
        drop(conn);
        let _ = daemon_handle.join();
    }

    /// DR-0021 改訂: Ro client から bytes 送ると Error ack (`client.ro-rejected`)。
    ///
    /// 旧実装は silent-drop 互換のため `Ok` ack を返していたが、ack の意味論を
    /// 「子の input stream に確実に到達した」に統一するため Error ack に変更。
    /// client 側は `Err(Error::Remote(msg))` で受け取る。
    #[test]
    fn send_raw_bytes_ro_client_receives_error_ack() {
        use hyoui::daemon::{DaemonConfig, Session};
        use std::time::Duration;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("ack-ro.sock");
        let sock_for_daemon = sock_path.clone();

        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "ack-ro-test",
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

        std::thread::sleep(Duration::from_millis(200));

        // Ro client として attach。
        let opts = AttachOptions {
            mode: Mode::Ro,
            caps: hyoui::protocol::MVP_CAPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..AttachOptions::default()
        };
        let mut conn = connect_with_retry(&sock_path, opts).expect("attach Ro");

        let result = conn.send_raw_bytes(b"hello");
        match result {
            Err(hyoui::Error::Remote(msg)) => {
                assert!(
                    msg.contains("read-only") || msg.contains("ro-rejected"),
                    "Error::Remote message should mention Ro reject: got {msg:?}"
                );
            }
            other => {
                panic!("Ro client send_raw_bytes must return Err(Error::Remote(..)), got {other:?}")
            }
        }

        let kill = hyoui::protocol::messages::Kill {
            signal: None,
            wait: true,
        };
        let _ = conn.send_control(&ControlMessage::Kill(kill));
        drop(conn);
        let _ = daemon_handle.join();
    }

    /// DR-0021 改訂: lock holder と異なる client から bytes 送ると Error ack
    /// (`client.lock-not-held`)。
    ///
    /// 2 つの Rw client A / B が同 daemon に接続。A が lock 取得 → B が input 送信
    /// → daemon が Error ack を返し、B は `Err(Error::Remote)`。
    #[test]
    fn send_raw_bytes_lock_not_held_receives_error_ack() {
        use hyoui::daemon::{DaemonConfig, Session};
        use hyoui::protocol::messages::{LockAcquire, LockResponse, LockResult};
        use std::time::Duration;

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("ack-lock.sock");
        let sock_for_daemon = sock_path.clone();

        let daemon_handle = std::thread::spawn(move || {
            let cfg = DaemonConfig::new(
                "ack-lock-test",
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

        std::thread::sleep(Duration::from_millis(200));

        let make_opts = || AttachOptions {
            mode: Mode::Rw,
            caps: hyoui::protocol::MVP_CAPS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..AttachOptions::default()
        };

        let mut conn_a = connect_with_retry(&sock_path, make_opts()).expect("attach A");
        let mut conn_b = connect_with_retry(&sock_path, make_opts()).expect("attach B");

        // A が lock 取得 (daemon 側 schema: wait/timeout のみ、token は response で返る)。
        conn_a
            .send_control(&ControlMessage::LockAcquire(LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            }))
            .expect("A lock acquire send");
        // LockResponse が来るまで control を読む (= ModeChange/LeaderNotify を skip)。
        let mut acquired = false;
        for _ in 0..20 {
            match conn_a.recv_control(None) {
                Ok(ControlMessage::LockResponse(LockResponse {
                    result: LockResult::Acquired,
                    ..
                })) => {
                    acquired = true;
                    break;
                }
                Ok(_) => continue,
                Err(e) => panic!("A: failed to receive lock response: {e:?}"),
            }
        }
        assert!(acquired, "A must acquire the lock");

        // B が lock holder でない状態で bytes 送信 → Error ack。
        let result = conn_b.send_raw_bytes(b"hello from B");
        match result {
            Err(hyoui::Error::Remote(msg)) => {
                assert!(
                    msg.contains("lock") || msg.contains("lock-not-held"),
                    "Error::Remote message should mention lock: got {msg:?}"
                );
            }
            other => panic!(
                "non-lock-holder send_raw_bytes must return Err(Error::Remote(..)), got {other:?}"
            ),
        }

        let kill = hyoui::protocol::messages::Kill {
            signal: None,
            wait: true,
        };
        let _ = conn_a.send_control(&ControlMessage::Kill(kill));
        drop(conn_a);
        drop(conn_b);
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
        use hyoui::daemon::{DaemonConfig, Session};
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
            index: None,
            namespace: None,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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

        use hyoui::daemon::{DaemonConfig, Session};
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
            index: None,
            namespace: None,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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

        use hyoui::daemon::{DaemonConfig, Session};
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
            index: None,
            namespace: None,
            specs: vec![InputSpec::Wait("GO".into())],
            timeout: std::time::Duration::from_secs(3),
            lock_token: None,
            max_file_bytes: hyoui::cli::DEFAULT_INPUT_MAX_FILE_BYTES,
            auto_lock_timeout_acquire: hyoui::cli::DEFAULT_INPUT_AUTO_LOCK_TIMEOUT,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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
        use hyoui::daemon::{DaemonConfig, Session};

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
            index: None,
            namespace: None,
            specs: vec![InputSpec::WaitIdle(std::time::Duration::from_millis(200))],
            timeout: std::time::Duration::from_secs(3),
            lock_token: None,
            max_file_bytes: hyoui::cli::DEFAULT_INPUT_MAX_FILE_BYTES,
            auto_lock_timeout_acquire: hyoui::cli::DEFAULT_INPUT_AUTO_LOCK_TIMEOUT,
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
            let kill = hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            };
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

    /// kawaz 指摘 #3: 「live daemon → status query が必ず成功する」を assert する
    /// integration test。旧実装は 300ms / 500ms timeout で graceful `-` 表示に
    /// 逃げる経路があり、「live なのに status 取れない」を検出できなかった。
    /// この test が pass し続ける = timeout false-positive 経路が無いこと、
    /// `cwd` / `argv` / `clients` が required field として正しく載っていることの両方を保証。
    #[test]
    fn enrich_live_daemon_yields_required_fields() {
        use hyoui::daemon::{DaemonConfig, Session};

        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("live-enrich.sock");
        let argv_vec = vec!["/bin/sleep".to_string(), "30".to_string()];

        let mut cfg = DaemonConfig::new("live-enrich-test", sock_path.clone(), argv_vec.clone());
        // daemon が cwd を載せられるよう Some を渡す (= daemonize 経路と同じ semantics)。
        // bug fix 2026-06-11 以降、子 PTY は exec 前に `cfg.cwd` へ chdir するため、
        // **実在する dir** を渡す必要がある (= 存在しないと chdir 失敗で child が
        // _exit(127) し、handshake が成立しない)。`/tmp` は両 OS で実在。
        let test_cwd = std::path::PathBuf::from("/tmp");
        cfg.cwd = Some(test_cwd.clone());
        let session = Session::start(cfg).expect("daemon start");
        let daemon_handle = std::thread::spawn(move || session.serve());

        // listener bind 完了を待つ (= retry budget は connect_with_retry 経由で吸収)。
        let mut entries = vec![ListEntry {
            session: "live-enrich-test".into(),
            namespace: hyoui::cli::DEFAULT_NAMESPACE.to_string(),
            socket_path: sock_path.clone(),
            started_unix_ms: 0,
            dur: std::time::Duration::ZERO,
            status: ListEntryStatus::Live {
                cwd: String::new(),
                argv: Vec::new(),
                clients: 0,
                child_stopped: false,
                child_pid: None,
                child_pgid: None,
                on_child_suspend: None,
                daemon_version: String::new(),
            },
        }];

        // retry 経路を介さず直接 enrich を叩く。本物 daemon は即応答するので
        // timeout / hang はあり得ない (= もし hang したら test runner の timeout で
        // 落ちる、それ自体が「timeout 経路無し」を間接的に証明する)。
        enrich_entries_with_status(&mut entries);

        match &entries[0].status {
            ListEntryStatus::Live {
                cwd, argv, clients, ..
            } => {
                assert_eq!(
                    cwd,
                    &test_cwd.to_string_lossy(),
                    "live daemon must report exact cwd"
                );
                assert_eq!(argv, &argv_vec, "live daemon must report exact argv");
                // probe 自身が 1 client として handshake を張っているため、`clients` は 1。
                // この挙動は `hyoui status` と同じ (= プローブ接続も client list に乗る)。
                assert_eq!(
                    *clients, 1,
                    "expected exactly 1 client (= the probe itself); got {clients}"
                );
            }
            ListEntryStatus::Stale => {
                panic!(
                    "live daemon must NOT be demoted to Stale (= status query 必ず成功する前提)"
                );
            }
        }

        // daemon を kill して serve thread を畳む。
        let opts = AttachOptions {
            mode: Mode::Rw,
            ..AttachOptions::default()
        };
        if let Ok(mut conn) = connect_with_retry(&sock_path, opts) {
            let _ = conn.send_control(&ControlMessage::Kill(hyoui::protocol::messages::Kill {
                signal: None,
                wait: true,
            }));
            drop(conn);
        }
        let _ = daemon_handle.join();
    }

    /// listener 不在 path に対して enrich を呼ぶと Stale 格下げになることを assert。
    /// `hyoui: warning: ...` stderr が出るが test 上は無視 (= eprintln 出力検証は別 task)。
    #[test]
    fn enrich_demotes_unreachable_socket_to_stale() {
        let sock_dir = make_0700_dir();
        let sock_path = sock_dir.path().join("nope.sock");
        // listener を bind しない (= connect で ECONNREFUSED or ENOENT になる)。
        let mut entries = vec![ListEntry {
            session: "fake".into(),
            namespace: hyoui::cli::DEFAULT_NAMESPACE.to_string(),
            socket_path: sock_path,
            started_unix_ms: 0,
            dur: std::time::Duration::ZERO,
            status: ListEntryStatus::Live {
                cwd: String::new(),
                argv: Vec::new(),
                clients: 0,
                child_stopped: false,
                child_pid: None,
                child_pgid: None,
                on_child_suspend: None,
                daemon_version: String::new(),
            },
        }];
        enrich_entries_with_status(&mut entries);
        assert!(
            matches!(entries[0].status, ListEntryStatus::Stale),
            "unreachable socket must be demoted to Stale"
        );
    }
}
