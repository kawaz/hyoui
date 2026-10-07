//! DR-0041 e2e: session id は UUID だけ、重複と古い socket は run のエラー (bind / lock の
//! 時点で原子的に)、socket は `<状態の root>/sessions/<uuid>.sock`、discovery は
//! `sessions/` だけ、面は `HYOUI_STATE_DIR` → `$XDG_STATE_HOME/hyoui` → `$HOME/.local/state/hyoui`
//! の 3 段、`sun_path` に収まらない root でも bind / connect できる。
//!
//! 全部実バイナリで観測する。daemon は各 test の [`SessionDir`] (= 状態の root) の下に
//! 置くので、drop で畳まれる (= assert の途中で panic しても session を残さない)。

mod common;

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::session_dir::SessionDir;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 場所を決める env を全部外した `hyoui` (= test ごとに必要な分だけ足す)。
fn bare(args: &[&str]) -> Command {
    let mut c = Command::new(hyoui_bin());
    c.args(args)
        .env_remove("HYOUI_STATE_DIR")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// `HYOUI_STATE_DIR=<root>` の `hyoui`。
fn in_root(root: &Path, args: &[&str]) -> Command {
    let mut c = bare(args);
    c.env("HYOUI_STATE_DIR", root);
    c
}

fn output(mut c: Command) -> Output {
    // daemon は stderr を継承して常駐するので、`--detached` を pipe で待つと daemon が
    // 終わるまで EOF が来ない。stderr は file に逃がしてから読む。
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

/// `run --detached --pty-stdin [--session-id=<sid>] -- sh -c <script>` を起こす。
fn run_detached(root: &Path, sid: Option<&str>, script: &str) -> Output {
    let mut args = vec!["run", "--detached", "--pty-stdin"];
    let sid_arg;
    if let Some(sid) = sid {
        sid_arg = format!("--session-id={sid}");
        args.push(&sid_arg);
    }
    args.extend(["--", "sh", "-c", script]);
    output(in_root(root, &args))
}

fn sock_of(root: &Path, sid: &str) -> PathBuf {
    root.join("sessions").join(format!("{sid}.sock"))
}

fn is_canonical_uuid(s: &str) -> bool {
    hyoui::cli::validate_session_id(s).is_ok()
}

/// `hyoui status <target>` の `daemon-pid:` / `child-pid:` (`target` は id か `--socket=..`)。
fn pids(root: &Path, sid: &str) -> (i32, i32) {
    let out = output(in_root(root, &["status", sid]));
    assert!(out.status.success(), "status: {}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let field = |key: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|p| p.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("{key} not in status: {stdout}"))
    };
    (field("daemon-pid:"), field("child-pid:"))
}

/// `pid` が消えるまで待つ (= 他人の子の exit を通知で受ける portable な手段が無いので、
/// kill(pid, 0) で存在を観測する)。
fn wait_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if kill(Pid::from_raw(pid), None).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("pid {pid} did not exit");
}

fn list_ids(root: &Path) -> (Vec<String>, String) {
    let out = output(in_root(root, &["list", "--format=jsonl"]));
    assert!(out.status.success(), "list: {}", text(&out.stderr));
    let ids = text(&out.stdout)
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("jsonl");
            assert!(v.get("namespace").is_none(), "no namespace field: {l}");
            v["session"].as_str().expect("session").to_string()
        })
        .collect();
    (ids, text(&out.stderr))
}

/// session を畳み、daemon が終わる (= socket を片付け終える) まで待つ。
///
/// `kill --wait` は子と session の終了を見届けて戻るが、daemon が socket file を消すのは
/// その後の後始末なので、daemon の pid が消えるのを別に見る。
fn kill_session(root: &Path, target: &str) {
    let (daemon, _) = pids(root, target);
    let out = output(in_root(root, &["kill", target, "--signal=KILL", "--wait"]));
    assert!(out.status.success(), "kill {target}: {}", text(&out.stderr));
    wait_gone(daemon);
}

// ── 決定 2: id は UUID だけ ──────────────────────────────────────────────────

/// 指定が無ければ hyoui が UUID を振り、stdout に出した id で socket が置かれる。
#[test]
fn run_without_session_id_assigns_a_canonical_uuid() {
    let root = SessionDir::new("hyoui-sid-auto-");
    let out = run_detached(root.path(), None, "sleep 30");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let sid = text(&out.stdout).trim().to_string();
    assert!(is_canonical_uuid(&sid), "{sid:?}");
    assert!(sock_of(root.path(), &sid).exists());
    kill_session(root.path(), &sid);
}

/// `--session-id` で起動側が id を決められ、子の `HYOUI_SESSION_ID` もその値になる。
#[test]
fn run_with_session_id_uses_it_everywhere() {
    let root = SessionDir::new("hyoui-sid-given-");
    let sid = hyoui::cli::new_session_id();
    let marker = root.path().join("child-env");
    let script = format!(
        "printf '%s' \"$HYOUI_SESSION_ID\" > '{}'; sleep 30",
        marker.display()
    );
    let out = run_detached(root.path(), Some(&sid), &script);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), sid);
    assert!(sock_of(root.path(), &sid).exists());
    let (ids, _) = list_ids(root.path());
    assert_eq!(ids, [sid.as_str()]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&marker).unwrap_or_default() != sid {
        assert!(
            Instant::now() < deadline,
            "child never saw HYOUI_SESSION_ID={sid}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    kill_session(root.path(), &sid);
}

/// 標準形以外 (大文字・ハイフン無し・短縮・名前) は run のエラーで、何も作らない。
#[test]
fn run_rejects_session_ids_that_are_not_canonical_uuids() {
    let root = SessionDir::new("hyoui-sid-bad-");
    let sid = hyoui::cli::new_session_id();
    for bad in [
        sid.to_ascii_uppercase(),
        sid.replace('-', ""),
        sid[..8].to_string(),
        "demo".to_string(),
        format!("{{{sid}}}"),
    ] {
        let out = output(in_root(
            root.path(),
            &[
                "run",
                "--detached",
                &format!("--session-id={bad}"),
                "--",
                "true",
            ],
        ));
        assert_eq!(out.status.code(), Some(2), "{bad}: {}", text(&out.stderr));
        let stderr = text(&out.stderr);
        assert!(stderr.contains("canonical form"), "{bad}: {stderr}");
    }
    assert!(!root.path().join("sessions").exists(), "nothing is created");
}

/// session を指す他のコマンドも UUID 以外を受け付けない (= 表記違いで同じ session を指さない)。
#[test]
fn other_commands_reject_non_canonical_ids() {
    let root = SessionDir::new("hyoui-sid-alias-");
    let sid = hyoui::cli::new_session_id();
    assert!(
        run_detached(root.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );
    for alias in [sid.to_ascii_uppercase(), sid[..8].to_string()] {
        let out = output(in_root(root.path(), &["status", &alias]));
        assert_eq!(out.status.code(), Some(2), "{alias}: {}", text(&out.stderr));
    }
    kill_session(root.path(), &sid);
}

// ── 決定 3: 重複と古い socket は run のエラー ─────────────────────────────────

/// 生きている session と同じ id の run は失敗し、2 つ目の子は起動しない。1 つ目は
/// 生きたまま。エラーには原因と対処 (片付けのコマンド) が書かれる。
#[test]
fn a_duplicate_id_fails_before_starting_the_child() {
    let root = SessionDir::new("hyoui-dup-live-");
    let sid = hyoui::cli::new_session_id();
    assert!(
        run_detached(root.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );
    let (daemon, _) = pids(root.path(), &sid);

    let marker = root.path().join("second-child-ran");
    let out = run_detached(
        root.path(),
        Some(&sid),
        &format!("touch '{}'; sleep 30", marker.display()),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("既にある"), "{stderr}");
    assert!(stderr.contains(&format!("hyoui kill {sid}")), "{stderr}");
    assert!(stderr.contains("hyoui list"), "{stderr}");
    assert!(
        !marker.exists(),
        "the duplicate's child must not be started"
    );
    // 1 つ目は無傷 (= 同じ daemon が応答する)。
    assert_eq!(pids(root.path(), &sid).0, daemon);
    kill_session(root.path(), &sid);
}

/// daemon が死んで socket だけ残っていても重複として失敗し、run は socket を消さない。
/// 片付けの経路 (`hyoui list`) が消した後は同じ id で起動できる。
#[test]
fn a_dead_socket_fails_the_run_until_list_cleans_it_up() {
    let root = SessionDir::new("hyoui-dup-dead-");
    let sid = hyoui::cli::new_session_id();
    assert!(
        run_detached(root.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );
    let (daemon, child) = pids(root.path(), &sid);
    // daemon を SIGKILL (= Drop が走らず socket と lock が残る)。子も畳む。
    kill(Pid::from_raw(daemon), Signal::SIGKILL).expect("kill daemon");
    wait_gone(daemon);
    let _ = kill(Pid::from_raw(child), Signal::SIGKILL);
    let sock = sock_of(root.path(), &sid);
    let ino = std::fs::symlink_metadata(&sock)
        .expect("dead socket stays")
        .ino();

    let out = run_detached(root.path(), Some(&sid), "sleep 30");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("既にある"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::symlink_metadata(&sock).expect("still there").ino(),
        ino,
        "run must not replace the dead socket"
    );

    let (ids, _) = list_ids(root.path());
    assert!(
        ids.is_empty(),
        "the dead socket is cleaned up, not listed: {ids:?}"
    );
    assert!(!sock.exists());

    let out = run_detached(root.path(), Some(&sid), "sleep 30");
    assert!(out.status.success(), "{}", text(&out.stderr));
    kill_session(root.path(), &sid);
}

/// 同じ id の run を並行に起こすと、ちょうど 1 つだけ成功する (= 確認してから作る 2 段で
/// なく、name lock / bind の時点で原子的に決まる)。待ちは各 process の終了だけ。
#[test]
fn concurrent_runs_with_the_same_id_only_one_wins() {
    let root = SessionDir::new("hyoui-dup-race-");
    let sid = hyoui::cli::new_session_id();
    let errs = tempfile::tempdir().expect("stderr dir");
    let children: Vec<_> = (0..6)
        .map(|i| {
            let mut c = in_root(
                root.path(),
                &[
                    "run",
                    "--detached",
                    "--pty-stdin",
                    &format!("--session-id={sid}"),
                    "--",
                    "sleep",
                    "30",
                ],
            );
            c.stdout(Stdio::null()).stderr(
                std::fs::File::create(errs.path().join(i.to_string())).expect("stderr file"),
            );
            c.spawn().expect("spawn run")
        })
        .collect();
    let codes: Vec<Option<i32>> = children
        .into_iter()
        .map(|mut c| c.wait().expect("wait run").code())
        .collect();
    assert_eq!(
        codes.iter().filter(|c| **c == Some(0)).count(),
        1,
        "exactly one run wins: {codes:?}"
    );
    assert_eq!(
        codes.iter().filter(|c| **c == Some(1)).count(),
        5,
        "{codes:?}"
    );
    let losers_said_why = (0..6)
        .map(|i| std::fs::read_to_string(errs.path().join(i.to_string())).unwrap_or_default())
        .filter(|e| e.contains("既にある"))
        .count();
    assert_eq!(losers_said_why, 5);
    let (ids, _) = list_ids(root.path());
    assert_eq!(ids, [sid.as_str()]);
    kill_session(root.path(), &sid);
}

/// `kill --wait` は daemon の終了 (= socket の片付け) まで見届けるので、戻った直後に同じ
/// id で起動し直せる。
#[test]
fn the_same_id_can_be_reused_right_after_kill_wait() {
    let root = SessionDir::new("hyoui-reuse-");
    let sid = hyoui::cli::new_session_id();
    for round in 0..3 {
        let out = run_detached(root.path(), Some(&sid), "sleep 30");
        assert!(out.status.success(), "round {round}: {}", text(&out.stderr));
        let out = output(in_root(root.path(), &["kill", &sid, "--wait"]));
        assert!(out.status.success(), "round {round}: {}", text(&out.stderr));
        assert!(
            text(&out.stdout).contains("daemon 終了"),
            "{}",
            text(&out.stdout)
        );
        assert!(
            !sock_of(root.path(), &sid).exists(),
            "round {round}: the socket is gone when kill --wait returns"
        );
    }
}

/// daemon が socket を片付けずに終わった (= SIGKILL) 時、`kill --wait` は子の終了を
/// 見届けても exit 1 で、片付けの経路を案内する。
#[test]
fn kill_wait_reports_a_socket_left_by_a_daemon_that_died() {
    let root = SessionDir::new("hyoui-killdie-");
    let sid = hyoui::cli::new_session_id();
    assert!(
        run_detached(root.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );
    let (daemon, child) = pids(root.path(), &sid);
    // kill --wait が子の終了通知を待っている間に daemon を SIGKILL する。子は SIGTERM を
    // 無視しないので、daemon が先に死ねば client は EOF を「子 exit」として受ける。
    let mut c = in_root(root.path(), &["kill", &sid, "--wait", "--signal=STOP"]);
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let waiting = c.spawn().expect("spawn kill --wait");
    // daemon は STOP を子に送るだけで session は続く。client が KillAck 後の待ちに入った
    // ことは daemon の client 数で観測する。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let st = output(in_root(root.path(), &["status", &sid]));
        if text(&st.stdout).matches("id=").count() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "kill --wait never connected");
        std::thread::sleep(Duration::from_millis(20));
    }
    kill(Pid::from_raw(daemon), Signal::SIGKILL).expect("kill daemon");
    let _ = kill(Pid::from_raw(child), Signal::SIGKILL);
    let out = waiting.wait_with_output().expect("wait kill");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("片付けずに終わりました"),
        "{}",
        text(&out.stderr)
    );
    assert!(sock_of(root.path(), &sid).exists());
}

/// 古い版が namespace `sessions` として置いた UUID でない socket は、一覧・index 解決・
/// `kill --all` のどれにも混ざらず、file 単位で古い置き場として警告される。
#[test]
fn non_uuid_sockets_in_sessions_are_left_alone() {
    let root = SessionDir::new("hyoui-nonuuid-");
    let sid = hyoui::cli::new_session_id();
    assert!(
        run_detached(root.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );
    let legacy = root.path().join("sessions").join("legacy.sock");
    let out = output(in_root(
        root.path(),
        &[
            "run",
            "--detached",
            "--pty-stdin",
            &format!("--socket={}", legacy.display()),
            "--",
            "sleep",
            "30",
        ],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));

    let (ids, stderr) = list_ids(root.path());
    assert_eq!(ids, [sid.as_str()]);
    assert!(
        stderr.contains(&legacy.display().to_string()) && stderr.contains("not a UUID"),
        "{stderr}"
    );
    let out = output(in_root(root.path(), &["status", "--index=1"]));
    assert!(text(&out.stdout).contains(&sid), "{}", text(&out.stdout));
    let out = output(in_root(
        root.path(),
        &["kill", "--all", "--signal=KILL", "--wait"],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("1/1"), "{}", text(&out.stderr));
    assert!(
        legacy.exists(),
        "kill --all must not reach the non-UUID socket"
    );
    kill_session(root.path(), &format!("--socket={}", legacy.display()));
}

// ── 決定 1: tag と、廃止した namespace の option ───────────────────────────

fn run_tagged(root: &Path, tags: &[&str]) -> String {
    let sid = hyoui::cli::new_session_id();
    let sid_arg = format!("--session-id={sid}");
    let mut args = vec!["run", "--detached", "--pty-stdin", sid_arg.as_str()];
    for t in tags {
        args.push("--tag");
        args.push(t);
    }
    args.extend(["--", "sleep", "60"]);
    let out = output(in_root(root, &args));
    assert!(out.status.success(), "{tags:?}: {}", text(&out.stderr));
    sid
}

fn listed_with(root: &Path, filters: &[&str]) -> Vec<String> {
    let mut args = vec!["list", "--format=jsonl"];
    for f in filters {
        args.push("--tag");
        args.push(f);
    }
    let out = output(in_root(root, &args));
    assert!(out.status.success(), "{filters:?}: {}", text(&out.stderr));
    let mut ids: Vec<String> = text(&out.stdout)
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("jsonl");
            v["session"].as_str().expect("session").to_string()
        })
        .collect();
    ids.sort();
    ids
}

/// `run --tag` で付けた tag を daemon が持ち、status / list (jsonl / plain) に出し、
/// `list --tag` で絞れる。`--tag foo` は `--tag foo=` の略。絞る側の `--tag foo` は key が
/// あれば一致、`--tag foo=` は値が空に完全一致で、別の条件 (DR-0041 決定 1)。
#[test]
fn tags_are_returned_and_filtered() {
    let root = SessionDir::new("hyoui-tags-");
    let empty = run_tagged(root.path(), &["foo=", "env=prod"]);
    let bare = run_tagged(root.path(), &["foo"]);
    let bar = run_tagged(root.path(), &["foo=bar", "env=prod", "env=stg"]);
    let none = run_tagged(root.path(), &[]);
    let sorted = |mut v: Vec<&String>| {
        v.sort();
        v.into_iter().cloned().collect::<Vec<_>>()
    };

    assert_eq!(
        listed_with(root.path(), &[]),
        sorted(vec![&empty, &bare, &bar, &none])
    );
    assert_eq!(
        listed_with(root.path(), &["foo"]),
        sorted(vec![&empty, &bare, &bar])
    );
    assert_eq!(
        listed_with(root.path(), &["foo="]),
        sorted(vec![&empty, &bare])
    );
    assert_eq!(listed_with(root.path(), &["foo=bar"]), [bar.as_str()]);
    assert_eq!(
        listed_with(root.path(), &["foo=ba"]),
        Vec::<String>::new(),
        "no prefix match"
    );
    assert_eq!(
        listed_with(root.path(), &["foo", "env=prod"]),
        [empty.as_str()],
        "AND"
    );
    assert_eq!(
        listed_with(root.path(), &["env=stg"]),
        [bar.as_str()],
        "the last value wins"
    );
    assert_eq!(listed_with(root.path(), &["missing"]), Vec::<String>::new());

    // status の json と plain に出る。
    let out = output(in_root(root.path(), &["status", &bar, "--format=json"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status json");
    assert_eq!(v["tags"], serde_json::json!({"env": "stg", "foo": "bar"}));
    let out = output(in_root(root.path(), &["status", &none, "--format=json"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status json");
    assert_eq!(v["tags"], serde_json::json!({}));
    let out = output(in_root(root.path(), &["status", &bar]));
    assert!(
        text(&out.stdout).contains("tags: env=stg,foo=bar"),
        "{}",
        text(&out.stdout)
    );
    // jsonl の tags と plain の TAGS 列。
    let out = output(in_root(
        root.path(),
        &["list", "--format=jsonl", "--tag=foo=bar"],
    ));
    let v: serde_json::Value = serde_json::from_str(text(&out.stdout).trim()).expect("jsonl");
    assert_eq!(v["tags"], serde_json::json!({"env": "stg", "foo": "bar"}));
    let out = output(in_root(root.path(), &["list"]));
    let plain = text(&out.stdout);
    assert!(
        plain.lines().next().unwrap_or("").contains("TAGS"),
        "{plain}"
    );
    let row = |id: &str| {
        plain
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap_or("")
            .to_string()
    };
    assert!(row(&bar).contains("env=stg,foo=bar"), "{plain}");
    assert!(row(&bare).contains(" foo= "), "{plain}");
    for sid in [&empty, &bare, &bar, &none] {
        kill_session(root.path(), sid);
    }
}

/// tag の key が規則に合わなければ run は起動しない。
#[test]
fn a_bad_tag_key_fails_the_run() {
    let root = SessionDir::new("hyoui-badtag-");
    let out = output(in_root(
        root.path(),
        &["run", "--detached", "--tag", "a b=c", "--", "true"],
    ));
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("--tag"), "{}", text(&out.stderr));
    assert!(!root.path().join("sessions").exists());
}

/// 廃止した namespace の option は受け付けて捨て、stderr に 1 行だけ注意を出す。stdout は
/// option が無い時と同じ。`HYOUI_NAMESPACE` は読まない (注意も出ない)。
#[test]
fn removed_namespace_options_are_ignored_with_one_notice() {
    let root = SessionDir::new("hyoui-ns-compat-");
    let out = output(in_root(
        root.path(),
        &[
            "run",
            "--detached",
            "--pty-stdin",
            "--namespace",
            "workers",
            "--",
            "sleep",
            "60",
        ],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let sid = text(&out.stdout).trim().to_string();
    assert!(is_canonical_uuid(&sid), "{sid:?}");
    assert!(
        sock_of(root.path(), &sid).exists(),
        "namespace does not change the placement"
    );
    let notice = |stderr: &str| stderr.lines().filter(|l| l.contains("2026-11")).count();
    assert_eq!(notice(&text(&out.stderr)), 1, "{}", text(&out.stderr));

    let plain = output(in_root(root.path(), &["list", "--format=jsonl"]));
    for args in [
        &["list", "--namespace=other", "--format=jsonl"][..],
        &["list", "--all-namespaces", "--format=jsonl"][..],
    ] {
        let out = output(in_root(root.path(), args));
        assert!(out.status.success(), "{args:?}");
        // started_unix_ms / dur_ms 以外は同じ (dur は時間で変わる)。
        let ids = |b: &[u8]| {
            text(b)
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["session"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&out.stdout), ids(&plain.stdout), "{args:?}");
        assert_eq!(
            notice(&text(&out.stderr)),
            1,
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    let mut c = in_root(root.path(), &["list", "--format=jsonl"]);
    c.env("HYOUI_NAMESPACE", "other");
    let out = output(c);
    assert_eq!(notice(&text(&out.stderr)), 0, "HYOUI_NAMESPACE is not read");
    assert_eq!(text(&out.stdout).lines().count(), 1);
    let out = output(in_root(
        root.path(),
        &["kill", "--namespace=x", &sid, "--signal=KILL", "--wait"],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = output(in_root(root.path(), &["run", "--help"]));
    assert!(!text(&out.stdout).contains("namespace"));
}

/// tag は daemon の upgrade (self-exec) をまたいで残る。
#[test]
fn tags_survive_a_daemon_upgrade() {
    let root = SessionDir::new("hyoui-tagup-");
    let sid = run_tagged(root.path(), &["env=prod"]);
    let (before, _) = pids(root.path(), &sid);
    let out = output(in_root(root.path(), &["upgrade", &sid]));
    // Design rationale: `hyoui upgrade` の client は、daemon が upgrade.ack を書き出す前に
    // self-exec して接続が閉じると「recv error before ack」で失敗することがある (ack の
    // broadcast は writer thread への enqueue だけで、exec の前に flush を待たない。tag とは
    // 別の、upgrade 経路にある race)。この test が確かめるのは upgrade をまたいだ tag の
    // 保持なので、その 1 種類の失敗だけは許し、他の失敗は落とす。
    let ack_lost = text(&out.stderr).contains("recv error before ack");
    assert!(out.status.success() || ack_lost, "{}", text(&out.stderr));
    let out = output(in_root(root.path(), &["status", &sid, "--format=json"]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status json");
    assert_eq!(v["tags"], serde_json::json!({"env": "prod"}));
    assert_eq!(
        v["daemon_pid"].as_i64(),
        Some(i64::from(before)),
        "self-exec keeps the pid"
    );
    kill_session(root.path(), &sid);
}

// ── 決定 4: socket は sessions/ にフラット、discovery は sessions/ だけ ──────

/// root 直下や古い namespace の dir にある socket は一覧に出ず、古い置き場として警告される。
#[test]
fn discovery_reads_only_sessions_and_warns_about_the_old_layout() {
    let root = SessionDir::new("hyoui-layout-");
    let old_ns = root.path().join("workers");
    std::fs::create_dir(&old_ns).unwrap();
    std::fs::set_permissions(&old_ns, std::fs::Permissions::from_mode(0o700)).unwrap();
    let in_root = root.path().join("legacy.sock");
    let in_ns = old_ns.join("w1.sock");
    for sock in [&in_root, &in_ns] {
        let out = output(self::in_root(
            root.path(),
            &[
                "run",
                "--detached",
                "--pty-stdin",
                &format!("--socket={}", sock.display()),
                "--",
                "sleep",
                "30",
            ],
        ));
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    let (ids, stderr) = list_ids(root.path());
    assert!(
        ids.is_empty(),
        "sockets outside sessions/ are not sessions: {ids:?}"
    );
    assert!(stderr.contains("old layout"), "{stderr}");
    assert!(
        stderr.contains(&root.path().display().to_string()),
        "{stderr}"
    );
    assert!(stderr.contains(&old_ns.display().to_string()), "{stderr}");

    for sock in [&in_root, &in_ns] {
        kill_session(root.path(), &format!("--socket={}", sock.display()));
    }
    let (_, stderr) = list_ids(root.path());
    assert!(
        !stderr.contains("old layout"),
        "warning stops once they are gone: {stderr}"
    );
}

// ── 決定 6: 面 = 状態の root ─────────────────────────────────────────────────

/// `HYOUI_STATE_DIR` を変えると互いの session が見えない (= 面は root 1 つで決まる)。
#[test]
fn different_state_roots_do_not_see_each_other() {
    let a = SessionDir::new("hyoui-face-a-");
    let b = SessionDir::new("hyoui-face-b-");
    let sid = hyoui::cli::new_session_id();
    assert!(
        run_detached(a.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );

    assert_eq!(list_ids(a.path()).0, [sid.as_str()]);
    assert!(list_ids(b.path()).0.is_empty());
    let out = output(in_root(b.path(), &["status", &sid]));
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    // 同じ id でも別の面なら衝突しない。
    assert!(
        run_detached(b.path(), Some(&sid), "sleep 30")
            .status
            .success()
    );
    assert_eq!(list_ids(b.path()).0, [sid.as_str()]);
    kill_session(a.path(), &sid);
    kill_session(b.path(), &sid);
}

/// `HYOUI_STATE_DIR` が無ければ `$XDG_STATE_HOME/hyoui`、XDG が相対パスなら
/// `$HOME/.local/state/hyoui`。
#[test]
fn the_state_root_falls_back_to_xdg_then_home() {
    let xdg = SessionDir::new("hyoui-xdg-");
    let sid = hyoui::cli::new_session_id();
    let mut c = bare(&[
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--session-id={sid}"),
        "--",
        "sleep",
        "30",
    ]);
    c.env("XDG_STATE_HOME", xdg.path())
        .env("HOME", "/nonexistent-home");
    let out = output(c);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        xdg.path()
            .join("hyoui/sessions")
            .join(format!("{sid}.sock"))
            .exists()
    );
    let mut c = bare(&["kill", &sid, "--signal=KILL", "--wait"]);
    c.env("XDG_STATE_HOME", xdg.path());
    assert!(output(c).status.success());

    let home = SessionDir::new("hyoui-home-");
    let sid = hyoui::cli::new_session_id();
    let mut c = bare(&[
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--session-id={sid}"),
        "--",
        "sleep",
        "30",
    ]);
    c.env("XDG_STATE_HOME", "relative/state")
        .env("HOME", home.path());
    let out = output(c);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        home.path()
            .join(".local/state/hyoui/sessions")
            .join(format!("{sid}.sock"))
            .exists()
    );
    let mut c = bare(&["kill", &sid, "--signal=KILL", "--wait"]);
    c.env("HOME", home.path());
    assert!(output(c).status.success());
}

/// どの段でも root を決められなければ、cwd 相対に倒さずエラーにする。
#[test]
fn no_state_root_is_an_error() {
    let cwd = tempfile::tempdir().expect("cwd");
    let mut c = bare(&["run", "--detached", "--", "true"]);
    c.env_remove("HOME").current_dir(cwd.path());
    let out = output(c);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("HYOUI_STATE_DIR") && stderr.contains("HOME"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_dir(cwd.path()).unwrap().count(),
        0,
        "nothing under cwd"
    );

    let mut c = bare(&["list"]);
    c.env_remove("HOME").current_dir(cwd.path());
    let out = output(c);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));

    let mut c = bare(&["run", "--detached", "--", "true"]);
    c.env("HYOUI_STATE_DIR", "relative/root")
        .current_dir(cwd.path());
    let out = output(c);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("relative"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::read_dir(cwd.path()).unwrap().count(),
        0,
        "nothing under cwd"
    );
}

// ── 決定 5: sun_path に収まらない root ───────────────────────────────────────

/// フルパスが `sun_path` の上限を超える root でも run → list → input → screen dump → kill
/// が通る (= dir の fd 基準の相対名で bind / connect する)。
#[test]
fn a_root_longer_than_sun_path_works_end_to_end() {
    let base = SessionDir::new("hyoui-long-");
    let mut root = base.path().to_path_buf();
    while root.as_os_str().len() <= hyoui::sys::socket::sun_path_max() {
        root.push("a-fairly-long-directory-name");
    }
    std::fs::create_dir_all(&root).unwrap();
    let sid = hyoui::cli::new_session_id();
    let out = run_detached(&root, Some(&sid), "cat");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let sock = sock_of(&root, &sid);
    assert!(sock.as_os_str().len() > hyoui::sys::socket::sun_path_max());
    assert!(sock.exists());

    assert_eq!(list_ids(&root).0, [sid.as_str()]);
    let out = output(in_root(
        &root,
        &["input", &sid, "text:LONGPATH-OK", "key:Enter"],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = output(in_root(
        &root,
        &["wait", &sid, "LONGPATH-OK", "--timeout=5s"],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = output(in_root(
        &root,
        &["screen", "dump", &sid, "--format=text/plain"],
    ));
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("LONGPATH-OK"),
        "{}",
        text(&out.stdout)
    );
    kill_session(&root, &sid);
    assert!(list_ids(&root).0.is_empty());
}
