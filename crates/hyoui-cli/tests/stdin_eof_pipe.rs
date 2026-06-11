//! DR-0019 §5: pipe-through `--stdin-eof=detach|send-eof` の e2e。
//!
//! 非 tty stdin (= pipe) で `hyoui run` を起動したときの挙動を検証する:
//!
//! - default (= send-eof): `echo "1+2" | hyoui run -- bc` で bc が EOT を read EOF
//!   として解釈し、計算結果 `3` を出して自然 exit する (= pipe-through の透過性回復)
//! - `--stdin-eof=detach`: 現行挙動。EOF で client が切断するだけで bc は daemon
//!   配下に残る (= session が live のまま)
//!
//! HyouiTestRunner は PTY (= tty stdin) 経路なので使えない。pipe stdin を直接渡す
//! `std::process::Command` で起動する。
//!
//! `#[ignore]`: 子 process + 実プロセス起動を伴うため、ローカルで `--ignored` 実行。

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// ユニークな runtime dir を作り、`XDG_RUNTIME_DIR` に使う (= 他 test と socket 衝突回避)。
fn unique_runtime_dir() -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "hyoui-stdineof-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&base).expect("create runtime dir");
    base
}

/// send-eof (default): `echo "1+2" | hyoui run -- bc` で bc が 3 を出して exit する。
#[ignore = "子 process 起動 + 実時間を使う、ローカルで --ignored 実行 (DR-0019 §5)"]
#[test]
fn pipe_send_eof_default_terminates_bc() {
    let rt = unique_runtime_dir();
    let mut child = Command::new(hyoui_bin())
        .args(["run", "--session=stdineof-send", "--", "bc"])
        .env("XDG_RUNTIME_DIR", &rt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hyoui run");

    // pipe に式を書いて close (= EOF)。send-eof default なら bc に EOT が届く。
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"1+2\n").expect("write stdin");
        // drop で close → EOF。
    }

    // bc が exit すれば hyoui run も exit する。timeout 付き wait。
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(s) = child.try_wait().expect("try_wait") {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("hyoui run が send-eof default で exit しなかった (= bc が残った疑い)");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let out = child.wait_with_output().expect("wait_with_output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains('3'),
        "bc の計算結果 3 が stdout に出るはず: stdout={stdout:?} status={status:?}"
    );

    let _ = std::fs::remove_dir_all(&rt);
}

/// detach: `echo "1+2" | hyoui run --stdin-eof=detach -- bc` では EOF で client が
/// 切断するだけで bc は daemon 配下に残る (= session が live)。
#[ignore = "子 process 起動 + 実時間を使う、ローカルで --ignored 実行 (DR-0019 §5)"]
#[test]
fn pipe_detach_leaves_child_under_daemon() {
    let rt = unique_runtime_dir();
    let session = "stdineof-detach";
    // stdout/stderr は null にする。detach では daemon (= bc を抱えたまま) が live で
    // 残るので、もし test 側 pipe を daemon が継承すると `wait_with_output` が EOF を
    // 取れず永久 block する。stdout 内容は本 test では検証しない (= list で確認する)。
    let mut child = Command::new(hyoui_bin())
        .args([
            "run",
            "--session",
            session,
            "--stdin-eof=detach",
            "--",
            "bc",
        ])
        .env("XDG_RUNTIME_DIR", &rt)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyoui run");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"1+2\n").expect("write stdin");
    }

    // detach なので EOF 観測で client (= hyoui run の exec attach) は速やかに exit する。
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("detach 指定で client が exit しなかった");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // client が抜けても bc は daemon 配下で live なはず。`hyoui list` に session が残る。
    let list = Command::new(hyoui_bin())
        .args(["list"])
        .env("XDG_RUNTIME_DIR", &rt)
        .output()
        .expect("hyoui list");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains(session),
        "detach では子が daemon 配下に残り list に session が出るはず: list={list_out:?}"
    );

    // 後始末: session を kill。
    let _ = Command::new(hyoui_bin())
        .args(["kill", session, "--signal", "KILL"])
        .env("XDG_RUNTIME_DIR", &rt)
        .output();
    let _ = std::fs::remove_dir_all(&rt);
}
