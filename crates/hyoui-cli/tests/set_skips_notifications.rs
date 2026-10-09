//! `hyoui set` は rw で接続するので、set.ack を待つ間に要求と無関係な通知 (子の停止の
//! session.child.stopped.notify 等) が届く。それらを読み飛ばして ack を待ち、成功する。
//!
//! 本物の daemon で子の停止を ack の前に挟むのは時間の競争になるので、偽 daemon で
//! 「set.request を受けてから通知を送り、その後に set.ack を返す」順序を作る。

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::mock_daemon::{MockDaemon, drain_until_closed, recv, send};
use hyoui::protocol::ControlMessage;
use hyoui::protocol::messages::{
    LeaderNotify, SessionChildStoppedNotify, SessionExitNotify, SetAck, UpgradeAck,
};

const DEADLINE: Duration = Duration::from_secs(20);

fn run_set(daemon: &MockDaemon) -> std::process::Output {
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_hyoui")));
    cmd.args([
        "set",
        &format!("--socket={}", daemon.socket().display()),
        "on-child-suspend=notify",
    ])
    .env_remove("HYOUI_SESSION_ID")
    .env_remove("HYOUI_LOCK_TOKEN")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    common::pty::capture_with_deadline(&mut cmd, DEADLINE).expect("hyoui set")
}

fn expect_set_request(stream: &mut std::os::unix::net::UnixStream) {
    match recv(stream) {
        ControlMessage::SetRequest(req) => {
            assert_eq!(req.key, "on-child-suspend");
            assert_eq!(req.value, "notify");
        }
        other => panic!("expected set.request, got {other:?}"),
    }
}

#[test]
fn set_waits_for_the_ack_past_unsolicited_notifications() {
    let daemon = MockDaemon::spawn(&["set-v1"], |stream| {
        expect_set_request(stream);
        send(
            stream,
            &ControlMessage::SessionChildStoppedNotify(SessionChildStoppedNotify {
                pid: 4242,
                signal: Some("SIGTSTP".into()),
            }),
        );
        send(
            stream,
            &ControlMessage::LeaderNotify(LeaderNotify { client_id: None }),
        );
        send(stream, &ControlMessage::UpgradeAck(UpgradeAck {}));
        send(
            stream,
            &ControlMessage::SetAck(SetAck {
                key: "on-child-suspend".into(),
                value: "notify".into(),
            }),
        );
        drain_until_closed(stream);
    });
    let out = run_set(&daemon);
    daemon.join();
    assert!(
        out.status.success(),
        "set は通知を読み飛ばして ack で成功するはず: rc={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "set ok: on-child-suspend=notify\n"
    );
}

#[test]
fn set_reports_the_child_exit_when_it_arrives_before_the_ack() {
    let daemon = MockDaemon::spawn(&["set-v1"], |stream| {
        expect_set_request(stream);
        send(
            stream,
            &ControlMessage::SessionExitNotify(SessionExitNotify {
                exit_status: 3,
                signal: None,
            }),
        );
    });
    let out = run_set(&daemon);
    daemon.join();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("session の子が終了しました (exit status 3)"),
        "子の終了を伝えるはず: {stderr}"
    );
}
