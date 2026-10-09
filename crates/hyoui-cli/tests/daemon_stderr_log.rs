//! DR-0037 段 2 e2e: 起動後の daemon は呼び出し元の stderr を持たず、fd 2 と logger の書き先は
//! `<状態の root>/sessions/logs/<id>.log`。ready 通知の前の失敗は呼び出し元の stderr に出る。
//!
//! 待ちはすべて pipe の EOF・process の終了・`hyoui kill --wait` で行い、時間で判定しない。
//! daemon は logger を止めてから name lock を手放すので、`hyoui kill --wait` が戻った時には
//! ログは書き終わり、空のログは消えている (= 直後にファイルを確かめてよい)。

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

/// `hyoui kill --wait <id>` (= 子の exit と daemon の終了 (name lock の解放) を見届けて戻る)。
fn kill_and_wait(dir: &Path, session: &str) {
    kill_and_wait_with(hyoui(dir), session);
}

/// [`kill_and_wait`] を任意の Command (= env を変えたもの) で行う。`target` は id か `--socket=`。
fn kill_and_wait_with(mut cmd: Command, target: &str) {
    let out = common::pty::capture_with_deadline(
        cmd.args(["kill", "--wait", target])
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

/// ログを `sessions/logs/<id>.log` に作れる状態にして、その path を返す (sessions/ と logs/ は
/// 0700)。
fn prepared_log_path(dir: &Path, session: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = log_path(dir, session);
    for d in [dir.join("sessions"), dir.join("sessions").join("logs")] {
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

/// 使えないログ (`prepare` が `path` に置く) がある時: 起動は止まらず、呼び出し元に警告が
/// 出て、ready 後の daemon のログ (`--debug-dump-server` の open 失敗) はそこに書かれない。
fn assert_unusable_log_is_refused(
    name: &str,
    prepare: impl FnOnce(&Path),
    check: impl FnOnce(&Path),
) {
    let dir = SessionDir::new(&format!("hyoui-log-{name}-"));
    let id = hyoui::cli::new_session_id();
    let path = prepared_log_path(dir.path(), &id);
    prepare(&path);
    let dump = dir.path().join("no-such-dir").join("x.dump");
    let mut cmd = hyoui(dir.path());
    cmd.args([
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--session-id={id}"),
        &format!("--debug-dump-server={}", dump.display()),
        "--",
        "cat",
    ]);
    let (status, out) = capture_merged(cmd).expect("run blocked on the unusable log");
    assert!(status.success(), "{name}: {out:?}");
    assert!(
        out.contains("を開けないため、起動後の daemon のログは捨てます"),
        "{name}: no warning on the caller stderr: {out:?}"
    );
    assert_eq!(out.lines().last(), Some(id.as_str()), "{name}: {out:?}");
    kill_and_wait(dir.path(), &id);
    check(&path);
}

/// 既にある FIFO (読み手あり) は open で止まらず断る。FIFO には何も書かれない。
#[test]
fn an_existing_fifo_log_is_refused() {
    use std::io::Read;
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    let reader = std::cell::RefCell::new(None);
    assert_unusable_log_is_refused(
        "fifo",
        |p| {
            nix::unistd::mkfifo(p, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
            *reader.borrow_mut() = Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
                    .open(p)
                    .unwrap(),
            );
        },
        |p| {
            assert!(std::fs::symlink_metadata(p).unwrap().file_type().is_fifo());
            let mut buf = [0u8; 64];
            let got = reader.borrow_mut().as_mut().unwrap().read(&mut buf);
            // 書き手は全員去った後なので、何も書かれていなければ EOF (0) になる。
            assert!(matches!(got, Ok(0)), "fifo received data: {got:?}");
        },
    );
}

/// symlink はたどらず断る。先のファイルは変わらない。
#[test]
fn a_symlinked_log_is_refused() {
    let target = std::cell::RefCell::new(PathBuf::new());
    assert_unusable_log_is_refused(
        "symlink",
        |p| {
            let t = p.parent().unwrap().parent().unwrap().join("victim");
            std::fs::write(&t, b"keep\n").unwrap();
            std::os::unix::fs::symlink(&t, p).unwrap();
            *target.borrow_mut() = t;
        },
        |p| {
            assert!(
                std::fs::symlink_metadata(p)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read(&*target.borrow()).unwrap(), b"keep\n");
        },
    );
}

/// group / other が読める既存ファイルは断る。中身と mode は変わらない。
#[test]
fn a_log_readable_by_others_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    assert_unusable_log_is_refused(
        "mode",
        |p| {
            std::fs::write(p, b"pre\n").unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
        },
        |p| {
            assert_eq!(std::fs::read(p).unwrap(), b"pre\n");
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644);
        },
    );
}

/// 明示した socket のログは socket の隣 (`<socket の stem>.log`) で、`sessions/logs/` は使わない。
#[test]
fn an_explicit_socket_logs_next_to_the_socket() {
    use std::os::unix::fs::PermissionsExt;
    let dir = SessionDir::new("hyoui-log-explicit-");
    let alt = dir.path().join("alt");
    std::fs::create_dir(&alt).unwrap();
    std::fs::set_permissions(&alt, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sock = alt.join("mine.sock");
    let dump = dir.path().join("no-such-dir").join("x.dump");
    let mut cmd = hyoui(dir.path());
    cmd.args([
        "run",
        "--detached",
        "--pty-stdin",
        &format!("--socket={}", sock.display()),
        &format!("--debug-dump-server={}", dump.display()),
        "--",
        "cat",
    ]);
    let (status, out) = capture_merged(cmd).expect("run did not return");
    assert!(status.success(), "{out:?}");
    let id = out.trim_end_matches('\n').to_string();
    kill_and_wait_with(hyoui(dir.path()), &format!("--socket={}", sock.display()));
    let log = std::fs::read_to_string(alt.join("mine.log")).expect("log next to the socket");
    assert!(log.contains("--debug-dump open"), "{log:?}");
    assert!(!log_path(dir.path(), &id).exists());
}

/// upgrade (self-exec) で再開した daemon も、状態の root が無い環境の明示 socket で、socket の
/// 隣のログに書く (= 呼び出し元の stderr や引き継いだ fd に頼らない)。
#[test]
fn an_upgraded_daemon_logs_next_to_an_explicit_socket_without_a_state_root() {
    use std::os::unix::fs::PermissionsExt;
    let dir = SessionDir::new("hyoui-log-upgrade-");
    let alt = dir.path().join("alt");
    std::fs::create_dir(&alt).unwrap();
    std::fs::set_permissions(&alt, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sock = alt.join("up.sock");
    let socket_arg = format!("--socket={}", sock.display());
    let rootless = || {
        let mut c = hyoui(dir.path());
        c.env_remove("HYOUI_STATE_DIR")
            .env_remove("HOME")
            .env_remove("XDG_STATE_HOME");
        c
    };
    let mut cmd = rootless();
    cmd.args(["run", "--detached", "--pty-stdin", &socket_arg, "--", "cat"]);
    let (status, out) = capture_merged(cmd).expect("run did not return");
    assert!(status.success(), "{out:?}");

    let up = common::pty::capture_with_deadline(
        rootless()
            .args(["upgrade", &socket_arg])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        DEADLINE,
    )
    .expect("hyoui upgrade");
    // `tags_survive_a_daemon_upgrade` と同じく、ack が exec に先を越される race だけは許す。
    let err = String::from_utf8_lossy(&up.stderr);
    assert!(
        up.status.success() || err.contains("recv error before ack"),
        "{err}"
    );
    kill_and_wait_with(rootless(), &socket_arg);
    let log = std::fs::read_to_string(alt.join("up.log")).expect("log next to the socket");
    assert!(log.contains("upgrade-resume ready"), "{log:?}");
}
