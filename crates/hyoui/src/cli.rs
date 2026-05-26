//! Command-line argument parser for the `hyoui` binary.
//!
//! This module is a pure function over `&[String]` (argv excluding argv[0]).
//! It performs no I/O and spawns no processes, so it can be exhaustively
//! covered by unit tests.
//!
//! # Subcommand layout
//!
//! ```text
//! hyoui <subcommand> [options]
//! ```
//!
//! Initially supported subcommands:
//!
//! * `run` — execute a child command inside a PTY as a transparent proxy.
//!   Mirrors the original (single-command) bootstrap CLI; the child argv goes
//!   after a `--` separator: `hyoui run [opts] -- cmd [args...]`.
//! * `completion <shell>` — print a shell completion script.
//!
//! Reserved (not yet implemented): `send`, `attach`, `status` for socket-based
//! remote control.
//!
//! When no subcommand is given, or an unknown subcommand is supplied, or the
//! user passes `--help` / `-h`, the parser returns `Command::Help`. There is
//! intentionally **no** shortcut that treats `hyoui -- cmd` as `hyoui run --
//! cmd`; the subcommand must be explicit.

use std::fmt;

/// Operating mode for the `run` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// Pass the parent terminal through (default).
    Interactive,
    /// Drive the child with a virtual PTY of fixed size; no terminal needed.
    Headless,
}

/// Behavior when the child process is suspended (STOPPED).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnChildSuspend {
    /// Follow the child: the parent also stops (SIGSTOP raised on self).
    Follow,
    /// Resume the child immediately by sending SIGCONT.
    AutoResume,
}

/// Behavior when the parent process is suspended (receives SIGTSTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnParentSuspend {
    /// Stop the child group first, then stop the parent.
    Transparent,
    /// Stop only the parent; leave the child running.
    Decouple,
}

/// Shell whose completion script is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Shell {
    /// Bourne-Again SHell.
    Bash,
    /// Z Shell.
    Zsh,
    /// Friendly Interactive SHell.
    Fish,
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        })
    }
}

/// Topic for which to render help text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HelpTopic {
    /// Help for the top-level invocation (subcommand list + global options).
    Top,
    /// Help for the `run` subcommand.
    Run,
    /// Help for the `attach` subcommand (detach key bindings, modes 等)。
    Attach,
    /// Help for the `list` subcommand.
    List,
    /// Help for the `kill` subcommand.
    Kill,
    /// Help for the `status` subcommand.
    Status,
    /// Help for the `tail` subcommand.
    Tail,
    /// Help for the `wait` subcommand (predicate / timeout / exit code 一覧)。
    Wait,
    /// Help for the `completion` subcommand.
    Completion,
    /// User invoked an unknown subcommand; render top-level help with note.
    UnknownSubcommand(String),
}

/// Fully parsed `run` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// Operating mode.
    pub mode: Mode,
    /// Virtual screen columns (used in headless mode; default 80).
    pub cols: i32,
    /// Virtual screen rows (used in headless mode; default 24).
    pub rows: i32,
    /// Overall timeout in milliseconds, or `None` if unset.
    /// 負値は意味を持たないので `u64` (WaitConfig.timeout_ms と整合)。
    pub timeout_ms: Option<u64>,
    /// Output idle timeout in milliseconds, or `None` if unset.
    pub idle_timeout_ms: Option<u64>,
    /// Substring pattern that, when seen in PTY output, terminates the child.
    pub until: Option<String>,
    /// Explicit socket path, or `None` to auto-generate.
    pub socket: Option<String>,
    /// `--detached`: daemon を別 process で起動して親はすぐ exit。socket path を
    /// stdout に 1 行 print してから親が終わる。attach は別 process から行う。
    pub detached: bool,
    /// `--session`: 自動採番 (run-<pid>) ではなく明示 session id を使う。
    /// socket path 自動解決にもこの値が入る。
    pub session: Option<String>,
    /// Action when the child is suspended (preset by mode unless overridden).
    pub on_child_suspend: OnChildSuspend,
    /// Action when the parent is suspended (preset by mode unless overridden).
    pub on_parent_suspend: OnParentSuspend,
    /// argv of the child command.
    pub command: Vec<String>,
}

/// `attach` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachConfig {
    /// Target socket path. `Some(p)` で explicit、`None` なら session_id から resolve。
    pub socket: Option<String>,
    /// Target session id (= socket path resolver の入力)。`socket` 指定時は無視。
    pub session_id: Option<String>,
    /// 動作 mode (rw / ro)。MVP は文字列のみ受理。
    pub mode_str: Option<String>,
    /// `--exclusive` (= 起動時占有要求)。
    pub exclusive: bool,
    /// `--detach-others` (= attach 時に他 client を奪取)。
    pub detach_others: bool,
}

/// `list` subcommand configuration (R5-H3)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListConfig {
    /// `--prune-stale` (= 接続不能 socket を unlink して掃除)。
    ///
    /// daemon が panic / SIGKILL で異常終了すると `UnixSock::drop` が走らず
    /// socket file が残留し、`hyoui list` で live と区別できなくなる (R5-H3)。
    /// `--prune-stale` は connect 試行で死活確認し、`ECONNREFUSED` 等で
    /// 失敗した socket を unlink で除去する。
    pub prune_stale: bool,
}

/// `kill` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// 子 PTY に送る signal 番号 (= default SIGTERM)。
    pub signum: Option<u8>,
}

/// `status` subcommand の出力形式 (= `--format=plain|json`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum StatusFormat {
    /// Plain text (= human readable、default)。`key: value` 1 行ごと
    #[default]
    Plain,
    /// JSON (= scripting / jq 用、1 行 JSON object) — H5
    Json,
}

/// `status` subcommand configuration (Phase 11)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--format=plain|json` (= default `Plain`、H5: scripting で grep/cut の罠回避)。
    pub format: StatusFormat,
}

/// `tail` subcommand configuration (Phase 11)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--follow` で daemon が live stream を継続送信。
    pub follow: bool,
    /// `--strip-ansi` で daemon 側で escape を strip 済の TailData を流す。
    pub strip_ansi: bool,
    /// `--since=<ms>` (= 過去 ms 以内の chunk を bundle)、`None` なら全体。
    pub since_ms: Option<u64>,
    /// `--last-bytes=<n>` (= 末尾 n bytes に絞る)、`None` なら制限なし。
    pub last_bytes: Option<u64>,
}

/// `wait` subcommand の predicate (Phase 11)。CLI 表記から daemon protocol の
/// `WaitPredicate` に対応する。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WaitCliPredicate {
    /// `text:<str>` (= 部分文字列マッチ)。
    Text(String),
    /// `pattern:<regex>`。
    Pattern(String),
    /// `wait-idle:<ms>` or `wait:<ms>` (= 静寂 ms)。
    Idle(u64),
}

/// `wait` subcommand configuration (Phase 11)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// 待ち条件 (= `text:` / `pattern:` / `wait[-idle]:`)。
    pub predicate: WaitCliPredicate,
    /// `--timeout=<ms>` (絶対 timeout)、`None` なら無限。
    pub timeout_ms: Option<u64>,
    /// `--no-strip-escapes` で options.strip_escapes = false (default true)。
    pub strip_escapes: bool,
    /// `--newline-convert-lf` で CRLF → LF 正規化。
    pub newline_convert_lf: bool,
}

/// Result of parsing argv (excluding argv[0]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// Display usage text and exit 0.
    Help {
        /// What help to show.
        topic: HelpTopic,
    },
    /// Display the library version and exit 0.
    Version,
    /// Execute `run` with the given configuration.
    Run(RunConfig),
    /// Attach to an existing daemon session.
    Attach(AttachConfig),
    /// List existing daemon sessions (= socket dir scan)。
    List(ListConfig),
    /// Kill (= SIGTERM 等を子に送る) a daemon session by session id / socket。
    Kill(KillConfig),
    /// Print session status (clients/leader/lock/scrollback) and exit。
    Status(StatusConfig),
    /// Tail scrollback (optional --follow for live stream)。
    Tail(TailConfig),
    /// Wait until predicate (text/pattern/idle) matches, then exit。
    Wait(WaitConfig),
    /// Print a completion script for the given shell.
    Completion {
        /// Target shell.
        shell: Shell,
    },
    /// Parsing failed. Caller should print the message + top-level usage and
    /// exit with a non-zero status.
    Error(String),
}

// =============================================================================
// Public entry points
// =============================================================================

/// Parse the entire command line (argv excluding argv[0]).
pub fn parse_args(args: &[String]) -> Command {
    // Top-level: no args -> top help.
    if args.is_empty() {
        return Command::Help {
            topic: HelpTopic::Top,
        };
    }

    // Top-level flags allowed before any subcommand.
    let head = args[0].as_str();
    match head {
        "--help" | "-h" => {
            return Command::Help {
                topic: HelpTopic::Top,
            };
        }
        "--version" | "-V" => return Command::Version,
        _ => {}
    }

    let rest = &args[1..];
    match head {
        "run" => parse_run(rest),
        "attach" => parse_attach(rest),
        "list" => parse_list(rest),
        "kill" => parse_kill(rest),
        "status" => parse_status(rest),
        "tail" => parse_tail(rest),
        "wait" => parse_wait(rest),
        "completion" => parse_completion(rest),
        // Reserved for future stages.
        "send" | "detach" => Command::Error(format!(
            "subcommand `{head}` is reserved but not yet implemented"
        )),
        other => Command::Help {
            topic: HelpTopic::UnknownSubcommand(other.to_string()),
        },
    }
}

fn parse_list(args: &[String]) -> Command {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Command::Help {
            topic: HelpTopic::List,
        };
    }
    let mut cfg = ListConfig::default();
    for a in args {
        match a.as_str() {
            "--prune-stale" => cfg.prune_stale = true,
            other => return Command::Error(format!("list: unexpected argument: {other}")),
        }
    }
    Command::List(cfg)
}

fn parse_kill(args: &[String]) -> Command {
    let mut cfg = KillConfig {
        socket: None,
        session_id: None,
        signum: None,
    };
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Kill,
                };
            }
            "--socket" => match value {
                Some(v) => cfg.socket = Some(v),
                None => return Command::Error("--socket requires a value".into()),
            },
            "--signum" => match value.as_deref() {
                Some(v) => match v.parse::<u8>() {
                    Ok(n) => cfg.signum = Some(n),
                    Err(_) => {
                        return Command::Error(format!("invalid --signum value: {v}"));
                    }
                },
                None => return Command::Error("--signum requires a value".into()),
            },
            other if other.starts_with('-') => {
                return Command::Error(format!("unknown kill option: {other}"));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }
    match positionals.len() {
        0 => {
            if cfg.socket.is_none() {
                return Command::Error(
                    "kill: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui kill <session-id>` / `hyoui list` で session 一覧を確認できます"
                        .into(),
                );
            }
        }
        1 => {
            let sid = positionals.into_iter().next().unwrap();
            // R5-AUD-C2: positional session_id を validate (= path traversal 早期 reject)
            if let Err(e) = validate_session_id(&sid) {
                return Command::Error(format!("kill: {e}"));
            }
            cfg.session_id = Some(sid);
        }
        _ => return Command::Error("kill: too many positional arguments".into()),
    }
    Command::Kill(cfg)
}

/// shared helper: --socket / --help / positional session_id を抜き出す。
/// 残ったオプションは caller がコールバックで処理する。
#[allow(clippy::result_large_err)] // Command 内 String/Vec の Err サイズは parse path のみで許容
fn parse_session_targeted<F>(
    name: &str,
    args: &[String],
    help_topic: HelpTopic,
    mut on_option: F,
) -> Result<(Option<String>, Option<String>), Command>
where
    F: FnMut(&str, Option<String>) -> Result<bool, Command>,
{
    let mut socket: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt_name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match opt_name.as_str() {
            "--help" | "-h" => {
                return Err(Command::Help { topic: help_topic });
            }
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Err(Command::Error(format!("{name}: --socket requires a value"))),
            },
            other if other.starts_with("--") => {
                let cb_consumed = on_option(other, value)?;
                if !cb_consumed {
                    consumed_extra = false;
                }
            }
            other if other.starts_with('-') => {
                return Err(Command::Error(format!("{name}: unknown option: {other}")));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }
    let session_id = match positionals.len() {
        0 => {
            if socket.is_none() {
                return Err(Command::Error(format!(
                    "{name}: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui {name} <session-id>` / `hyoui list` で session 一覧を確認できます"
                )));
            }
            None
        }
        1 => {
            let sid = positionals.into_iter().next().unwrap();
            // R5-AUD-C2: positional session_id を validate (= path traversal 早期 reject)
            if let Err(e) = validate_session_id(&sid) {
                return Err(Command::Error(format!("{name}: {e}")));
            }
            Some(sid)
        }
        _ => {
            return Err(Command::Error(format!(
                "{name}: too many positional arguments"
            )));
        }
    };
    Ok((socket, session_id))
}

#[allow(clippy::result_large_err)]
fn parse_status(args: &[String]) -> Command {
    let mut format = StatusFormat::Plain;
    let res = parse_session_targeted("status", args, HelpTopic::Status, |opt, value| match opt {
        "--format" => {
            let v =
                value.ok_or_else(|| Command::Error("status: --format requires a value".into()))?;
            match v.as_str() {
                "plain" => {
                    format = StatusFormat::Plain;
                    Ok(true)
                }
                "json" => {
                    format = StatusFormat::Json;
                    Ok(true)
                }
                other => Err(Command::Error(format!(
                    "status: --format must be `plain` or `json`, got {other:?}"
                ))),
            }
        }
        other => Err(Command::Error(format!("status: unknown option: {other}"))),
    });
    match res {
        Ok((socket, session_id)) => Command::Status(StatusConfig {
            socket,
            session_id,
            format,
        }),
        Err(c) => c,
    }
}

#[allow(clippy::result_large_err)]
fn parse_tail(args: &[String]) -> Command {
    let mut follow = false;
    let mut strip_ansi = false;
    let mut since_ms: Option<u64> = None;
    let mut last_bytes: Option<u64> = None;
    let res = parse_session_targeted("tail", args, HelpTopic::Tail, |opt, value| match opt {
        "--follow" => {
            follow = true;
            Ok(false)
        }
        "--strip-ansi" => {
            strip_ansi = true;
            Ok(false)
        }
        "--since" => {
            let v = value.ok_or_else(|| Command::Error("tail: --since requires a value".into()))?;
            let ms =
                parse_duration_ms(&v).map_err(|e| Command::Error(format!("tail: --since: {e}")))?;
            since_ms = Some(ms);
            Ok(true)
        }
        "--last-bytes" => {
            let v = value
                .ok_or_else(|| Command::Error("tail: --last-bytes requires a value".into()))?;
            let n = v
                .parse::<u64>()
                .map_err(|_| Command::Error(format!("tail: --last-bytes: bad number: {v}")))?;
            last_bytes = Some(n);
            Ok(true)
        }
        other => Err(Command::Error(format!("tail: unknown option: {other}"))),
    });
    match res {
        Ok((socket, session_id)) => Command::Tail(TailConfig {
            socket,
            session_id,
            follow,
            strip_ansi,
            since_ms,
            last_bytes,
        }),
        Err(c) => c,
    }
}

fn parse_wait(args: &[String]) -> Command {
    let mut predicate: Option<WaitCliPredicate> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut strip_escapes = true;
    let mut newline_convert_lf = false;
    let mut socket: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt_name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match opt_name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Wait,
                };
            }
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Command::Error("wait: --socket requires a value".into()),
            },
            "--timeout" => match value {
                Some(v) => match parse_duration_ms(&v) {
                    Ok(ms) => timeout_ms = Some(ms),
                    Err(e) => return Command::Error(format!("wait: --timeout: {e}")),
                },
                None => return Command::Error("wait: --timeout requires a value".into()),
            },
            "--no-strip-escapes" => {
                strip_escapes = false;
                consumed_extra = false;
            }
            "--newline-convert-lf" => {
                newline_convert_lf = true;
                consumed_extra = false;
            }
            other if other.starts_with('-') => {
                return Command::Error(format!("wait: unknown option: {other}"));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }
    // positionals: 1 つは session_id、もう 1 つ (or 1 つだけ) が predicate。
    // 順序は session_id → predicate / predicate (--socket 使うとき) のいずれか。
    // predicate は "text:" / "pattern:" / "wait:" / "wait-idle:" prefix で識別。
    let mut session_id: Option<String> = None;
    for p in positionals {
        match parse_wait_predicate(&p) {
            Ok(Some(pred)) => {
                if predicate.is_some() {
                    return Command::Error("wait: predicate specified more than once".into());
                }
                predicate = Some(pred);
            }
            Ok(None) => {
                if session_id.is_some() {
                    return Command::Error(format!("wait: unexpected argument: {p}"));
                }
                // R5-AUD-C2: positional session_id を validate (= path traversal 早期 reject)
                if let Err(e) = validate_session_id(&p) {
                    return Command::Error(format!("wait: {e}"));
                }
                session_id = Some(p);
            }
            Err(e) => {
                return Command::Error(format!("wait: predicate `{p}`: {e}"));
            }
        }
    }
    let predicate = match predicate {
        Some(p) => p,
        None => {
            return Command::Error(
                "wait: predicate (text:.. / pattern:.. / wait[-idle]:..) required".into(),
            );
        }
    };
    if session_id.is_none() && socket.is_none() {
        return Command::Error(
            "wait: session id (positional) または --socket=<path> が必要です。\
             例: `hyoui wait <session-id> text:READY --timeout=5s` / `hyoui list` で session 一覧を確認できます"
                .into(),
        );
    }
    Command::Wait(WaitConfig {
        socket,
        session_id,
        predicate,
        timeout_ms,
        strip_escapes,
        newline_convert_lf,
    })
}

/// `text:<str>` / `pattern:<regex>` / `wait:<dur>` / `wait-idle:<dur>` の
/// CLI prefix を [`WaitCliPredicate`] に変換。
///
/// 戻り値:
/// - `Ok(Some(pred))`: 認識済 prefix + valid payload
/// - `Ok(None)`: prefix にマッチしない (= caller は positional session_id 扱い)
/// - `Err(msg)`: prefix にマッチしたが payload (= duration) の parse 失敗
///   (= Round2 #9: 旧版は `.ok()` で潰して silently None だったため、user が
///   単位なし `wait-idle:500` 等を渡したとき「unexpected argument」という誤メッセージ
///   が出た。明示 Err で `parse_duration_ms` の本来の error message を上位に伝える)
fn parse_wait_predicate(s: &str) -> Result<Option<WaitCliPredicate>, String> {
    if let Some(rest) = s.strip_prefix("text:") {
        return Ok(Some(WaitCliPredicate::Text(rest.to_string())));
    }
    if let Some(rest) = s.strip_prefix("pattern:") {
        return Ok(Some(WaitCliPredicate::Pattern(rest.to_string())));
    }
    let idle_rest = s
        .strip_prefix("wait-idle:")
        .or_else(|| s.strip_prefix("wait:"));
    if let Some(rest) = idle_rest {
        return parse_duration_ms(rest)
            .map(|ms| Some(WaitCliPredicate::Idle(ms)))
            .map_err(|e| format!("invalid duration in predicate: {e}"));
    }
    Ok(None)
}

/// 期間文字列を ms に変換する (kawaz/timespec.mbt の duration parser を参考)。
///
/// ## 文法
///
/// ```text
/// duration := SP? component (SP? sign SP? component)* SP?
/// component := digits ('.' digits)? SP? unit
/// digits := DIGIT (DIGIT | '_')*
/// sign := '+' | '-'
/// unit := short_unit | long_unit
/// short_unit := "ns" | "us" | "μs" | "ms" | "s" | "m" | "h" | "d" | "w"
/// long_unit := "millisecond"|"milliseconds" | "second"|"seconds"|"sec"
///            | "minute"|"minutes"|"min" | "hour"|"hours"
///            | "day"|"days" | "week"|"weeks"
/// ```
///
/// ## 仕様
///
/// - **単位必須**: bare 数字 / 空文字列は error
/// - **decimal 対応**: `1.5h` = 90 分
/// - **underscore separator**: `1_000.5s` = 1000.5 秒 (= 1_000_500 ms)
/// - **連結加算**: `1h30m` = 1 時間 + 30 分。同 group 内 segment は加算
/// - **符号付き group**: `1d-4h` = 1 日 group - 4 時間 group = 20 時間
/// - **whitespace tolerant**: `1 h 2 m` / `1h 2m` も accept
/// - **sub-ms 精度 (ns / us / μs) も accept、集積後に ms へ floor**:
///   `1500us` = 1 ms (= 1500000ns / 1000000 を floor)、
///   `999us` = 0 ms (= 999000ns / 1000000 = 0)、
///   `500us 600us` = 1 ms (= 集積値 1100000ns で 1ms を超えた分は取り入れ)。
///   timespec.mbt は YAGNI で reject していたが、本実装は集積 floor 方針
/// - **`y` / `M` (年 / 月) は reject** (= 単位固定でないため)
/// - **最終 total が負なら error** (= hyoui の duration は正値前提)
fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let total_ns = parse_duration_ns_signed(s)?;
    if total_ns < 0 {
        return Err(format!(
            "duration resolved to negative value ({total_ns}ns) in {s:?}"
        ));
    }
    // ns → ms に floor (= 1ms 未満は切り捨て、集積値が 1ms を超えた分のみ取り入れ)
    let total_ms = total_ns / 1_000_000;
    u64::try_from(total_ms).map_err(|_| format!("duration overflows u64 ms: {s:?}"))
}

/// 符号付き ns で返す internal helper (= negative 許容、集積精度 ns)。
fn parse_duration_ns_signed(s: &str) -> Result<i128, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    let chars: Vec<char> = s.chars().collect();
    let mut pos = 0usize;

    let mut total_ns: i128 = 0;
    let mut group_ns: i128 = 0;
    let mut group_sign: i128 = 1;
    let mut parsed_any = false;

    while pos < chars.len() {
        pos = skip_spaces(&chars, pos);
        if pos >= chars.len() {
            break;
        }
        // group 区切り符号 (= D5: 先頭 group の前に sign は不可 = grammar 違反)
        let mut new_group = false;
        match chars[pos] {
            '+' | '-' => {
                if !parsed_any {
                    return Err(format!(
                        "leading sign before any component at position {pos} in {s:?}"
                    ));
                }
                total_ns = total_ns
                    .checked_add(group_sign.checked_mul(group_ns).ok_or("overflow")?)
                    .ok_or("overflow")?;
                group_ns = 0;
                group_sign = if chars[pos] == '-' { -1 } else { 1 };
                pos += 1;
                new_group = true;
            }
            _ => {}
        }
        pos = skip_spaces(&chars, pos);
        // 符号の後ろに digit が無いと invalid (例: `1m-` / `1m+`)
        if pos >= chars.len() {
            if new_group {
                return Err(format!("trailing sign without component in {s:?}"));
            }
            break;
        }
        if !chars[pos].is_ascii_digit() {
            if new_group {
                return Err(format!(
                    "expected digit after sign at position {pos} in {s:?}"
                ));
            }
            // grammar 上、component の前は `+` / `-` か whitespace か EOF のみ。
            // それ以外の文字 (= `_` / `.` / alpha など) が残っているのは trailing
            // junk または unit 直後の不正な char → 明示 error にする (D2/D3)。
            return Err(format!(
                "unexpected character {:?} at position {pos} in {s:?} \
                 (component separator must be '+' / '-' / whitespace)",
                chars[pos]
            ));
        }

        let (int_part, frac_billion, new_pos) = parse_number(&chars, pos)?;
        pos = skip_spaces(&chars, new_pos);
        let (ns_mul, unit_end) = parse_unit(&chars, pos)?;
        if unit_end == pos {
            return Err(format!("missing unit after number in {s:?}"));
        }
        pos = unit_end;

        let mut seg_ns: i128 = (int_part as i128)
            .checked_mul(ns_mul)
            .ok_or("duration component overflow")?;
        if frac_billion > 0 {
            // frac_billion は (frac × 1e9) の整数表現。ns_mul を掛けて 1e9 で割れば
            // 「frac × unit_ns」を整数精度で得られる (= D4: 旧 f64 経由を排除)。
            let frac_ns: i128 = (frac_billion as i128)
                .checked_mul(ns_mul)
                .ok_or("frac component overflow")?
                / 1_000_000_000;
            seg_ns = seg_ns.checked_add(frac_ns).ok_or("frac add overflow")?;
        }
        group_ns = group_ns.checked_add(seg_ns).ok_or("group accum overflow")?;
        parsed_any = true;
    }
    if !parsed_any {
        return Err(format!("no duration segments parsed from {s:?}"));
    }
    total_ns = total_ns
        .checked_add(group_sign.checked_mul(group_ns).ok_or("overflow")?)
        .ok_or("overflow")?;
    Ok(total_ns)
}

fn skip_spaces(chars: &[char], start: usize) -> usize {
    let mut pos = start;
    while pos < chars.len() && (chars[pos] == ' ' || chars[pos] == '\t') {
        pos += 1;
    }
    pos
}

/// `123.456` / `1_000_000.5_0` 形式の数値を読む。
///
/// 文法 (= timespec.mbt 相当):
/// - `digits := DIGIT (DIGIT | '_')*` (= 必ず DIGIT で始まり、以降 `_` を separator として許容)
/// - `'_' を先頭` / `'_' 連続` / `数字なしの '_' のみ` は **error**
/// - `('.' digits)?` (= 小数点を入れたら必ず 1 桁以上の digits が続く)
/// - 旧実装 (Round3 まで) は `_5s` / `1.s` / `1h_2m` を silently 通していた。grammar
///   通りに厳格化 (= レビュー指摘 D2/D3)
///
/// 戻り値: `(int_part_i64, frac_part_in_per_billion_i64, new_pos)`。
/// frac は分母 1_000_000_000 (= 9 桁) で整数化することで f64 経由の overflow を回避
/// (= レビュー指摘 D4)。それ以上の精度は floor で切り捨て。
fn parse_number(chars: &[char], start: usize) -> Result<(i64, i64, usize), String> {
    let mut pos = start;
    // 1. 最初の 1 文字は必ず digit (= leading `_` 禁止)
    let first = chars
        .get(pos)
        .copied()
        .ok_or_else(|| "expected digit".to_string())?;
    let first_d = first
        .to_digit(10)
        .ok_or_else(|| format!("expected digit at position {pos}"))?;
    let mut int_part: i64 = first_d as i64;
    pos += 1;
    // 2. 以降は DIGIT または `_`、ただし `_` 連続不可 + 末尾 `_` 不可
    let mut last_was_underscore = false;
    while pos < chars.len() {
        let c = chars[pos];
        if c == '_' {
            if last_was_underscore {
                return Err(format!("consecutive '_' at position {pos}"));
            }
            last_was_underscore = true;
            pos += 1;
            continue;
        }
        if let Some(d) = c.to_digit(10) {
            int_part = int_part
                .checked_mul(10)
                .and_then(|v| v.checked_add(d as i64))
                .ok_or("integer part overflow")?;
            last_was_underscore = false;
            pos += 1;
        } else {
            break;
        }
    }
    if last_was_underscore {
        return Err(format!("trailing '_' in number at position {pos}"));
    }

    // 3. 小数部 (= `.` の後ろに必ず 1 桁以上の digit が必要)
    let mut frac_billion: i64 = 0; // frac × 1_000_000_000 を整数で蓄える
    if pos < chars.len() && chars[pos] == '.' {
        pos += 1;
        // 小数点直後の 1 文字目も必ず digit
        let first = chars
            .get(pos)
            .copied()
            .ok_or_else(|| format!("expected fractional digit after '.' at position {pos}"))?;
        let first_d = first
            .to_digit(10)
            .ok_or_else(|| format!("expected fractional digit after '.' at position {pos}"))?;
        let mut frac_digits: u32 = 1;
        frac_billion = frac_billion
            .checked_add((first_d as i64) * 10i64.pow(9 - frac_digits))
            .ok_or("frac overflow")?;
        pos += 1;
        let mut last_was_underscore = false;
        while pos < chars.len() {
            let c = chars[pos];
            if c == '_' {
                if last_was_underscore {
                    return Err(format!("consecutive '_' in fractional at position {pos}"));
                }
                last_was_underscore = true;
                pos += 1;
                continue;
            }
            if let Some(d) = c.to_digit(10) {
                if frac_digits < 9 {
                    frac_digits += 1;
                    frac_billion = frac_billion
                        .checked_add((d as i64) * 10i64.pow(9 - frac_digits))
                        .ok_or("frac overflow")?;
                }
                // 9 桁を超えた小数は精度を捨てる (= ns 単位 timer なので 9 桁で十分)
                last_was_underscore = false;
                pos += 1;
            } else {
                break;
            }
        }
        if last_was_underscore {
            return Err(format!("trailing '_' in fractional at position {pos}"));
        }
    }
    Ok((int_part, frac_billion, pos))
}

/// `parse_unit` は (ns_multiplier, end_pos) を返す。未知単位 / 拒否単位は Err。
///
/// **case-insensitive** (= レビュー指摘 H2): `1H` / `1Min` / `1MS` 等は lowercase
/// 化してから match する。`μ` (Greek mu) は ASCII 範囲外なのでそのまま保持。
/// 例外: 月の慣習表記 `M` (= Java) は単独単位として **error 候補** に乗せたいが、
/// case-insensitive 化すると `m`/`M` を区別できなくなる。そこで `m`/`M` は同等
/// に minute 扱いとし、月は長形 `month` / `months` のみで明示 reject する
/// (= 「単位は文脈で明確」優先、minute の頻度 >> month の頻度なので m を取る)。
fn parse_unit(chars: &[char], start: usize) -> Result<(i128, usize), String> {
    if start >= chars.len() {
        return Err("missing unit".into());
    }
    let mut end = start;
    while end < chars.len() && (chars[end].is_ascii_alphabetic() || chars[end] == 'μ') {
        end += 1;
    }
    if end == start {
        return Ok((0, start)); // no unit chars
    }
    // word 全体を lowercase 化、ただし `μ` (= U+03BC) は ASCII 外なので保存
    let word: String = chars[start..end]
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    const NS: i128 = 1;
    const US: i128 = 1_000;
    const MS: i128 = 1_000_000;
    const SEC: i128 = 1_000_000_000;
    let ns = match word.as_str() {
        "ns" => NS,
        "us" | "μs" => US,
        "ms" | "millisecond" | "milliseconds" => MS,
        "s" | "sec" | "second" | "seconds" => SEC,
        "m" | "min" | "minute" | "minutes" => 60 * SEC,
        "h" | "hour" | "hours" => 3600 * SEC,
        "d" | "day" | "days" => 86_400 * SEC,
        "w" | "week" | "weeks" => 604_800 * SEC,
        // explicit rejects: 年/月 (= 単位固定でない)
        // 注: 短形 `M` は minute と被るため month 用 reject に含めない。
        // ユーザが `1M` と書くと 1 分扱い (慣習衝突を minute 優先で解消)。
        "y" | "year" | "years" | "month" | "months" => {
            return Err(format!(
                "calendar unit {word:?} not supported (lengths vary; \
                 use d/days for fixed-length day counts)"
            ));
        }
        _ => return Err(format!("unknown unit {word:?}")),
    };
    Ok((ns, end))
}

fn parse_attach(args: &[String]) -> Command {
    let mut cfg = AttachConfig {
        socket: None,
        session_id: None,
        mode_str: None,
        exclusive: false,
        detach_others: false,
    };

    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Attach,
                };
            }
            "--socket" => match value {
                Some(v) => cfg.socket = Some(v),
                None => return Command::Error("--socket requires a value".into()),
            },
            "--mode" => match value {
                Some(v) => cfg.mode_str = Some(v),
                None => return Command::Error("--mode requires a value".into()),
            },
            "--exclusive" => {
                cfg.exclusive = true;
                consumed_extra = false; // bool flag は次 arg 食わない
            }
            "--detach-others" => {
                cfg.detach_others = true;
                consumed_extra = false;
            }
            other if other.starts_with('-') => {
                return Command::Error(format!("unknown attach option: {other}"));
            }
            _ => {
                // positional (= session id)
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }

    match positionals.len() {
        0 => {
            if cfg.socket.is_none() {
                return Command::Error(
                    "attach: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui attach <session-id>` / `hyoui list` で session 一覧を確認できます"
                        .into(),
                );
            }
        }
        1 => cfg.session_id = Some(positionals.into_iter().next().unwrap()),
        _ => return Command::Error("attach: too many positional arguments".into()),
    }

    Command::Attach(cfg)
}

/// Render the usage text for the given help topic.
pub fn usage(topic: &HelpTopic) -> String {
    match topic {
        HelpTopic::Top => usage_top(None),
        HelpTopic::UnknownSubcommand(name) => usage_top(Some(name.as_str())),
        HelpTopic::Run => usage_run(),
        HelpTopic::Attach => usage_attach(),
        HelpTopic::List => usage_list(),
        HelpTopic::Kill => usage_kill(),
        HelpTopic::Status => usage_status(),
        HelpTopic::Tail => usage_tail(),
        HelpTopic::Wait => usage_wait(),
        HelpTopic::Completion => usage_completion(),
    }
}

// =============================================================================
// Subcommand parsers
// =============================================================================

fn parse_run(args: &[String]) -> Command {
    // `run` accepts options, then optional `--`, then the child argv.
    if args.is_empty() {
        return Command::Error("no command given (use `hyoui run [opts] -- cmd [args...]`)".into());
    }

    // Recognise `run --help`.
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help" || a == "-h")
    {
        return Command::Help {
            topic: HelpTopic::Run,
        };
    }

    let mut mode = Mode::Interactive;
    let mut explicit_cols: Option<i32> = None;
    let mut explicit_rows: Option<i32> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut idle_timeout_ms: Option<u64> = None;
    let mut until: Option<String> = None;
    let mut socket: Option<String> = None;
    let mut on_child_suspend: Option<OnChildSuspend> = None;
    let mut on_parent_suspend: Option<OnParentSuspend> = None;
    let mut command: Vec<String> = Vec::new();
    let mut detached = false;
    let mut session: Option<String> = None;

    let mut i = 0usize;
    let mut in_command = false;

    while i < args.len() {
        let arg = args[i].as_str();
        if in_command {
            command.push(args[i].clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            in_command = true;
            i += 1;
            continue;
        }

        let (name, inline_value) = split_eq(arg);

        // Take the value for an option: inline (`--x=v`) or following arg.
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };

        // Process the option. On success, advance past the value too.
        match name.as_str() {
            "--mode" => match value.as_deref() {
                Some("interactive") => mode = Mode::Interactive,
                Some("headless") => mode = Mode::Headless,
                Some(other) => return Command::Error(format!("invalid --mode value: {other}")),
                None => return Command::Error("--mode requires a value".into()),
            },
            "--size" => match value.as_deref() {
                Some(v) => match parse_size(v) {
                    Some((c, r)) => {
                        explicit_cols = Some(c);
                        explicit_rows = Some(r);
                    }
                    None => {
                        return Command::Error(format!(
                            "invalid --size value (expected COLSxROWS): {v}"
                        ));
                    }
                },
                None => return Command::Error("--size requires a value".into()),
            },
            "--cols" => match value.as_deref() {
                Some(v) => match parse_int(v) {
                    Some(c) => explicit_cols = Some(c),
                    None => return Command::Error(format!("invalid --cols value: {v}")),
                },
                None => return Command::Error("--cols requires a value".into()),
            },
            "--rows" => match value.as_deref() {
                Some(v) => match parse_int(v) {
                    Some(r) => explicit_rows = Some(r),
                    None => return Command::Error(format!("invalid --rows value: {v}")),
                },
                None => return Command::Error("--rows requires a value".into()),
            },
            "--timeout" => match value.as_deref() {
                Some(v) => match parse_duration_ms(v) {
                    Ok(ms) => timeout_ms = Some(ms),
                    Err(e) => return Command::Error(format!("--timeout: {e}")),
                },
                None => return Command::Error("--timeout requires a value".into()),
            },
            "--idle-timeout" => match value.as_deref() {
                Some(v) => match parse_duration_ms(v) {
                    Ok(ms) => idle_timeout_ms = Some(ms),
                    Err(e) => return Command::Error(format!("--idle-timeout: {e}")),
                },
                None => return Command::Error("--idle-timeout requires a value".into()),
            },
            "--until" => match value {
                Some(v) => until = Some(v),
                None => return Command::Error("--until requires a value".into()),
            },
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Command::Error("--socket requires a value".into()),
            },
            "--on-child-suspend" => match value.as_deref() {
                Some("follow") => on_child_suspend = Some(OnChildSuspend::Follow),
                Some("auto-resume") => on_child_suspend = Some(OnChildSuspend::AutoResume),
                Some(other) => {
                    return Command::Error(format!("invalid --on-child-suspend value: {other}"));
                }
                None => return Command::Error("--on-child-suspend requires a value".into()),
            },
            "--on-parent-suspend" => match value.as_deref() {
                Some("transparent") => on_parent_suspend = Some(OnParentSuspend::Transparent),
                Some("decouple") => on_parent_suspend = Some(OnParentSuspend::Decouple),
                Some(other) => {
                    return Command::Error(format!("invalid --on-parent-suspend value: {other}"));
                }
                None => return Command::Error("--on-parent-suspend requires a value".into()),
            },
            "--detached" => {
                detached = true;
                consumed_extra = false; // bool flag は次 arg を食わない
            }
            "--session" => match value {
                Some(v) => {
                    if let Err(e) = validate_session_id(&v) {
                        return Command::Error(format!("--session: {e}"));
                    }
                    session = Some(v);
                }
                None => return Command::Error("--session requires a value".into()),
            },
            other => return Command::Error(format!("unknown option: {other}")),
        }

        // Advance past option name (and value if it was a separate arg).
        if consumed_extra {
            i += 1;
        }
        i += 1;
    }

    if command.is_empty() {
        return Command::Error("no command given (use `-- cmd [args...]`)".into());
    }

    // Mode-driven preset defaults for suspend behavior, unless overridden.
    let final_child_suspend = on_child_suspend.unwrap_or(match mode {
        Mode::Headless => OnChildSuspend::AutoResume,
        Mode::Interactive => OnChildSuspend::Follow,
    });
    let final_parent_suspend = on_parent_suspend.unwrap_or(match mode {
        Mode::Headless => OnParentSuspend::Decouple,
        Mode::Interactive => OnParentSuspend::Transparent,
    });

    // Virtual size: default to 80x24 when unspecified.
    let cols = explicit_cols.unwrap_or(80);
    let rows = explicit_rows.unwrap_or(24);

    Command::Run(RunConfig {
        mode,
        cols,
        rows,
        timeout_ms,
        idle_timeout_ms,
        until,
        socket,
        detached,
        session,
        on_child_suspend: final_child_suspend,
        on_parent_suspend: final_parent_suspend,
        command,
    })
}

fn parse_completion(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Error("completion requires a shell name (bash|zsh|fish)".into());
    }
    if args[0] == "--help" || args[0] == "-h" {
        return Command::Help {
            topic: HelpTopic::Completion,
        };
    }
    if args.len() > 1 {
        return Command::Error(format!(
            "completion accepts exactly one argument, got {}",
            args.len()
        ));
    }
    match args[0].as_str() {
        "bash" => Command::Completion { shell: Shell::Bash },
        "zsh" => Command::Completion { shell: Shell::Zsh },
        "fish" => Command::Completion { shell: Shell::Fish },
        other => Command::Error(format!(
            "unknown shell: {other} (supported: bash, zsh, fish)"
        )),
    }
}

// =============================================================================
// Usage texts
// =============================================================================

fn usage_top(unknown: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(name) = unknown {
        out.push_str(&format!("error: unknown subcommand `{name}`\n\n"));
    }
    out.push_str(
        "hyoui — terminal-aware process proxy\n\
        \n\
        USAGE:\n    \
            hyoui <subcommand> [options]\n\
        \n\
        SUBCOMMANDS:\n    \
            run         Run a command inside a PTY as a transparent proxy\n    \
            attach      Attach to an existing daemon session\n    \
            list        List daemon sessions (= socket dir scan)\n    \
            kill        Send signal to a daemon session and terminate it\n    \
            status      Print session status (clients/leader/lock/scrollback)\n    \
            tail        Stream scrollback / live output (--follow で継続)\n    \
            wait        Wait until predicate (text/pattern/idle) matches\n    \
            completion  Print a shell completion script (bash|zsh|fish)\n\
        \n\
        RESERVED (not yet implemented):\n    \
            send, detach   将来 protocol 拡張用に予約\n\
        \n\
        GLOBAL OPTIONS:\n    \
            -h, --help     Show this help and exit\n    \
            -V, --version  Show version and exit\n\
        \n\
        Run `hyoui <subcommand> --help` for per-subcommand help.\n",
    );
    out
}

fn usage_run() -> String {
    String::from(
        "hyoui run — run a command inside a PTY as a transparent proxy\n\
        \n\
        USAGE:\n    \
            hyoui run [options] -- cmd [args...]\n\
        \n\
        OPTIONS:\n    \
            --mode=interactive|headless   Operating mode (default: interactive)\n    \
            --size COLSxROWS              Virtual screen size, e.g. 80x24 (headless)\n    \
            --cols N                      Virtual screen columns (headless)\n    \
            --rows M                      Virtual screen rows (headless)\n    \
            --timeout DUR                 Overall timeout (DUR フォーマットは下記参照)\n    \
            --idle-timeout DUR            Output idle timeout (= 子 PTY 出力が止まったら exit)\n    \
            --until PATTERN               Terminate when PATTERN appears in output\n    \
            --socket PATH                 Unix socket path for input injection\n    \
            --on-child-suspend=follow|auto-resume\n                                  \
                Action when the child is stopped\n                                  \
                (default: follow; headless: auto-resume)\n    \
            --on-parent-suspend=transparent|decouple\n                                  \
                Action when the parent is stopped\n                                  \
                (default: transparent; headless: decouple)\n    \
            -h, --help                    Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            SHELL            Fallback command when none is given (legacy)\n    \
            XDG_RUNTIME_DIR  Base directory for the auto-generated socket path\n    \
            TMPDIR           Socket path base when XDG_RUNTIME_DIR is unset\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。sub-ms (ns/us/μs) は accept、内部 ns 集積 → ms に floor\n    \
            (例: 500us 600us = 1.1ms → 1ms)。bare 数字 / 年 (y) / 月 (M) は **error**。\n    \
            case-insensitive。\n",
    )
}

fn usage_attach() -> String {
    String::from(
        "hyoui attach — attach to an existing daemon session\n\
        \n\
        USAGE:\n    \
            hyoui attach <session-id> [options]\n    \
            hyoui attach --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH         Explicit socket path (alternative to session-id)\n    \
            --mode rw|ro|rw-no-leader\n                          \
                Operating mode (default: rw)\n    \
            --exclusive           Demand exclusive session ownership at start\n    \
            --detach-others       Drop other attached clients on connect\n    \
            -h, --help            Show this help and exit\n\
        \n\
        DETACH KEY (= session を生かしたまま client だけ抜ける):\n    \
            Ctrl-A d              detach (session 維持 + 自分だけ Detach 送って終了)\n    \
            Ctrl-A Ctrl-A         escape — literal Ctrl-A を子 PTY に送る\n    \
            Ctrl-A <other>        prefix と当該キー両方を捨てる (= screen 慣例)\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_DETACH_PREFIX   detach prefix byte をカスタマイズ。値の形式:\n                                  \
                * `Ctrl-B` / `^B` (= 0x02)\n                                  \
                * `0x02` (hex)\n                                  \
                * `2` (decimal 0..=255)\n                                  \
                * `none` / `off` / `disable` (= detach key 無効化)\n                                  \
                未設定なら Ctrl-A (0x01)\n    \
            HYOUI_LOCK_TOKEN      lock token を env で渡す (= handshake.token)\n\
        \n\
        EXAMPLES:\n    \
            hyoui attach demo                       # session_id=demo に attach\n    \
            hyoui attach --socket=/tmp/x.sock       # 直接 socket 指定\n    \
            hyoui attach demo --mode=ro             # 読み取り専用 attach\n    \
            hyoui attach demo --detach-others       # 他 client を蹴って奪う\n    \
            HYOUI_DETACH_PREFIX=Ctrl-B hyoui attach demo  # prefix を Ctrl-B に変更\n\
        \n\
        RELATED:\n    \
            hyoui run --detached    daemon を background 起動\n    \
            hyoui list              attach 可能な session 一覧\n    \
            hyoui status <id>       session 状態を 1 度取得\n    \
            hyoui tail <id>         scrollback / live stream を流す\n    \
            hyoui wait <id> ...     条件達成まで block する\n    \
            hyoui kill <id>         daemon に SIGTERM を送って終了\n",
    )
}

fn usage_status() -> String {
    String::from(
        "hyoui status — print session status and exit\n\
        \n\
        USAGE:\n    \
            hyoui status <session-id>\n    \
            hyoui status --socket=<path>\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        OUTPUT (plaintext key:value 1 行ごと):\n    \
            session-id: <name>\n    \
            child-pid: <pid>  または  child-pid: (exited)\n    \
            scrollback-bytes: <N>\n    \
            lock-holder: client <id>  または  lock-holder: (none)\n    \
            clients:\n              \
                - id=<n> mode=<Rw|Ro|RwNoLeader>[ leader]\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了\n    \
            1   connect / I/O 失敗\n    \
            2   引数不足 (session-id も --socket も無し)\n",
    )
}

fn usage_tail() -> String {
    String::from(
        "hyoui tail — stream scrollback / live output\n\
        \n\
        USAGE:\n    \
            hyoui tail <session-id> [options]\n    \
            hyoui tail --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH        Explicit socket path (alternative to session-id)\n    \
            --follow             子 PTY exit / TailEnd まで stream を継続する\n    \
            --strip-ansi         ANSI escape を strip 済の bytes を受け取る (best-effort)\n    \
            --since DUR          過去 DUR 以内の chunk のみ流す。単位必須 (例: 500ms / 2s / 1m)\n    \
            --last-bytes N       末尾 N bytes に絞る\n    \
            -h, --help           Show this help and exit\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            単位: ns / us / μs / ms / s / m / h / d / w (短形)、または\n    \
            millisecond(s) / second(s) / sec / minute(s) / min / hour(s) /\n    \
            day(s) / week(s) (長形)。\n    \
            decimal: 1.5h, underscore: 1_000ms, 連結: 1h30m, 加減: 1d-4h。\n    \
            sub-ms (ns/us/μs) も accept、内部 ns 集積後に ms へ floor:\n              \
                500us 600us → 1.1ms → 1ms (= 集積で 1ms 超過分のみ取り入れ)\n              \
                999us → 0.999ms → 0ms\n    \
            bare 数字 (= 単位なし) は **error**。年 (y) / 月 (M) は単位固定でない\n    \
            ため対応せず。\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= TailEnd 受信 or socket close)\n    \
            1   connect / I/O 失敗\n\
        \n\
        EXAMPLES:\n    \
            hyoui tail demo                       # 全 scrollback 1 度だけ流して exit\n    \
            hyoui tail demo --follow              # live stream を継続\n    \
            hyoui tail demo --since=10s           # 過去 10 秒分\n    \
            hyoui tail demo --last-bytes=8192     # 末尾 8 KiB\n\
        \n\
        RELATED:\n    \
            hyoui wait <id> ...       条件達成まで block (= polling 代替)\n    \
            hyoui status <id>         clients / lock 状態を 1 度取得\n",
    )
}

fn usage_wait() -> String {
    String::from(
        "hyoui wait — wait until predicate matches\n\
        \n\
        USAGE:\n    \
            hyoui wait <session-id> <predicate> [options]\n    \
            hyoui wait --socket=<path> <predicate> [options]\n\
        \n\
        PREDICATES:\n    \
            text:<str>          substring 一致 (literal match)\n    \
            pattern:<regex>     regex 一致 (regex crate、unicode-perl features)\n    \
            wait-idle:<dur>     <dur> 静寂で成立 (= 子の master 出力が無い時間)\n    \
            wait:<dur>          wait-idle のエイリアス\n\
        \n\
        OPTIONS:\n    \
            --socket PATH         Explicit socket path (alternative to session-id)\n    \
            --timeout DUR         絶対 timeout。**指定なしは無限 wait**\n    \
            --no-strip-escapes    マッチ前に ANSI escape を strip しない (default は strip)\n    \
            --newline-convert-lf  CRLF → LF 正規化\n    \
            -h, --help            Show this help and exit\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。sub-ms (ns/us/μs) は accept、内部 ns 集積 → ms に floor\n    \
            (例: 500us 600us = 1.1ms → 1ms)。bare 数字 / 年 (y) / 月 (M) は **error**。\n\
        \n\
        EXIT CODE:\n    \
            0   Matched\n    \
            1   Timeout\n    \
            2   Cancelled (= client detach / connection lost)\n    \
            3   ChildExited (= 子 PTY が条件未達のまま exit)\n    \
            ※ 旧版は ChildExited を 130 にしていたが、慣例 (128+SIGINT) と衝突\n      \
            するため 3 に変更\n\
        \n\
        EXAMPLES:\n    \
            hyoui wait demo text:READY --timeout=5s\n    \
            hyoui wait demo pattern:'ITEM-\\d+' --timeout=30s\n    \
            hyoui wait demo wait-idle:500ms\n",
    )
}

fn usage_list() -> String {
    String::from(
        "hyoui list — list daemon sessions (= socket dir scan + liveness probe)\n\
        \n\
        USAGE:\n    \
            hyoui list [--prune-stale]\n\
        \n\
        OPTIONS:\n    \
            --prune-stale   stale socket (= connect 不能) を unlink で削除\n    \
            -h, --help      Show this help and exit\n\
        \n\
        OUTPUT (TAB separated, 1 line per session):\n    \
            <session-id>\\t<live|stale>\\t<socket-path>\n\
        \n\
        LIVENESS PROBE (R5-H3):\n    \
            各 socket に対し best-effort connect 試行 (= 100ms timeout)。\n    \
            成功なら `live`、ECONNREFUSED / timeout なら `stale` 表示。\n    \
            stale は daemon の panic / SIGKILL で socket が unlink されずに\n    \
            残留した状態。`hyoui list --prune-stale` で掃除可能。\n\
        \n\
        SCAN ORDER (= socket_path::resolve と同順、最初に見つかった dir のみ):\n    \
            1. $XDG_RUNTIME_DIR/hyoui/\n    \
            2. $TMPDIR/hyoui-<uid>/  (TMPDIR 未設定なら /tmp/hyoui-<uid>/)\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= 0 件でも成功扱い、stderr に `no sessions found` を 1 行)\n\
        \n\
        EXAMPLES:\n    \
            hyoui list                              # 全 session を一覧 (live/stale 表示)\n    \
            hyoui list --prune-stale                # stale socket を削除して live のみ残す\n    \
            hyoui list | awk '$2 == \"live\" {print $1}'  # live session id を抽出\n\
        \n\
        RELATED:\n    \
            hyoui status <id>   session 1 件の詳細\n    \
            hyoui attach <id>   session に接続\n    \
            hyoui kill <id>     session を終了\n",
    )
}

fn usage_kill() -> String {
    String::from(
        "hyoui kill — send signal to a daemon session and terminate it\n\
        \n\
        USAGE:\n    \
            hyoui kill <session-id> [options]\n    \
            hyoui kill --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --signum N      Signal number to send to the child PTY (default: SIGTERM=15)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        EXIT CODE:\n    \
            0   送信完了 (= daemon が close するのを待ってから exit)\n    \
            1   connect / send 失敗\n    \
            2   引数不足 (session-id も --socket も無し)\n\
        \n\
        EXAMPLES:\n    \
            hyoui kill demo                         # session_id=demo に SIGTERM\n    \
            hyoui kill demo --signum=9              # SIGKILL を送る\n    \
            hyoui kill --socket=/tmp/x.sock         # socket 直指定で kill\n\
        \n\
        RELATED:\n    \
            hyoui list          attach 可能な session 一覧 (= 対象選び)\n    \
            hyoui status <id>   session の現在状態を確認\n",
    )
}

fn usage_completion() -> String {
    String::from(
        "hyoui completion — print a shell completion script\n\
        \n\
        USAGE:\n    \
            hyoui completion <shell>\n\
        \n\
        OPTIONS:\n    \
            -h, --help      Show this help and exit\n\
        \n\
        SHELLS:\n    \
            bash    Bourne-Again SHell。`source <(hyoui completion bash)` 等で読み込む\n    \
            zsh     Z Shell。`fpath` に置く or `eval` 経由で読み込む\n    \
            fish    Friendly Interactive SHell。`~/.config/fish/completions/` 配下に置く\n\
        \n\
        EXAMPLES:\n    \
            # bash: 現在の shell に直接読ませる\n    \
            source <(hyoui completion bash)\n\
        \n    \
            # zsh: fpath 配下に保存して再起動で有効化\n    \
            hyoui completion zsh > ~/.zsh/completions/_hyoui\n\
        \n    \
            # fish: 自動読み込みディレクトリへ配置\n    \
            hyoui completion fish > ~/.config/fish/completions/hyoui.fish\n\
        \n\
        EXIT CODE:\n    \
            0   script を stdout に出力して正常終了\n    \
            2   shell 名未指定 / 未知 shell / 引数過多\n\
        \n\
        RELATED:\n    \
            hyoui --help        全 subcommand 一覧\n",
    )
}

// =============================================================================
// Parsing helpers (mirror the bootstrap MoonBit implementation)
// =============================================================================

/// Parse a non-negative integer string. Returns `None` on invalid input.
fn parse_int(s: &str) -> Option<i32> {
    if s.is_empty() {
        return None;
    }
    let mut value: i32 = 0;
    for ch in s.chars() {
        if let Some(d) = ch.to_digit(10) {
            value = value.checked_mul(10)?.checked_add(d as i32)?;
        } else {
            return None;
        }
    }
    Some(value)
}

/// Parse a "COLSxROWS" size string. Returns `(cols, rows)` or `None`.
fn parse_size(s: &str) -> Option<(i32, i32)> {
    let bytes = s.as_bytes();
    let mut x_index: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'x' || b == b'X' {
            x_index = Some(i);
            break;
        }
    }
    let x_index = x_index?;
    if x_index == 0 || x_index + 1 >= bytes.len() {
        return None;
    }
    let cols_str = &s[..x_index];
    let rows_str = &s[x_index + 1..];
    match (parse_int(cols_str), parse_int(rows_str)) {
        (Some(c), Some(r)) => Some((c, r)),
        _ => None,
    }
}

/// Split `"--name=value"` into `("--name", Some("value"))`, or `"--name"` into
/// `("--name", None)`.
fn split_eq(arg: &str) -> (String, Option<String>) {
    if let Some(idx) = arg.find('=') {
        (arg[..idx].to_string(), Some(arg[idx + 1..].to_string()))
    } else {
        (arg.to_string(), None)
    }
}

/// `session_id` の最大長 (= 64 chars、POSIX `NAME_MAX` の半分以下に抑える)。
///
/// socket file 名は `<session_id>.sock` なので、parent dir + name で
/// `PATH_MAX` を割ることはまずないが、上限を切ることで CBOR / ANSI escape
/// 等の異常入力経路を早期 reject する (R5-AUD-C2 path traversal 対策)。
pub const MAX_SESSION_ID_LEN: usize = 64;

/// `session_id` を path traversal / 制御文字 / 過長から守る whitelist validator。
///
/// 許可: `[A-Za-z0-9._-]{1,64}`。さらに以下を明示 reject:
///
/// - 空 string (= "")
/// - `.` 単独、`..` 単独 (= path 構成要素として親 dir 参照になる)
/// - `/` / `\` を含む (= path separator、whitelist 外だが冗長 reject)
///
/// CLI argv parser 段階での早期 reject と、`socket_path::resolve` の前段
/// 防御の **双方** で呼ばれる (= R5-AUD-C2 defense-in-depth)。
///
/// # Errors
///
/// validator に反する場合、人間可読な reason 文字列を返す。
pub fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty() {
        return Err("session_id must not be empty".into());
    }
    if session_id.len() > MAX_SESSION_ID_LEN {
        return Err(format!(
            "session_id too long ({} bytes, max {MAX_SESSION_ID_LEN})",
            session_id.len()
        ));
    }
    if session_id == "." || session_id == ".." {
        return Err(format!(
            "session_id {session_id:?} is a path traversal component"
        ));
    }
    for (idx, ch) in session_id.char_indices() {
        let ok = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-';
        if !ok {
            return Err(format!(
                "session_id contains invalid character {ch:?} at byte {idx} \
                 (allowed: [A-Za-z0-9._-])"
            ));
        }
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Vec<String>` from string literals — keeps tests terse.
    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    // -------- Ported from cli_wbtest.mbt (21 tests, all under `run`) --------

    #[test]
    fn no_args_shows_help() {
        match parse_args(&args(&[])) {
            Command::Help {
                topic: HelpTopic::Top,
            } => {}
            other => panic!("expected top Help, got {other:?}"),
        }
    }

    #[test]
    fn help_flag_shows_help() {
        match parse_args(&args(&["--help"])) {
            Command::Help {
                topic: HelpTopic::Top,
            } => {}
            other => panic!("expected top Help, got {other:?}"),
        }
    }

    #[test]
    fn run_missing_command_is_error() {
        match parse_args(&args(&["run", "--mode=headless"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_empty_command_after_dashdash_is_error() {
        match parse_args(&args(&["run", "--"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_simple_command() {
        match parse_args(&args(&["run", "--", "echo", "hello"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.command, vec!["echo".to_string(), "hello".to_string()]);
                assert_eq!(cfg.mode, Mode::Interactive);
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Follow);
                assert_eq!(cfg.on_parent_suspend, OnParentSuspend::Transparent);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_headless_preset_flips_suspend_defaults() {
        match parse_args(&args(&["run", "--mode=headless", "--", "cat"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.mode, Mode::Headless);
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::AutoResume);
                assert_eq!(cfg.on_parent_suspend, OnParentSuspend::Decouple);
                assert_eq!(cfg.cols, 80);
                assert_eq!(cfg.rows, 24);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_explicit_suspend_overrides_headless_preset() {
        match parse_args(&args(&[
            "run",
            "--mode=headless",
            "--on-child-suspend=follow",
            "--on-parent-suspend=transparent",
            "--",
            "cat",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Follow);
                assert_eq!(cfg.on_parent_suspend, OnParentSuspend::Transparent);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_size_parses_cols_and_rows() {
        match parse_args(&args(&["run", "--size", "120x40", "--", "vim"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.cols, 120);
                assert_eq!(cfg.rows, 40);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_cols_and_rows_separately() {
        match parse_args(&args(&[
            "run", "--cols", "100", "--rows", "30", "--", "top",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.cols, 100);
                assert_eq!(cfg.rows, 30);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_invalid_size_is_error() {
        match parse_args(&args(&["run", "--size", "abc", "--", "cat"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_with_unit() {
        match parse_args(&args(&["run", "--timeout", "5s", "--", "sleep", "10"])) {
            Command::Run(cfg) => assert_eq!(cfg.timeout_ms, Some(5000)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_concatenated_units() {
        match parse_args(&args(&["run", "--timeout", "1m30s", "--", "sleep", "200"])) {
            Command::Run(cfg) => assert_eq!(cfg.timeout_ms, Some(90_000)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_bare_number_is_error() {
        match parse_args(&args(&["run", "--timeout", "5", "--", "sleep", "10"])) {
            Command::Error(_) => {}
            other => panic!("expected Error (bare numbers not allowed), got {other:?}"),
        }
    }

    #[test]
    fn run_idle_timeout_and_until() {
        match parse_args(&args(&[
            "run",
            "--idle-timeout=2s",
            "--until",
            "DONE",
            "--",
            "make",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.idle_timeout_ms, Some(2000));
                assert_eq!(cfg.until, Some("DONE".to_string()));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_socket_explicit_path() {
        match parse_args(&args(&["run", "--socket", "/tmp/x.sock", "--", "sh"])) {
            Command::Run(cfg) => assert_eq!(cfg.socket, Some("/tmp/x.sock".to_string())),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_unknown_option_is_error() {
        match parse_args(&args(&["run", "--bogus", "--", "cat"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_option_without_value_is_error() {
        match parse_args(&args(&["run", "--timeout"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_command_args_with_leading_dashes_preserved() {
        match parse_args(&args(&["run", "--", "ls", "-la", "--color"])) {
            Command::Run(cfg) => {
                assert_eq!(
                    cfg.command,
                    vec!["ls".to_string(), "-la".to_string(), "--color".to_string(),]
                );
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_invalid_mode_is_error() {
        match parse_args(&args(&["run", "--mode=weird", "--", "cat"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // parse_seconds_ms は撤去済 (= duration parser に統合)

    #[test]
    fn parse_size_edge_cases() {
        assert_eq!(parse_size("80x24"), Some((80, 24)));
        assert_eq!(parse_size("80X24"), Some((80, 24)));
        assert_eq!(parse_size("x24"), None);
        assert_eq!(parse_size("80x"), None);
        assert_eq!(parse_size("80"), None);
    }

    #[test]
    fn usage_top_non_empty() {
        let text = usage(&HelpTopic::Top);
        assert!(!text.is_empty());
        assert!(text.contains("SUBCOMMANDS"));
        assert!(text.contains("run"));
        assert!(text.contains("completion"));
    }

    // -------- New tests for subcommand-style CLI --------

    #[test]
    fn short_help_flag() {
        assert!(matches!(
            parse_args(&args(&["-h"])),
            Command::Help {
                topic: HelpTopic::Top
            }
        ));
    }

    #[test]
    fn version_flag_long() {
        assert!(matches!(
            parse_args(&args(&["--version"])),
            Command::Version
        ));
    }

    #[test]
    fn version_flag_short() {
        assert!(matches!(parse_args(&args(&["-V"])), Command::Version));
    }

    #[test]
    fn unknown_subcommand_returns_help_with_topic() {
        match parse_args(&args(&["foo"])) {
            Command::Help {
                topic: HelpTopic::UnknownSubcommand(name),
            } => {
                assert_eq!(name, "foo");
            }
            other => panic!("expected UnknownSubcommand Help, got {other:?}"),
        }
    }

    #[test]
    fn run_alone_is_error() {
        match parse_args(&args(&["run"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_help_shows_run_topic() {
        assert!(matches!(
            parse_args(&args(&["run", "--help"])),
            Command::Help {
                topic: HelpTopic::Run
            }
        ));
        assert!(matches!(
            parse_args(&args(&["run", "-h"])),
            Command::Help {
                topic: HelpTopic::Run
            }
        ));
    }

    #[test]
    fn run_help_after_dashdash_is_command_arg_not_help() {
        // `--help` after `--` is part of the child command, not hyoui's help.
        match parse_args(&args(&["run", "--", "cmd", "--help"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.command, vec!["cmd".to_string(), "--help".to_string()]);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn completion_bash() {
        assert!(matches!(
            parse_args(&args(&["completion", "bash"])),
            Command::Completion { shell: Shell::Bash }
        ));
    }

    #[test]
    fn completion_zsh() {
        assert!(matches!(
            parse_args(&args(&["completion", "zsh"])),
            Command::Completion { shell: Shell::Zsh }
        ));
    }

    #[test]
    fn completion_fish() {
        assert!(matches!(
            parse_args(&args(&["completion", "fish"])),
            Command::Completion { shell: Shell::Fish }
        ));
    }

    #[test]
    fn completion_no_shell_is_error() {
        assert!(matches!(
            parse_args(&args(&["completion"])),
            Command::Error(_)
        ));
    }

    #[test]
    fn completion_unknown_shell_is_error() {
        assert!(matches!(
            parse_args(&args(&["completion", "tcsh"])),
            Command::Error(_)
        ));
    }

    #[test]
    fn completion_too_many_args_is_error() {
        assert!(matches!(
            parse_args(&args(&["completion", "bash", "extra"])),
            Command::Error(_)
        ));
    }

    #[test]
    fn completion_help() {
        assert!(matches!(
            parse_args(&args(&["completion", "--help"])),
            Command::Help {
                topic: HelpTopic::Completion
            }
        ));
    }

    /// R5-FB6: `hyoui completion --help` の usage 出力が completion 専用 topic を
    /// 含んでいること (= R4-H1 で list/kill の help 配線を直したのと同じパターン
    /// を completion にも適用)。旧版は `usage_completion()` が骨組み 1 行だけで
    /// SHELLS / EXAMPLES / RELATED が欠落していた。
    #[test]
    fn completion_help_routes_to_completion_topic() {
        // -h でも --help でも completion topic に飛ぶ
        for flag in ["--help", "-h"] {
            match parse_args(&args(&["completion", flag])) {
                Command::Help {
                    topic: HelpTopic::Completion,
                } => {}
                other => {
                    panic!("expected HelpTopic::Completion for `completion {flag}`, got {other:?}")
                }
            }
        }
        // 中身は usage_completion() 由来 (= 上の usage_subcommand_help_routes_to_topic
        // と機能重複だが、SHELLS / EXAMPLES / RELATED 節の存在を明示確認する
        // regression guard)
        let text = usage(&HelpTopic::Completion);
        for needle in [
            "hyoui completion",
            "SHELLS:",
            "EXAMPLES:",
            "RELATED:",
            "bash",
            "zsh",
            "fish",
        ] {
            assert!(
                text.contains(needle),
                "usage_completion() must contain `{needle}`; got:\n{text}"
            );
        }
        // top-level help と混同していないこと (= R4-H1 regression guard)
        assert!(
            !text.contains("SUBCOMMANDS:\n"),
            "usage_completion() must not contain top-level SUBCOMMANDS; got:\n{text}"
        );
    }

    #[test]
    fn reserved_subcommands_return_error() {
        // attach / list / kill / status / tail / wait は実装済 (= 別 test)。
        // `send` / `detach` はまだ reserved。
        for name in ["send", "detach"] {
            match parse_args(&args(&[name])) {
                Command::Error(msg) => assert!(msg.contains(name), "msg = {msg}"),
                other => panic!("expected Error for `{name}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_status_with_session_id() {
        match parse_args(&args(&["status", "demo"])) {
            Command::Status(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.socket.is_none());
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn parse_status_requires_session_or_socket() {
        match parse_args(&args(&["status"])) {
            Command::Error(msg) => assert!(msg.contains("session id") || msg.contains("socket")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_tail_with_follow_and_since() {
        match parse_args(&args(&["tail", "demo", "--follow", "--since=1s"])) {
            Command::Tail(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.follow);
                assert_eq!(cfg.since_ms, Some(1_000));
            }
            other => panic!("expected Tail, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_text_predicate() {
        match parse_args(&args(&["wait", "demo", "text:READY", "--timeout=5s"])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.predicate, WaitCliPredicate::Text("READY".into()));
                assert_eq!(cfg.timeout_ms, Some(5_000));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_duration_ms_basic_units() {
        assert_eq!(parse_duration_ms("500ms"), Ok(500));
        assert_eq!(parse_duration_ms("2s"), Ok(2_000));
        assert_eq!(parse_duration_ms("1m"), Ok(60_000));
        assert_eq!(parse_duration_ms("3h"), Ok(3 * 3_600_000));
        assert_eq!(parse_duration_ms("1d"), Ok(86_400_000));
        assert_eq!(parse_duration_ms("1w"), Ok(7 * 86_400_000));
    }

    #[test]
    fn parse_duration_ms_concatenation() {
        // 1m5s200ms = 60000 + 5000 + 200 = 65200
        assert_eq!(parse_duration_ms("1m5s200ms"), Ok(65_200));
        // 1m1s = 61000
        assert_eq!(parse_duration_ms("1m1s"), Ok(61_000));
        // 1ms (= ms 単位、minute 1 + second ではない)
        assert_eq!(parse_duration_ms("1ms"), Ok(1));
    }

    #[test]
    fn parse_duration_ms_signed_arithmetic() {
        // 1d-4h = 24h - 4h = 20h = 72_000_000
        assert_eq!(parse_duration_ms("1d-4h"), Ok(20 * 3_600_000));
        // 1d+4h = 28h
        assert_eq!(parse_duration_ms("1d+4h"), Ok(28 * 3_600_000));
        // 途中で負になっても最終が正なら OK
        assert_eq!(
            parse_duration_ms("2h-30m+15m"),
            Ok((120 - 30 + 15) * 60_000)
        );
    }

    #[test]
    fn parse_duration_ms_sub_ms_accepted_with_floor() {
        // ns / us / μs は accept、集積後に ms へ floor (= 1ms 超過分のみ取り入れ)。
        // timespec.mbt は YAGNI で reject していたが、hyoui は集積 floor 方針 (kawaz 確定)。
        assert_eq!(parse_duration_ms("999us"), Ok(0)); // 0.999 ms → floor → 0
        assert_eq!(parse_duration_ms("1500us"), Ok(1)); // 1.5 ms → floor → 1
        assert_eq!(parse_duration_ms("1000us"), Ok(1)); // 1.0 ms
        assert_eq!(parse_duration_ms("999999ns"), Ok(0)); // 0.999999 ms → 0
        assert_eq!(parse_duration_ms("1000000ns"), Ok(1)); // 1.0 ms
        assert_eq!(parse_duration_ms("2000μs"), Ok(2)); // 2.0 ms (multi-byte μ)
        // 集積で 1 ms を超えた分は取り入れ
        assert_eq!(parse_duration_ms("500us 600us"), Ok(1)); // 1.1 ms → floor → 1
        assert_eq!(parse_duration_ms("500us 500us"), Ok(1)); // 1.0 ms ぴったり
        assert_eq!(parse_duration_ms("999us 1us"), Ok(1)); // 1.0 ms 境界
        // 完全混在: 1ms + 1500us = 2.5 ms → 2 ms
        assert_eq!(parse_duration_ms("1ms 1500us"), Ok(2));
    }

    #[test]
    fn parse_duration_ms_long_unit_forms() {
        assert_eq!(parse_duration_ms("3minutes"), Ok(180_000));
        assert_eq!(parse_duration_ms("1hour"), Ok(3_600_000));
        assert_eq!(parse_duration_ms("2days"), Ok(172_800_000));
        assert_eq!(parse_duration_ms("1week"), Ok(604_800_000));
        assert_eq!(parse_duration_ms("500milliseconds"), Ok(500));
        assert_eq!(parse_duration_ms("30sec"), Ok(30_000));
        assert_eq!(parse_duration_ms("5min"), Ok(300_000));
    }

    #[test]
    fn parse_duration_ms_decimal_support() {
        // timespec.mbt 仕様: 1.5h = 5400000ms
        assert_eq!(parse_duration_ms("1.5h"), Ok(5_400_000));
        assert_eq!(parse_duration_ms("3.5s"), Ok(3_500));
        assert_eq!(parse_duration_ms("0.5m"), Ok(30_000));
    }

    #[test]
    fn parse_duration_ms_underscore_separator() {
        assert_eq!(parse_duration_ms("3_600_000ms"), Ok(3_600_000));
        assert_eq!(parse_duration_ms("1_000s"), Ok(1_000_000));
        assert_eq!(parse_duration_ms("1_000.5s"), Ok(1_000_500));
        assert_eq!(parse_duration_ms("1_000.5_0s"), Ok(1_000_500));
    }

    #[test]
    fn parse_duration_ms_whitespace_tolerant() {
        assert_eq!(parse_duration_ms("1h 2m"), Ok(3_720_000));
        assert_eq!(parse_duration_ms(" 1h 2m "), Ok(3_720_000));
        assert_eq!(parse_duration_ms(" 1 h 2 m "), Ok(3_720_000));
    }

    #[test]
    fn parse_duration_ms_duplicate_unit_merge() {
        // 1h5m1h = (1+1)h + 5m = 2h5m = 7_500_000
        assert_eq!(parse_duration_ms("1h5m1h"), Ok(7_500_000));
    }

    #[test]
    fn parse_duration_ms_mixed_long_short() {
        assert_eq!(parse_duration_ms("1hour 30m"), Ok(5_400_000));
        assert_eq!(parse_duration_ms("2 days 5h"), Ok(190_800_000));
    }

    #[test]
    fn parse_duration_ms_rejects_bare_number() {
        assert!(parse_duration_ms("0").is_err());
        assert!(parse_duration_ms("500").is_err());
        assert!(parse_duration_ms("1000").is_err());
    }

    #[test]
    fn parse_duration_ms_rejects_invalid() {
        assert!(parse_duration_ms("").is_err());
        assert!(parse_duration_ms("xs").is_err());
        assert!(parse_duration_ms("1y").is_err()); // y = year は不採用
        assert!(parse_duration_ms("1year").is_err());
        assert!(parse_duration_ms("1month").is_err()); // month 長形は reject
        assert!(parse_duration_ms("ms").is_err()); // 数字なし
        assert!(parse_duration_ms("1m-").is_err()); // 末尾 - 不完全
        assert!(parse_duration_ms("1m+").is_err()); // 末尾 + 不完全
    }

    #[test]
    fn parse_duration_ms_strict_grammar() {
        // D2: leading / consecutive / trailing '_' は error
        assert!(parse_duration_ms("_5s").is_err());
        assert!(parse_duration_ms("5__0s").is_err());
        assert!(parse_duration_ms("5_s").is_err());
        // segments 間の `_` も grammar 違反 (= sign で区切るべき)
        assert!(parse_duration_ms("1h_2m").is_err());
        // D3: trailing dot + 単位 (`1.s`) は error、`.5s` も error
        assert!(parse_duration_ms("1.s").is_err());
        assert!(parse_duration_ms(".5s").is_err());
        assert!(parse_duration_ms("1.").is_err());
        // D5: leading `+`/`-` は grammar で許されてない
        assert!(parse_duration_ms("+5m").is_err());
        // `-5m` も leading sign → error (= 別経路で「最終 < 0」も error だが、
        // 文法層で先に弾く)
        assert!(parse_duration_ms("-5m").is_err());
    }

    #[test]
    fn parse_duration_ms_case_insensitive() {
        // H2: 単位は case-insensitive
        assert_eq!(parse_duration_ms("1S"), Ok(1_000));
        assert_eq!(parse_duration_ms("1H"), Ok(3_600_000));
        assert_eq!(parse_duration_ms("1MIN"), Ok(60_000));
        assert_eq!(parse_duration_ms("1Min"), Ok(60_000));
        assert_eq!(parse_duration_ms("1MS"), Ok(1));
        // 短形 m は minute (case-insensitive)
        assert_eq!(parse_duration_ms("1M"), Ok(60_000));
        // month 長形は引き続き reject
        assert!(parse_duration_ms("1MONTH").is_err());
    }

    #[test]
    fn parse_duration_ms_negative_total_rejected() {
        // 最終 total が負なら error (= D5 で leading sign は文法層で先に弾くが、
        // 中間段階で負になる入力は最終 negative-check で弾く)
        assert!(parse_duration_ms("1h-2h").is_err());
    }

    #[test]
    fn status_tail_wait_help_routes_to_topic() {
        for (sub, expected) in [
            ("status", HelpTopic::Status),
            ("tail", HelpTopic::Tail),
            ("wait", HelpTopic::Wait),
        ] {
            let cmd = parse_args(&args(&[sub, "--help"]));
            match cmd {
                Command::Help { ref topic } if *topic == expected => {}
                other => panic!("expected Help({expected:?}) for {sub}, got {other:?}"),
            }
        }
    }

    #[test]
    fn top_help_lists_new_subcommands() {
        let text = usage(&HelpTopic::Top);
        for sub in ["run", "attach", "list", "kill", "status", "tail", "wait"] {
            assert!(text.contains(sub), "top help should list `{sub}`");
        }
        assert!(!text.contains("attach, status   Socket-based"));
    }

    #[test]
    fn parse_wait_idle_predicate() {
        match parse_args(&args(&["wait", "demo", "wait-idle:500ms"])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.predicate, WaitCliPredicate::Idle(500));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_pattern_predicate() {
        match parse_args(&args(&["wait", "demo", "pattern:ITEM-\\d+"])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.predicate, WaitCliPredicate::Pattern("ITEM-\\d+".into()));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_missing_predicate_errors() {
        match parse_args(&args(&["wait", "demo"])) {
            Command::Error(msg) => assert!(msg.contains("predicate")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn attach_with_session_id() {
        match parse_args(&args(&["attach", "demo"])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.socket, None);
                assert!(!cfg.exclusive);
                assert!(!cfg.detach_others);
                assert_eq!(cfg.mode_str, None);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_with_socket_option() {
        match parse_args(&args(&["attach", "--socket", "/tmp/x.sock"])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/x.sock"));
                assert_eq!(cfg.session_id, None);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_help_routes_to_attach_topic() {
        match parse_args(&args(&["attach", "--help"])) {
            Command::Help {
                topic: HelpTopic::Attach,
            } => {}
            other => panic!("expected Help(Attach), got {other:?}"),
        }
    }

    #[test]
    fn attach_help_text_mentions_detach_key() {
        let text = usage(&HelpTopic::Attach);
        assert!(text.contains("Ctrl-A d"), "help should document Ctrl-A d");
        assert!(text.contains("escape"), "help should document escape");
        assert!(text.contains("--mode"), "help should mention --mode option");
    }

    #[test]
    fn attach_with_mode_and_flags() {
        match parse_args(&args(&[
            "attach",
            "demo",
            "--mode",
            "ro",
            "--exclusive",
            "--detach-others",
        ])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.mode_str.as_deref(), Some("ro"));
                assert!(cfg.exclusive);
                assert!(cfg.detach_others);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_with_no_args_errors() {
        match parse_args(&args(&["attach"])) {
            Command::Error(msg) => assert!(msg.contains("attach")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_with_too_many_positionals_errors() {
        match parse_args(&args(&["attach", "a", "b"])) {
            Command::Error(msg) => assert!(msg.contains("attach")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_unknown_option_errors() {
        match parse_args(&args(&["attach", "demo", "--bogus"])) {
            Command::Error(msg) => assert!(msg.contains("bogus") || msg.contains("attach")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn no_legacy_shortcut_for_dashdash_at_top() {
        // `hyoui -- cmd` must NOT be treated as `hyoui run -- cmd`.
        // `--` is an unknown subcommand here.
        match parse_args(&args(&["--", "echo", "hi"])) {
            Command::Help {
                topic: HelpTopic::UnknownSubcommand(name),
            } => assert_eq!(name, "--"),
            other => panic!("expected UnknownSubcommand Help, got {other:?}"),
        }
    }

    #[test]
    fn run_mode_separate_value() {
        // `--mode interactive` (space-separated) should work too.
        match parse_args(&args(&["run", "--mode", "headless", "--", "cat"])) {
            Command::Run(cfg) => assert_eq!(cfg.mode, Mode::Headless),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn usage_run_non_empty() {
        let text = usage(&HelpTopic::Run);
        assert!(text.contains("hyoui run"));
        assert!(text.contains("--mode"));
    }

    #[test]
    fn usage_unknown_subcommand_mentions_name() {
        let text = usage(&HelpTopic::UnknownSubcommand("frob".into()));
        assert!(text.contains("frob"));
        assert!(text.contains("SUBCOMMANDS"));
    }

    // R4-H1: each subcommand's `--help` must route to the subcommand-specific
    // HelpTopic (not Top). Regression: `hyoui kill --help` previously printed
    // top-level help, which gave users no info about --signum, exit codes, etc.

    #[test]
    fn list_help_routes_to_list_topic() {
        match parse_args(&args(&["list", "--help"])) {
            Command::Help {
                topic: HelpTopic::List,
            } => {}
            other => panic!("expected Help{{List}}, got {other:?}"),
        }
        match parse_args(&args(&["list", "-h"])) {
            Command::Help {
                topic: HelpTopic::List,
            } => {}
            other => panic!("expected Help{{List}}, got {other:?}"),
        }
    }

    /// R5-H3: `list` の引数なし呼び出しは `prune_stale = false` の
    /// `ListConfig` を返す (= default 動作: liveness 確認のみ、削除しない)。
    #[test]
    fn list_without_flag_returns_default_config() {
        match parse_args(&args(&["list"])) {
            Command::List(cfg) => {
                assert!(!cfg.prune_stale, "default should not prune");
            }
            other => panic!("expected List(default), got {other:?}"),
        }
    }

    /// R5-H3: `list --prune-stale` は `prune_stale = true` の `ListConfig` を返す。
    #[test]
    fn list_prune_stale_flag_sets_config() {
        match parse_args(&args(&["list", "--prune-stale"])) {
            Command::List(cfg) => {
                assert!(cfg.prune_stale, "--prune-stale should enable prune");
            }
            other => panic!("expected List(prune_stale=true), got {other:?}"),
        }
    }

    /// R5-H3: 未知の flag は `Command::Error` を返す (= 既存 list の挙動踏襲)。
    #[test]
    fn list_rejects_unknown_flag() {
        match parse_args(&args(&["list", "--bogus"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--bogus"), "error should mention the flag");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn kill_help_routes_to_kill_topic() {
        match parse_args(&args(&["kill", "--help"])) {
            Command::Help {
                topic: HelpTopic::Kill,
            } => {}
            other => panic!("expected Help{{Kill}}, got {other:?}"),
        }
        match parse_args(&args(&["kill", "-h"])) {
            Command::Help {
                topic: HelpTopic::Kill,
            } => {}
            other => panic!("expected Help{{Kill}}, got {other:?}"),
        }
    }

    // NOTE: status/tail/wait の help routing は status_tail_wait_help_routes_to_topic
    // (上記) で既にカバー済み。R4-H1 で新規追加した list/kill は上の専用 test を、
    // 全 subcommand の help text が subcommand-specific であることは下の
    // subcommand_help_text_is_subcommand_specific でまとめてチェックする。

    /// Each subcommand's usage text must contain the subcommand name and at least
    /// one subcommand-specific keyword, so `hyoui <sub> --help` does NOT look
    /// like the top-level help (= the original R4-H1 bug).
    #[test]
    fn subcommand_help_text_is_subcommand_specific() {
        let cases: &[(HelpTopic, &str, &[&str])] = &[
            (HelpTopic::Run, "hyoui run", &["--mode", "--timeout"]),
            (
                HelpTopic::Attach,
                "hyoui attach",
                &["DETACH KEY", "--exclusive"],
            ),
            (HelpTopic::List, "hyoui list", &["SCAN ORDER"]),
            (HelpTopic::Kill, "hyoui kill", &["--signum", "SIGTERM"]),
            (HelpTopic::Status, "hyoui status", &["OUTPUT", "child-pid"]),
            (HelpTopic::Tail, "hyoui tail", &["--follow", "--since"]),
            (HelpTopic::Wait, "hyoui wait", &["PREDICATES", "wait-idle"]),
            (
                HelpTopic::Completion,
                "hyoui completion",
                &["bash", "zsh", "fish", "EXAMPLES", "SHELLS"],
            ),
        ];
        for (topic, head, must_have) in cases {
            let text = usage(topic);
            assert!(
                text.contains(head),
                "topic {topic:?} usage must contain `{head}`; got:\n{text}"
            );
            for needle in must_have.iter() {
                assert!(
                    text.contains(needle),
                    "topic {topic:?} usage must contain `{needle}`; got:\n{text}"
                );
            }
            // Must NOT look like top-level help (= R4-H1 regression guard).
            assert!(
                !text.contains("SUBCOMMANDS:\n"),
                "topic {topic:?} usage must not contain top-level SUBCOMMANDS list; got:\n{text}"
            );
        }
    }

    // ------------------------------------------------------------------
    // R5-AUD-C2: session_id whitelist regression tests (CLI parser side)
    // ------------------------------------------------------------------

    #[test]
    fn parse_run_rejects_invalid_session_id() {
        // `hyoui run --session=<bad>` で path traversal / 制御文字 等を早期 reject。
        let bad = [
            "../../.ssh/control", // path traversal
            "../etc",
            "a/b",           // separator
            "a\\b",          // windows separator
            "..",            // dot-dot literal
            ".",             // dot literal
            "",              // empty (--session= 等で来る)
            "a\nb",          // newline (control char)
            "a\x1b[31mhack", // ANSI escape
            "name with space",
        ];
        for sid in bad {
            let arg = format!("--session={sid}");
            match parse_args(&args(&["run", &arg, "--", "true"])) {
                Command::Error(msg) => {
                    assert!(
                        msg.contains("--session") || msg.contains("session_id"),
                        "error for {sid:?} should mention --session/session_id, got: {msg}"
                    );
                }
                other => panic!("expected Error for invalid session_id {sid:?}, got {other:?}"),
            }
        }

        // 過長 (65 chars) も reject。
        let too_long = "a".repeat(MAX_SESSION_ID_LEN + 1);
        let arg = format!("--session={too_long}");
        match parse_args(&args(&["run", &arg, "--", "true"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("too long"),
                    "error for too-long should mention 'too long', got: {msg}"
                );
            }
            other => panic!("expected Error for too-long session_id, got {other:?}"),
        }
    }

    #[test]
    fn parse_run_accepts_normal_session_id() {
        // 正常系: 一般的な session 名は通る (= 回帰時に既存ユーザを巻き込まない確認)。
        for sid in ["demo", "run-12345", "session_01", "build.2025-05-27"] {
            let arg = format!("--session={sid}");
            match parse_args(&args(&["run", &arg, "--", "true"])) {
                Command::Run(cfg) => {
                    assert_eq!(cfg.session.as_deref(), Some(sid));
                }
                other => panic!("expected Run for valid session_id {sid:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_kill_rejects_invalid_session_id() {
        // positional session_id 経由 (= `hyoui kill <bad>`) でも reject。
        match parse_args(&args(&["kill", "../../.ssh/control"])) {
            Command::Error(msg) => {
                assert!(msg.contains("kill"), "error should mention 'kill': {msg}");
                assert!(
                    msg.contains("invalid character") || msg.contains("path traversal"),
                    "error should explain why, got: {msg}"
                );
            }
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    #[test]
    fn parse_status_rejects_invalid_session_id() {
        // parse_session_targeted 経由 (status/attach/tail) も reject。
        match parse_args(&args(&["status", "a/b"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_rejects_invalid_session_id() {
        // parse_wait の positional path も reject (= predicate と紛らわしいので
        // session_id 側に落ちた値が validate されることを確認)。
        match parse_args(&args(&["wait", "../foo", "text:READY"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }
}
