//! test 用の状態の root (= socket 置き場、`HYOUI_STATE_DIR` に渡す dir) と、その寿命に
//! 結び付けた session 後始末。
//!
//! detached session の daemon は fork + setsid で test process から切り離されて
//! init の子になる。test が自分で畳み損ねると (= setup 途中の panic、client が先に
//! 抜けて harness の subtree kill が daemon に届かない等) daemon と子は誰にも回収
//! されず、runtime dir の TempDir が消えて socket が無くなった後も残り続ける
//! (= daemon は socket file の消滅を監視しない)。
//!
//! そこで **runtime dir 自体に後始末の責任を持たせる**: [`SessionDir`] の `Drop` は
//! TempDir を消す前に配下の全 socket を走査し、listen している daemon を畳む。
//! session を起こした経路 (`run --detached` / PTY 内の `run` / attach client の
//! 有無) に依らず、socket を runtime dir 配下に置く限り漏れない。test 側の明示
//! cleanup はそのまま使ってよい (= 畳み済みの socket は走査で素通りする)。

#![allow(dead_code)] // 各 test は subset しか使わないため

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use super::pty::{capture_with_deadline, find_descendants};

/// CLI helper (`status` / `kill`) の実行 deadline。wedge した daemon で test の
/// teardown が無限に止まらないようにする。
const CLI_DEADLINE: Duration = Duration::from_secs(5);

/// daemon の exit を見届ける deadline。
const EXIT_DEADLINE: Duration = Duration::from_secs(5);

/// mode 0700 の TempDir。`Drop` で配下の session を畳んでから dir を消す。
pub struct SessionDir {
    dir: tempfile::TempDir,
}

impl SessionDir {
    /// `prefix` で TempDir を作り mode 0700 にする (= `ensure_socket_dir` 要件)。
    pub fn new(prefix: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create runtime dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod 0700 on runtime dir");
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        reap_sessions_under(self.dir.path());
    }
}

/// `root` 配下 (再帰) の socket のうち、daemon が listen しているものを全て畳む。
///
/// 1. connect できない socket は daemon が居ない残骸なので何もしない
/// 2. `hyoui status` で daemon pid を取り、`hyoui kill --signal=KILL --wait
///    --kill-on-timeout` で正規経路から畳む
/// 3. それでも daemon が残ったら (= kill が connect できない / 応答しない)、daemon の
///    子孫を SIGCONT → SIGKILL してから daemon 自身を SIGKILL する
///
/// teardown 中に panic すると unwind 中の二重 panic で abort するため、失敗は
/// stderr に出すだけにする (= 黙って残さず、test 出力で見えるようにする)。
pub fn reap_sessions_under(root: &Path) {
    for sock in sockets_under(root) {
        if !connectable(&sock) {
            continue;
        }
        let daemon_pid = daemon_pid_of(root, &sock);
        let _ = capture_with_deadline(
            hyoui_cmd(root).args([
                "kill",
                &format!("--socket={}", sock.display()),
                "--signal=KILL",
                "--wait",
                "--kill-on-timeout",
            ]),
            CLI_DEADLINE,
        );
        let Some(pid) = daemon_pid else {
            if connectable(&sock) {
                eprintln!(
                    "test teardown: daemon pid を特定できず session が残っている可能性: {}",
                    sock.display()
                );
            }
            continue;
        };
        if wait_exit(pid, EXIT_DEADLINE) {
            continue;
        }
        eprintln!(
            "test teardown: hyoui kill で daemon (pid {pid}) が終わらないので signal で止める: {}",
            sock.display()
        );
        force_kill_tree(pid);
        if !wait_exit(pid, EXIT_DEADLINE) {
            eprintln!(
                "test teardown: daemon (pid {pid}) が SIGKILL 後も残っている: {}",
                sock.display()
            );
        }
    }
}

/// socket に connect できるか (= listen している daemon が居るか)。
///
/// `<root>/sessions/<uuid>.sock` は `sun_path` に収まらない長さになりうるので、hyoui の
/// connect (= 長いパスは dir の fd 基準で開く、DR-0041 決定 5) を使う。
fn connectable(sock: &Path) -> bool {
    hyoui::sys::socket::connect(sock).is_ok()
}

/// `root` 配下の socket file を再帰で列挙する (= `sessions/` も `--socket` の直置きも含む)。
fn sockets_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_socket() {
                found.push(entry.path());
            }
        }
    }
    found
}

/// `hyoui status --socket=<sock>` の `daemon-pid:` を返す。取れなければ socket を
/// 開いている process (= listen 中の daemon) を `lsof` で引く。
fn daemon_pid_of(root: &Path, sock: &Path) -> Option<i32> {
    let from_status = capture_with_deadline(
        hyoui_cmd(root)
            .args(["status", &format!("--socket={}", sock.display())])
            .stdout(Stdio::piped()),
        CLI_DEADLINE,
    )
    .ok()
    .and_then(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("daemon-pid:"))
            .and_then(|rest| rest.trim().parse::<i32>().ok())
    });
    from_status.or_else(|| {
        let out = capture_with_deadline(
            Command::new("lsof")
                .arg("-t")
                .arg(sock)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
            CLI_DEADLINE,
        )
        .ok()?;
        let me = std::process::id() as i32;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<i32>().ok())
            .find(|&p| p != me)
    })
}

/// runtime dir に隔離した `hyoui` の Command (stdout / stderr は null、必要なら上書き)。
fn hyoui_cmd(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hyoui"));
    c.env("HYOUI_STATE_DIR", root)
        .env("TMPDIR", root)
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// `pid` が消えるまで待つ (= daemon は init の子なので reap は init に任せる)。
///
/// 他人の子の exit を通知で受ける portable な手段が無いため、短い間隔で存在確認する
/// (= harness の他の待ち helper と同じ 20ms 刻み、上限は `timeout`)。
fn wait_exit(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if kill(Pid::from_raw(pid), None).is_err() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// daemon の子孫 (= PTY の子とその孫) を SIGCONT で起こしてから SIGKILL し、最後に
/// daemon 自身を SIGKILL する (= stopped の子は daemon が消えても自分では終わらない)。
fn force_kill_tree(pid: i32) {
    let descendants = find_descendants(pid).unwrap_or_default();
    for p in &descendants {
        let _ = kill(Pid::from_raw(p.pid), Signal::SIGCONT);
    }
    for p in &descendants {
        let _ = kill(Pid::from_raw(p.pid), Signal::SIGKILL);
    }
    let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
}
