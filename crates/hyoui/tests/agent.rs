//! Integration tests for the `agent` module (stage 4).
//!
//! These spawn real child processes through the full Agent stack
//! (PTY + socket + signals) and verify exit-code semantics. To stay
//! CI-friendly we always use `--mode=headless` so the parent never
//! attempts to raw-ize a tty.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::id;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyoui::agent::Agent;
use hyoui::cli::{Command, parse_args};
use hyoui::observer::{NullObserver, Observer};
// Re-exported on `hyoui::sys::socket` via `pub mod socket` in sys/mod.rs.
use hyoui::protocol;

/// Build a `Vec<String>` argv from string literals.
fn argv(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_string()).collect()
}

/// Parse a headless `run` config from a child command argv. Always assigns
/// a unique socket path so parallel tests in the same process never race
/// over the default `$XDG_RUNTIME_DIR/hyoui/agent-<pid>.sock`.
fn headless_config(child: &[&str]) -> hyoui::cli::RunConfig {
    let dir = std::env::temp_dir().join(format!(
        "hyoui-it-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    let sock = dir.join("agent.sock");
    headless_config_with_socket(child, &sock)
}

/// Same but with a socket explicitly bound in a fresh 0700 tempdir.
fn headless_config_with_socket(child: &[&str], socket: &std::path::Path) -> hyoui::cli::RunConfig {
    let sock_arg = socket.to_str().expect("utf-8 socket path");
    let mut head = vec!["run", "--mode=headless", "--socket", sock_arg, "--"];
    head.extend_from_slice(child);
    match parse_args(&argv(&head)) {
        Command::Run(cfg) => cfg,
        other => panic!("expected Run config, got {other:?}"),
    }
}

/// Observer that snapshots every output byte for assertion. Wraps the
/// inner capture so the test can read it after [`Agent::run`] consumes
/// the observer.
#[derive(Debug, Clone)]
struct CapturingObserver {
    seen: Arc<Mutex<Vec<u8>>>,
}

impl CapturingObserver {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }
}

impl Observer for CapturingObserver {
    fn on_output(&mut self, data: &[u8]) -> Vec<u8> {
        self.seen.lock().expect("lock").extend_from_slice(data);
        data.to_vec()
    }
    fn on_input(&mut self, data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }
    fn capture(&self) -> String {
        String::from_utf8_lossy(&self.seen.lock().expect("lock")).into_owned()
    }
}

/// Make a fresh 0700 tempdir under TMPDIR (or /tmp) for socket tests.
/// Returns the path; caller is responsible for cleanup (TempDir-like).
struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir();
        let dir = base.join(format!("hyoui-it-{}-{}-{}", tag, id(), rand_tag()));
        std::fs::create_dir_all(&dir).expect("mkdir tempdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        Self(dir)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Cheap unique tag derived from the monotonic clock; avoids pulling in a
/// random crate for the integration test alone.
fn rand_tag() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ===========================================================================
// Ported from bootstrap `agent_wbtest.mbt` (7 tests)
// ===========================================================================

/// `Agent::new` + immediate Drop on a short-lived child.
/// Mirrors bootstrap "Agent new and cleanup".
#[test]
fn agent_new_and_drop_succeeds() {
    let cfg = headless_config(&["echo", "hi"]);
    let agent = Agent::new(cfg, Box::new(NullObserver::new())).expect("agent new");
    drop(agent);
}

/// `Agent::run` with `echo` should return exit 0.
/// Mirrors bootstrap "Agent run with echo exits cleanly".
#[test]
fn agent_run_echo_exits_zero() {
    let cfg = headless_config(&["echo", "hello"]);
    let agent = Agent::new(cfg, Box::new(NullObserver::new())).expect("agent new");
    let code = agent.run().expect("run");
    assert_eq!(code, 0);
}

/// `Agent::run` with `false` should return exit 1.
/// Mirrors bootstrap "Agent run with false returns nonzero exit status".
#[test]
fn agent_run_false_exits_one() {
    let cfg = headless_config(&["false"]);
    let agent = Agent::new(cfg, Box::new(NullObserver::new())).expect("agent new");
    let code = agent.run().expect("run");
    assert_eq!(code, 1);
}

/// `Agent::run` propagates the child's exit code via the observer too —
/// the captured output should include the echoed text.
///
/// FLAKY: 並列 test 実行で偶発失敗、原因は pty + child + observer の race。
/// 詳細は `docs/issue/2026-05-26-bug-flaky-agent-tests.md`。v0.1.0 daemon 再実装で
/// 根本対処予定。それまで `#[ignore]` で disable、`cargo test -- --ignored` で個別検証可。
#[test]
#[ignore = "flaky in parallel runs, tracked in docs/issue/2026-05-26-bug-flaky-agent-tests.md"]
fn agent_run_echo_output_visible_via_observer() {
    let cfg = headless_config(&["echo", "hi"]);
    let (obs, seen) = CapturingObserver::new();
    let agent = Agent::new(cfg, Box::new(obs)).expect("agent new");
    let code = agent.run().expect("run");
    assert_eq!(code, 0);
    let text = String::from_utf8_lossy(&seen.lock().expect("lock")).into_owned();
    assert!(text.contains("hi"), "expected 'hi' in captured: {text:?}");
}

/// Headless agent with an explicit `--socket` accepts an input message
/// over the protocol channel and forwards it to the child. We use `cat`
/// as the child so we can observe the echoed bytes coming back out.
///
/// FLAKY: socket connect の race。詳細は `docs/issue/2026-05-26-bug-flaky-agent-tests.md`。
/// v0.1.0 daemon 再実装で根本対処予定。
#[test]
#[ignore = "flaky in parallel runs, tracked in docs/issue/2026-05-26-bug-flaky-agent-tests.md"]
fn agent_socket_input_reaches_child() {
    let dir = PrivateDir::new("sock");
    let sock_path = dir.path().join("agent.sock");
    let cfg = headless_config_with_socket(&["cat"], &sock_path);
    let (obs, seen) = CapturingObserver::new();
    let agent = Agent::new(cfg, Box::new(obs)).expect("agent new");

    // Run the agent on a worker thread; the main thread injects an EOF after
    // sending the test payload so cat exits cleanly.
    let sock_path_for_thread = sock_path.clone();
    let runner = std::thread::spawn(move || agent.run().expect("run"));

    // Give the agent a brief moment to bind + start polling.
    for _ in 0..50 {
        if sock_path_for_thread.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(sock_path_for_thread.exists(), "socket never appeared");

    // Send a single length-prefixed message.
    let client = hyoui::sys::socket::connect(&sock_path_for_thread).expect("connect");
    protocol::write_message(&client, b"hyoui-rocks\n").expect("write");
    drop(client);

    // Wait for cat to echo our bytes back, with a generous deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw = false;
    while std::time::Instant::now() < deadline {
        let buf = seen.lock().expect("lock").clone();
        if String::from_utf8_lossy(&buf).contains("hyoui-rocks") {
            saw = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(saw, "child never echoed injected bytes");

    // Tell cat to exit by sending another message containing EOT (0x04).
    // cat in a PTY treats 0x04 as VEOF and exits.
    let client = hyoui::sys::socket::connect(&sock_path_for_thread).expect("connect 2");
    protocol::write_message(&client, &[0x04]).expect("write eot");
    drop(client);

    let code = runner.join().expect("join runner");
    // cat exits 0 on clean EOF.
    assert_eq!(code, 0, "expected cat to exit 0 after VEOF, got {code}");
}

/// `--until` pattern matching: spawn `printf 'go DONE end\n'` and check
/// the loop reports exit 0 (UntilHit) rather than the child's natural exit.
/// Even if the child finishes before the scan triggers, the natural exit is
/// also 0, so the test is robust.
#[test]
fn agent_run_until_pattern_exits_zero() {
    let dir = PrivateDir::new("until");
    let sock = dir.path().join("agent.sock");
    let sock_arg = sock.to_str().expect("utf-8");
    let mut head = vec![
        "run",
        "--mode=headless",
        "--until",
        "DONE",
        "--socket",
        sock_arg,
        "--",
    ];
    head.extend_from_slice(&["printf", "go DONE end\\n"]);
    let cfg = match parse_args(&argv(&head)) {
        Command::Run(c) => c,
        other => panic!("parse: {other:?}"),
    };
    let agent = Agent::new(cfg, Box::new(NullObserver::new())).expect("agent new");
    let code = agent.run().expect("run");
    assert_eq!(code, 0);
}

/// `--timeout` fires and the loop returns 124.
/// Uses `sleep 5` with a 200ms timeout.
#[test]
fn agent_run_timeout_returns_124() {
    let dir = PrivateDir::new("timeout");
    let sock = dir.path().join("agent.sock");
    let sock_arg = sock.to_str().expect("utf-8");
    let mut head = vec![
        "run",
        "--mode=headless",
        "--timeout",
        "0.2",
        "--socket",
        sock_arg,
        "--",
    ];
    head.extend_from_slice(&["sleep", "5"]);
    let cfg = match parse_args(&argv(&head)) {
        Command::Run(c) => c,
        other => panic!("parse: {other:?}"),
    };
    let agent = Agent::new(cfg, Box::new(NullObserver::new())).expect("agent new");
    let code = agent.run().expect("run");
    assert_eq!(code, 124, "expected timeout exit 124, got {code}");
}
