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

use std::process::ExitCode;

use hyoui::cli::{Command, HelpTopic, parse_args, usage};
use hyoui::daemon::{DaemonConfig, Session};

mod completion;
mod socket_path;

fn main() -> ExitCode {
    // Skip argv[0]: parse_args expects the trailing arguments only.
    let argv: Vec<String> = std::env::args().skip(1).collect();
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

        Command::Run(cfg) => {
            let session_id = socket_path::auto_session_id();
            let sock = match socket_path::resolve(cfg.socket.as_deref(), &session_id) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("hyoui: socket path 解決失敗: {e}");
                    return ExitCode::from(1);
                }
            };
            let cols = u16::try_from(cfg.cols).unwrap_or(80);
            let rows = u16::try_from(cfg.rows).unwrap_or(24);
            let mut dcfg = DaemonConfig::new(session_id, sock, cfg.command);
            dcfg.cols = cols;
            dcfg.rows = rows;

            let session = match Session::start(dcfg) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("hyoui: daemon 起動失敗: {e}");
                    return ExitCode::from(1);
                }
            };
            match session.run() {
                Ok(code) => {
                    // shell convention: 8 bit mask
                    let masked = u8::try_from(code & 0xFF).unwrap_or(255);
                    ExitCode::from(masked)
                }
                Err(e) => {
                    eprintln!("hyoui: daemon 実行エラー: {e}");
                    ExitCode::from(1)
                }
            }
        }

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
