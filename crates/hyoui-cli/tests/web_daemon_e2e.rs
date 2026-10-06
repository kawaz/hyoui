//! `hyoui web daemon` CLI boundary の E2E (= DR-0034 P2 / DR-0038)。
//!
//! 登録簿は `XDG_STATE_HOME` 配下、unit の config は `XDG_CONFIG_HOME` 配下なので、
//! 隔離した HOME を渡せば実機の登録を触らずに add / list / remove を通せる。add の
//! bind 確認と `daemon run` の bind 観測は実 port を掴むので、他と衝突しない port を
//! 使い、必ず子を落とす。

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

/// 隔離 HOME の面の状態の root (= `XDG_STATE_HOME/hyoui`)。
fn state_root(home: &Path) -> std::path::PathBuf {
    home.join(".local/state/hyoui")
}

/// この面 (= 隔離 HOME) の `state_dir` と listen を持つ `[web]`。
fn listen_config(home: &Path, listen: &str) -> String {
    format!(
        "[web]\nstate_dir = \"{}\"\nlisten = \"{listen}\"\n",
        state_root(home).display()
    )
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn error_text(output: &Output) -> String {
    error_json(output)["error"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn error_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr was not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// `add <name>` は置き場に `<name>.toml` を生成する: 土台があれば `extends`、今の
/// 面の `state_dir`、listen (既定 `127.0.0.1:43690`)、`binary_path` (既定 = 自分自身)。
/// 登録簿は config の path だけを持ち、listen は config から読み返す (DR-0038 決定 2 / 9)。
#[test]
fn add_generates_the_unit_config_and_registers_its_path() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "unstable",
            "--listen",
            "127.0.0.1:43991",
        ],
        home.path(),
    );
    assert_success(&output, "add");
    let added = json(&output);
    let config = home.path().join(".config/hyoui/web/unstable.toml");
    assert_eq!(added["name"], "unstable");
    assert_eq!(added["generated"], true);
    assert_eq!(added["config"], config.to_str().unwrap());
    assert_eq!(
        added["state_dir"],
        state_root(home.path()).to_str().unwrap()
    );
    assert_eq!(added["listen"], "127.0.0.1:43991");
    assert_eq!(added["enabled"], true);
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

    // 土台が無いので extends は書かない。
    let text = std::fs::read_to_string(&config).expect("generated config");
    assert_eq!(
        text,
        format!(
            "[web]\nstate_dir = \"{}\"\nlisten = \"127.0.0.1:43991\"\nbinary_path = \"{}\"\n",
            state_root(home.path()).display(),
            env!("CARGO_BIN_EXE_hyoui")
        )
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
    write_config(
        home.path(),
        "unstable",
        &listen_config(home.path(), "127.0.0.1:43981"),
    );
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

/// 土台 (`base.toml`) があれば生成する config は `extends` で重ね、`--binary` は
/// 絶対 path にして書く (DR-0038 決定 9)。
#[test]
fn add_extends_the_base_and_takes_the_binary() {
    let home = tempfile::tempdir().expect("isolated HOME");
    write_config(home.path(), "base", "[web]\nassets_dir = \"/assets\"\n");
    let output = command(
        &[
            "web",
            "daemon",
            "add",
            "brew",
            "--binary",
            "bin/hyoui",
            "--listen=127.0.0.1:43982",
        ],
        home.path(),
    )
    .current_dir(home.path())
    .output()
    .expect("spawn hyoui");
    assert_success(&output, "add");
    let added = json(&output);
    // cwd は実体の path で見える (macOS の `/var` → `/private/var`)。
    let binary = std::fs::canonicalize(home.path())
        .unwrap()
        .join("bin/hyoui");
    assert_eq!(added["binary_path"], binary.to_str().unwrap());
    // ビルド前に登録してよい (DR-0034 決定 2)。warning で知らせる。
    assert_eq!(added["binary_exists"], false);
    assert!(added["warnings"].is_array(), "{added}");
    let text = std::fs::read_to_string(home.path().join(".config/hyoui/web/brew.toml")).unwrap();
    assert!(
        text.starts_with("extends = \"base.toml\"\n\n[web]\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!("binary_path = \"{}\"", binary.display())),
        "{text}"
    );
    // listen を省いた生成の既定値 (`127.0.0.1:43690`) は実機のポート状況に左右されない
    // unit test (`a_new_config_listens_on_the_default_unless_given`) で固定する。
}

/// 置き場に同名のファイルが既にあれば生成せずそれを登録する。中の `state_dir` が
/// 今の面と違えば (= 別の面が同じ名前を使っている) 断り、黙って上書きしない
/// (DR-0038 決定 9)。
#[test]
fn add_registers_an_existing_file_and_refuses_another_state_root() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let mine = write_config(
        home.path(),
        "mine",
        &listen_config(home.path(), "127.0.0.1:43983"),
    );
    let before = std::fs::read_to_string(&mine).unwrap();
    let output = hyoui(&["web", "daemon", "add", "mine"], home.path());
    assert_success(&output, "add of an existing file");
    let added = json(&output);
    assert_eq!(added["generated"], false);
    assert_eq!(added["listen"], "127.0.0.1:43983");
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), before);

    // 既にあるファイルには --listen / --binary を書けないので断る (= 黙って捨てない)。
    write_config(
        home.path(),
        "again",
        &listen_config(home.path(), "127.0.0.1:43984"),
    );
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "again",
            "--listen",
            "127.0.0.1:43985",
        ],
        home.path(),
    );
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("already exists"),
        "{}",
        error_text(&output)
    );

    // 別の面の config は面の食い違いとして断り、ファイルはそのまま残す。
    let theirs = write_config(
        home.path(),
        "theirs",
        "[web]\nstate_dir = \"/elsewhere/hyoui\"\nlisten = \"127.0.0.1:43986\"\n",
    );
    let output = hyoui(&["web", "daemon", "add", "theirs"], home.path());
    assert!(!output.status.success());
    let error = error_json(&output);
    let message = error["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("belongs to the state root /elsewhere/hyoui")
            && message.contains(&format!(
                "this hyoui runs with the state root {}",
                state_root(home.path()).display()
            )),
        "{error}"
    );
    assert_eq!(error["state_dir"], "/elsewhere/hyoui");
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("another state root uses the unit name `theirs`"),
        "{error}"
    );
    assert!(
        std::fs::read_to_string(&theirs)
            .unwrap()
            .contains("/elsewhere/hyoui")
    );

    // `state_dir` の無い config も断り、書くべき値を案内する。
    write_config(home.path(), "bare", "[web]\nlisten = \"127.0.0.1:43987\"\n");
    let output = hyoui(&["web", "daemon", "add", "bare"], home.path());
    assert!(!output.status.success());
    let error = error_json(&output);
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no `[web].state_dir`"),
        "{error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!(
                "state_dir = \"{}\"",
                state_root(home.path()).display()
            )),
        "{error}"
    );

    // 土台から継いだ `state_dir` は認めない (= unit の config ファイル自身に要る)。
    write_config(
        home.path(),
        "base",
        &format!(
            "[web]\nstate_dir = \"{}\"\n",
            state_root(home.path()).display()
        ),
    );
    write_config(
        home.path(),
        "heir",
        "extends = \"base.toml\"\n[web]\nlisten = \"127.0.0.1:43978\"\n",
    );
    let output = hyoui(&["web", "daemon", "add", "heir"], home.path());
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("inherits `[web].state_dir` through `extends`"),
        "{}",
        error_text(&output)
    );

    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(
        listed["units"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
}

/// 生成した config が土台のせいで読めなければ、登録せずに生成したファイルを消し、
/// どのファイルを直すかを hint に書く (DR-0038 決定 2 / 9)。
#[test]
fn add_removes_a_generated_config_that_cannot_be_read() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let base = write_config(home.path(), "base", "[web\n");
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "fresh",
            "--listen",
            "127.0.0.1:43977",
        ],
        home.path(),
    );
    assert!(!output.status.success());
    let error = error_json(&output);
    assert!(
        error["error"]
            .as_str()
            .unwrap_or_default()
            .contains("the generated config cannot be read"),
        "{error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("fix the TOML in {}", base.display())),
        "{error}"
    );
    assert!(!home.path().join(".config/hyoui/web/fresh.toml").exists());
    assert!(!units_dir(home.path()).join("fresh.toml").exists());

    // 土台を直せば同じ add が通る (= 半端なファイルが残って再試行を塞がない)。
    std::fs::write(&base, "[web]\n").unwrap();
    assert_success(
        &hyoui(
            &[
                "web",
                "daemon",
                "add",
                "fresh",
                "--listen",
                "127.0.0.1:43977",
            ],
            home.path(),
        ),
        "add after fixing the base",
    );
}

/// `--config <path>` は任意の置き場の既存ファイルをその名前で登録する。相対 path は
/// cwd から絶対 path にし、`state_dir` は同じく確かめる (DR-0038 決定 9)。
#[test]
fn add_with_config_registers_that_file_under_the_name() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let dir = home.path().join("elsewhere");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("custom.toml"),
        format!(
            "{}binary_path = \"/opt/homebrew/bin/hyoui\"\n",
            listen_config(home.path(), "127.0.0.1:43988")
        ),
    )
    .unwrap();
    let output = command(
        &["web", "daemon", "add", "picked", "--config", "custom.toml"],
        home.path(),
    )
    .current_dir(&dir)
    .output()
    .expect("spawn hyoui");
    assert_success(&output, "add --config");
    let added = json(&output);
    assert_eq!(added["name"], "picked");
    assert_eq!(added["generated"], false);
    // cwd は実体の path で見える (macOS の `/var` → `/private/var`)。
    assert_eq!(
        added["config"],
        std::fs::canonicalize(&dir)
            .unwrap()
            .join("custom.toml")
            .to_str()
            .unwrap()
    );
    assert_eq!(added["binary_path"], "/opt/homebrew/bin/hyoui");
    // 置き場に `<name>.toml` は作らない。
    assert!(!home.path().join(".config/hyoui/web/picked.toml").exists());

    std::fs::write(
        dir.join("foreign.toml"),
        "[web]\nstate_dir = \"/elsewhere/hyoui\"\nlisten = \"127.0.0.1:43989\"\n",
    )
    .unwrap();
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "foreign",
            "--config",
            dir.join("foreign.toml").to_str().unwrap(),
        ],
        home.path(),
    );
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("belongs to the state root /elsewhere/hyoui"),
        "{}",
        error_text(&output)
    );
}

/// `list` は登録簿が空でも、監督者が居なくても成功する (= 障害時に最初に打つ口)。
#[test]
fn list_answers_on_an_empty_registry() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = hyoui(&["web", "daemon", "list"], home.path());
    assert!(output.status.success());
    assert_eq!(json(&output)["units"], serde_json::json!([]));
}

/// 同じ面の登録簿に同じ宛先の unit がある時、名前の二重登録、読めない config、
/// 不正な unit 名は JSON エラーで断る。断った add は config を残さない。
#[test]
fn conflicting_addresses_bad_configs_and_bad_names_are_refused() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert_success(
        &hyoui(
            &[
                "web",
                "daemon",
                "add",
                "stable",
                "--listen",
                "127.0.0.1:43992",
            ],
            home.path(),
        ),
        "add stable",
    );

    // 綴りが違っても同じ宛先なら拒否し、どの unit が持っているかと次の手を示す。
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "second",
            "--listen",
            "localhost:43992",
        ],
        home.path(),
    );
    assert!(!output.status.success());
    let error = error_json(&output);
    assert_eq!(error["conflicting_unit"], "stable");
    let message = error["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("already used by unit `stable`") && message.contains("--listen"),
        "{error}"
    );
    assert!(!home.path().join(".config/hyoui/web/second.toml").exists());

    // 既存の config を登録する時は、どのファイルの listen を直すかを言う。
    let clash = write_config(
        home.path(),
        "clash",
        &listen_config(home.path(), "127.0.0.1:43992"),
    );
    let output = hyoui(&["web", "daemon", "add", "clash"], home.path());
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains(&format!("change `listen` in {}", clash.display())),
        "{}",
        error_text(&output)
    );

    // 同じ名前の二重登録も断り、置き場の既存ファイルを書き換えない。
    let stable = home.path().join(".config/hyoui/web/stable.toml");
    let before = std::fs::read_to_string(&stable).unwrap();
    let output = hyoui(&["web", "daemon", "add", "stable"], home.path());
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("already registered"),
        "{}",
        error_text(&output)
    );
    assert_eq!(std::fs::read_to_string(&stable).unwrap(), before);

    // 無い config / 壊れた config は登録しない (= 監督者が起こすたびに落ちる unit を作らない)。
    let output = hyoui(
        &[
            "web",
            "daemon",
            "add",
            "absent",
            "--config",
            home.path().join("absent.toml").to_str().unwrap(),
        ],
        home.path(),
    );
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("not found"),
        "{}",
        error_text(&output)
    );
    write_config(home.path(), "broken", "extends = \"nowhere.toml\"\n");
    let output = hyoui(&["web", "daemon", "add", "broken"], home.path());
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("nowhere.toml"),
        "{}",
        error_text(&output)
    );

    for bad in ["with space", "../escape", "dot.name"] {
        let output = hyoui(&["web", "daemon", "add", bad], home.path());
        assert!(!output.status.success(), "accepted `{bad}`");
        assert!(
            error_text(&output).contains("not a valid unit name"),
            "`{bad}`"
        );
    }

    // 拒否された登録は登録簿に残らない。
    let listed = json(&hyoui(&["web", "daemon", "list"], home.path()));
    assert_eq!(listed["units"].as_array().map(Vec::len), Some(1));
}

/// 登録簿に無いプロセスが listen しているポートは、bind を試して使用中と断る。
/// 空いているポートを自動で選ばない (DR-0038 決定 9)。
#[test]
fn add_refuses_a_port_another_process_listens_on() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let taken = holder.local_addr().unwrap().to_string();
    let output = hyoui(
        &["web", "daemon", "add", "busy", "--listen", &taken],
        home.path(),
    );
    assert!(!output.status.success());
    let error = error_json(&output);
    let message = error["error"].as_str().unwrap_or_default();
    assert!(
        message.contains(&format!("`{taken}` is in use by another process"))
            && message.contains("choose another address with --listen"),
        "{error}"
    );
    assert_eq!(error["listen"], taken.as_str());
    assert!(!home.path().join(".config/hyoui/web/busy.toml").exists());
    assert!(!units_dir(home.path()).join("busy.toml").exists());

    // 離せば同じポートで通る。
    drop(holder);
    assert_success(
        &hyoui(
            &["web", "daemon", "add", "busy", "--listen", &taken],
            home.path(),
        ),
        "add after the port is released",
    );

    // port 0 (= kernel に任せる) は確かめない。
    assert_success(
        &hyoui(
            &["web", "daemon", "add", "anyport", "--listen", "127.0.0.1:0"],
            home.path(),
        ),
        "add with port 0",
    );
}

/// remove は登録簿から消し、未登録の名前は断る。config ファイル自体は消さない。
#[test]
fn remove_drops_the_registration() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert_success(
        &hyoui(
            &[
                "web",
                "daemon",
                "add",
                "stable",
                "--listen",
                "127.0.0.1:43995",
            ],
            home.path(),
        ),
        "add",
    );
    let config = home.path().join(".config/hyoui/web/stable.toml");

    let removed = json(&hyoui(&["web", "daemon", "remove", "stable"], home.path()));
    assert_eq!(removed["removed"], true);
    assert!(!units_dir(home.path()).join("stable.toml").exists());
    assert!(
        config.exists(),
        "the user's config file is not ours to delete"
    );

    // 残った config は、同じ面なら同じ名前でそのまま登録し直せる。
    let again = hyoui(&["web", "daemon", "add", "stable"], home.path());
    assert_success(&again, "re-add");
    assert_eq!(json(&again)["generated"], false);

    let output = hyoui(&["web", "daemon", "remove", "missing"], home.path());
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("no web gateway unit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// web の状態は `hyoui/web/` に置き、`hyoui-web` という dir は作らない (DR-0038 決定 4)。
#[test]
fn web_state_lives_under_hyoui_web() {
    let home = tempfile::tempdir().expect("isolated HOME");
    assert_success(
        &hyoui(
            &[
                "web",
                "daemon",
                "add",
                "stable",
                "--listen",
                "127.0.0.1:43996",
            ],
            home.path(),
        ),
        "add",
    );
    assert!(units_dir(home.path()).join("stable.toml").exists());
    assert!(!home.path().join(".local/state/hyoui-web").exists());
    assert!(!home.path().join(".config/hyoui-web").exists());
}

/// help は親と各 leaf で別の surface を出す。名前を省いた add / 何も付けない run も
/// help (= unit の名前に既定値を持たせない、DR-0038 決定 9)。
#[test]
fn help_routes_through_web_daemon_tree() {
    let home = tempfile::tempdir().expect("isolated HOME");
    for (args, needle) in [
        (&["web"][..], "hyoui web daemon run"),
        (&["web", "daemon"][..], "run"),
        (&["web", "daemon", "add"][..], "hyoui web daemon add <name>"),
        (
            &["web", "daemon", "run"][..],
            "hyoui web daemon run --no-config",
        ),
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
    // help は何も書かない。
    assert!(!home.path().join(".config/hyoui/web").exists());
    assert!(!units_dir(home.path()).exists());

    // gateway を起動する 2 本目の口は無い (DR-0038 決定 7)。
    let output = hyoui(&["web", "--listen=127.0.0.1:43998"], home.path());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("hyoui web daemon run"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// gateway を spawn し、bind を告げる 1 行が来たら `/healthz` を叩いて結果を返す。
///
/// `listen` が port 0 の時は、告げられた実際の宛先を叩く。返す宛先は bind した先。
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

    let bound = banner
        .trim()
        .strip_prefix("hyoui web: listening on http://")
        .map(str::to_owned);
    let probe = match bound {
        Some(bound) if read.is_ok() && (listen.ends_with(":0") || bound == listen) => {
            http_get(&bound, "/healthz")
        }
        _ => Err(format!("gateway did not bind {listen}")),
    };

    let _ = child.kill();
    let _ = child.wait();
    (banner, probe)
}

fn assert_healthy(banner: &str, probe: Result<String, String>) {
    let response = probe.unwrap_or_else(|error| panic!("{error} (banner={banner:?})"));
    assert!(response.contains("200"), "{response}");
    assert!(response.trim_end().ends_with("ok"), "{response}");
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
    assert_success(
        &hyoui(
            &["web", "daemon", "add", "unstable", "--listen", listen],
            home.path(),
        ),
        "add",
    );

    let (banner, probe) = run_and_probe(&["web", "daemon", "run", "unstable"], home.path(), listen);
    assert!(
        banner.contains(listen),
        "gateway did not report the unit's listen address: banner={banner:?}"
    );
    assert_healthy(&banner, probe);
}

/// `run --config <path>` は登録簿を通さずその config で起動し、`run --no-config` は
/// config を読まず組み込みの既定値と `--listen` だけで起動する (DR-0038 決定 9)。
#[test]
fn run_with_config_and_without_config() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let listen = "127.0.0.1:43999";
    let config = home.path().join("loose.toml");
    std::fs::write(&config, listen_config(home.path(), listen)).unwrap();
    let (banner, probe) = run_and_probe(
        &["web", "daemon", "run", "--config", config.to_str().unwrap()],
        home.path(),
        listen,
    );
    assert!(banner.contains(listen), "banner={banner:?}");
    assert_healthy(&banner, probe);
    // 登録簿は読みも書きもしない。
    assert!(!units_dir(home.path()).exists());

    let (banner, probe) = run_and_probe(
        &[
            "web",
            "daemon",
            "run",
            "--no-config",
            "--listen",
            "127.0.0.1:0",
        ],
        home.path(),
        "127.0.0.1:0",
    );
    assert!(banner.contains("127.0.0.1:"), "banner={banner:?}");
    assert_healthy(&banner, probe);
}

/// config を読む run は、別の面の config (= `state_dir` が今の root と違う) で起動
/// しない (DR-0038 決定 9)。
#[test]
fn run_refuses_a_config_of_another_state_root() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let config = home.path().join("foreign.toml");
    std::fs::write(
        &config,
        "[web]\nstate_dir = \"/elsewhere/hyoui\"\nlisten = \"127.0.0.1:43980\"\n",
    )
    .unwrap();
    let output = hyoui(
        &["web", "daemon", "run", "--config", config.to_str().unwrap()],
        home.path(),
    );
    assert!(!output.status.success());
    let message = error_text(&output);
    assert!(
        message.contains("belongs to the state root /elsewhere/hyoui")
            && message.contains(&state_root(home.path()).display().to_string()),
        "{message}"
    );

    // 登録済みの unit の config が後から別の面のものに書き換わった場合も同じ。
    assert_success(
        &hyoui(
            &[
                "web",
                "daemon",
                "add",
                "moved",
                "--listen",
                "127.0.0.1:43980",
            ],
            home.path(),
        ),
        "add",
    );
    write_config(
        home.path(),
        "moved",
        "[web]\nstate_dir = \"/elsewhere/hyoui\"\nlisten = \"127.0.0.1:43980\"\n",
    );
    let output = hyoui(&["web", "daemon", "run", "moved"], home.path());
    assert!(!output.status.success());
    assert!(
        error_text(&output).contains("belongs to the state root /elsewhere/hyoui"),
        "{}",
        error_text(&output)
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
