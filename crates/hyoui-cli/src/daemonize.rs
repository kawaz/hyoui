//! `hyoui run --detached` の daemonize 実装。
//!
//! 親 process は `current_exe` を `__daemonize-run --socket=PATH --session=ID --
//! CMD ARGS...` で spawn し、子の socket bind 完了を待ってから **session 名**を
//! stdout に 1 行出して exit する (= `hyoui attach <session>` にそのまま渡せる)。
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
    namespace: String,
    on_child_suspend: hyoui::cli::OnChildSuspend,
    cmd: Vec<String>,
) -> ExitCode {
    match spawn_detached_daemon_and_wait_ready(
        session_id_override,
        socket_override,
        initial_size,
        until,
        scrollback_rows,
        debug_dump,
        namespace,
        on_child_suspend,
        cmd,
    ) {
        Ok((session_id, _sock)) => {
            // 子は live + bind 完了。親は exit。stdout には **session 名** を出力する。
            // (旧版は socket path を出していたが、socket path を session 引数として
            // 渡すと `session_id too long` で弾かれ、README の Quickstart
            // `S=$(hyoui run --detached ...)` → `hyoui attach $S` が動かなかった。
            // breaking change だが v0.x なので OK。)
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
#[allow(clippy::too_many_arguments)]
pub fn spawn_detached_daemon_and_wait_ready(
    session_id_override: Option<String>,
    socket_override: Option<String>,
    initial_size: Option<(u16, u16)>,
    until: Option<String>,
    scrollback_rows: Option<usize>,
    debug_dump: Option<String>,
    namespace: String,
    on_child_suspend: hyoui::cli::OnChildSuspend,
    cmd: Vec<String>,
) -> Result<(String, PathBuf), ExitCode> {
    let session_id = session_id_override.unwrap_or_else(socket_path::auto_session_id);
    let sock = match socket_path::resolve_in_namespace(
        socket_override.as_deref(),
        &session_id,
        &namespace,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui: socket path 解決失敗: {e} (namespace: {namespace})");
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
        // DR-0018: 解決済 namespace を daemon child に伝える (= 子 PTY へ env 注入する値)。
        namespace,
        // DR-0019: 子 STOPPED 時の daemon 挙動を伝える。
        on_child_suspend: Some(child_suspend_str(on_child_suspend).to_string()),
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
    /// DR-0018: 解決済 session namespace。daemon child が `Session::start` 前に
    /// `HYOUI_NAMESPACE=<ns>` を自 env に set し、execvp される子 PTY が継承する。
    /// 旧 daemon child との互換のため `default` で skip (= 未設定なら "default" 扱い)。
    #[serde(default = "default_namespace_field")]
    namespace: String,

    /// DR-0019: 子 STOPPED 時の daemon 挙動 (= `hyoui run --on-child-suspend`)。
    /// "notify" (default) または "auto-resume"。未設定 / 未知値は notify 扱い。
    #[serde(
        rename = "on_child_suspend",
        skip_serializing_if = "Option::is_none",
        default
    )]
    on_child_suspend: Option<String>,
}

/// `DaemonizeInit.namespace` の serde default (= 旧 init JSON 互換)。
fn default_namespace_field() -> String {
    hyoui::cli::DEFAULT_NAMESPACE.to_string()
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
    let cols = init.cols.unwrap_or(80);
    let rows = init.rows.unwrap_or(24);
    let ready_fd: Option<i32> = Some(init.ready_fd);
    let until = init.until.clone();
    let scrollback_rows = init.scrollback_rows;
    let debug_dump = init.debug_dump.clone();
    let on_child_suspend = parse_child_suspend(init.on_child_suspend.as_deref());

    // DR-0018: 子 PTY に `HYOUI_NAMESPACE` を **常時注入** (= default でも注入)。
    // daemon child 自身の env に set しておくと、`Session::start` が fork+execvp する
    // 子 PTY がそれを継承する。ここはまだ single-threaded (= Session::start 前) なので
    // set_var_at_startup の契約を満たす。`HYOUI_DAEMONIZE_INIT` (= 上で unset 済) と
    // 違い、これは子に意図的に伝える env なので unset しない。
    // 用途: ns 内でネスト起動した hyoui が指定なしで同 ns を引き継ぐ (= 自己検出にも使える)。
    hyoui::sys::env::set_var_at_startup("HYOUI_NAMESPACE", &init.namespace);

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
}
