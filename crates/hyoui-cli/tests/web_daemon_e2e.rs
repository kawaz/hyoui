//! `hyoui web daemon` CLI boundary の E2E (= DR-0034 P2 / DR-0038)。
//!
//! 登録簿は `XDG_STATE_HOME` 配下、unit の config は `XDG_CONFIG_HOME` 配下なので、
//! 隔離した HOME を渡せば実機の登録を触らずに add / list / remove を通せる。`daemon run` の bind 観測だけは
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
    home.join(".local/state/hyoui/web/units")
}

/// web の config の既定の置き場に `[web]` を書き、その path を返す。
fn write_config(home: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let dir = home.join(".config/hyoui/web");
    std::fs::create_dir_all(&dir).expect("config dir");
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, body).expect("write config");
    path
}

fn listen_config(listen: &str) -> String {
    format!("[web]\nlisten = \"{listen}\"\n")
}

fn error_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr was not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// add は config の path だけを登録簿に書き、listen は config から読み返す
/// (DR-0038 決定 2)。名前は basename から拡張子を除いたもの。
#[test]
fn add_records_the_config_path_and_list_reads_listen_from_it() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let config = write_config(home.path(), "unstable", &listen_config("127.0.0.1:43991"));
    let output = hyoui(
        &["web", "daemon", "add", config.to_str().unwrap()],
        home.path(),
    );
    assert!(
        output.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let added = json(&output);
    assert_eq!(added["name"], "unstable");
    assert_eq!(added["config"], config.to_str().unwrap());
    assert_eq!(added["listen"], "127.0.0.1:43991");
    assert_eq!(added["enabled"], true);
    // config に `binary_path` が無ければ自分自身 (= repo build から add すればその build)。
    assert_eq!(added["binary_path"], env!("CARGO_BIN_EXE_hyoui"));
    assert_eq!(added["binary_exists"], true);
    // 監督者が居ないので起動はしておらず、その理由が出力に残る (DR-0034 決定 4)。
    assert_eq!(added["supervisor"]["running"], false);
    assert_eq!(added["supervisor"]["notified"], false);
    assert!(
        added["note"]
            .as_str()
            .unwrap_or_default()
            .contains("not running"),
        "{added}"
    );

    // 登録簿は 1 unit 1 ファイルで、tmp の残骸を残さない。中身は参照だけ。
    let files: Vec<_> = std::fs::read_dir(units_dir(home.path()))
        .expect("units dir")
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, [std::ffi::OsString::from("unstable.toml")]);
    let registered =
        std::fs::read_to_string(units_dir(home.path()).join("unstable.toml")).expect("unit file");
    assert!(!registered.contains("listen"), "{registered}");
    assert!(!registered.contains("43991"), "{registered}");

    // config を書き換えると、登録し直さずに list の listen が追従する。
    write_config(home.path(), "unstable", &listen_config("127.0.0.1:43981"));
    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(listed["units"].as_array().map(Vec::len), Some(1));
    assert_eq!(listed["units"][0]["name"], "unstable");
    assert_eq!(listed["units"][0]["config"], config.to_str().unwrap());
    assert_eq!(listed["units"][0]["listen"], "127.0.0.1:43981");
    // 監督者に聞けないものは推し量らない。
    assert_eq!(listed["units"][0]["running"], false);
    assert_eq!(listed["units"][0]["pid"], serde_json::Value::Null);
    assert_eq!(listed["supervisor"]["running"], false);
    assert!(listed["note"].is_string(), "{listed}");

    // config が消えれば listen は分からないと言う (= 推し量らない)。
    std::fs::remove_file(&config).unwrap();
    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(listed["units"][0]["listen"], serde_json::Value::Null);
    assert!(listed["units"][0]["config_error"].is_string(), "{listed}");
}

/// `--name` で名前を与え、config の `binary_path` は登録簿に写る。相対 path は
/// cwd から絶対 path にして書く。
#[test]
fn add_takes_a_name_and_the_binary_path_from_the_config() {
    let home = tempfile::tempdir().expect("isolated HOME");
    write_config(
        home.path(),
        "base",
        "[web]\nbinary_path = \"/opt/homebrew/bin/hyoui\"\n",
    );
    write_config(
        home.path(),
        "stable",
        "extends = \"base.toml\"\n[web]\nlisten = \"127.0.0.1:43982\"\n",
    );
    let output = command(
        &["web", "daemon", "add", "--name", "brew", "stable.toml"],
        home.path(),
    )
    .current_dir(home.path().join(".config/hyoui/web"))
    .output()
    .expect("spawn hyoui");
    assert!(
        output.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let added = json(&output);
    assert_eq!(added["name"], "brew");
    assert_eq!(added["listen"], "127.0.0.1:43982");
    assert_eq!(added["binary_path"], "/opt/homebrew/bin/hyoui");
    let config = added["config"].as_str().expect("config");
    assert!(Path::new(config).is_absolute(), "{config}");
    assert!(config.ends_with("hyoui/web/stable.toml"), "{config}");
}

/// `list` は登録簿が空でも、監督者が居なくても成功する (= 障害時に最初に打つ口)。
#[test]
fn list_answers_on_an_empty_registry() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(&["web", "daemon", "list"], home.path());
    assert!(output.status.success());
    assert_eq!(json(&output)["units"], serde_json::json!([]));
}

/// 同じ宛先の二重登録、読めない config、不正な unit 名は JSON エラーで断る。
#[test]
fn conflicting_addresses_bad_configs_and_bad_names_are_refused() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let stable = write_config(home.path(), "stable", &listen_config("127.0.0.1:43992"));
    assert!(
        hyoui(
            &["web", "daemon", "add", stable.to_str().unwrap()],
            home.path()
        )
        .status
        .success()
    );

    // 綴りが違っても同じ宛先なら拒否し、どの unit が持っているかを示す。
    let second = write_config(home.path(), "second", &listen_config("localhost:43992"));
    let output = hyoui(
        &["web", "daemon", "add", second.to_str().unwrap()],
        home.path(),
    );
    assert!(!output.status.success());
    let error = error_json(&output);
    assert_eq!(error["conflicting_unit"], "stable");
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("already"),
        "{error}"
    );

    // 同じ名前の二重登録も断る。
    let other = write_config(home.path(), "other", &listen_config("127.0.0.1:43993"));
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "--name=stable",
            other.to_str().unwrap(),
        ],
        home.path(),
    );
    assert!(!output.status.success());

    // 無い config / 壊れた config は登録しない (= 監督者が起こすたびに落ちる unit を作らない)。
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            home.path().join("absent.toml").to_str().unwrap(),
        ],
        home.path(),
    );
    assert!(!output.status.success());
    assert!(
        error_json(&output)["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not found"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let broken = write_config(home.path(), "broken", "extends = \"nowhere.toml\"\n");
    let output = hyoui(
        &["web", "daemon", "add", broken.to_str().unwrap()],
        home.path(),
    );
    assert!(!output.status.success());
    assert!(
        error_json(&output)["error"]
            .as_str()
            .unwrap_or_default()
            .contains("nowhere.toml"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    for bad in ["with space", "../escape", "dot.name"] {
        let output = hyoui(
            &[
                "web",
                "daemon",
                "add",
                "--name",
                bad,
                other.to_str().unwrap(),
            ],
            home.path(),
        );
        assert!(!output.status.success(), "accepted `{bad}`");
        assert!(
            error_json(&output)["error"]
                .as_str()
                .unwrap_or_default()
                .contains("not a valid unit name"),
            "`{bad}`"
        );
    }

    // 拒否された登録は登録簿に残らない。
    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(listed["units"].as_array().map(Vec::len), Some(1));
}

/// remove は登録簿から消し、未登録の名前は断る。config ファイル自体は消さない。
#[test]
fn remove_drops_the_registration() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let config = write_config(home.path(), "stable", &listen_config("127.0.0.1:43995"));
    assert!(
        hyoui(
            &["web", "daemon", "add", config.to_str().unwrap()],
            home.path()
        )
        .status
        .success()
    );

    let removed = json(&hyoui(&["web", "daemon", "remove", "stable"], home.path()));
    assert_eq!(removed["removed"], true);
    assert!(!units_dir(home.path()).join("stable.toml").exists());
    assert!(
        config.exists(),
        "the user's config file is not ours to delete"
    );

    let output = hyoui(&["web", "daemon", "remove", "stable"], home.path());
    assert!(!output.status.success());
    assert!(
        error_json(&output)["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no web gateway unit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// web の状態は `hyoui/web/` に置き、`hyoui-web` という dir は作らない (DR-0038 決定 4)。
#[test]
fn web_state_lives_under_hyoui_web() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let config = write_config(home.path(), "stable", &listen_config("127.0.0.1:43996"));
    assert!(
        hyoui(
            &["web", "daemon", "add", config.to_str().unwrap()],
            home.path()
        )
        .status
        .success()
    );
    assert!(units_dir(home.path()).join("stable.toml").exists());
    assert!(!home.path().join(".local/state/hyoui-web").exists());
    assert!(!home.path().join(".config/hyoui-web").exists());
}

/// help は親と各 leaf で別の surface を出す。
#[test]
fn help_routes_through_web_daemon_tree() {
    let home = tempfile::tempdir().expect("isolated HOME");
    for (args, needle) in [
        (&["web"][..], "hyoui web daemon run"),
        (&["web", "daemon"][..], "run"),
        (&["web", "daemon", "add"][..], "--name"),
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

    // gateway を起動する 2 本目の口は無い (DR-0038 決定 2)。
    let output = hyoui(&["web", "--listen=127.0.0.1:43998"], home.path());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("hyoui web daemon run"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// gateway を spawn し、bind を告げる 1 行が来たら `/healthz` を叩いて結果を返す。
fn run_and_probe(args: &[&str], home: &Path, listen: &str) -> (String, Result<String, String>) {
    let mut child = command(args, home)
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
    let _ = child.wait();
    (banner, probe)
}

/// `daemon run <name>` は登録簿が指す config の listen で実際に bind する (DR-0038 決定 2)。
///
/// 監督者が子を exec するのと同じ経路なので、ここが config を読めていなければ
/// 監督者も正しい待ち先に上げられない。
#[test]
fn run_binds_the_listen_address_from_the_unit_config() {
    let home = tempfile::tempdir().expect("isolated HOME");
    // 他の test と衝突しない port を 1 つ選ぶ。
    let listen = "127.0.0.1:43997";
    let config = write_config(home.path(), "unstable", &listen_config(listen));
    assert!(
        hyoui(
            &["web", "daemon", "add", config.to_str().unwrap()],
            home.path()
        )
        .status
        .success()
    );

    let (banner, probe) = run_and_probe(&["web", "daemon", "run", "unstable"], home.path(), listen);
    assert!(
        banner.contains(listen),
        "gateway did not report the unit's listen address: banner={banner:?}"
    );
    let response = probe.unwrap_or_else(|error| panic!("{error} (banner={banner:?})"));
    assert!(response.contains("200"), "{response}");
    assert!(response.trim_end().ends_with("ok"), "{response}");
}

/// 名前を省いた `daemon run` は登録簿を見ず、既定の置き場の `config.toml` を読む。
#[test]
fn run_without_a_name_reads_the_default_config() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let listen = "127.0.0.1:43999";
    write_config(home.path(), "config", &listen_config(listen));
    // 登録簿に別の unit が居ても、名前なしの run はそれを選ばない。
    let other = write_config(home.path(), "other", &listen_config("127.0.0.1:43980"));
    assert!(
        hyoui(
            &["web", "daemon", "add", other.to_str().unwrap()],
            home.path()
        )
        .status
        .success()
    );

    let (banner, probe) = run_and_probe(&["web", "daemon", "run"], home.path(), listen);
    assert!(banner.contains(listen), "banner={banner:?}");
    assert!(
        probe
            .unwrap_or_else(|error| panic!("{error} (banner={banner:?})"))
            .contains("200")
    );
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
