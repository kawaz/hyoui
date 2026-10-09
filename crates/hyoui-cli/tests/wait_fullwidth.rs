//! DR-0006 §9.1 e2e: 全角文字の連続に `hyoui wait` / `hyoui input` の `wait:` spec が一致する。
//!
//! 全角文字は画面上 2 列を占め、daemon の screen state では「先頭 cell + 継続 cell」になる。
//! wait の照合 text も `screen dump` の text と同じく継続 cell を出さず、端末で連続して
//! 見える文字列がそのまま連続する。実 daemon の snapshot (sparse cells) を経由する経路を
//! 実バイナリで確かめる。

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::session_dir::SessionDir;

fn hyoui(root: &Path, args: &[&str]) -> Output {
    // daemon は stderr を継承して常駐するので、`--detached` を pipe で待つと daemon が
    // 終わるまで EOF が来ない。stderr は file に逃がしてから読む。
    let err = root.join("stderr.log");
    Command::new(PathBuf::from(env!("CARGO_BIN_EXE_hyoui")))
        .args(args)
        .env("HYOUI_STATE_DIR", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("HYOUI_SESSION_ID")
        .env_remove("HYOUI_LOCK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(err).expect("stderr file"))
        .output()
        .expect("spawn hyoui")
}

#[test]
fn wait_matches_consecutive_fullwidth_text() {
    let dir = SessionDir::new("hyoui-wait-fw-");
    let root = dir.path();
    let boot = hyoui(
        root,
        &[
            "run",
            "--detached",
            "--pty-stdin",
            "--",
            "sh",
            "-c",
            "printf '[hyoui] 子プロセスが停止中\\n'; exec cat",
        ],
    );
    assert!(boot.status.success(), "run --detached");
    let session = String::from_utf8_lossy(&boot.stdout).trim().to_string();

    let dump = hyoui(root, &["screen", "dump", &session, "--format=text"]);
    let waited = hyoui(root, &["wait", &session, "停止中", "--timeout=10s"]);
    assert!(
        waited.status.success(),
        "wait '停止中' は連続した全角文字に一致するはず (rc={:?}, dump={:?})",
        waited.status.code(),
        String::from_utf8_lossy(&dump.stdout)
    );
    // 半角と全角の境目も連続して見える (= `] 子` の間は空白 1 個だけ)。
    let waited = hyoui(root, &["wait", &session, r"\] 子プロセス", "--timeout=5s"]);
    assert!(
        waited.status.success(),
        "半角の後の全角文字列にも一致するはず"
    );

    let input = hyoui(root, &["input", &session, "wait:停止中", "--timeout=5s"]);
    assert!(
        input.status.success(),
        "input の wait: spec も同じ照合で一致するはず (rc={:?})",
        input.status.code()
    );
}
