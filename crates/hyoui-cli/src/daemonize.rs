//! `hyoui run --detached` の daemonize 実装。
//!
//! 親 process は `current_exe` を `run --detached -- CMD ARGS...` (init 情報は env
//! `HYOUI_DAEMONIZE_INIT`) で spawn し、子の socket bind 完了を待ってから **session id**
//! を stdout に 1 行出して exit する (= `hyoui attach <session>` にそのまま渡せる)。
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
/// を待ってから親は exit。stdout に **session 名** を 1 行出力する
/// (= `hyoui attach <session>` にそのまま渡せる)。
#[allow(clippy::too_many_arguments)]
pub fn run_detached_parent(
    session_id_override: Option<String>,
    socket_override: Option<String>,
    initial_size: Option<(u16, u16)>,
    until: Option<String>,
    scrollback_rows: Option<usize>,
    debug_dump: Option<String>,
    on_child_suspend: hyoui::cli::OnChildSuspend,
    timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
    scrub_env: Option<hyoui::sys::env_scrub::ScrubPlan>,
    login: bool,
    term_fallback: String,
    child_stdin: bool,
    tags: std::collections::BTreeMap<String, String>,
    cmd: Vec<String>,
) -> ExitCode {
    match spawn_detached_daemon_and_wait_ready(
        session_id_override,
        socket_override,
        initial_size,
        until,
        scrollback_rows,
        debug_dump,
        on_child_suspend,
        timeout_ms,
        idle_timeout_ms,
        scrub_env,
        login,
        term_fallback,
        child_stdin,
        tags,
        cmd,
    ) {
        Ok((session_id, _sock)) => {
            // 子は live + bind 完了。親は exit。stdout には **session id** を出力する
            // (= `S=$(hyoui run --detached ...)` → `hyoui attach $S` にそのまま渡せる)。
            println!("{session_id}");
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
///
/// `child_stdin = true` なら呼び出し元の stdin を daemon に継承させ、daemon がそれを子の
/// fd 0 にする (DR-0042 決定 1 / 2)。`false` なら daemon の stdin は `/dev/null` で、子の
/// stdin は PTY。
#[allow(clippy::too_many_arguments)]
pub fn spawn_detached_daemon_and_wait_ready(
    session_id_override: Option<String>,
    socket_override: Option<String>,
    initial_size: Option<(u16, u16)>,
    until: Option<String>,
    scrollback_rows: Option<usize>,
    debug_dump: Option<String>,
    on_child_suspend: hyoui::cli::OnChildSuspend,
    timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
    scrub_env: Option<hyoui::sys::env_scrub::ScrubPlan>,
    login: bool,
    term_fallback: String,
    child_stdin: bool,
    tags: std::collections::BTreeMap<String, String>,
    cmd: Vec<String>,
) -> Result<(String, PathBuf), ExitCode> {
    let session_id = session_id_override.unwrap_or_else(socket_path::auto_session_id);
    let sock = match socket_path::resolve(socket_override.as_deref(), &session_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: socket path 解決失敗: {e} (session: {session_id})");
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
        // DR-0019: 子 STOPPED 時の daemon 挙動を伝える。
        on_child_suspend: Some(child_suspend_str(on_child_suspend).to_string()),
        // DR-0019 §4: overall / idle timeout を daemon に伝える (= --until と同経路)。
        timeout_ms,
        idle_timeout_ms,
        // DR-0023: 親で解決した env scrub patterns を daemon child に渡す。
        scrub_env,
        login,
        // DR-0039 決定 1: 呼び出し元に TERM が無い時に子へ設定する値 (config 由来)。
        term_fallback: Some(term_fallback),
        // DR-0042: 引き継いだ stdin を子の fd 0 にするか。
        child_stdin,
        // DR-0041 決定 1: session の tag (daemon が status で返す)。
        tags,
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
    if login {
        // ps から login 起動と分かるように見せる (= 解釈は init JSON の `login`)。
        child.arg("--login");
    }
    child.arg("--");
    for c in cmd {
        child.arg(c);
    }
    // 子の stdio: stdin は子に渡す時だけ呼び出し元のものを引き継ぎ (DR-0042、daemon が子の
    // fd 0 にする)、それ以外は /dev/null。stdout は /dev/null、stderr は inherit
    // (= §2.3.5 採用パターン、daemon 起動失敗時の error 文字列を parent / ユーザに伝える)。
    if init.child_stdin {
        child.stdin(Stdio::inherit());
    } else {
        child.stdin(Stdio::null());
    }
    child.stdout(Stdio::null());
    child.stderr(Stdio::inherit());

    let spawn_result = child.spawn();
    // 親側の write 端 fd は spawn 後 close (= 子が close したときに親側 read が
    // EOF を返せるように)。
    let _ = nix::unistd::close(wr_raw);

    let child_proc = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hyoui: spawn 失敗: {e}");
            return Err(ExitCode::from(1));
        }
    };
    let child_pid = child_proc.id();

    // ready pipe から 1 byte 読む (= 子が ready 通知)。fd 継承異常などで子が
    // ready byte を書かず、かつ close もしない (= EOF が来ない) 場合に親が無言
    // hang するのを防ぐため、poll で READY_TIMEOUT を被せる。
    let n = read_ready_with_timeout(&rd, READY_TIMEOUT);
    drop(rd);

    match n {
        ReadyOutcome::Ready => Ok((session_id, sock)),
        ReadyOutcome::Eof => {
            // 子が ready を書かずに pipe を閉じた (= Session::start 失敗で early exit 等)。
            eprintln!(
                "hyoui: daemon child failed to start (pid: {child_pid}, socket: {})",
                sock.display()
            );
            Err(ExitCode::from(1))
        }
        ReadyOutcome::Timeout => {
            eprintln!(
                "hyoui: daemon child が {timeout_s}s 以内に ready 通知を返しませんでした \
                 (= fd 継承異常 / 起動遅延の可能性、pid: {child_pid}, socket: {sock})\n\
                 \x20      `hyoui list` で起動有無を確認し、不要なら `kill {child_pid}` で停止してください。",
                timeout_s = READY_TIMEOUT.as_secs(),
                sock = sock.display(),
            );
            Err(ExitCode::from(1))
        }
        ReadyOutcome::Error(e) => {
            eprintln!(
                "hyoui: daemon child の ready 通知 read 失敗: {e} (pid: {child_pid}, socket: {})",
                sock.display()
            );
            Err(ExitCode::from(1))
        }
    }
}

/// daemon child の ready 通知を待つ poll タイムアウト。
///
/// 子は `Session::start` 直後 (= socket bind 完了直後) に ready byte を書くため、
/// 健全時は数十〜数百 ms で届く。fd 継承異常で子が ready を書かず close もしない
/// 異常時に親が無言 hang するのを防ぐための上限値。
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// [`read_ready_with_timeout`] の結果。
enum ReadyOutcome {
    /// ready byte (1 byte) を受信した。
    Ready,
    /// 子が ready を書かず pipe を閉じた (= EOF / read 0)。
    Eof,
    /// timeout 超過で ready byte が届かなかった。
    Timeout,
    /// poll / read の I/O error。
    Error(String),
}

/// ready pipe の read 端を poll で監視し、最大 `timeout` 待って 1 byte 読む。
///
/// EINTR は signal 割り込みなので通算 deadline で re-poll する。
fn read_ready_with_timeout(
    rd: &impl std::os::fd::AsFd,
    timeout: std::time::Duration,
) -> ReadyOutcome {
    use hyoui::sys::poll::{PollFlags, PollOutcome, poll};
    use nix::poll::{PollFd, PollTimeout};
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return ReadyOutcome::Timeout;
        }
        let mut fds = [PollFd::new(rd.as_fd(), PollFlags::POLLIN)];
        let to = PollTimeout::try_from(remaining.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(PollTimeout::NONE);
        match poll(&mut fds, to) {
            Ok(PollOutcome::Ready(_)) => {
                let re = fds[0].revents().unwrap_or(PollFlags::empty());
                if re.contains(PollFlags::POLLIN) {
                    let mut buf = [0u8; 1];
                    return match nix::unistd::read(rd, &mut buf) {
                        Ok(1) => ReadyOutcome::Ready,
                        Ok(_) => ReadyOutcome::Eof, // read 0 = EOF (= 子が close)
                        Err(e) => ReadyOutcome::Error(e.to_string()),
                    };
                }
                if re.contains(PollFlags::POLLHUP) {
                    // 子が ready を書かず pipe を閉じた。残データがあるかもしれないので
                    // 1 回 read を試みる (= POLLHUP でも buffered byte は読める)。
                    let mut buf = [0u8; 1];
                    return match nix::unistd::read(rd, &mut buf) {
                        Ok(1) => ReadyOutcome::Ready,
                        Ok(_) => ReadyOutcome::Eof,
                        Err(e) => ReadyOutcome::Error(e.to_string()),
                    };
                }
                // 想定外 revents は re-poll。
            }
            Ok(PollOutcome::Timeout) => return ReadyOutcome::Timeout,
            Ok(PollOutcome::Interrupted) => continue,
            Ok(_) => continue,
            Err(e) => return ReadyOutcome::Error(e.to_string()),
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

    /// DR-0019: 子 STOPPED 時の daemon 挙動 (= `hyoui run --on-child-suspend`)。
    /// "notify" (default) または "auto-resume"。未設定 / 未知値は notify 扱い。
    #[serde(
        rename = "on_child_suspend",
        skip_serializing_if = "Option::is_none",
        default
    )]
    on_child_suspend: Option<String>,

    /// DR-0019 §4: overall timeout ミリ秒 (= `hyoui run --timeout`)。
    /// 旧 init JSON 互換のため `default` で skip (= 未設定なら無効)。
    #[serde(
        rename = "timeout_ms",
        skip_serializing_if = "Option::is_none",
        default
    )]
    timeout_ms: Option<u64>,

    /// DR-0019 §4: idle timeout ミリ秒 (= `hyoui run --idle-timeout`)。
    /// 旧 init JSON 互換のため `default` で skip (= 未設定なら無効)。
    #[serde(
        rename = "idle_timeout_ms",
        skip_serializing_if = "Option::is_none",
        default
    )]
    idle_timeout_ms: Option<u64>,

    /// DR-0024: 子 PTY env scrub の解決済 plan (= patterns + keep)。
    /// - `None` = scrub 完全 disable (= `--no-scrub-env` または config の
    ///   `scrub_env_enabled = false`、または旧 init JSON 互換)
    /// - `Some(plan)` = daemon child が `env_scrub::apply` でこれを適用
    ///   (= 空 patterns は target builtin なし & user 設定なしの no-op を表現)
    #[serde(rename = "scrub_env", skip_serializing_if = "Option::is_none", default)]
    scrub_env: Option<hyoui::sys::env_scrub::ScrubPlan>,

    /// DR-0039 決定 1: `hyoui run --login`。daemon child が passwd から shell を引き、
    /// 子 PTY を最小 env + login argv[0] で起動する。旧 init JSON 互換のため `default`
    /// (= false) で skip。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    login: bool,

    /// DR-0042: 引き継いだ stdin (fd 0) を子の fd 0 にする。`false` (= field 無しの init
    /// JSON を含む) なら子の stdin は PTY。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    child_stdin: bool,

    /// DR-0039 決定 1: 呼び出し元に `TERM` が無い (未設定 / 空) 時に子へ設定する値
    /// (= config `[session] term_fallback`)。旧 init JSON 互換のため `default` (= None)
    /// で、None は builtin 既定値に倒す。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    term_fallback: Option<String>,

    /// DR-0041 決定 1: session の tag (`hyoui run --tag`)。field の無い init JSON は空。
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty", default)]
    tags: std::collections::BTreeMap<String, String>,
}

/// `cli::OnChildSuspend` を DaemonizeInit JSON で運ぶ文字列に変換。
///
/// `OnChildSuspend` は `#[non_exhaustive]` のため wildcard が必要。
/// 未知 variant は安全側の "notify" (= 勝手に子を起こさない) に倒す。
fn child_suspend_str(p: hyoui::cli::OnChildSuspend) -> &'static str {
    match p {
        hyoui::cli::OnChildSuspend::AutoResume => "auto-resume",
        _ => "notify",
    }
}

/// DaemonizeInit JSON の文字列を daemon 層の `ChildSuspendPolicy` に解決。
/// 未設定 / 未知値は notify (= 安全側、勝手に子を起こさない) に倒す。
fn parse_child_suspend(s: Option<&str>) -> hyoui::daemon::ChildSuspendPolicy {
    match s {
        Some("auto-resume") => hyoui::daemon::ChildSuspendPolicy::AutoResume,
        _ => hyoui::daemon::ChildSuspendPolicy::Notify,
    }
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
    // 0 は「サイズが取れなかった / 不正指定」なので未指定と同じ daemon default に
    // 倒す (= 1x1 に clamp すると「起動はしたが画面が壊れている」に化けるため、
    // 初期サイズ経路だけは clamp ではなく default fallback を選ぶ)。
    let cols = init.cols.filter(|&c| c != 0).unwrap_or(80);
    let rows = init.rows.filter(|&r| r != 0).unwrap_or(24);
    let ready_fd: Option<i32> = Some(init.ready_fd);
    let until = init.until.clone();
    let scrollback_rows = init.scrollback_rows;
    let debug_dump = init.debug_dump.clone();
    let on_child_suspend = parse_child_suspend(init.on_child_suspend.as_deref());
    let timeout_ms = init.timeout_ms;
    let idle_timeout_ms = init.idle_timeout_ms;

    // DR-0024: 子 PTY 継承用 environ から親 Internal Context env (例: 親 Claude Code
    // の `CLAUDE_CODE_SESSION_ID` 等) を scrub する。`HYOUI_SESSION_ID`
    // 注入の **前** に実施するのは、user config の kill_glob が `HYOUI_*` を巻き添えに
    // してしまうのを protected guard で防ぐが、それでも順序として「漏れ削除 → 意図的注入」
    // が読みやすいため。daemon は single-threaded (= Session::start 前) なので `apply` の
    // async-signal-unsafe 制約に抵触しない。
    if let Some(plan) = init.scrub_env.as_ref() {
        let _result = hyoui::sys::env_scrub::apply(plan);
        // log は default 無音 (DR-0024 §10)。観測したい場合は将来 HYOUI_VERBOSE 等で
        // opt-in する (Future work)。_result は捨てるが apply は環境を実際に書き換え済。
    }

    // DR-0020 §1: 子 PTY に `HYOUI_SESSION_ID` を **常時注入** (= 自己参照の必然、
    // tmux `$TMUX` / screen `$STY` 慣行と同枠の透過例外)。daemon child 自身の env に
    // set しておくと `Session::start` が fork+execvp する子 PTY がそれを継承する。
    // ここは `Session::start` 前で single-threaded なので set_var_at_startup の契約を
    // 満たす。子 process (= shell / AI agent) が自セッションを操作する省略時解決
    // (DR-0020 §2) の入力になる。`HYOUI_DAEMONIZE_INIT` (= 上で unset 済) と違い、
    // 子に意図的に伝える env なので unset しない。
    hyoui::sys::env::set_var_at_startup("HYOUI_SESSION_ID", &session_id);

    // DR-0039 決定 1: 子の TERM は呼び出し元を引き継ぎ、無い (未設定 / 空) 時だけ
    // config の既定値にする。`--login` も普通の run もここで決めた 1 つの値を使う
    // (`--login` は下の最小 env に、普通の run は継承される daemon の environ に載る)。
    // daemon の environ は呼び出し元の写しなので、ここで読む TERM が呼び出し元の値。
    let term_fallback = init
        .term_fallback
        .clone()
        .unwrap_or_else(hyoui::config::default_term_fallback);
    let caller_term = std::env::var("TERM").ok();
    let session_term =
        hyoui::sys::login::resolve_session_term(caller_term.as_deref(), &term_fallback);
    if let Some(t) = session_term.as_deref()
        && caller_term.as_deref() != Some(t)
    {
        hyoui::sys::env::set_var_at_startup("TERM", t);
    }

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

    // DR-0042 決定 2: 引き継いだ stdin (fd 0) を CLOEXEC 付きの別 fd に移し、fd 0 は
    // /dev/null に戻す。`Session::start_with_child_stdin` が子の fd 0 に dup2 し、spawn 直後に
    // 閉じる (= daemon は pipe を持ち続けない。fd 0 で持ったままだと upgrade の self-exec に
    // 漏れ、子が閉じた後も書き手に EPIPE が届かない)。
    //
    // 渡せない時は子を spawn せず daemon の起動失敗にする (DR-0042 決定 6、fail closed)。
    // 子の stdin を PTY に倒して続けると、利用者が選んでいない配線 (= `--pty-stdin` 相当) で
    // 子が動き、pipe の中身は届かないまま黙って進む。ready を書かずに終わるので、親 (run の
    // CLI) は起動失敗として非 0 で終わる (stderr は親から継承しているので文言も届く)。
    let child_stdin = if init.child_stdin {
        match take_stdin_for_child() {
            Ok(fd) => Some(fd),
            Err(e) => {
                eprintln!("{}", child_stdin_failure_message(&e));
                return ExitCode::from(1);
            }
        }
    } else {
        None
    };

    // setsid で新セッションリーダーになる (= controlling tty 切り離し)。
    // 既に session leader の場合は EPERM、無視。
    let _ = nix::unistd::setsid();
    // umask 077 で以降の file 作成を mode 0600 系にする
    nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
    // chdir / (= cwd を free 化、umount 妨げない慣習)
    let _ = nix::unistd::chdir("/");

    // DR-0039 決定 1: `--login` は子の argv / exec 先 / environ を最小 env から組み直す。
    // daemon 自身の environ (= 面の root を決める HYOUI_STATE_DIR / XDG_* 等) は触らない。
    // 子に渡す environ だけを `child_exec` に持たせ、hyoui の常時注入 env (DR-0020) は
    // 最小化しても残す。HYOUI_LOCK_TOKEN は最小 env に入らない (= 子に漏れない)。
    let (cmd, child_exec) = if init.login {
        let user = match hyoui::sys::login::current_user() {
            Ok(u) => u,
            Err(e) => {
                eprintln!("hyoui: --login: passwd の引き当てに失敗: {e}");
                return ExitCode::from(1);
            }
        };
        let mut plan = hyoui::sys::login::plan(
            &user,
            &hyoui::sys::login::initial_path(),
            &hyoui::sys::login::LoginCaller::from_env(session_term.clone()),
            &cmd,
        );
        plan.env
            .push(("HYOUI_SESSION_ID".to_string(), init.session.clone()));
        // DR-0041 決定 6: 面の変数は最小 env でも引き継ぐ (= 子の中で起こす hyoui が親と
        // 同じ面を使い、HYOUI_SESSION_ID の自己参照が届く)。設定されている時だけ渡す。
        if let Some(face) = std::env::var_os("HYOUI_STATE_DIR").filter(|v| !v.is_empty()) {
            plan.env.push((
                "HYOUI_STATE_DIR".to_string(),
                face.to_string_lossy().into_owned(),
            ));
        }
        (
            plan.argv,
            Some(hyoui::daemon::ChildLaunch {
                path: plan.path,
                env: plan.env,
            }),
        )
    } else {
        (cmd, None)
    };

    let mut dcfg = DaemonConfig::new(session_id, socket, cmd);
    dcfg.child_exec = child_exec;
    dcfg.cwd = Some(invoked_cwd);
    dcfg.cols = cols;
    dcfg.rows = rows;
    // Round2 #2: HYOUI_LOCK_TOKEN env を daemon 側の expected_token に伝搬。
    // detached daemon child は親 process から env を受け継ぐ。
    if let Ok(token) = std::env::var("HYOUI_LOCK_TOKEN")
        && !token.is_empty()
    {
        dcfg.expected_token = Some(token);
    }
    // 取り込み後は env を unset する (= HYOUI_DAEMONIZE_INIT と同じ流儀)。daemon が
    // spawn する子 PTY (= 実コマンド) に HYOUI_LOCK_TOKEN が継承されると、子が env
    // 経由で lock token を読めて認証境界を越えてしまう (= lock holder になりすませる)。
    // daemon の expected_token として取り込んだ後は子に漏らさない。
    hyoui::sys::env::remove_var_at_startup("HYOUI_LOCK_TOKEN");
    // R5-FB1: --until pattern を daemon に配線。
    if let Some(needle) = until
        && !needle.is_empty()
    {
        dcfg.until = Some(needle);
    }
    // DR-0019: 子 STOPPED 時の daemon 挙動 (notify / auto-resume) を配線。
    dcfg.on_child_suspend = on_child_suspend;
    // DR-0019 §4: overall / idle timeout を配線 (= daemon 側終了条件、--until と同経路)。
    dcfg.timeout_ms = timeout_ms;
    dcfg.idle_timeout_ms = idle_timeout_ms;
    // DR-0041 決定 1: session の tag。
    dcfg.tags = init.tags.clone();
    // DR-0013 §8 + §8 Update: scrollback rows 上限を daemon に配線。
    if let Some(n) = scrollback_rows {
        dcfg.screen_vt100_scrollback_rows = n;
    }
    // `--debug-dump=<path>` を daemon に配線 (= 子 PTY raw bytes を file に append)。
    if let Some(p) = debug_dump {
        dcfg.debug_dump_path = Some(PathBuf::from(p));
    }

    let session = match Session::start_with_child_stdin(dcfg, child_stdin) {
        Ok(s) => s,
        Err(hyoui::Error::SocketExists(path)) => {
            eprintln!("{}", session_exists_message(&init.session, &path));
            return ExitCode::from(1);
        }
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

/// 同じ id の socket が既にあって起動できなかった時の文言 (DR-0041 決定 3)。
///
/// run は相手の daemon の生死を判定しない (= 片付けの経路の責務) ので、原因は「同じ id
/// の socket が既にある」だけを言い、対処に生きている時と死んで残った時の両方を書く。
fn session_exists_message(session_id: &str, socket: &std::path::Path) -> String {
    format!(
        "hyoui: run: session id {session_id} の socket が既にあるため起動しません ({})\n\
         \x20      別の id で起動するか (--session-id を外すと hyoui が振ります)、その session を\n\
         \x20      片付けてから再実行してください: 動いていれば `hyoui kill {session_id}`、\n\
         \x20      daemon が死んで socket だけ残っていれば `hyoui list` が片付けます\n\
         \x20      (`hyoui list` が stale と表示して残す socket は、daemon が居ないことを\n\
         \x20      確かめてから手で消してください)。",
        socket.display()
    )
}

/// 呼び出し元の stdin を子に渡せなかった時の文言 (DR-0042 決定 6)。原因と次の行動を書く。
fn child_stdin_failure_message(cause: &str) -> String {
    format!(
        "hyoui: 呼び出し元の stdin を子に渡せないため、子を起動しません ({cause})。\n\
         \x20      開ける fd の上限 (`ulimit -n`) と stdin を確かめて起動し直すか、stdin を子に渡さない\n\
         \x20      なら `hyoui run --pty-stdin` で起動してください (子の stdin も PTY になります)。"
    )
}

/// fd 0 を CLOEXEC 付きの新しい fd に複製して返し、fd 0 を /dev/null に置き換える
/// (DR-0042 決定 2、子の fd 0 にする呼び出し元の stdin)。
fn take_stdin_for_child() -> Result<std::os::fd::OwnedFd, String> {
    let raw = nix::fcntl::fcntl(std::io::stdin(), nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(3))
        .map_err(|e| format!("fcntl(F_DUPFD_CLOEXEC): {e}"))?;
    let fd = hyoui::sys::raw::own_raw_fd(raw);
    hyoui::sys::raw::redirect_stdin_to_devnull().map_err(|e| format!("dup2(/dev/null, 0): {e}"))?;
    Ok(fd)
}

/// DR-0028 Phase 1: self-exec upgrade で継承した fd から daemon serve を再開する。
///
/// 呼び出し前提: main entry が `HYOUI_UPGRADE_RESUME=1` を検知して本関数に dispatch
/// している (= single-threaded、`set_var_at_startup` / `remove_var_at_startup`
/// の契約を満たす)。
///
/// 手順:
/// 1. `daemon::upgrade::read_upgrade_env()` で env から fd 番号 / session_id /
///    socket path / cols / rows / child_pid を取り出す
/// 2. 継承 fd を `own_raw_fd` で `OwnedFd` 化 (= execve 前に CLOEXEC 解除済み、
///    kernel が exec 後も fd を保持している前提)
/// 3. `Session::from_upgrade_inherited` で Session を組み立て、通常の serve loop
///    に合流する
/// 4. upgrade 系 env は unset で孫プロセスに漏らさない
///
/// Phase 1 制約: `DaemonConfig` は最小 field のみ復元する (= session_id / socket /
/// cmd 空 / cols / rows)。until / on_child_suspend / scrollback 上限 / record 継続
/// 等の高度な引き継ぎは Phase 2 で一時ファイル (CBOR) 経由に拡張する。**exec 元
/// daemon で有効だった `--until` / `--timeout` は本 PoC では消える** ため、実運用の
/// upgrade はまだ推奨できない (= DR-0028 §Phase 2 gate 到達まで隠し経路)。
pub fn run_upgrade_resume_child() -> ExitCode {
    use hyoui::daemon::upgrade;
    use nix::unistd::Pid;

    let env = match upgrade::read_upgrade_env() {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    // 孫プロセスへの漏れを防ぐため upgrade env は全て unset (= HYOUI_DAEMONIZE_INIT
    // 経路と同じ流儀)。ENV_UPGRADE_STATE_FILE も含む (= 読み込み後の path 情報が
    // 孫 process に漏れないように)。
    for k in [
        upgrade::ENV_UPGRADE_RESUME,
        upgrade::ENV_UPGRADE_PTY_FD,
        upgrade::ENV_UPGRADE_LISTENER_FD,
        upgrade::ENV_UPGRADE_LOCK_FD,
        upgrade::ENV_UPGRADE_CHILD_PID,
        upgrade::ENV_UPGRADE_SESSION,
        upgrade::ENV_UPGRADE_SOCKET,
        upgrade::ENV_UPGRADE_COLS,
        upgrade::ENV_UPGRADE_ROWS,
        upgrade::ENV_UPGRADE_STATE_FILE,
    ] {
        hyoui::sys::env::remove_var_at_startup(k);
    }

    // 継承 fd を OwnedFd 化。前 daemon が CLOEXEC を解いてから execve したので
    // kernel には有効な fd として残っている。
    let master_owned = hyoui::sys::raw::own_raw_fd(env.pty_fd);
    let listener_owned = hyoui::sys::raw::own_raw_fd(env.listener_fd);
    // 継承 lock fd は同じ open file description なので再 flock は成功する (= exec 跨ぎで
    // lock は外れない)。失敗時は panic せず None にし、`UnixSock::resume_inherited` の
    // 取り直しに任せる (= 子を抱えた upgrade を lock 都合で落とさない)。
    let lock = env.lock_fd.and_then(|fd| {
        let file = std::fs::File::from(hyoui::sys::raw::own_raw_fd(fd));
        match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
            Ok(held) => Some(held),
            Err((_, e)) => {
                eprintln!("hyoui: warning: inherited daemon lock fd {fd} unusable: {e}");
                None
            }
        }
    });

    // DR-0028 Phase 2: state file (CBOR versioned) から DaemonConfig / scrollback
    // bytes / 子 PID を復元する。state file が読めない / decode 失敗 / version
    // mismatch のいずれかで fallback へ (= env 最小 subset で dummy cmd resume)。
    let (dcfg, scrollback_bytes, child_pid) = match env
        .state_file
        .as_deref()
        .map(upgrade::read_and_consume_state_file)
    {
        Some(Ok(state)) => {
            let cmd = if state.cmd.is_empty() {
                vec!["<upgrade-resume>".to_string()]
            } else {
                state.cmd.clone()
            };
            let mut dcfg = hyoui::daemon::DaemonConfig::new(
                state.session_id.clone(),
                state.socket_path.clone(),
                cmd,
            );
            let (cols, rows) = hyoui::daemon::normalize_size(state.cols, state.rows);
            dcfg.cols = cols;
            dcfg.rows = rows;
            dcfg.scrollback_bytes = state.scrollback_bytes;
            dcfg.screen_input_log_bytes = state.screen_input_log_bytes;
            dcfg.screen_vt100_scrollback_rows = state.screen_vt100_scrollback_rows;
            dcfg.client_buffer_bytes = state.client_buffer_bytes;
            dcfg.expected_token = state.expected_token.clone();
            dcfg.until = state.until.clone();
            dcfg.debug_dump_path = state.debug_dump_path.clone();
            dcfg.cwd = state.cwd.clone();
            dcfg.on_child_suspend = upgrade::parse_on_child_suspend(&state.on_child_suspend);
            dcfg.timeout_ms = state.timeout_ms;
            dcfg.idle_timeout_ms = state.idle_timeout_ms;
            dcfg.tags = state.tags.clone();
            eprintln!(
                "hyoui: upgrade-resume state file loaded (session={}, cmd={:?}, scrollback={} bytes, prev_boot_id={})",
                state.session_id,
                state.cmd,
                state.scrollback.len(),
                state.daemon_boot_id_prev,
            );
            (dcfg, state.scrollback, state.child_pid)
        }
        Some(Err(msg)) => {
            eprintln!(
                "hyoui: upgrade-resume state file unusable ({msg}); env-only minimum path (= scrollback lost, config detail lost)"
            );
            let mut dcfg = hyoui::daemon::DaemonConfig::new(
                env.session_id.clone(),
                env.socket.clone(),
                vec!["<upgrade-resume>".to_string()],
            );
            let (cols, rows) = hyoui::daemon::normalize_size(env.cols, env.rows);
            dcfg.cols = cols;
            dcfg.rows = rows;
            (dcfg, Vec::new(), env.child_pid)
        }
        None => {
            eprintln!(
                "hyoui: upgrade-resume no state file env; env-only minimum path (= scrollback lost, config detail lost)"
            );
            let mut dcfg = hyoui::daemon::DaemonConfig::new(
                env.session_id.clone(),
                env.socket.clone(),
                vec!["<upgrade-resume>".to_string()],
            );
            let (cols, rows) = hyoui::daemon::normalize_size(env.cols, env.rows);
            dcfg.cols = cols;
            dcfg.rows = rows;
            (dcfg, Vec::new(), env.child_pid)
        }
    };

    let mut session = match hyoui::daemon::Session::from_upgrade_inherited(
        dcfg,
        master_owned,
        listener_owned,
        lock,
        Pid::from_raw(child_pid),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyoui (upgrade-resume child): Session::from_upgrade_inherited failed: {e}");
            return ExitCode::from(1);
        }
    };
    if !scrollback_bytes.is_empty() {
        session.set_upgrade_scrollback(scrollback_bytes);
    }

    eprintln!(
        "hyoui: upgrade-resume ready (session={}, socket={}, child_pid={}, pty_fd={}, listener_fd={})",
        env.session_id,
        env.socket.display(),
        child_pid,
        env.pty_fd,
        env.listener_fd,
    );

    match session.serve() {
        Ok(_code) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyoui::cli::OnChildSuspend;
    use hyoui::daemon::ChildSuspendPolicy;

    /// DR-0019: `--on-child-suspend=auto-resume` が DaemonizeInit JSON を round-trip
    /// して daemon 層の `ChildSuspendPolicy::AutoResume` に解決される (= 配線の正本)。
    #[test]
    fn daemonize_init_propagates_auto_resume() {
        let init = DaemonizeInit {
            socket: "/tmp/x.sock".into(),
            session: "demo".into(),
            ready_fd: 7,
            on_child_suspend: Some(child_suspend_str(OnChildSuspend::AutoResume).to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&init).expect("serialize");
        assert!(json.contains("auto-resume"));
        let decoded: DaemonizeInit = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            parse_child_suspend(decoded.on_child_suspend.as_deref()),
            ChildSuspendPolicy::AutoResume
        );
    }

    /// notify は明示でも round-trip して `Notify` に解決される。
    #[test]
    fn daemonize_init_propagates_notify() {
        let init = DaemonizeInit {
            socket: "/tmp/x.sock".into(),
            session: "demo".into(),
            ready_fd: 7,
            on_child_suspend: Some(child_suspend_str(OnChildSuspend::Notify).to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&init).expect("serialize");
        let decoded: DaemonizeInit = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            parse_child_suspend(decoded.on_child_suspend.as_deref()),
            ChildSuspendPolicy::Notify
        );
    }

    /// 旧 init JSON (= on_child_suspend field 無し) は None → Notify に倒れる
    /// (= 安全側、勝手に子を起こさない)。
    #[test]
    fn daemonize_init_missing_field_defaults_to_notify() {
        let json = r#"{"socket":"/tmp/x.sock","session":"demo","ready_fd":7}"#;
        let decoded: DaemonizeInit = serde_json::from_str(json).expect("deserialize legacy");
        assert_eq!(decoded.on_child_suspend, None);
        assert_eq!(
            parse_child_suspend(decoded.on_child_suspend.as_deref()),
            ChildSuspendPolicy::Notify
        );
    }

    /// 未知値も安全側の Notify に倒す。
    #[test]
    fn parse_child_suspend_unknown_falls_back_to_notify() {
        assert_eq!(
            parse_child_suspend(Some("bogus")),
            ChildSuspendPolicy::Notify
        );
        assert_eq!(parse_child_suspend(None), ChildSuspendPolicy::Notify);
        assert_eq!(
            parse_child_suspend(Some("auto-resume")),
            ChildSuspendPolicy::AutoResume
        );
    }

    /// DR-0019 §4: timeout / idle-timeout が DaemonizeInit JSON を round-trip する
    /// (= --until と同経路の配線の正本)。
    #[test]
    fn daemonize_init_propagates_timeouts() {
        let init = DaemonizeInit {
            socket: "/tmp/x.sock".into(),
            session: "demo".into(),
            ready_fd: 7,
            timeout_ms: Some(60_000),
            idle_timeout_ms: Some(5_000),
            ..Default::default()
        };
        let json = serde_json::to_string(&init).expect("serialize");
        assert!(json.contains("timeout_ms"), "json: {json}");
        assert!(json.contains("idle_timeout_ms"), "json: {json}");
        let decoded: DaemonizeInit = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.timeout_ms, Some(60_000));
        assert_eq!(decoded.idle_timeout_ms, Some(5_000));
    }

    /// DR-0042: 子に stdin を渡すかが DaemonizeInit JSON を round-trip する。field 無しの
    /// JSON は渡さない (= 子の stdin は PTY)。
    #[test]
    fn daemonize_init_propagates_child_stdin() {
        for child_stdin in [true, false] {
            let init = DaemonizeInit {
                socket: "/tmp/x.sock".into(),
                session: "demo".into(),
                ready_fd: 7,
                child_stdin,
                ..Default::default()
            };
            let json = serde_json::to_string(&init).expect("serialize");
            let decoded: DaemonizeInit = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded.child_stdin, child_stdin);
        }
        let legacy = r#"{"socket":"/tmp/x.sock","session":"demo","ready_fd":7}"#;
        let decoded: DaemonizeInit = serde_json::from_str(legacy).expect("deserialize legacy");
        assert!(!decoded.child_stdin);
    }

    /// 旧 init JSON (= timeout field 無し) は None に倒れる (= 無効、互換維持)。
    #[test]
    fn daemonize_init_missing_timeout_fields_default_to_none() {
        let json = r#"{"socket":"/tmp/x.sock","session":"demo","ready_fd":7}"#;
        let decoded: DaemonizeInit = serde_json::from_str(json).expect("deserialize legacy");
        assert_eq!(decoded.timeout_ms, None);
        assert_eq!(decoded.idle_timeout_ms, None);
    }
}
