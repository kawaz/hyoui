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

    // DR-0015 Task #N (2026-05-29 kawaz 指示、完全形): 旧 `__daemonize-run` hidden
    // subcommand + `--cols/--rows/--socket/--session/--ready-fd/--scrollback-rows/
    // --debug-dump/--until` 全 internal arg を **env 1 つにシリアライズ**して渡す。
    // ps からは `hyoui run --detached -- cmd args...` の最終形 (= user-facing と同じ)。
    //
    // daemon は env `HYOUI_DAEMONIZE_INIT` を main entry 直後に parse → unset で
    // 孫 process には漏れない。pipe は ready 通知 (= daemon → parent の bind 完了
    // 通知、env では実装不可) のみ残置。
    let init = DaemonizeInit {
        socket: sock.to_string_lossy().into_owned(),
        session: session_id.clone(),
        ready_fd: wr_raw,
        cols: initial_size.map(|(c, _)| c),
        rows: initial_size.map(|(_, r)| r),
        until: until.filter(|s| !s.is_empty()),
        scrollback_rows,
        debug_dump: debug_dump.filter(|s| !s.is_empty()),
    };
    let init_json = match serde_json::to_string(&init) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyoui: daemon init serialize 失敗: {e}");
            return Err(ExitCode::from(1));
        }
    };

    let mut child = Command::new(exe);
    child.env("HYOUI_DAEMONIZE_INIT", &init_json);
    child.arg("run");
    child.arg("--detached");
    child.arg("--");
    for c in cmd {
        child.arg(c);
    }
    // 子の stdio: stdin は /dev/null (= 旧 stdin pipe size 経路は env 化で廃止)、
    // stdout は /dev/null、stderr は inherit (= §2.3.5 採用パターン、daemon 起動
    // 失敗時の error 文字列を parent / ユーザに伝える)。
    child.stdin(Stdio::null());
    child.stdout(Stdio::null());
    child.stderr(Stdio::inherit());

    let spawn_result = child.spawn();
    // 親側の write 端 fd は spawn 後 close (= 子が close したときに親側 read が
    // EOF を返せるように)。
    let _ = nix::unistd::close(wr_raw);

    if let Err(e) = spawn_result {
        eprintln!("hyoui: spawn 失敗: {e}");
        return Err(ExitCode::from(1));
    }

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

/// DR-0015 Task #N (= kawaz 指示 2026-05-29): daemon 子 process に init 情報を
/// env で渡すための JSON schema。
///
/// 独自 format (= `key=val|key=val`) は escape rule 不在 + path に `|` 含む edge
/// case で壊れる脆さがあるため JSON を採用 (= serde_json で safe encode/decode)。
/// env value は text-safe (= UTF-8 で完結、null byte 不可) なので JSON が自然。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct DaemonizeInit {
    socket: String,
    session: String,
    #[serde(rename = "ready_fd")]
    ready_fd: std::os::fd::RawFd,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    cols: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    until: Option<String>,
    #[serde(
        rename = "scrollback_rows",
        skip_serializing_if = "Option::is_none",
        default
    )]
    scrollback_rows: Option<usize>,
    #[serde(
        rename = "debug_dump",
        skip_serializing_if = "Option::is_none",
        default
    )]
    debug_dump: Option<String>,
}

/// daemon 子 process の本体 (= env `HYOUI_DAEMONIZE_INIT` で JSON init を受け取る)。
///
/// DR-0015 Task #N (2026-05-29 kawaz 指示、env JSON):
/// - 旧 `__daemonize-run` hidden subcommand 廃止
/// - 旧 `--socket=PATH --session=ID --cols=N --rows=N --ready-fd=N --until=... --
///   scrollback-rows=N --debug-dump=...` の internal arg 全廃
/// - **env `HYOUI_DAEMONIZE_INIT` (= JSON) で全 init 情報を受け取る**経路に統一
/// - 残る argv: parent が put した `["run", "--detached", "--", cmd, args...]`
///   (= ps からは通常の `hyoui run --detached -- cmd` に見える、internal flag ゼロ)
///
/// caller (= `hyoui-cli` main entry) が env 存在で本関数に dispatch、子 process
/// の argv は本関数では使わず env から init 情報 + その argv は `run --detached
/// -- cmd args...` から cmd 部分を取得する。
pub fn run_daemon_child() -> ExitCode {
    // env から JSON init を取得 + unset (= 孫 process 漏れ防止)。
    let init_json = match std::env::var("HYOUI_DAEMONIZE_INIT") {
        Ok(s) => s,
        Err(_) => {
            eprintln!("hyoui: daemon child requires HYOUI_DAEMONIZE_INIT env (= internal)");
            return ExitCode::from(2);
        }
    };
    hyoui::sys::env::remove_var_at_startup("HYOUI_DAEMONIZE_INIT");

    let init: DaemonizeInit = match serde_json::from_str(&init_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("hyoui: HYOUI_DAEMONIZE_INIT parse 失敗: {e}");
            return ExitCode::from(2);
        }
    };

    let socket = PathBuf::from(&init.socket);
    let session_id = init.session.clone();
    let cols = init.cols.unwrap_or(80);
    let rows = init.rows.unwrap_or(24);
    let ready_fd: Option<i32> = Some(init.ready_fd);
    let until = init.until.clone();
    let scrollback_rows = init.scrollback_rows;
    let debug_dump = init.debug_dump.clone();

    // 子 cmd は argv `["run", "--detached", "--", cmd, args...]` の "--" 以降を取得。
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut cmd: Vec<String> = Vec::new();
    let mut in_cmd = false;
    for arg in &argv {
        if in_cmd {
            cmd.push(arg.clone());
            continue;
        }
        if arg == "--" {
            in_cmd = true;
        }
    }

    // cwd 取得は chdir("/") の **直前**に行う (= `hyoui run` を叩いた起点 dir を
    // capture して `hyoui list` 表示に使う)。chdir 後だと "/" になってしまう。
    // `current_dir()` は ENOENT (= cwd dir が削除済) 等で失敗する場合がある。
    // status.response の `cwd` は required field なので、daemon は必ず value を
    // 載せる必要がある。失敗時は `/` で fallback (= chdir 先と一致、嘘ではない)。
    let invoked_cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("hyoui: warning: current_dir 取得失敗 ({e}); cwd を `/` として記録");
        std::path::PathBuf::from("/")
    });

    // setsid で新セッションリーダーになる (= controlling tty 切り離し)。
    // 既に session leader の場合は EPERM、無視。
    let _ = nix::unistd::setsid();
    // umask 077 で以降の file 作成を mode 0600 系にする
    nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
    // chdir / (= cwd を free 化、umount 妨げない慣習)
    let _ = nix::unistd::chdir("/");

    let mut dcfg = DaemonConfig::new(session_id, socket, cmd);
    dcfg.cwd = Some(invoked_cwd);
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
