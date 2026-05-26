//! PoC 01b: daemon 化 (double-fork + stdin/stdout/stderr detach) 検証
//!
//! 01-daemon-fork の改良版: daemon が stdin/stdout/stderr を /dev/null にリダイレクトすることで、
//! parent shell の pipe が即解放されるか確認 (= cargo run | tail が 30 秒待たない)

use nix::sys::wait::waitpid;
use nix::unistd::{ForkResult, fork, getppid, setsid};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOG_PATH: &str = "/tmp/hyoui-poc-01b-daemon.log";
const DAEMON_TICKS: usize = 6;
const TICK_INTERVAL: Duration = Duration::from_secs(5);

fn ts() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{:.3}", now.as_secs_f64())
}

fn main() {
    println!("=== PoC 01b: daemon double-fork + stdio detach ===");
    println!("parent pid: {}", std::process::id());
    let _ = std::fs::remove_file(LOG_PATH);

    match unsafe { fork() }.expect("first fork failed") {
        ForkResult::Parent { child } => {
            waitpid(child, None).ok();
            println!("parent exiting; shell prompt + pipeline should return immediately");
        }
        ForkResult::Child => {
            setsid().expect("setsid failed");
            match unsafe { fork() }.expect("second fork failed") {
                ForkResult::Parent { .. } => {
                    std::process::exit(0);
                }
                ForkResult::Child => {
                    // grandchild = daemon
                    // stdin/stdout/stderr を /dev/null にリダイレクト
                    let devnull = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open("/dev/null")
                        .expect("open /dev/null");
                    let nullfd = devnull.as_raw_fd();
                    // SAFETY: dup2 to 0/1/2 is well-defined; we are sole owner of these fds in
                    // this freshly-forked process. nix 0.31 の dup2 は OwnedFd を要求するため
                    // libc 直叩きの方が単純。
                    unsafe {
                        libc::dup2(nullfd, 0);
                        libc::dup2(nullfd, 1);
                        libc::dup2(nullfd, 2);
                    }
                    drop(devnull);

                    let mut log = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(LOG_PATH)
                        .expect("open log file");
                    let pid = std::process::id();
                    writeln!(
                        log,
                        "[{}] daemon (pid {}) detached stdio, PPID = {}",
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
