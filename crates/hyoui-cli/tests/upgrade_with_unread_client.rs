//! DR-0028 §4 / DR-0037 段階 4: upgrade の self-exec の前に、client の送信 queue に積んだ frame
//! を送り切る。
//!
//! daemon は upgrade の要求 (`upgrade.request` の受理、または隠し trigger の SIGUSR1) の後、
//! 全 client の送信 queue が空になるのを上限 1 秒で見届けてから exec する。読む client には
//! 溜まっていた出力と `upgrade.ack` が届き、読まない client は上限で諦められる (upgrade は
//! 止まらない)。
//!
//! どの test も、子に約 3 MB を出させてから upgrade する。3 MB は socket buffer より十分大きく、
//! 送信 queue の既定の上限 (8 MiB) より小さいので、出力を読まずにいた client の queue には
//! 出力の大半が切られずに残る。queue に残っていることは、handshake の確立 (応答と status の
//! client 一覧) と、出力を daemon が読み終えた後に socket に届いている量が出力より小さいこと
//! で確かめてから upgrade する。

mod common;

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use hyoui::protocol::messages::UpgradeRequest;
use hyoui::protocol::{
    ControlMessage, Frame, HandshakeRequest, MVP_CAPS, Mode, TYPE_CBOR_CONTROL, TYPE_RAW_DATA,
};

/// 子が出す量 (bytes) と、出し終えた印。
const BURST: usize = 3_000_000;
const BURST_END: &[u8] = b"UNREAD_BURST_END";

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

fn status(root: &Path, sid: &str) -> String {
    let out = output(in_root(root, &["status", sid]));
    assert!(out.status.success(), "status: {}", text(&out.stderr));
    text(&out.stdout)
}

fn daemon_pid(root: &Path, sid: &str) -> i32 {
    let stdout = status(root, sid);
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("daemon-pid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|p| p.parse::<i32>().ok())
        .unwrap_or_else(|| panic!("daemon-pid not in status: {stdout}"))
}

/// 1 行読んだら約 3 MB 出して止まる子の session を起こす。
fn start_burst_session(root: &Path) -> String {
    let sid = hyoui::cli::new_session_id();
    let sid_arg = format!("--session-id={sid}");
    let script = format!(
        "read x; head -c {BURST} /dev/zero | tr '\\0' y; echo; echo {}; sleep 60",
        String::from_utf8_lossy(BURST_END)
    );
    let out = output(in_root(
        root,
        &[
            "run",
            "--detached",
            "--pty-stdin",
            &sid_arg,
            "--",
            "/bin/sh",
            "-c",
            &script,
        ],
    ));
    assert!(out.status.success(), "run: {}", text(&out.stderr));
    sid
}

/// rw で attach して handshake の応答だけ読み、client id を返す。以後は呼び出し側が読むまで
/// 読まない (= SIGSTOP された `hyoui attach` と同じく、daemon からは受信 buffer が埋まったまま
/// 空かない client に見える)。
fn attach_and_pause(sock: &Path) -> (UnixStream, u64) {
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
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
    let f = Frame::decode_from(&mut s).expect("handshake response frame");
    let id = match ControlMessage::decode_from(f.body.as_slice()).expect("decode response") {
        ControlMessage::HandshakeResponse(r) => r.client_id,
        other => panic!("expected handshake.response, got {other:?}"),
    };
    (s, id)
}

/// socket に届いていて未読の bytes 数 (読まずに覗く)。
fn unread_in_socket(s: &UnixStream) -> usize {
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    nix::sys::socket::recv(
        s.as_raw_fd(),
        &mut buf,
        nix::sys::socket::MsgFlags::MSG_PEEK | nix::sys::socket::MsgFlags::MSG_DONTWAIT,
    )
    .unwrap_or(0)
}

/// 子に出力させ、`clients` が attach 済みであることと、出力の大半が socket に届かずに daemon の
/// 送信 queue に残っていることを確かめる。
fn burst_into_paused_clients(root: &Path, sid: &str, clients: &[(&UnixStream, u64)]) {
    let listed = status(root, sid);
    for (_, id) in clients {
        assert!(
            listed.contains(&format!("id={id} ")),
            "client {id} must be attached before the burst: {listed}"
        );
    }
    let out = output(in_root(root, &["input", sid, "key:Enter"]));
    assert!(out.status.success(), "input: {}", text(&out.stderr));
    let out = output(in_root(
        root,
        &["wait", sid, &text(BURST_END), "--timeout=30s"],
    ));
    assert!(out.status.success(), "wait: {}", text(&out.stderr));
    for (s, id) in clients {
        let in_socket = unread_in_socket(s);
        assert!(
            in_socket > 0 && in_socket < BURST / 2,
            "client {id}: the burst reached the socket only partly ({in_socket} bytes), \
             the rest must be in the daemon's send queue"
        );
    }
}

/// EOF (= exec で socket が閉じる) まで読み、raw_data の body の連結と control message を返す。
fn read_until_eof(s: &mut UnixStream) -> (Vec<u8>, Vec<ControlMessage>) {
    let mut all = Vec::new();
    let _ = s.read_to_end(&mut all);
    let mut cur = std::io::Cursor::new(&all[..]);
    let mut raw = Vec::new();
    let mut controls = Vec::new();
    while let Ok(f) = Frame::decode_from(&mut cur) {
        match f.ty {
            TYPE_RAW_DATA => raw.extend(f.body),
            TYPE_CBOR_CONTROL => {
                if let Ok(m) = ControlMessage::decode_from(f.body.as_slice()) {
                    controls.push(m);
                }
            }
            _ => {}
        }
    }
    (raw, controls)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// 出力を溜めた client が upgrade を要求すると、溜まっていた出力と upgrade.ack を全部受け取って
/// から socket が閉じる。読まない別の client が居ても、upgrade はその client を上限 (1 秒) で
/// 諦めて進み、upgrade 後の daemon は同じ pid で status に応答する。
#[test]
fn upgrade_ack_reaches_a_requester_with_a_backlog_while_another_client_does_not_read() {
    let root = SessionDir::new("hyoui-upgrade-unread-");
    let sid = start_burst_session(root.path());
    let before = daemon_pid(root.path(), &sid);
    let sock = root.path().join("sessions").join(format!("{sid}.sock"));

    let (mut stuck, stuck_id) = attach_and_pause(&sock);
    let (mut requester, requester_id) = attach_and_pause(&sock);
    burst_into_paused_clients(
        root.path(),
        &sid,
        &[(&stuck, stuck_id), (&requester, requester_id)],
    );

    let start = Instant::now();
    Frame::cbor_control(
        ControlMessage::UpgradeRequest(UpgradeRequest { binary_path: None })
            .encode_to_vec()
            .expect("encode upgrade.request"),
    )
    .encode_to(&mut requester)
    .expect("send upgrade.request");
    requester.flush().expect("flush");
    let (raw, controls) = read_until_eof(&mut requester);
    let elapsed = start.elapsed();
    assert!(
        contains(&raw, BURST_END),
        "the requester must receive its whole backlog before the exec ({} bytes)",
        raw.len()
    );
    assert!(
        controls
            .iter()
            .any(|m| matches!(m, ControlMessage::UpgradeAck(_))),
        "the requester must receive upgrade.ack before the exec"
    );
    // 待つのは送信 queue の上限 1 秒 + exec の分だけ (読まない client で止まらない)。
    assert!(
        elapsed < Duration::from_secs(5),
        "upgrade must not wait on the unread client beyond its budget, took {elapsed:?}"
    );

    assert_eq!(
        daemon_pid(root.path(), &sid),
        before,
        "the upgraded daemon answers status and keeps the pid (self-exec)"
    );
    let (stuck_raw, _) = read_until_eof(&mut stuck);
    assert!(
        stuck_raw.len() < BURST,
        "the unread client was given up at the budget ({} bytes)",
        stuck_raw.len()
    );

    let out = output(in_root(
        root.path(),
        &["kill", &sid, "--signal=KILL", "--wait"],
    ));
    assert!(out.status.success(), "kill: {}", text(&out.stderr));
}

/// 隠し trigger (SIGUSR1) で始めた upgrade も、client の送信 queue に溜まっていた出力を送り
/// 切ってから exec する (DR-0028 §4)。
#[test]
fn sigusr1_upgrade_delivers_the_frames_left_in_the_send_queue() {
    let root = SessionDir::new("hyoui-upgrade-usr1-");
    let sid = start_burst_session(root.path());
    let before = daemon_pid(root.path(), &sid);
    let sock = root.path().join("sessions").join(format!("{sid}.sock"));

    let (mut reader, reader_id) = attach_and_pause(&sock);
    burst_into_paused_clients(root.path(), &sid, &[(&reader, reader_id)]);

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(before),
        nix::sys::signal::Signal::SIGUSR1,
    )
    .expect("send SIGUSR1 to the daemon");
    let (raw, _) = read_until_eof(&mut reader);
    assert!(
        contains(&raw, BURST_END),
        "frames left in the send queue must be delivered before the exec ({} bytes)",
        raw.len()
    );

    assert_eq!(
        daemon_pid(root.path(), &sid),
        before,
        "the upgraded daemon answers status and keeps the pid (self-exec)"
    );

    let out = output(in_root(
        root.path(),
        &["kill", &sid, "--signal=KILL", "--wait"],
    ));
    assert!(out.status.success(), "kill: {}", text(&out.stderr));
}
