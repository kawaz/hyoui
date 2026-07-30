//! DR-0029 §2 e2e: Ctrl+Z 単発は attach client 自身を suspend する。
//!
//! 「外側 shell に戻り、`fg` で同じ接続に戻れる」ことは **job control を持つ親 shell**
//! が居ないと観測できない (= POSIX は orphaned process group の SIGTSTP を破棄するので、
//! test process が直接 spawn した client は停止しない)。そこで hyoui を入れ子にする:
//!
//! ```text
//! outer session (detached) ── /bin/bash -i        ← job control を持つ親 shell
//!                                └─ hyoui attach ── inner session の覗き窓 (= 被験体)
//! inner session (detached) ── /bin/cat            ← 走り続けるべき子
//! ```
//!
//! 操作は `hyoui input --socket=<outer>` で outer の PTY に byte を流す (= bash の
//! 端末に打つのと同じ)。foreground の attach client は raw mode なので、0x1a は line
//! discipline を通らず **client の stdin read** に届く (= ガードの対象になる)。
//!
//! `#[ignore]`: 入れ子 PTY + job control signal を使うため CI 安定性の都合でローカル実行
//! (= `cargo test -- --ignored`)。jobcontrol_*.rs と同じ運用。

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::pty::{HyouiTestRunner, ProcessState, find_children, process_state_of};

fn hyoui_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 入れ子環境 (= outer bash + inner cat) 一式。
struct Nested {
    runner: HyouiTestRunner,
    outer_sock: std::path::PathBuf,
    inner_sock: std::path::PathBuf,
    /// outer session の子 (= `/bin/bash -i`) の pid。
    shell_pid: i32,
    /// inner session の子 (= `/bin/cat`) の pid。
    child_pid: i32,
}

impl Nested {
    /// outer (bash) / inner (cat) の detached session を起こし、pid を解決する。
    fn setup(name: &str) -> Self {
        let runner = HyouiTestRunner::new();
        let outer_sock = runner.socket_path(&format!("{name}-outer"));
        let inner_sock = runner.socket_path(&format!("{name}-inner"));
        // bash は job control を持つ interactive shell として起こす (= `-i`)。rc は読ま
        // せない (= 環境依存の prompt / alias を排除)。
        spawn_detached(
            &runner,
            &[
                "run",
                "--detached",
                &format!("--socket={}", outer_sock.display()),
                "--",
                "/bin/bash",
                "--norc",
                "--noprofile",
                "-i",
            ],
        );
        spawn_detached(
            &runner,
            &[
                "run",
                "--detached",
                &format!("--socket={}", inner_sock.display()),
                "--",
                "/bin/cat",
            ],
        );
        let shell_pid = wait_child_pid(&runner, &outer_sock).expect("outer session の子 pid");
        let child_pid = wait_child_pid(&runner, &inner_sock).expect("inner session の子 pid");
        Self {
            runner,
            outer_sock,
            inner_sock,
            shell_pid,
            child_pid,
        }
    }

    /// outer の bash に 1 行打ち込む (= 末尾に Enter)。
    fn type_line(&self, line: &str) {
        self.input(&[format!("text:{line}"), "key:Enter".to_string()]);
    }

    /// outer の PTY へ input spec 列を送る。
    fn input(&self, specs: &[String]) {
        let mut args = vec![
            "input".to_string(),
            format!("--socket={}", self.outer_sock.display()),
        ];
        args.extend(specs.iter().cloned());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run_hyoui(&self.runner, &argv);
    }

    /// bash から inner session へ attach し、その client の pid を返す。
    fn attach_from_shell(&self) -> i32 {
        self.type_line(&format!(
            "{} attach --socket={}",
            hyoui_bin().display(),
            self.inner_sock.display()
        ));
        wait_for(Duration::from_secs(10), || {
            find_children(self.shell_pid)
                .ok()?
                .into_iter()
                // `ps -o comm=` は macOS では argv を含まないので実行ファイル名で絞る
                // (= bash の子はこの時点で attach client だけ)。
                .find(|p| p.comm.contains("hyoui"))
                .map(|p| p.pid)
        })
        .unwrap_or_else(|| {
            panic!(
                "bash の子として attach client が起動するはず。outer 画面:\n{}",
                self.outer_screen()
            )
        })
    }
}

impl Nested {
    /// outer session (= bash) の画面を text で取る (= 失敗時の診断用)。
    fn outer_screen(&self) -> String {
        let out = run_hyoui(
            &self.runner,
            &[
                "screen",
                "dump",
                &format!("--socket={}", self.outer_sock.display()),
                "--format=text",
            ],
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

/// `hyoui run --detached` を起動する。
///
/// stdout/stderr は **必ず `Stdio::null()`** にする: fork した daemon が pipe の write 端を
/// 継承したまま常駐するため、`output()` のような pipe 経由の待ち方をすると EOF が来ずに
/// 親が永久 block する (= 実測)。
fn spawn_detached(runner: &HyouiTestRunner, args: &[&str]) {
    let status = Command::new(hyoui_bin())
        .args(args)
        .env("XDG_RUNTIME_DIR", runner.runtime_dir())
        .env("TMPDIR", runner.runtime_dir())
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn detached session");
    assert!(status.success(), "run --detached が失敗: {status:?}");
}

impl Drop for Nested {
    /// detached session は誰も待っていないので、test 終了時に正規経路で畳む
    /// (= `runtime_dir` の TempDir を消すだけでは daemon / 子 / client が残る)。
    fn drop(&mut self) {
        for sock in [&self.inner_sock, &self.outer_sock] {
            let _ = Command::new(hyoui_bin())
                .args(["kill", &format!("--socket={}", sock.display())])
                .env("XDG_RUNTIME_DIR", self.runner.runtime_dir())
                .env("TMPDIR", self.runner.runtime_dir())
                .env_remove("HYOUI_LOCK_TOKEN")
                .env_remove("HYOUI_SESSION_ID")
                .env_remove("HYOUI_NAMESPACE")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// `hyoui <args>` を runner の runtime dir で実行する (= test 間の隔離を保つ)。
fn run_hyoui(runner: &HyouiTestRunner, args: &[&str]) -> std::process::Output {
    Command::new(hyoui_bin())
        .args(args)
        .env("XDG_RUNTIME_DIR", runner.runtime_dir())
        .env("TMPDIR", runner.runtime_dir())
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run hyoui")
}

/// `hyoui status` の `child-pid:` 行から子 pid を取り出す (= daemon 起動待ちも兼ねる)。
fn wait_child_pid(runner: &HyouiTestRunner, socket: &std::path::Path) -> Option<i32> {
    wait_for(Duration::from_secs(10), || {
        let out = run_hyoui(
            runner,
            &["status", &format!("--socket={}", socket.display())],
        );
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .find_map(|l| l.strip_prefix("child-pid:"))?
            .split_whitespace()
            .next()?
            .parse::<i32>()
            .ok()
    })
}

/// `f` が `Some` を返すまで 50ms 間隔で待つ。
fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `pid` の `ps` state 主分類が `primary` になるまで待つ。
fn wait_state(pid: i32, primary: char, timeout: Duration) -> Option<ProcessState> {
    wait_for(timeout, || {
        process_state_of(pid).ok().filter(|p| p.is_state(primary))
    })
}

/// `pid` が消えるまで待つ。
fn wait_gone(pid: i32, timeout: Duration) -> bool {
    wait_for(timeout, || process_state_of(pid).err().map(|_| ())).is_some()
}

fn state_of(pid: i32) -> String {
    process_state_of(pid).map_or_else(|_| "(gone)".to_string(), |p| p.stat)
}

/// Ctrl+Z 単発 = client が stopped になり、子は走り続ける。`fg` で復帰し、復帰後の
/// 打鍵が同じ接続を通って子まで届く (= 覗き窓を閉じていない証拠、DR-0029 §2)。
#[ignore = "入れ子 PTY + job control signal を使う、ローカルで --ignored 実行 (DR-0029 §2)"]
#[test]
fn single_ctrl_z_suspends_client_and_fg_restores_it() {
    let n = Nested::setup("ctrlz-suspend");
    let client = n.attach_from_shell();

    n.input(&["key:C-z".to_string()]);
    let stopped = wait_state(client, 'T', Duration::from_secs(10)).unwrap_or_else(|| {
        panic!(
            "Ctrl+Z 単発で client が停止するはず: state={}",
            state_of(client)
        )
    });
    assert!(
        stopped.is_state('T'),
        "client は stopped であるべき: {stopped:?}"
    );
    let child = process_state_of(n.child_pid).expect("子 (/bin/cat) は生存しているはず");
    assert!(
        !child.is_state('T'),
        "client suspend は子に影響してはいけない: {child:?}"
    );

    // `fg` で復帰 (= 停止前と同じ接続に戻る)。
    n.type_line("fg");
    let resumed = wait_state(client, 'S', Duration::from_secs(10))
        .unwrap_or_else(|| panic!("fg で client が復帰するはず: state={}", state_of(client)));
    assert!(resumed.is_state('S'), "fg 後は running: {resumed:?}");

    // 復帰後の打鍵が子まで届く (= cat の echo が inner の画面に出る)。
    n.type_line("HELLO-AFTER-FG");
    let seen = wait_for(Duration::from_secs(10), || {
        let out = run_hyoui(
            &n.runner,
            &[
                "screen",
                "dump",
                &format!("--socket={}", n.inner_sock.display()),
                "--format=text",
            ],
        );
        String::from_utf8_lossy(&out.stdout)
            .contains("HELLO-AFTER-FG")
            .then_some(())
    });
    assert!(
        seen.is_some(),
        "fg 復帰後の入力が子まで届くはず (= attach は畳まれていない)"
    );
}

/// suspend 中の client を kill しても子は無影響 (= client は覗き窓に過ぎない)。
#[ignore = "入れ子 PTY + job control signal を使う、ローカルで --ignored 実行 (DR-0029 §2)"]
#[test]
fn killing_suspended_client_leaves_child_running() {
    let n = Nested::setup("ctrlz-kill");
    let client = n.attach_from_shell();

    n.input(&["key:C-z".to_string()]);
    wait_state(client, 'T', Duration::from_secs(10)).expect("client が停止するはず");

    // 停止中の client を強制終了 (= バックグラウンドに残った窓の始末)。
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(client),
        nix::sys::signal::Signal::SIGKILL,
    );
    assert!(
        wait_gone(client, Duration::from_secs(10)),
        "kill した client は消えるはず"
    );

    let child = process_state_of(n.child_pid).expect("子は kill の影響を受けないはず");
    assert!(
        !child.is_state('T') && !child.is_state('Z'),
        "子は走り続けるべき: {child:?}"
    );
    // daemon 側も生きていて、新しい窓を開けられる。
    let status = run_hyoui(
        &n.runner,
        &["status", &format!("--socket={}", n.inner_sock.display())],
    );
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("child-state: running"),
        "daemon は健在で子は running のはず: {:?}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// 親 shell が消えたら、suspend 中の client は親なしのゴミとして残らない。
///
/// POSIX の orphaned process group 規則 (= stopped メンバーを含む pgrp が orphan 化
/// すると kernel が SIGHUP + SIGCONT を配送する) に乗る。client は SIGHUP に handler を
/// 張らない (= default の terminate) ので、そのまま終了する。
#[ignore = "入れ子 PTY + job control signal を使う、ローカルで --ignored 実行 (DR-0029 §2)"]
#[test]
fn suspended_client_dies_when_parent_shell_disappears() {
    let n = Nested::setup("ctrlz-orphan");
    let client = n.attach_from_shell();

    n.input(&["key:C-z".to_string()]);
    wait_state(client, 'T', Duration::from_secs(10)).expect("client が停止するはず");

    // 親 shell を即死させる (= 端末が閉じた / shell が落ちた状況)。
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(n.shell_pid),
        nix::sys::signal::Signal::SIGKILL,
    );
    assert!(
        wait_gone(client, Duration::from_secs(10)),
        "親 shell 消滅で suspend 中の client も終了するはず (state={})",
        state_of(client)
    );

    let child = process_state_of(n.child_pid).expect("子は生存しているはず");
    assert!(
        !child.is_state('Z'),
        "client の自滅は子に波及してはいけない: {child:?}"
    );
}
