//! issue 2026-06-11 優先2: daemon SIGCONT → stopped child 連動起こしの検証。
//!
//! 防衛策 (session.rs handle_suspend_signals): 「daemon が SIGCONT で再開した時、
//! child が STOPPED なら killpg(child, SIGCONT)」が実機で発火することを確認する。
//!
//! root cause だった「自前 waitpid(WUNTRACED) は SIGCHLD 経由で既に Stopped
//! transition が消費済のため StillAlive を返し、起こしが不発」を、latch 済の
//! `ChildLifecycle::is_stopped()` 参照に直して修正した。
//!
//! 経路:
//! ```
//! 子 PTY 内 self-stop (= sh -c 'kill -STOP $$; echo RESUMED')
//!   → daemon が SIGCHLD で stopped を観測・latch (notify policy なので起こさない)
//!   → daemon process に外部から kill -CONT
//!   → daemon の SIGCONT handler → handle_suspend_signals が is_stopped() を見て
//!     killpg(child, SIGCONT)
//!   → 子が復帰して RESUMED を出力
//! ```

mod common;

use std::time::Duration;

use common::pty::HyouiTestRunner;

/// self-stop してから marker を出力する子。
const SELF_STOP_THEN_ECHO: &[&str] = &["sh", "-c", "kill -STOP $$; echo RESUMED_BY_DAEMON_CONT"];

/// daemon に kill -CONT を送ると、latch 済の stopped child を killpg(SIGCONT) で
/// 起こす (= 修正後は発火する)。default(notify) policy なので daemon は self-stop
/// 観測時には起こさず、外部 CONT を契機に初めて起こす。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行"]
#[test]
fn daemon_sigcont_wakes_stopped_child() {
    let runner = HyouiTestRunner::new();
    // DR-0030 の default では rw attach が stopped child を即 resume するため、外部
    // daemon SIGCONT を送る前に子が exit してしまう。この test が保証する daemon 側の
    // SIGCONT 防衛策だけを観測するため、runner 固有 config で attach 側 resume を止める。
    // rw leader 接続は維持するので、daemon/attach の構造自体は通常の `hyoui run` と同じ。
    let mut h = runner.spawn_hyoui_with_config(
        "jobcontrol-daemon-cont",
        &[
            "run",
            "--",
            SELF_STOP_THEN_ECHO[0],
            SELF_STOP_THEN_ECHO[1],
            SELF_STOP_THEN_ECHO[2],
        ],
        "[attach]\nresume_stopped_child = false\n",
    );

    // leader 接続が成立するまで待つ (= daemon が稼働し socket 確立)。
    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    // 子が self-stop して daemon が stopped を latch するまで少し待つ。
    // notify policy では起こされないので RESUMED はまだ出ない。
    std::thread::sleep(Duration::from_millis(500));
    let early = String::from_utf8_lossy(&h.drain_output()).to_string();
    assert!(
        !early.contains("RESUMED_BY_DAEMON_CONT"),
        "外部 CONT 前に marker が出てはいけない (= notify policy で起こさない): {early:?}"
    );

    // daemon process に kill -CONT を送る。
    let daemon = h.daemon_pid().expect("daemon process が見つからない");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGCONT,
    )
    .expect("kill -CONT daemon");

    // daemon の防衛策が latch 済 stopped child を killpg(SIGCONT) で起こす →
    // 子が復帰して marker を出力する。
    let out = h
        .wait_for("RESUMED_BY_DAEMON_CONT", Duration::from_secs(8))
        .expect("daemon CONT で stopped child が起きて marker を出すはず");
    assert!(
        out.contains("RESUMED_BY_DAEMON_CONT"),
        "daemon CONT 後の出力に marker が含まれない: {out:?}"
    );

    let _ = h.kill();
}

/// child が回り続ける子 (= 外部から SIGSTOP/SIGCONT で止め・再開する観測用)。
const ECHO_LOOP: &[&str] = &[
    "sh",
    "-c",
    "i=0; while :; do echo TICK_$i; i=$((i+1)); sleep 0.2; done",
];

/// M2 (同一 drain batch ケース): 「daemon を STOP → child を STOP → daemon に CONT」
/// の順序で、CONT 時に SIGCHLD(child STOP) と SIGCONT(daemon) が同一 drain batch に
/// 入る。`handle_suspend_signals` は `poll_with_transition` より先に走るため
/// `is_stopped()` latch がまだ立っておらず、latch 参照だけでは起こしが不発する。
/// 直接 waitpid フォールバックで child を起こせることを確認する。
///
/// 検証は child の `ps` stat を直接観測する (= 出力ストリームの "TICK_" は screen
/// state の redraw や残バッファでも再出現しうるため、stat 観測のほうが厳密。
/// 実機確認: フォールバック無しだと CONT 後も child は `T` (stopped) のまま、
/// フォールバック有りだと `S`/`R` に復帰する)。
#[ignore = "PTY child + signal を使う、ローカルで --ignored 実行"]
#[test]
fn daemon_cont_wakes_child_stopped_during_daemon_stop() {
    use common::pty::{find_descendants, process_state_of};

    let runner = HyouiTestRunner::new();
    let mut h = runner.spawn_hyoui(
        "jobcontrol-batch-cont",
        &["run", "--", ECHO_LOOP[0], ECHO_LOOP[1], ECHO_LOOP[2]],
    );

    assert!(
        h.wait_for_leader_ready(Duration::from_secs(10)),
        "attach client が leader として daemon に接続しない"
    );

    // 子が回り始める (= TICK が出る) のを待つ。
    h.wait_for("TICK_", Duration::from_secs(8))
        .expect("子が動き始めて TICK を出すはず");

    let daemon = h.daemon_pid().expect("daemon process が見つからない");
    // 子孫から実際に echo loop を回している sh process を特定する。
    let descendants = find_descendants(daemon).expect("子孫プロセス列挙");
    let child = descendants
        .iter()
        .find(|p| p.comm.contains("sh"))
        .map(|p| p.pid)
        .or_else(|| descendants.first().map(|p| p.pid))
        .expect("daemon の子孫 (= PTY child) が見つからない");
    let daemon_pid = nix::unistd::Pid::from_raw(daemon);
    let child_pid = nix::unistd::Pid::from_raw(child);

    // 1. daemon を STOP (= daemon process が止まり SIGCHLD を drain できなくなる)。
    nix::sys::signal::kill(daemon_pid, nix::sys::signal::Signal::SIGSTOP)
        .expect("kill -STOP daemon");
    std::thread::sleep(Duration::from_millis(300));

    // 2. daemon STOP 中に child を STOP (= child STOP の SIGCHLD が pending のまま溜まる)。
    nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGSTOP).expect("kill -STOP child");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        process_state_of(child).expect("child stat").is_state('T'),
        "前提: child STOP 後は stopped (T) であるべき"
    );

    // 3. daemon に CONT (= SIGCHLD(child STOP) と SIGCONT(daemon) を同一 batch で drain)。
    nix::sys::signal::kill(daemon_pid, nix::sys::signal::Signal::SIGCONT)
        .expect("kill -CONT daemon");

    // 4. daemon が child を SIGCONT で起こすこと (= フォールバックが効いた証跡)。
    //    最大 5 秒、child が stopped (T) でなくなるまで poll する。
    let mut woke = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match process_state_of(child) {
            Ok(st) if !st.is_state('T') => {
                woke = true;
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    assert!(
        woke,
        "daemon CONT 後に child が起きていない (= 同一 batch フォールバック不発)。\
         child stat={:?}",
        process_state_of(child).map(|s| s.stat).ok()
    );

    let _ = h.kill();
}
