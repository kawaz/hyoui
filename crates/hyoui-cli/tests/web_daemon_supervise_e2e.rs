//! `hyoui web daemon supervise` と制御 socket の E2E (= DR-0034 P3)。
//!
//! 隔離した `XDG_STATE_HOME` に登録簿と socket を置き、実際の監督者プロセスを
//! 起こして観測する。gateway は実 port を掴むので、他の test と重ならない番号を
//! 使い、監督者は必ず pid 指定で落とす。

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// 本 test file が使う port の範囲 (= 他の e2e と重ねない)。
const PORT_BASE: u16 = 43810;

fn hyoui(args: &[&str], home: &Path) -> Output {
    command(args, home).output().expect("spawn hyoui")
}

fn command(args: &[&str], home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hyoui"));
    command
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .stdin(Stdio::null());
    command
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}): {} / stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn error_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr was not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// 監督者を起こし、落とし忘れない持ち物。
struct Supervised {
    home: tempfile::TempDir,
    child: Child,
}

impl Supervised {
    fn start(home: tempfile::TempDir) -> Self {
        let child = command(&["web", "daemon", "supervise"], home.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn supervisor");
        let started = Self { home, child };
        started.await_socket();
        started
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        hyoui(args, self.path())
    }

    fn socket(&self) -> PathBuf {
        self.path().join(".local/state/hyoui-web/supervisor.sock")
    }

    /// 制御 socket が答えるようになるまで待つ。
    fn await_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(self.socket()).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the supervisor never opened {}", self.socket().display());
    }

    /// 条件が満たされるまで `status` を読み直す。
    fn await_status(
        &self,
        what: &str,
        mut done: impl FnMut(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = serde_json::Value::Null;
        while Instant::now() < deadline {
            let output = self.run(&["web", "daemon", "status"]);
            if output.status.success() {
                last = json(&output);
                if done(&last) {
                    return last;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}; last status was {last:#}");
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        // 監督者に SIGTERM を送れば子も道連れで降りる (= 決定 6)。
        send(self.child.id() as i32, Signal::SIGTERM);
        let _ = self.child.wait();
        let mut stderr = String::new();
        if let Some(pipe) = self.child.stderr.as_mut() {
            let _ = pipe.read_to_string(&mut stderr);
        }
    }
}

fn unit_row<'a>(status: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    status["units"]
        .as_array()?
        .iter()
        .find(|unit| unit["name"] == name)
}

/// 監督者が登録簿の enabled な unit を起こし、落ちたら上げ直す (= 決定 3)。
#[test]
fn the_supervisor_starts_units_and_restarts_them_when_they_die() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let port = PORT_BASE;
    assert!(
        hyoui(
            &[
                "web",
                "daemon",
                "add",
                "unstable",
                &format!("--port={port}")
            ],
            home.path()
        )
        .status
        .success()
    );

    let supervisor = Supervised::start(home);
    let status = supervisor.await_status("the unit to come up", |status| {
        unit_row(status, "unstable").is_some_and(|unit| unit["running"] == true)
    });

    let unit = unit_row(&status, "unstable").expect("the unit is listed");
    assert_eq!(unit["enabled"], true);
    assert!(unit["pid"].as_u64().is_some(), "{unit}");
    assert!(unit["started_at"].as_str().is_some(), "{unit}");
    assert_eq!(unit["listen"], format!("127.0.0.1:{port}"));
    assert_eq!(unit["binary_exists"], true);
    // 走っている本人が答えた版と、その実行ファイルが答えた版が並ぶ (= 決定 7a)。
    assert_eq!(unit["version"]["running"]["version"], hyoui_version());
    assert_eq!(unit["version"]["on_disk"]["version"], hyoui_version());
    assert_eq!(unit["version"]["restart_needed"], false);
    assert_eq!(unit["restarts"], 0);

    // 子を外から殺すと、監督者が backoff を置いて上げ直す。
    let first_pid = unit["pid"].as_u64().expect("pid") as i32;
    send(first_pid, Signal::SIGKILL);

    let after = supervisor.await_status("the unit to be restarted", |status| {
        unit_row(status, "unstable").is_some_and(|unit| {
            unit["running"] == true && unit["pid"].as_u64() != Some(first_pid as u64)
        })
    });
    let unit = unit_row(&after, "unstable").expect("the unit is listed");
    assert!(
        unit["restarts"].as_u64().is_some_and(|count| count >= 1),
        "{unit}"
    );
    assert!(
        unit["last_exit"]
            .as_str()
            .is_some_and(|reason| reason.contains("signal")),
        "{unit}"
    );
}

/// `add` / `remove` は走行中の監督者に即反映される (= 決定 4)。
#[test]
fn add_and_remove_reach_a_running_supervisor() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let supervisor = Supervised::start(home);
    let port = PORT_BASE + 1;

    let added = json(&supervisor.run(&["web", "daemon", "add", "late", &format!("--port={port}")]));
    // 監督者が居るので、その場で要求が届いている。
    assert_eq!(added["supervisor"]["running"], true);
    assert_eq!(added["supervisor"]["notified"], true);
    assert!(added["note"].is_null(), "{added}");

    supervisor.await_status("the added unit to come up", |status| {
        unit_row(status, "late").is_some_and(|unit| unit["running"] == true)
    });

    // remove は先に停止を頼み、子が降りてから登録を消す。
    let removed = json(&supervisor.run(&["web", "daemon", "remove", "late"]));
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["supervisor"]["notified"], true);

    let status = json(&supervisor.run(&["web", "daemon", "status"]));
    assert!(unit_row(&status, "late").is_none(), "{status}");
    assert!(
        !supervisor
            .path()
            .join(".local/state/hyoui-web/units/late.toml")
            .exists()
    );
}

/// `stop` した unit は `restart --all` で復活しない (= 決定 5)。
#[test]
fn a_stopped_unit_is_not_revived_by_restart_all() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let port = PORT_BASE + 2;
    assert!(
        hyoui(
            &["web", "daemon", "add", "keeper", &format!("--port={port}")],
            home.path()
        )
        .status
        .success()
    );
    let supervisor = Supervised::start(home);
    supervisor.await_status("the unit to come up", |status| {
        unit_row(status, "keeper").is_some_and(|unit| unit["running"] == true)
    });

    let stopped = json(&supervisor.run(&["web", "daemon", "stop", "keeper"]));
    let unit = unit_row(&stopped, "keeper").expect("the unit is listed");
    assert_eq!(unit["enabled"], false);

    supervisor.await_status("the unit to go down", |status| {
        unit_row(status, "keeper").is_some_and(|unit| unit["running"] == false)
    });

    // `--all` は enabled な unit だけを対象にし、停止中はスキップした旨を載せる。
    let restarted = json(&supervisor.run(&["web", "daemon", "restart", "--all"]));
    assert!(
        restarted["notes"]
            .as_array()
            .is_some_and(|notes| notes.iter().any(|note| note
                .as_str()
                .is_some_and(|note| note.contains("skipped `keeper`")))),
        "{restarted}"
    );
    let unit = unit_row(&restarted, "keeper").expect("the unit is listed");
    assert_eq!(unit["enabled"], false);
    assert_eq!(unit["running"], false);

    // 名前を明示した restart は start と同義で、desired state を立て直す。
    let named = json(&supervisor.run(&["web", "daemon", "restart", "keeper"]));
    assert_eq!(
        unit_row(&named, "keeper").expect("the unit is listed")["enabled"],
        true
    );
}

/// `restart --all` は 1 台ずつ入れ替え、入れ替わった先が応答する (= 決定 5)。
#[test]
fn restart_all_replaces_running_units_one_at_a_time() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let first = PORT_BASE + 3;
    let second = PORT_BASE + 4;
    for (name, port) in [("alpha", first), ("beta", second)] {
        assert!(
            hyoui(
                &["web", "daemon", "add", name, &format!("--port={port}")],
                home.path()
            )
            .status
            .success()
        );
    }
    let supervisor = Supervised::start(home);
    let before = supervisor.await_status("both units to come up", |status| {
        ["alpha", "beta"]
            .iter()
            .all(|name| unit_row(status, name).is_some_and(|unit| unit["running"] == true))
    });
    let pids: Vec<u64> = ["alpha", "beta"]
        .iter()
        .map(|name| unit_row(&before, name).unwrap()["pid"].as_u64().unwrap())
        .collect();

    let output = supervisor.run(&["web", "daemon", "restart", "--all"]);
    assert!(
        output.status.success(),
        "restart --all failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let restarted = json(&output);

    // 両方が新しい pid になっている (= 入れ替わった)。
    for (index, name) in ["alpha", "beta"].iter().enumerate() {
        let unit = unit_row(&restarted, name).expect("the unit is listed");
        assert_eq!(unit["running"], true, "{name}: {unit}");
        assert_ne!(unit["pid"].as_u64(), Some(pids[index]), "{name}: {unit}");
    }

    // 入れ替わった先が実際に応答する (= /healthz を待ってから次へ進んでいる)。
    for port in [first, second] {
        assert!(
            http_get(&format!("127.0.0.1:{port}"), "/healthz")
                .expect("the replaced gateway answers")
                .trim_end()
                .ends_with("ok")
        );
    }
}

/// 監督者が居なければ start / stop / restart / log は hint 付きで断る (= 決定 4)。
#[test]
fn control_verbs_refuse_without_a_supervisor() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert!(
        hyoui(
            &["web", "daemon", "add", "stable", "--port=43809"],
            home.path()
        )
        .status
        .success()
    );

    for args in [
        &["web", "daemon", "start", "stable"][..],
        &["web", "daemon", "stop", "--all"][..],
        &["web", "daemon", "restart", "--all"][..],
        &["web", "daemon", "log"][..],
    ] {
        let output = hyoui(args, home.path());
        assert!(!output.status.success(), "args={args:?}");
        let error = error_json(&output);
        assert_eq!(error["kind"], "supervisor_not_running", "args={args:?}");
        let hint = error["hint"].as_str().unwrap_or_default();
        assert!(hint.contains("hyoui web service start"), "args={args:?}");
    }

    // `status` と `list` は監督者不在でも登録簿から答える (= 障害時の入口)。
    for args in [
        &["web", "daemon", "status"][..],
        &["web", "daemon", "list"][..],
    ] {
        let output = hyoui(args, home.path());
        assert!(output.status.success(), "args={args:?}");
        let body = json(&output);
        assert_eq!(body["supervisor"]["running"], false, "args={args:?}");
        assert_eq!(body["units"][0]["name"], "stable", "args={args:?}");
        assert_eq!(body["units"][0]["running"], false, "args={args:?}");
        assert!(body["note"].is_string(), "args={args:?}");
    }
}

/// 監督者は子の出力を unit 名のファイルに集め、`log` がそれを返す (= 決定 9)。
#[test]
fn unit_output_is_collected_per_unit() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let port = PORT_BASE + 5;
    assert!(
        hyoui(
            &["web", "daemon", "add", "talker", &format!("--port={port}")],
            home.path()
        )
        .status
        .success()
    );
    let supervisor = Supervised::start(home);
    supervisor.await_status("the unit to come up", |status| {
        unit_row(status, "talker").is_some_and(|unit| unit["running"] == true)
    });

    // gateway は bind 後に listen 先を書くので、その行が集まる。
    let log_path = supervisor
        .path()
        .join(".local/state/hyoui-web/logs/talker.log");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::fs::read_to_string(&log_path).is_ok_and(|text| text.contains("listening on")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let collected = std::fs::read_to_string(&log_path).expect("the log file exists");
    assert!(
        collected.contains(&format!("127.0.0.1:{port}")),
        "{collected}"
    );

    // `log` は JSONL で、どの unit の行かを添える。
    let output = supervisor.run(&["web", "daemon", "log", "talker"]);
    assert!(
        output.status.success(),
        "log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().expect("at least one line");
    let line: serde_json::Value = serde_json::from_str(first).expect("JSONL");
    assert_eq!(line["name"], "talker");
    assert!(line["line"].is_string(), "{line}");
}

/// `hyoui version` は CLI / 監督者 / 全 unit の版を 1 回で並べる (= 決定 7a)。
#[test]
fn version_reports_the_cli_the_supervisor_and_the_units() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let port = PORT_BASE + 6;
    assert!(
        hyoui(
            &[
                "web",
                "daemon",
                "add",
                "unstable",
                &format!("--port={port}")
            ],
            home.path()
        )
        .status
        .success()
    );

    // 監督者が居ない間は「走っている版」を誰も言えない。
    let without = json(&hyoui(&["version"], home.path()));
    assert_eq!(without["cli"]["version"], hyoui_version());
    assert!(without["supervisor"].is_null(), "{without}");
    assert_eq!(without["units"][0]["name"], "unstable");
    assert!(without["units"][0]["running"].is_null(), "{without}");
    assert_eq!(without["units"][0]["on_disk"]["version"], hyoui_version());
    assert_eq!(without["units"][0]["restart_needed"], false);

    let supervisor = Supervised::start(home);
    supervisor.await_status("the unit to come up", |status| {
        unit_row(status, "unstable").is_some_and(|unit| unit["running"] == true)
    });

    let with = json(&supervisor.run(&["version"]));
    assert_eq!(with["cli"]["version"], hyoui_version());
    assert_eq!(with["supervisor"]["running"]["version"], hyoui_version());
    assert_eq!(with["supervisor"]["on_disk"]["version"], hyoui_version());
    assert_eq!(with["supervisor"]["restart_needed"], false);
    assert_eq!(with["units"][0]["running"]["version"], hyoui_version());
    assert_eq!(with["units"][0]["restart_needed"], false);

    // `--version` は 1 行テキストのまま。
    let text = supervisor.run(&["--version"]);
    let line = String::from_utf8_lossy(&text.stdout);
    assert!(line.starts_with("hyoui "), "{line}");
    assert!(!line.contains('{'), "{line}");
}

/// 監督者を止めると抱えている子も止まる (= 決定 6)。
#[test]
fn stopping_the_supervisor_stops_its_children() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let port = PORT_BASE + 7;
    assert!(
        hyoui(
            &["web", "daemon", "add", "doomed", &format!("--port={port}")],
            home.path()
        )
        .status
        .success()
    );

    let child_pid;
    let socket;
    {
        let supervisor = Supervised::start(home);
        let status = supervisor.await_status("the unit to come up", |status| {
            unit_row(status, "doomed").is_some_and(|unit| unit["running"] == true)
        });
        child_pid = unit_row(&status, "doomed").unwrap()["pid"]
            .as_u64()
            .unwrap() as i32;
        socket = supervisor.socket();
        assert!(http_get(&format!("127.0.0.1:{port}"), "/healthz").is_some());
        // ここで Drop が SIGTERM を送る。
    }

    // 子が降り、制御 socket も片付いている。
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut alive = true;
    while Instant::now() < deadline {
        if !is_alive(child_pid) {
            alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive, "the child gateway outlived its supervisor");
    assert!(!socket.exists(), "the control socket was left behind");
    assert!(http_get(&format!("127.0.0.1:{port}"), "/healthz").is_none());
}

fn hyoui_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn send(pid: i32, signal: Signal) {
    let _ = kill(Pid::from_raw(pid), signal);
}

/// signal を送らない `kill` は存在確認になる (= 降りていれば ESRCH)。
fn is_alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

/// 素の TCP で 1 リクエストだけ投げる (= HTTP client 依存を足さない)。
fn http_get(target: &str, path: &str) -> Option<String> {
    let mut stream = std::net::TcpStream::connect(target).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    BufReader::new(stream).read_to_string(&mut response).ok()?;
    response.contains(" 200 ").then_some(response)
}
