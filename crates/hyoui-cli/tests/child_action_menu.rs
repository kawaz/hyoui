//! DR-0032 §2 e2e: `show_child_action_menu` の child action menu。
//!
//! 経路:
//!
//! ```text
//! [session] on_child_suspend = "show_child_action_menu"
//!   → 子 PTY process に SIGSTOP
//!   → daemon が waitpid(WUNTRACED) で観測して SessionChildStoppedNotify
//!   → rw attach client が起こさず menu を画面下部に描画 (= 入力は menu が飲む)
//!   → 項目キーで SIGCONT / 終了系 signal / detach を実行
//! ```
//!
//! 観測は 2 側面 (DR-0014 の「期待 vs 実態」): 子の `ps` STAT (= kernel 側) と
//! `hyoui status` の `child-state:` (= daemon 側の認識)。
//!
//! `#[ignore]`: PTY child + signal を使うため CI では走らせずローカルで `--ignored` 実行
//! (= jobcontrol_*.rs / ctrlz_suspend_client.rs と同じ運用)。

mod common;

use std::time::Duration;

use common::pty::{HyouiTestRunner, SpawnedHyoui, find_descendants, process_state_of};
use nix::sys::signal::Signal;

/// menu を出す config (= DR-0032 §1 の 3 値のうち「起こさない」値)。
const MENU_CONFIG: &str = "[session]\non_child_suspend = \"show_child_action_menu\"\n";

/// menu の第 1 行 (= DR-0029 §1 通知行を N 行に拡張したもの)。
const MENU_HEADER: &str = "子プロセスが停止中";

/// menu が出ている状態の被験体一式。
///
/// `runner` を field で持つのは **socket を生かすため**: runner が抱える TempDir が
/// drop されると runtime dir ごと消えて daemon の socket が消滅する (= 実測で踏んだ)。
/// `let Stopped { h, .. } = ...` のような destructuring は runner を即 drop するので
/// 使わない。
struct Stopped {
    /// runtime dir (= socket 置き場) を握る runner。
    _runner: HyouiTestRunner,
    /// PTY 内の `hyoui run` (= daemon を fork した attach client)。
    h: SpawnedHyoui,
    /// 停止させた子 process の pid。
    child_pid: i32,
}

/// `run -- <cmd>` を menu config で起こし、子を SIGSTOP してから menu 表示を待つ。
fn stopped_with_menu(session: &str, child_argv: &[&str]) -> Stopped {
    let runner = HyouiTestRunner::new();
    let mut args = vec!["run", "--"];
    args.extend_from_slice(child_argv);
    let mut h = runner.spawn_hyoui_with_config(session, &args, MENU_CONFIG);
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    let attach_pid = h.pid().as_raw();
    let needle = child_argv[0].rsplit('/').next().unwrap_or(child_argv[0]);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let child_pid = loop {
        if let Ok(descendants) = find_descendants(attach_pid)
            && let Some(child) = descendants.iter().find(|p| p.comm.contains(needle))
        {
            break child.pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "子 process ({needle}) が見つからない"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    h.wait_for(MENU_HEADER, Duration::from_secs(5))
        .expect("停止検知で child action menu が描画されるはず");
    Stopped {
        _runner: runner,
        h,
        child_pid,
    }
}

/// 子が停止すると menu が出て、項目キー `c` で子が起きる (= 継続系 SIGCONT)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_appears_on_stop_and_c_resumes_child() {
    let mut s = stopped_with_menu("menu-resume", &["/bin/cat"]);
    assert!(
        process_state_of(s.child_pid)
            .expect("子の state")
            .is_state('T'),
        "menu を出す設定では子は停止したままであるべき"
    );
    assert!(
        wait_child_state(&s.h, "stopped", Duration::from_secs(5)),
        "daemon 側も stopped と認識しているはず: {:?}",
        s.h.status_text()
    );

    s.h.send_bytes(b"c").expect("menu key: c");
    assert!(
        wait_not_stopped(s.child_pid, Duration::from_secs(5)),
        "menu の `c` (SIGCONT) で子が起きるはず: {}",
        state_of(s.child_pid)
    );
    assert!(
        wait_child_state(&s.h, "running", Duration::from_secs(5)),
        "daemon 側の child-state も running に戻るはず: {:?}",
        s.h.status_text()
    );

    let _ = s.h.kill();
}

/// menu 表示中の入力は client が飲み、PTY へ forward されない (DR-0032 §2)。
///
/// 停止中に流した入力が resume 時にまとめて子へ流れ込む事故を防ぐ規定なので、
/// resume 後に子 (= `cat` の echo) がその文字列を出さないことを確認する。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_swallows_input_and_does_not_forward_to_child() {
    let mut s = stopped_with_menu("menu-swallow", &["/bin/cat"]);

    s.h.send_bytes(b"MENU-SWALLOWED\r").expect("menu 中の入力");
    std::thread::sleep(Duration::from_millis(300));

    // `c` で resume してから、forward が再開したことを別 marker で確かめる。
    s.h.send_bytes(b"c").expect("menu key: c");
    assert!(
        wait_not_stopped(s.child_pid, Duration::from_secs(5)),
        "子が resume しない: {}",
        state_of(s.child_pid)
    );
    s.h.send_bytes(b"AFTER-RESUME\r").expect("resume 後の入力");
    let after =
        s.h.wait_for("AFTER-RESUME", Duration::from_secs(5))
            .expect("resume 後の入力は子 (cat) が echo するはず");
    assert!(
        !after.contains("MENU-SWALLOWED"),
        "menu 中の入力は子へ流れてはいけない: {after:?}"
    );

    let _ = s.h.kill();
}

/// 終了系 `k` (SIGKILL) は停止中の子を実際に終わらせる (DR-0032 §2)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_sigkill_terminates_stopped_child() {
    let mut s = stopped_with_menu("menu-sigkill", &["/bin/cat"]);

    s.h.send_bytes(b"k").expect("menu key: k");
    assert!(
        wait_gone(s.child_pid, Duration::from_secs(5)),
        "SIGKILL は stopped な子にも即効くはず: {}",
        state_of(s.child_pid)
    );

    let _ = s.h.kill();
}

/// 終了系 `i` (SIGCONT + SIGINT) は停止中の子を起こしてから終わらせる (DR-0032 §2)。
///
/// SIGCONT を併送しないと signal が pending のままで何も起きない (= silent no-op) ので、
/// 「停止中に選んだ SIGINT が効く」ことが併送の実効エビデンスになる。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_sigint_wakes_and_terminates_stopped_child() {
    let mut s = stopped_with_menu("menu-sigint", &["/bin/cat"]);

    s.h.send_bytes(b"i").expect("menu key: i");
    assert!(
        wait_gone(s.child_pid, Duration::from_secs(5)),
        "SIGCONT 併送で SIGINT が配送され子が終わるはず: {}",
        state_of(s.child_pid)
    );

    let _ = s.h.kill();
}

/// 継続系 `d` (detach) は client を畳み、子は停止したまま残る (DR-0032 §2)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_detach_closes_client_and_leaves_child_stopped() {
    let mut s = stopped_with_menu("menu-detach", &["/bin/cat"]);

    s.h.send_bytes(b"d").expect("menu key: d");
    let code =
        s.h.wait_exit_code(Duration::from_secs(5))
            .expect("detach で client が終了するはず");
    assert_eq!(code, 0, "自発 detach は exit 0 (= 正常離脱)");
    assert!(
        process_state_of(s.child_pid)
            .expect("子の state")
            .is_state('T'),
        "detach 後も子は停止したまま残る (= 無人なので停止維持、DR-0030 §3)"
    );

    // 子は daemon 配下に残っているので、CLI から始末する。
    let _ = s.h.kill();
}

/// 無人時に子が止まった場合、次の rw attach 成立時 (= handshake snapshot が stopped) に
/// menu が出る (DR-0032 §2 発動条件 3 の前半 / §Consequences「attach 不在ウィンドウ」)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_appears_on_next_attach_when_child_stopped_while_unattended() {
    let runner = HyouiTestRunner::new();
    // 無人 (= detached) で起こしてから子を止める。menu を出す先が居ないので daemon は
    // notify のまま何もせず待つ。
    let mut boot = runner.spawn_hyoui_with_config(
        "menu-unattended",
        &["run", "--detached", "--", "/bin/cat"],
        MENU_CONFIG,
    );
    let child_pid = wait_for_child_pid(&runner, "menu-unattended");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), Signal::SIGSTOP)
        .expect("kill -STOP child");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !process_state_of(child_pid)
        .expect("子の state")
        .is_state('T')
    {
        assert!(std::time::Instant::now() < deadline, "子が停止しない");
        std::thread::sleep(Duration::from_millis(50));
    }

    // ここで初めて rw attach する (= handshake snapshot が stopped)。
    let mut h = runner.spawn_hyoui_with_config("menu-unattended", &["attach"], MENU_CONFIG);
    h.wait_for(MENU_HEADER, Duration::from_secs(10))
        .expect("attach 成立時に stopped なら menu が出るはず");
    assert!(
        process_state_of(child_pid)
            .expect("子の state")
            .is_state('T'),
        "menu を出す設定では attach しても子を起こさない"
    );

    let _ = h.kill();
    let _ = boot.kill();
}

/// `hyoui status` の `child-pid:` から子 pid を取る (= daemon 起動待ちも兼ねる)。
fn wait_for_child_pid(runner: &HyouiTestRunner, session: &str) -> i32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(out) = std::process::Command::new(env!("CARGO_BIN_EXE_hyoui"))
            .args([
                "status",
                &format!("--socket={}", runner.socket_path(session).display()),
            ])
            .env("XDG_RUNTIME_DIR", runner.runtime_dir())
            .env("TMPDIR", runner.runtime_dir())
            .env_remove("HYOUI_LOCK_TOKEN")
            .env_remove("HYOUI_SESSION_ID")
            .env_remove("HYOUI_NAMESPACE")
            .stdin(std::process::Stdio::null())
            .output()
            && let Some(pid) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("child-pid:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|s| s.parse::<i32>().ok())
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "daemon が起動して child-pid を返さない"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// daemon 側の `child-state:` (= `hyoui status` の 1 行) が `expected` になるまで待つ。
fn wait_child_state(h: &SpawnedHyoui, expected: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(text) = h.status_text()
            && let Some(line) = text.lines().find(|l| l.starts_with("child-state:"))
            && line.contains(expected)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 子の `ps` STAT が停止 (`T`) を抜けるまで待つ。
fn wait_not_stopped(pid: i32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if process_state_of(pid).is_ok_and(|st| !st.is_state('T')) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 子が終了する (= 消える or zombie になる) まで待つ。
fn wait_gone(pid: i32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match process_state_of(pid) {
            Err(_) => return true,
            // daemon が waitpid するまで zombie で残りうる (= 終了済み扱い)。
            Ok(st) if st.is_state('Z') => return true,
            Ok(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn state_of(pid: i32) -> String {
    process_state_of(pid).map_or_else(|_| "(gone)".to_string(), |p| p.stat)
}
