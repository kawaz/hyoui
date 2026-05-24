//! `hyoui` binary entry point.
//!
//! Dispatches the parsed [`hyoui::cli::Command`] tree:
//!
//! * `Help`     — print usage to stdout (exit 0 for explicit help, exit 2 for
//!   unknown subcommands so callers can detect misuse from the status code).
//! * `Version`  — print `hyoui <VERSION>` and exit 0.
//! * `Run(cfg)` — drive the PTY proxy via [`hyoui::agent::Agent`]; propagate
//!   the child's exit code (masked to 8 bits, matching shell convention).
//! * `Completion { shell }` — emit a hand-written completion script.
//! * `Error`    — print the diagnostic to stderr and exit 2.
//!
//! The `Agent` is consumed by `run()` (RAII cleanup on Drop); we use the
//! null observer because the binary doesn't surface lifecycle events yet.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use hyoui::agent::Agent;
use hyoui::cli::{Command, HelpTopic, parse_args, usage};
use hyoui::observer::NullObserver;

mod completion;

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

        Command::Run(cfg) => match Agent::new(cfg, Box::new(NullObserver::new())) {
            Ok(agent) => match agent.run() {
                Ok(code) => {
                    // Shells mask exit codes to 8 bits; mirror that so a
                    // child that exited 256 doesn't silently become 0.
                    let masked = u8::try_from(code & 0xFF).unwrap_or(255);
                    ExitCode::from(masked)
                }
                Err(e) => {
                    eprintln!("hyoui: {e}");
                    ExitCode::from(1)
                }
            },
            Err(e) => {
                eprintln!("hyoui: {e}");
                ExitCode::from(1)
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
    }
}
