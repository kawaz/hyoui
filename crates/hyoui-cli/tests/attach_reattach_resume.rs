//! DR-0029 §5 / DR-0032 §1: stopped child への再 attach auto-resume の CLI e2e。
//!
//! mock daemon が handshake snapshot の `child_stopped` を制御し、attach process が
//! 既存 `SessionChildResumeRequest` を送る条件を protocol frame 単位で検証する。
//!
//! DR-0032 で opt-out の書き方が bool から enum に変わったので、config は
//! `[session] on_child_suspend` の値で与える (= `show_child_action_menu` が旧
//! `resume_stopped_child = false` に相当)。

use std::io;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::Duration;

use hyoui::protocol::messages::HandshakeResponse;
use hyoui::protocol::{ControlMessage, Frame, Mode, TYPE_CBOR_CONTROL};
use tempfile::TempDir;

fn hyoui_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

fn run_case(mode: Mode, child_stopped: bool, on_child_suspend: Option<&str>) -> bool {
    let temp = TempDir::new().expect("tempdir");
    let socket = temp.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind mock daemon");

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept attach client");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");

        let request = Frame::decode_from(&mut stream).expect("decode handshake request");
        assert_eq!(request.ty, TYPE_CBOR_CONTROL);
        let requested_mode = match ControlMessage::decode_from(request.body.as_slice())
            .expect("decode handshake request control")
        {
            ControlMessage::HandshakeRequest(request) => request.mode,
            other => panic!("expected handshake.request, got {other:?}"),
        };
        assert_eq!(requested_mode, mode);

        let response = ControlMessage::HandshakeResponse(HandshakeResponse {
            caps: vec![],
            session_id: "reattach-resume-test".into(),
            client_id: 1,
            leader: mode == Mode::Rw,
            mode,
            child_stopped,
        });
        let body = response.encode_to_vec().expect("encode handshake response");
        Frame::cbor_control(body)
            .encode_to(&mut stream)
            .expect("send handshake response");

        match Frame::decode_from(&mut stream) {
            Ok(frame) => {
                assert_eq!(frame.ty, TYPE_CBOR_CONTROL);
                let resumed = matches!(
                    ControlMessage::decode_from(frame.body.as_slice())
                        .expect("decode post-handshake control"),
                    ControlMessage::SessionChildResumeRequest(_)
                );
                if resumed {
                    // client が stdin EOF で正常 detach するまで socket を保持する。ここで
                    // mock daemon が先に close すると attach は ConnectionLost を正しく返す。
                    let _ = Frame::decode_from(&mut stream);
                }
                resumed
            }
            Err(hyoui::protocol::FrameError::Protocol(
                hyoui::protocol::ProtocolError::UnexpectedEof(_),
            )) => false,
            Err(hyoui::protocol::FrameError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                false
            }
            Err(error) => panic!("unexpected post-handshake read error: {error:?}"),
        }
    });

    let xdg = temp.path().join("xdg");
    if let Some(setting) = on_child_suspend {
        let config_dir = xdg.join("hyoui");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join("config.toml"),
            format!("[session]\non_child_suspend = \"{setting}\"\n"),
        )
        .expect("write config");
    }

    let mode_arg = match mode {
        Mode::Rw => "rw",
        Mode::Ro => "ro",
        Mode::RwNoLeader => "rw-no-leader",
        _ => panic!("unsupported mode in test"),
    };
    let output = Command::new(hyoui_bin())
        .args([
            "attach",
            &format!("--socket={}", socket.display()),
            &format!("--mode={mode_arg}"),
            "--stdin-eof=detach",
            "--quiet",
        ])
        .env("XDG_CONFIG_HOME", &xdg)
        .env_remove("HYOUI_LOCK_TOKEN")
        .env_remove("HYOUI_SESSION_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run attach client");
    assert!(
        output.status.success(),
        "attach should exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    server.join().expect("mock daemon thread")
}

#[test]
fn rw_stopped_child_resumes_with_default_config() {
    // rw + stopped + config file 不在は default on。再 attach の操作意思で resume する。
    assert!(run_case(Mode::Rw, true, None));
}

#[test]
fn ro_stopped_child_never_resumes() {
    // ro は観察専用なので、default on でも stopped child を起こさない。
    assert!(!run_case(Mode::Ro, true, None));
}

#[test]
fn rw_no_leader_stopped_child_does_not_resume() {
    // rw-no-leader は入力権を持っても復帰主体ではないため、rw と区別して起こさない。
    assert!(!run_case(Mode::RwNoLeader, true, None));
}

#[test]
fn show_child_action_menu_suppresses_rw_reattach_resume() {
    // DR-0032 §1: menu を出す設定は rw + stopped でも resume request を送らない
    // (= 起こす代わりに menu を描く。旧 `resume_stopped_child = false` の置換)。
    assert!(!run_case(Mode::Rw, true, Some("show_child_action_menu")));
}

#[test]
fn auto_resume_always_still_resumes_on_handshake_snapshot() {
    // DR-0032 §1 の写像表: daemon が先に起こすので発動機会はほぼ無いが、handshake
    // snapshot が stopped だった race では client 側も安全側で起こす。
    assert!(run_case(Mode::Rw, true, Some("auto_resume_always")));
}

#[test]
fn rw_running_child_does_not_send_resume() {
    // fresh/running attach は user action があっても不要な resume request を送らない。
    assert!(!run_case(Mode::Rw, false, Some("auto_resume_on_attached")));
}
