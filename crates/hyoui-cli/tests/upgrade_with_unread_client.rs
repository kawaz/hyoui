//! DR-0028 §4 / DR-0037 段階 4: 読まない client が居ても、upgrade の要求元に
//! `upgrade.ack` が届く。
//!
//! daemon は upgrade の self-exec の前に、全 client の送信 queue が空になるのを上限 1 秒で
//! 見届ける。読まない client の queue は空にならないので上限で諦めて exec し、要求元
//! (`hyoui upgrade`) は先に ack を受け取っている。`hyoui upgrade` は ack を受け取れないと
//! 「recv error before ack」で exit 1 になる。

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use hyoui::protocol::{ControlMessage, Frame, HandshakeRequest, MVP_CAPS, Mode};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// `HYOUI_STATE_DIR=<root>` の `hyoui` (場所を決める他の env は外す)。
fn in_root(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(hyoui_bin());
    c.args(args)
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .env("HYOUI_STATE_DIR", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn output(mut c: Command) -> Output {
    // daemon は起動時の stderr を ready まで使うので、`--detached` を pipe で待たず file に
    // 逃がしてから読む。
    let dir = tempfile::tempdir().expect("stderr dir");
    let path = dir.path().join("stderr");
    c.stderr(std::fs::File::create(&path).expect("stderr file"));
    let mut out = c.output().expect("spawn hyoui");
    out.stderr = std::fs::read(&path).unwrap_or_default();
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn daemon_pid(root: &Path, sid: &str) -> i32 {
    let out = output(in_root(root, &["status", sid]));
    assert!(out.status.success(), "status: {}", text(&out.stderr));
    text(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("daemon-pid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|p| p.parse::<i32>().ok())
        .unwrap_or_else(|| panic!("daemon-pid not in status: {}", text(&out.stdout)))
}

/// handshake だけ送り、以後は一切読まない client (= SIGSTOP された `hyoui attach` と同じく、
/// daemon から見ると受信 buffer が埋まったまま空かない)。
fn attach_without_reading(sock: &Path) -> UnixStream {
    // 長い socket path でも届く口で繋ぐ (DR-0041 決定 5)。
    let mut s = hyoui::sys::socket::connect_stream(sock).expect("connect");
    let req = ControlMessage::HandshakeRequest(HandshakeRequest {
        caps: MVP_CAPS.iter().map(|c| (*c).to_string()).collect(),
        mode: Mode::Rw,
        exclusive: false,
        detach_others: false,
        token: None,
    });
    Frame::cbor_control(req.encode_to_vec().expect("encode handshake"))
        .encode_to(&mut s)
        .expect("send handshake");
    s.flush().expect("flush");
    s
}

#[test]
fn upgrade_ack_reaches_the_requester_while_another_client_does_not_read() {
    let root = SessionDir::new("hyoui-upgrade-unread-");
    let sid = hyoui::cli::new_session_id();
    let sid_arg = format!("--session-id={sid}");
    // 1 行読んだら約 3 MB 出して止まる子。3 MB は socket buffer より十分大きく、送信 queue の
    // 既定の上限 (8 MiB) より小さいので、読まない client は切られずに queue を抱えたまま残る。
    let out = output(in_root(
        root.path(),
        &[
            "run",
            "--detached",
            "--pty-stdin",
            &sid_arg,
            "--",
            "/bin/sh",
            "-c",
            "read x; head -c 3000000 /dev/zero | tr '\\0' y; echo; echo UNREAD_BURST_END; sleep 60",
        ],
    ));
    assert!(out.status.success(), "run: {}", text(&out.stderr));
    let before = daemon_pid(root.path(), &sid);

    let sock = root.path().join("sessions").join(format!("{sid}.sock"));
    let mut stuck = attach_without_reading(&sock);

    let out = output(in_root(root.path(), &["input", &sid, "key:Enter"]));
    assert!(out.status.success(), "input: {}", text(&out.stderr));
    let out = output(in_root(
        root.path(),
        &["wait", &sid, "UNREAD_BURST_END", "--timeout=30s"],
    ));
    assert!(out.status.success(), "wait: {}", text(&out.stderr));

    let start = Instant::now();
    let out = output(in_root(root.path(), &["upgrade", &sid]));
    let elapsed = start.elapsed();
    assert!(
        out.status.success(),
        "the requester must receive upgrade.ack: {}",
        text(&out.stderr)
    );
    // 待つのは送信 queue の上限 1 秒 + exec と再開の分だけ (読まない client で止まらない)。
    assert!(
        elapsed < Duration::from_secs(5),
        "upgrade must not wait on the unread client beyond its budget, took {elapsed:?}"
    );

    assert_eq!(
        daemon_pid(root.path(), &sid),
        before,
        "the upgraded daemon answers status and keeps the pid (self-exec)"
    );

    // 読まない client の socket は exec で閉じている (CLOEXEC): 届いていた分を読み切ると EOF。
    // macOS は相手が既に閉じた socket への SO_RCVTIMEO 設定を EINVAL で断る (= その時は
    // read_to_end がすぐ EOF に着くので上限は要らない)。
    let _ = stuck.set_read_timeout(Some(Duration::from_secs(10)));
    let mut all = Vec::new();
    stuck
        .read_to_end(&mut all)
        .expect("the unread client's socket is closed by the upgrade");
    assert!(
        all.len() < 3_000_000,
        "the unread client did not receive the whole burst ({} bytes)",
        all.len()
    );

    let out = output(in_root(
        root.path(),
        &["kill", &sid, "--signal=KILL", "--wait"],
    ));
    assert!(out.status.success(), "kill: {}", text(&out.stderr));
}
