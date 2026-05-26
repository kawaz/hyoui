//! PoC 06: lock token の環境変数継承
//!
//! tx の子 process に `HYOUI_LOCK_TOKEN` env を注入、子 (および子の子) から std::env::var で
//! pick up できるかを確認。Unix の env inheritance (execve で envp が渡る) の素直な利用。
//!
//! 実行:
//!   cargo run --example 06-lock-token-env

use std::process::Command;

fn generate_token() -> String {
    // 簡易ランダム (= microsecond + pid)、本実装では cryptographic random + base64 等
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros();
    format!("tok_{}_{}", std::process::id(), now)
}

fn main() {
    let token = generate_token();
    eprintln!("[parent] generated token: {token}");

    // Case 1: 直接子で env を読む
    let out1 = Command::new("sh")
        .args(["-c", r#"echo "env_token=$HYOUI_LOCK_TOKEN""#])
        .env("HYOUI_LOCK_TOKEN", &token)
        .output()
        .expect("spawn child 1");
    let s1 = String::from_utf8_lossy(&out1.stdout).trim().to_string();
    eprintln!("[parent] child 1 stdout: {s1:?}");
    let case1 = s1 == format!("env_token={token}");

    // Case 2: 子の子 (= grandchild) でも env が継承される
    let out2 = Command::new("sh")
        .args([
            "-c",
            // 子: 自分も env 持ってる + 孫に渡す
            r#"echo "child=$HYOUI_LOCK_TOKEN"; sh -c 'echo "grandchild=$HYOUI_LOCK_TOKEN"'"#,
        ])
        .env("HYOUI_LOCK_TOKEN", &token)
        .output()
        .expect("spawn child 2");
    let s2 = String::from_utf8_lossy(&out2.stdout);
    eprintln!("[parent] child 2 stdout: {s2:?}");
    let case2 =
        s2.contains(&format!("child={token}")) && s2.contains(&format!("grandchild={token}"));

    // Case 3: env を明示しない子は env を持たない (= env クリア相当)
    let out3 = Command::new("sh")
        .args(["-c", r#"echo "no_env=${HYOUI_LOCK_TOKEN:-EMPTY}""#])
        .env_clear()
        // sh が動くために最低限の env (PATH/HOME など)
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        )
        .output()
        .expect("spawn child 3");
    let s3 = String::from_utf8_lossy(&out3.stdout).trim().to_string();
    eprintln!("[parent] child 3 stdout: {s3:?}");
    let case3 = s3 == "no_env=EMPTY";

    // Case 4: rust 側で env から取る (= 本実装の hyoui send 等が想定する取り方)
    //   親プロセス自身に env をセットして std::env::var で取る
    // SAFETY: env var を書く前に他 thread が env を参照していないこと。
    // PoC は single-thread なので安全。
    unsafe {
        std::env::set_var("HYOUI_LOCK_TOKEN_TEST", &token);
    }
    let case4 = std::env::var("HYOUI_LOCK_TOKEN_TEST").as_deref() == Ok(token.as_str());
    eprintln!("[parent] case4 (std::env::var): {case4}");

    eprintln!(
        "[parent] cases: child_env={case1}, grandchild_env={case2}, env_clear={case3}, std_env_var={case4}"
    );

    if case1 && case2 && case3 && case4 {
        eprintln!("[parent] PASS");
    } else {
        eprintln!("[parent] FAIL");
        std::process::exit(1);
    }
}
