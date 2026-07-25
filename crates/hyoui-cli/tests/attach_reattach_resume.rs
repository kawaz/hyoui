//! DR-0026 §2: stopped child への再 attach auto-resume の CLI e2e。
//!
//! mock daemon が handshake snapshot の `child_stopped` を制御し、attach process が
//! 既存 `SessionChildResumeRequest` を送る条件を protocol frame 単位で検証する。

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

fn run_case(mode: Mode, child_stopped: bool, on_reattach: Option<bool>) -> bool {
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
    if let Some(enabled) = on_reattach {
        let config_dir = xdg.join("hyoui");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join("config.toml"),
            format!("[attach]\nresume_on_reattach = {enabled}\n"),
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
fn config_off_suppresses_rw_reattach_resume() {
    // 明示 opt-out は rw + stopped の組合せでも resume request を抑止する。
    assert!(!run_case(Mode::Rw, true, Some(false)));
}

#[test]
fn rw_running_child_does_not_send_resume() {
    // fresh/running attach は user action があっても不要な resume request を送らない。
    assert!(!run_case(Mode::Rw, false, Some(true)));
}
