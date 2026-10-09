//! `hyoui input` の `wait:` spec は、待っている間に子が止まって (= rw の接続に
//! session.child.stopped.notify が届いて) も失敗せず、そのまま待ち続ける。
//!
//! 子は SIGCONT を受けると印を出す perl。input が wait: で待ち始めた (= status の clients に
//! rw の client が見える) 後に子を SIGSTOP し、daemon が停止を観測した (= status の
//! child_stopped が true。notify は同じ処理の中で送り終えている) のを見てから SIGCONT する。
//! 印が出て wait: が一致し、input が 0 で終わることを見る。

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;

/// 各段の待ちの上限。
const DEADLINE: Duration = Duration::from_secs(10);

/// SIGCONT を受けると `CONT_MARK` を出す子。
const CHILD: &str = r#"$| = 1; $SIG{CONT} = sub { print "CONT_MARK\n" }; sleep 1000 while 1;"#;

fn hyoui(root: &Path) -> Command {
    let mut c = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_hyoui")));
    c.env("HYOUI_STATE_DIR", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn status(root: &Path, sid: &str) -> serde_json::Value {
    let out = common::pty::capture_with_deadline(
        hyoui(root).args(["status", sid, "--format=json"]),
        DEADLINE,
    )
    .expect("hyoui status");
    assert!(out.status.success(), "status: {out:?}");
    serde_json::from_slice(&out.stdout).expect("status json")
}

/// `cond` が status で成り立つまで待つ (= 他 process の状態変化を通知で受ける手段が無いので、
/// status を期限付きで繰り返し引く)。
fn wait_status(root: &Path, sid: &str, what: &str, cond: impl Fn(&serde_json::Value) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let v = status(root, sid);
        if cond(&v) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} が成り立たない: {v}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn input_wait_keeps_waiting_while_the_child_is_stopped() {
    let dir = SessionDir::new("hyoui-inwait-stop-");
    let root = dir.path().to_path_buf();
    let boot = common::pty::capture_with_deadline(
        hyoui(&root).args([
            "run",
            "--detached",
            "--pty-stdin",
            "--",
            "perl",
            "-e",
            CHILD,
        ]),
        DEADLINE,
    )
    .expect("run --detached");
    assert!(boot.status.success(), "run --detached: {boot:?}");
    let sid = String::from_utf8_lossy(&boot.stdout).trim().to_string();
    let child = status(&root, &sid)["child_pid"]
        .as_i64()
        .expect("child_pid") as i32;
    let child = nix::unistd::Pid::from_raw(child);

    let input = {
        let root = root.clone();
        let sid = sid.clone();
        std::thread::spawn(move || {
            common::pty::capture_with_deadline(
                hyoui(&root).args(["input", &sid, "wait:CONT_MARK", "--timeout=20s"]),
                Duration::from_secs(30),
            )
        })
    };

    wait_status(
        &root,
        &sid,
        "input の rw client が接続している",
        |v| {
            v["clients"]
                .as_array()
                .is_some_and(|cs| cs.iter().any(|c| c["mode"] != "ro"))
        },
    );
    nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGSTOP).expect("SIGSTOP");
    wait_status(&root, &sid, "daemon が子の停止を観測した", |v| {
        v["child_stopped"] == true
    });
    nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGCONT).expect("SIGCONT");

    let out = common::pty::join_with_deadline(input, Duration::from_secs(40), "hyoui input")
        .expect("hyoui input");
    assert!(
        out.status.success(),
        "input の wait: は子の停止通知で失敗せず、CONT 後の印に一致するはず: rc={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
