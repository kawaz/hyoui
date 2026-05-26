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
pub enum Mode {
    /// Pass the parent terminal through (default).
    Interactive,
    /// Drive the child with a virtual PTY of fixed size; no terminal needed.
    Headless,
}

/// Behavior when the child process is suspended (STOPPED).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnChildSuspend {
    /// Follow the child: the parent also stops (SIGSTOP raised on self).
    Follow,
    /// Resume the child immediately by sending SIGCONT.
    AutoResume,
}

/// Behavior when the parent process is suspended (receives SIGTSTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnParentSuspend {
    /// Stop the child group first, then stop the parent.
    Transparent,
    /// Stop only the parent; leave the child running.
    Decouple,
}

/// Shell whose completion script is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub enum HelpTopic {
    /// Help for the top-level invocation (subcommand list + global options).
    Top,
    /// Help for the `run` subcommand.
    Run,
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
    pub timeout_ms: Option<i64>,
    /// Output idle timeout in milliseconds, or `None` if unset.
    pub idle_timeout_ms: Option<i64>,
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

/// Result of parsing argv (excluding argv[0]).
#[derive(Debug, Clone, PartialEq, Eq)]
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
        "completion" => parse_completion(rest),
        // Reserved for future stages.
        "send" | "status" | "list" | "kill" | "detach" => Command::Error(format!(
            "subcommand `{head}` is reserved but not yet implemented"
        )),
        other => Command::Help {
            topic: HelpTopic::UnknownSubcommand(other.to_string()),
        },
    }
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
                    topic: HelpTopic::Top, // attach 専用 help は v0.2.0 で
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
                    "attach: session id (positional) or --socket required".into(),
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
    let mut timeout_ms: Option<i64> = None;
    let mut idle_timeout_ms: Option<i64> = None;
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
                Some(v) => match parse_seconds_ms(v) {
                    Some(ms) => timeout_ms = Some(ms),
                    None => return Command::Error(format!("invalid --timeout value: {v}")),
                },
                None => return Command::Error("--timeout requires a value".into()),
            },
            "--idle-timeout" => match value.as_deref() {
                Some(v) => match parse_seconds_ms(v) {
                    Some(ms) => idle_timeout_ms = Some(ms),
                    None => return Command::Error(format!("invalid --idle-timeout value: {v}")),
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
                Some(v) => session = Some(v),
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
            completion  Print a shell completion script (bash|zsh|fish)\n\
        \n\
        RESERVED (not yet implemented):\n    \
            send, attach, status   Socket-based remote control\n\
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
            --timeout SEC                 Overall timeout in seconds\n    \
            --idle-timeout SEC            Output idle timeout in seconds\n    \
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
            TMPDIR           Socket path base when XDG_RUNTIME_DIR is unset\n",
    )
}

fn usage_completion() -> String {
    String::from(
        "hyoui completion — print a shell completion script\n\
        \n\
        USAGE:\n    \
            hyoui completion <bash|zsh|fish>\n",
    )
}

// =============================================================================
// Parsing helpers (mirror the bootstrap MoonBit implementation)
// =============================================================================

/// Parse a seconds string (integer or decimal) into milliseconds.
/// Returns `None` on invalid input.
fn parse_seconds_ms(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut int_part: i64 = 0;
    let mut frac_part: i64 = 0;
    let mut frac_digits = 0;
    let mut seen_dot = false;
    let mut any_digit = false;
    for ch in s.chars() {
        if ch == '.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
        } else if ch.is_ascii_digit() {
            any_digit = true;
            let d = (ch as i64) - ('0' as i64);
            if !seen_dot {
                int_part = int_part * 10 + d;
            } else if frac_digits < 3 {
                frac_part = frac_part * 10 + d;
                frac_digits += 1;
            }
            // extra fractional digits beyond millisecond precision are dropped
        } else {
            return None;
        }
    }
    if !any_digit {
        return None;
    }
    // scale frac_part up to exactly 3 digits (milliseconds)
    while frac_digits < 3 {
        frac_part *= 10;
        frac_digits += 1;
    }
    Some(int_part * 1000 + frac_part)
}

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
    fn run_timeout_integer_seconds() {
        match parse_args(&args(&["run", "--timeout", "5", "--", "sleep", "10"])) {
            Command::Run(cfg) => assert_eq!(cfg.timeout_ms, Some(5000)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_decimal_seconds() {
        match parse_args(&args(&["run", "--timeout", "1.5", "--", "sleep", "10"])) {
            Command::Run(cfg) => assert_eq!(cfg.timeout_ms, Some(1500)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_idle_timeout_and_until() {
        match parse_args(&args(&[
            "run",
            "--idle-timeout=2",
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

    #[test]
    fn parse_seconds_ms_edge_cases() {
        assert_eq!(parse_seconds_ms("0"), Some(0));
        assert_eq!(parse_seconds_ms("10"), Some(10000));
        assert_eq!(parse_seconds_ms("0.001"), Some(1));
        assert_eq!(parse_seconds_ms("2.5"), Some(2500));
        assert_eq!(parse_seconds_ms(""), None);
        assert_eq!(parse_seconds_ms("abc"), None);
        assert_eq!(parse_seconds_ms("1.2.3"), None);
    }

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

    #[test]
    fn reserved_subcommands_return_error() {
        // attach は実装済 (= 別 test)。`send` / `status` / `list` / `kill` / `detach` は
        // まだ reserved (Phase A4+ で実装)。
        for name in ["send", "status", "list", "kill", "detach"] {
            match parse_args(&args(&[name])) {
                Command::Error(msg) => assert!(msg.contains(name), "msg = {msg}"),
                other => panic!("expected Error for `{name}`, got {other:?}"),
            }
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
}
