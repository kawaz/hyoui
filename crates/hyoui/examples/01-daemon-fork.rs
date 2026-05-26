//! PoC 01: daemon 化 (double-fork) 検証
//!
//! 目的:
//! - double-fork + setsid で完全に init/launchd の子に切り離せるか
//! - 親 shell prompt が即返るか
//! - 孫 process (= daemon) が orphan で生存し続けるか (PPID=1 or launchd の pid)
//!
//! 実行:
//!   cargo run --example 01-daemon-fork
//!   # 即プロンプトが返るはず
//!   cat /tmp/hyoui-poc-01-daemon.log     # daemon の動作ログ確認
//!   ps -p <daemon_pid> -o pid,ppid,stat,command  # 生存と PPID 確認

use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, getppid, setsid, ForkResult};
use std::fs::OpenOptions;
use std::io::Write;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOG_PATH: &str = "/tmp/hyoui-poc-01-daemon.log";
const DAEMON_TICKS: usize = 6;
const TICK_INTERVAL: Duration = Duration::from_secs(5);

fn ts() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{:.3}", now.as_secs_f64())
}

fn main() {
    println!("=== PoC 01: daemon double-fork ===");
    println!("parent pid: {}", std::process::id());
    println!("log file: {LOG_PATH}");

    // 既存ログを消す
    let _ = std::fs::remove_file(LOG_PATH);

    // first fork
    match unsafe { fork() }.expect("first fork failed") {
        ForkResult::Parent { child } => {
            println!("first fork: spawned intermediate pid {child}");
            // 中間プロセスを wait → 即終了する想定 (= ゾンビ化を防ぐ)
            match waitpid(child, None).expect("waitpid failed") {
                WaitStatus::Exited(_, code) => {
                    println!("intermediate exited with code {code}");
                }
                other => {
                    println!("intermediate ended unexpectedly: {other:?}");
                }
            }
            println!("parent exiting; shell prompt should return immediately");
            println!("check daemon: `ps -ef | grep daemon-fork` and `cat {LOG_PATH}`");
        }
        ForkResult::Child => {
            // 中間プロセス: setsid して新セッション、その後 grandchild を fork
            setsid().expect("setsid failed");
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(LOG_PATH)
                .expect("open log file");
            writeln!(
                log,
                "[{}] intermediate (pid {}) after setsid, PPID = {}",
                ts(),
                std::process::id(),
                getppid()
            )
            .ok();

            match unsafe { fork() }.expect("second fork failed") {
                ForkResult::Parent { child } => {
                    writeln!(
                        log,
                        "[{}] intermediate (pid {}) forked grandchild pid {}, exiting",
                        ts(),
                        std::process::id(),
                        child
                    )
                    .ok();
                    // 中間プロセスは即終了 → grandchild は完全に orphan
                    std::process::exit(0);
                }
                ForkResult::Child => {
                    // grandchild = daemon
                    let pid = std::process::id();
                    writeln!(
                        log,
                        "[{}] daemon (pid {}) started, my PPID = {}",
                        ts(),
                        pid,
                        getppid()
                    )
                    .ok();
                    log.flush().ok();
                    for i in 0..DAEMON_TICKS {
                        thread::sleep(TICK_INTERVAL);
                        writeln!(
                            log,
                            "[{}] daemon (pid {}) alive tick {i}, PPID = {}",
                            ts(),
                            pid,
                            getppid()
                        )
                        .ok();
                        log.flush().ok();
                    }
                    writeln!(log, "[{}] daemon (pid {}) exiting", ts(), pid).ok();
                }
            }
        }
    }
}
