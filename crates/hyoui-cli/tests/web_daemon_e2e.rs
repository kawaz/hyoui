//! `hyoui web daemon` CLI boundary の E2E (= DR-0034 P2)。
//!
//! 登録簿は `XDG_STATE_HOME` 配下なので、隔離した state dir を渡せば実機の
//! 登録を触らずに add / list / remove を通せる。`daemon run` の bind 観測だけは
//! 実 port を掴むので、他と衝突しない port を使い、必ず子を落とす。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
            "stdout was not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn units_dir(home: &Path) -> std::path::PathBuf {
    home.join(".local/state/hyoui-web/units")
}

/// add は解決済みの値を登録簿に書き、list が同じ値を読み返す。
#[test]
fn add_writes_resolved_values_and_list_reads_them_back() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(
        &["web", "daemon", "add", "unstable", "--port=43991"],
        home.path(),
    );
    assert!(
        output.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let added = json(&output);
    assert_eq!(added["name"], "unstable");
    assert_eq!(added["listen"], "127.0.0.1:43991");
    assert_eq!(added["enabled"], true);
    // `--binary` 未指定なら自分自身 (= repo build から add すればその build)。
    assert_eq!(added["binary"], env!("CARGO_BIN_EXE_hyoui"));
    assert_eq!(added["binary_exists"], true);
    // 監督者が居ないので起動はしておらず、その理由が出力に残る (決定 4)。
    assert_eq!(added["supervisor"]["running"], false);
    assert_eq!(added["supervisor"]["notified"], false);
    assert!(
        added["note"]
            .as_str()
            .unwrap_or_default()
            .contains("not running"),
        "{added}"
    );

    // 登録簿は 1 unit 1 ファイルで、tmp の残骸を残さない。
    let files: Vec<_> = std::fs::read_dir(units_dir(home.path()))
        .expect("units dir")
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, [std::ffi::OsString::from("unstable.toml")]);

    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(listed["units"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed["units"][0]["name"], "unstable");
    assert_eq!(listed["units"][0]["listen"], "127.0.0.1:43991");
    // 監督者に聞けないものは推し量らない。
    assert_eq!(listed["units"][0]["running"], false);
    assert_eq!(listed["units"][0]["pid"], serde_json::Value::Null);
    assert_eq!(listed["supervisor"]["running"], false);
    assert!(listed["note"].is_string(), "{listed}");
}

/// `list` は登録簿が空でも、監督者が居なくても成功する (= 障害時に最初に打つ口)。
#[test]
fn list_answers_on_an_empty_registry() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(&["web", "daemon", "list"], home.path());
    assert!(output.status.success());
    assert_eq!(json(&output)["units"], serde_json::json!([]));
}

/// 同じ宛先の二重登録と、不正な unit 名は JSON エラーで断る。
#[test]
fn conflicting_addresses_and_bad_names_are_refused() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert!(
        hyoui(
            &["web", "daemon", "add", "stable", "--listen=127.0.0.1:43992"],
            home.path()
        )
        .status
        .success()
    );

    // 綴りが違っても同じ宛先なら拒否し、どの unit が持っているかを示す。
    let output = hyoui(
        &["web", "daemon", "add", "second", "--listen=localhost:43992"],
        home.path(),
    );
    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("error JSON on stderr");
    assert_eq!(error["conflicting_unit"], "stable");
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("already"),
        "{error}"
    );

    // 同じ名前の二重登録も断る。
    let output = hyoui(
        &["web", "daemon", "add", "stable", "--port=43993"],
        home.path(),
    );
    assert!(!output.status.success());

    for bad in ["with space", "../escape", "dot.name"] {
        let output = hyoui(&["web", "daemon", "add", bad, "--port=43994"], home.path());
        assert!(!output.status.success(), "accepted `{bad}`");
        let error: serde_json::Value =
            serde_json::from_slice(&output.stderr).expect("error JSON on stderr");
        assert!(
            error["error"]
                .as_str()
                .unwrap_or_default()
                .contains("not a valid unit name"),
            "`{bad}`: {error}"
        );
    }

    // 拒否された登録は登録簿に残らない。
    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(listed["units"].as_array().map(Vec::len), Some(1));
}

/// remove は登録簿から消し、未登録の名前は断る。
#[test]
fn remove_drops_the_registration() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert!(
        hyoui(
            &["web", "daemon", "add", "stable", "--port=43995"],
            home.path()
        )
        .status
        .success()
    );

    let removed = json(&hyoui(&["web", "daemon", "remove", "stable"], home.path()));
    assert_eq!(removed["removed"], true);
    assert!(!units_dir(home.path()).join("stable.toml").exists());

    let output = hyoui(&["web", "daemon", "remove", "stable"], home.path());
    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("error JSON on stderr");
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no web gateway unit"),
        "{error}"
    );
}

/// 登録簿の root は session discovery の走査 base と分かれている (= 決定 2)。
///
/// `${XDG_STATE_HOME}/hyoui/` のサブ dir は namespace として扱われるので、そこに
/// gateway の状態を置くと `hyoui list` に session として並んでしまう。
#[test]
fn registry_units_do_not_appear_as_sessions() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert!(
        hyoui(
            &["web", "daemon", "add", "stable", "--port=43996"],
            home.path()
        )
        .status
        .success()
    );

    let output = hyoui(&["list"], home.path());
    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("hyoui-web"), "{stdout}");
    assert!(!stdout.contains("stable"), "{stdout}");
    assert!(!stdout.contains("43996"), "{stdout}");

    // 登録簿は session の木の外にある。
    assert!(units_dir(home.path()).join("stable.toml").exists());
    assert!(!home.path().join(".local/state/hyoui").exists());
}

/// help は親と各 leaf で別の surface を出す。
#[test]
fn help_routes_through_web_daemon_tree() {
    let home = tempfile::tempdir().expect("isolated HOME");
    for (args, needle) in [
        (&["web", "daemon"][..], "run"),
        (&["web", "daemon", "add"][..], "--port"),
        (&["web", "daemon", "remove"][..], "registration"),
        (&["web", "daemon", "run", "--help"][..], "foreground"),
        (&["web", "daemon", "list", "--help"][..], "supervisor"),
    ] {
        let output = hyoui(args, home.path());
        assert!(
            output.status.success(),
            "args={args:?}, stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(needle), "args={args:?}, stdout={stdout}");
    }
}

/// `daemon run <name>` は登録簿の listen で実際に bind する (= 決定 3)。
///
/// 監督者が子を exec するのと同じ経路なので、ここが登録簿を読めていなければ
/// P3 の監督者も正しい待ち先に上げられない。
#[test]
fn run_binds_the_listen_address_from_the_registry() {
    let home = tempfile::tempdir().expect("isolated HOME");
    // 他の test と衝突しない port を 1 つ選ぶ。
    let listen = "127.0.0.1:43997";
    assert!(
        hyoui(
            &["web", "daemon", "add", "unstable", "--listen", listen],
            home.path()
        )
        .status
        .success()
    );

    let mut child = command(&["web", "daemon", "run", "unstable"], home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gateway");

    // gateway は bind 後に listen 先を 1 行書く (`hyoui web: listening on ...`)。
    // その行が来た時点で bind 済みと分かるので、待ち時間を秒で見積もらずに済む。
    let mut reader = BufReader::new(child.stderr.take().expect("piped stderr"));
    let mut banner = String::new();
    let read = reader.read_line(&mut banner);

    let probe = if read.is_ok() && banner.contains(listen) {
        http_get(listen, "/healthz")
    } else {
        Err(format!("gateway did not bind {listen}"))
    };

    let _ = child.kill();
    let status = child.wait();

    assert!(
        banner.contains(listen),
        "gateway did not report the unit's listen address: banner={banner:?}, status={status:?}"
    );
    let response = probe.unwrap_or_else(|error| panic!("{error} (banner={banner:?})"));
    assert!(response.contains("200"), "{response}");
    assert!(response.trim_end().ends_with("ok"), "{response}");
}

/// 素の TCP で 1 リクエストだけ投げる (= HTTP client 依存を足さない)。
fn http_get(listen: &str, path: &str) -> Result<String, String> {
    let mut stream = std::net::TcpStream::connect(listen)
        .map_err(|error| format!("could not connect to {listen}: {error}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {listen}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| format!("could not send the request: {error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("could not read the response: {error}"))?;
    Ok(response)
}
