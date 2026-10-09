//! 応答を待つ間に daemon が接続を閉じた (= 子が終わって daemon が後始末に入った session に
//! 繋いだ) 時、一発 CLI は「frame decode failed」でなく、session が終わっていることと次の
//! 行動 (`hyoui list` で確かめる) を伝える。
//!
//! 本物の daemon で「接続を受けてから閉じる」瞬間に要求を当てるのは時間の競争になるので、
//! 偽 daemon で要求を読んだ直後に接続を閉じる。偽 daemon は client が送るものを全部読んでから
//! 閉じる (= client の書き込みは閉じる前に終わっているので、閉じた接続への書き込みは起きない)。

mod common;

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use common::mock_daemon::{MockDaemon, recv};
use hyoui::protocol::ControlMessage;

const DEADLINE: Duration = Duration::from_secs(20);

/// session が終わっていることを伝える文言 (`hyoui::Error::ConnectionClosed`)。
const CLOSED: &str =
    "daemon が接続を閉じました。session は既に終わっています (`hyoui list` で確かめてください)";

fn run_hyoui(daemon: &MockDaemon, args: &[&str]) -> Output {
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_hyoui")));
    cmd.args(args)
        .arg(format!("--socket={}", daemon.socket().display()))
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::pty::capture_with_deadline(&mut cmd, DEADLINE).expect("hyoui")
}

fn assert_reports_closed(out: &Output, expected_line: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert_eq!(
        stderr.trim_end(),
        expected_line,
        "session が終わっていることと次の行動だけを伝えるはず"
    );
}

/// `expect` の要求を読んでから接続を閉じる。
fn close_after(stream: &mut UnixStream, expect: fn(&ControlMessage) -> bool, what: &str) {
    let msg = recv(stream);
    assert!(expect(&msg), "expected {what}, got {msg:?}");
}

#[test]
fn input_reports_an_ended_session_when_the_auto_lock_reply_never_comes() {
    let daemon = MockDaemon::spawn(&[], |stream| {
        close_after(
            stream,
            |m| matches!(m, ControlMessage::LockAcquire(_)),
            "lock.acquire",
        );
    });
    let out = run_hyoui(&daemon, &["input", "text:x"]);
    daemon.join();
    assert_reports_closed(
        &out,
        &format!("hyoui: input: auto-lock acquire 失敗: {CLOSED}"),
    );
}

#[test]
fn status_reports_an_ended_session_when_the_reply_never_comes() {
    let daemon = MockDaemon::spawn(&[], |stream| {
        close_after(
            stream,
            |m| matches!(m, ControlMessage::StatusQuery(_)),
            "status.query",
        );
    });
    let out = run_hyoui(&daemon, &["status"]);
    daemon.join();
    assert_reports_closed(&out, &format!("hyoui: status: {CLOSED}"));
}

#[test]
fn set_reports_an_ended_session_when_the_ack_never_comes() {
    let daemon = MockDaemon::spawn(&["set-v1"], |stream| {
        close_after(
            stream,
            |m| matches!(m, ControlMessage::SetRequest(_)),
            "set.request",
        );
    });
    let out = run_hyoui(&daemon, &["set", "on-child-suspend=notify"]);
    daemon.join();
    assert_reports_closed(&out, &format!("hyoui: set: {CLOSED}"));
}

#[test]
fn connect_reports_an_ended_session_when_the_handshake_reply_never_comes() {
    // handshake の途中で閉じた時は、「daemon process が応答していない」等の connect 失敗の
    // hint を足さない (= 事実と食い違う)。
    let daemon = MockDaemon::spawn_raw(|stream| {
        close_after(
            stream,
            |m| matches!(m, ControlMessage::HandshakeRequest(_)),
            "handshake.request",
        );
    });
    let out = run_hyoui(&daemon, &["status"]);
    daemon.join();
    assert_reports_closed(&out, &format!("hyoui: status: {CLOSED}"));
}
