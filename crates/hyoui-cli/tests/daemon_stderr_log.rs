//! DR-0037 段 2 e2e: 起動後の daemon は呼び出し元の stderr を持たず、fd 2 と logger の書き先は
//! `<状態の root>/sessions/logs/<id>.log`。ready 通知の前の失敗は呼び出し元の stderr に出る。
//!
//! 待ちはすべて pipe の EOF・process の終了・`hyoui kill --wait` (daemon の終了を見届けて
//! 戻る) で行い、時間で判定しない。

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::session_dir::SessionDir;

/// `hyoui run` の戻りと pipe の EOF を待つ上限。
const DEADLINE: Duration = Duration::from_secs(10);

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 状態の root を `dir` に隔離した `hyoui` の Command。
fn hyoui(dir: &Path) -> Command {
    let mut c = Command::new(hyoui_bin());
    c.env("HYOUI_STATE_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null());
    c
}

/// `$(cmd 2>&1)` と同じ形で実行する: stdout と stderr を同じ pipe にし、pipe の EOF
/// (= 書き手が全員閉じた) まで読む。期限内に EOF が来なければ `None`。
fn capture_merged(mut cmd: Command) -> Option<(std::process::ExitStatus, String)> {
    // `std::io::pipe` の fd は CLOEXEC (= hyoui には dup2 した fd 1 / 2 だけが渡る。元の fd が
    // 漏れると daemon がそれを持ち続け、付け替えとは無関係に EOF が来ない)。
    let (rd, wr) = std::io::pipe().expect("pipe");
    let wr2 = wr.try_clone().expect("dup pipe");
    cmd.stdout(Stdio::from(wr)).stderr(Stdio::from(wr2));
    let mut child = cmd.spawn().expect("spawn hyoui");
    // spawn の後、test 側の書き端は Command と一緒に閉じる (= 残ると EOF が来ない)。
    drop(cmd);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let mut rd = rd;
        let _ = rd.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(DEADLINE);
    let status = child.wait().expect("wait hyoui");
    out.ok().map(|o| (status, o))
}

/// `hyoui kill --wait <id>` (= 子の exit と daemon の終了を見届けて戻る)。
fn kill_and_wait(dir: &Path, session: &str) {
    let out = common::pty::capture_with_deadline(
        hyoui(dir)
            .args(["kill", "--wait", session])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        DEADLINE,
    )
    .expect("hyoui kill");
    assert!(out.status.success(), "hyoui kill --wait failed: {out:?}");
}

fn log_path(dir: &Path, session: &str) -> PathBuf {
    dir.join("sessions")
        .join("logs")
        .join(format!("{session}.log"))
}

/// `$(hyoui run --detached -- cmd 2>&1)` は ready 通知で親が exit した時点で返る (= daemon は
/// 呼び出し元の stderr を持っていない)。出力は session id の 1 行だけ。
#[test]
fn merged_capture_returns_when_the_parent_exits() {
    let dir = SessionDir::new("hyoui-log-capture-");
    let mut cmd = hyoui(dir.path());
    cmd.args(["run", "--detached", "--pty-stdin", "--", "cat"]);
    let (status, out) = capture_merged(cmd).expect("$(hyoui run --detached 2>&1) did not return");
    assert!(status.success(), "{status:?} {out:?}");
    let id = out.trim_end_matches('\n');
    assert!(
        hyoui::cli::validate_session_id(id).is_ok() && !id.contains('\n'),
        "output must be the session id only: {out:?}"
    );
    kill_and_wait(dir.path(), id);
}

/// ready 通知の前の失敗 (同じ id の socket が既にある) は呼び出し元の stderr に出て、run は
/// 非 0 で終わる。
#[test]
fn a_startup_failure_is_reported_on_the_caller_stderr() {
    let dir = SessionDir::new("hyoui-log-startfail-");
    let mut first = hyoui(dir.path());
    first.args(["run", "--detached", "--pty-stdin", "--", "cat"]);
    let (_, out) = capture_merged(first).expect("first run");
    let id = out.trim_end_matches('\n').to_string();

    let mut second = hyoui(dir.path());
    second.args([
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--session-id={id}"),
        "--",
        "cat",
    ]);
    let (status, err) = capture_merged(second).expect("second run did not return");
    assert!(!status.success(), "duplicate id must fail: {err:?}");
    assert!(err.contains("socket が既にある"), "caller stderr: {err:?}");
    kill_and_wait(dir.path(), &id);
}

/// ready 通知の後に daemon が出すログ (`--debug-dump-server` の open 失敗) は呼び出し元には
/// 出ず、session のログに時刻付きで書かれ、session が終わっても残る。
#[test]
fn daemon_log_after_ready_goes_to_the_session_log() {
    let dir = SessionDir::new("hyoui-log-file-");
    let dump = dir.path().join("no-such-dir").join("x.dump");
    let mut cmd = hyoui(dir.path());
    cmd.args([
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--debug-dump-server={}", dump.display()),
        "--",
        "cat",
    ]);
    let (status, out) = capture_merged(cmd).expect("run did not return");
    assert!(status.success(), "{out:?}");
    assert!(
        !out.contains("debug-dump"),
        "caller saw the daemon log: {out:?}"
    );
    let id = out.trim_end_matches('\n').to_string();

    kill_and_wait(dir.path(), &id);
    let log = std::fs::read_to_string(log_path(dir.path(), &id)).expect("read session log");
    let line = log
        .lines()
        .find(|l| l.contains("--debug-dump open"))
        .unwrap_or_else(|| panic!("log has no debug-dump line: {log:?}"));
    // `YYYY-MM-DDTHH:MM:SSZ ` で始まる。
    let (stamp, _) = line.split_once(' ').expect("stamp");
    assert!(
        stamp.len() == 20 && stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T',
        "{line:?}"
    );
}

/// 何も書かれなかった session のログは、session の間はあり、終わると消える。
#[test]
fn an_empty_session_log_is_removed_when_the_session_ends() {
    let dir = SessionDir::new("hyoui-log-empty-");
    let mut cmd = hyoui(dir.path());
    cmd.args(["run", "--detached", "--pty-stdin", "--", "cat"]);
    let (_, out) = capture_merged(cmd).expect("run did not return");
    let id = out.trim_end_matches('\n').to_string();
    let path = log_path(dir.path(), &id);
    assert_eq!(
        std::fs::metadata(&path).map(|m| m.len()).ok(),
        Some(0),
        "live session must have an empty log at {}",
        path.display()
    );
    kill_and_wait(dir.path(), &id);
    assert!(
        !path.exists(),
        "empty log must be removed: {}",
        path.display()
    );
}
