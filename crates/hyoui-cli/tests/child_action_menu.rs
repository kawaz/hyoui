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

/// menu が出ている状態の被験体一式 (= runner を握って test 終了まで生かす)。
struct Stopped {
    /// runtime dir を握る runner (= drop で TempDir が消えるので保持が必要)。
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
    let Stopped {
        mut h, child_pid, ..
    } = stopped_with_menu("menu-resume", &["/bin/cat"]);

    assert!(
        process_state_of(child_pid)
            .expect("子の state")
            .is_state('T'),
        "menu を出す設定では子は停止したままであるべき"
    );

    h.send_bytes(b"c").expect("menu key: c");
    let resumed = {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let st = process_state_of(child_pid).expect("子の state");
            if !st.is_state('T') {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert!(resumed, "menu の `c` (SIGCONT) で子が起きるはず");

    let _ = h.kill();
}

/// menu 表示中の入力は client が飲み、PTY へ forward されない (DR-0032 §2)。
///
/// 停止中に流した入力が resume 時にまとめて子へ流れ込む事故を防ぐ規定なので、
/// resume 後に子 (= `cat` の echo) がその文字列を出さないことを確認する。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_swallows_input_and_does_not_forward_to_child() {
    let Stopped {
        mut h, child_pid, ..
    } = stopped_with_menu("menu-swallow", &["/bin/cat"]);

    // 表に無いキーだけで構成した marker (= 項目キー c/z/d/i/h/k/q を含まない)。
    h.send_bytes(b"MENU-SWALLOWED\r").expect("menu 中の入力");
    std::thread::sleep(Duration::from_millis(300));

    // `c` で resume してから、forward が再開したことを別 marker で確かめる。
    h.send_bytes(b"c").expect("menu key: c");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_state_of(child_pid)
        .expect("子の state")
        .is_state('T')
    {
        assert!(std::time::Instant::now() < deadline, "子が resume しない");
        std::thread::sleep(Duration::from_millis(50));
    }
    h.send_bytes(b"AFTER-RESUME\r").expect("resume 後の入力");
    let after = h
        .wait_for("AFTER-RESUME", Duration::from_secs(5))
        .expect("resume 後の入力は子 (cat) が echo するはず");
    assert!(
        !after.contains("MENU-SWALLOWED"),
        "menu 中の入力は子へ流れてはいけない: {after:?}"
    );

    let _ = h.kill();
}

/// 終了系 `k` (SIGKILL) は停止中の子を実際に終わらせる (DR-0032 §2)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_sigkill_terminates_stopped_child() {
    let Stopped {
        mut h, child_pid, ..
    } = stopped_with_menu("menu-sigkill", &["/bin/cat"]);

    h.send_bytes(b"k").expect("menu key: k");
    assert!(
        wait_gone(child_pid, Duration::from_secs(5)),
        "SIGKILL は stopped な子にも即効くはず: {}",
        state_of(child_pid)
    );

    let _ = h.kill();
}

/// 終了系 `i` (SIGCONT + SIGINT) は停止中の子を起こしてから終わらせる (DR-0032 §2)。
///
/// SIGCONT を併送しないと signal が pending のままで何も起きない (= silent no-op) ので、
/// 「停止中に選んだ SIGINT が効く」ことが併送の実効エビデンスになる。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_sigint_wakes_and_terminates_stopped_child() {
    let Stopped {
        mut h, child_pid, ..
    } = stopped_with_menu("menu-sigint", &["/bin/cat"]);

    h.send_bytes(b"i").expect("menu key: i");
    assert!(
        wait_gone(child_pid, Duration::from_secs(5)),
        "SIGCONT 併送で SIGINT が配送され子が終わるはず: {}",
        state_of(child_pid)
    );

    let _ = h.kill();
}

/// 継続系 `d` (detach) は client を畳み、子は停止したまま残る (DR-0032 §2)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行 (DR-0032 §2)"]
#[test]
fn menu_detach_closes_client_and_leaves_child_stopped() {
    let Stopped {
        mut h, child_pid, ..
    } = stopped_with_menu("menu-detach", &["/bin/cat"]);

    h.send_bytes(b"d").expect("menu key: d");
    let code = h
        .wait_exit_code(Duration::from_secs(5))
        .expect("detach で client が終了するはず");
    assert_eq!(code, 0, "自発 detach は exit 0 (= 正常離脱)");
    assert!(
        process_state_of(child_pid)
            .expect("子の state")
            .is_state('T'),
        "detach 後も子は停止したまま残る (= 無人なので停止維持、DR-0030 §3)"
    );

    // 子は daemon 配下に残っているので、CLI から始末する。
    let _ = h.kill();
}

fn wait_gone(pid: i32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match process_state_of(pid) {
            Err(_) => return true,
            // 子は daemon が waitpid するまで zombie で残りうる (= 終了済み扱い)。
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
