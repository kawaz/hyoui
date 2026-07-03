//! DR-0019 §5: pipe-through `--stdin-eof=detach|send-eof` の e2e。
//!
//! 非 tty stdin (= pipe) で `hyoui run` を起動したときの挙動を検証する:
//!
//! - default (= send-eof): pipe close で子に EOT が届き、line-oriented な子が
//!   read EOF として解釈して自然 exit する
//!   (= pipe-through の透過性回復)
//! - `--stdin-eof=detach`: 現行挙動。EOF で client が切断するだけで子は daemon
//!   配下に残る (= session が live のまま)
//!
//! 子プロセスは `cat` で統一する。本 file の検証対象は「client stdin の EOF が
//! 子 PTY への EOT 送出 (send-eof) / client detach (detach) に変換されること」で
//! あり、子に必要な性質は「stdin を読み、EOF で終了する最小プロセス」だけ。
//! 子の出力内容は検証対象外 (= 入力中継の検証は attach_interactive_input.rs が担う)
//! なので assert しない。外部コマンドの実装差 (BSD/GNU 等) にも依存しない。
//!
//! HyouiTestRunner は PTY (= tty stdin) 経路なので使えない。pipe stdin を直接渡す
//! `std::process::Command` で起動する。
//!
//! `#[ignore]`: 子 process + 実プロセス起動を伴うため、ローカルで `--ignored` 実行。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 各 test 共通の子プロセス: stdin を読み、EOF で exit 0 する最小の line-oriented
/// プロセス。出力は検証しないので何を書くかは問わない。
const READ_CHILD: &str = "cat";

fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// ユニークな runtime dir を作り、`XDG_RUNTIME_DIR` に使う (= 他 test と socket 衝突回避)。
/// 一意性は pid + プロセス内単調カウンタで担保する (= 旧実装の `Instant::now().elapsed()`
/// は計測直後ゆえ常にほぼ 0 で衝突しうる無意味な値だった)。
fn unique_runtime_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("hyoui-stdineof-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&base).expect("create runtime dir");
    base
}

/// test の panic / 早期 return でも子プロセス (= hyoui run の fork daemon + 子 PTY) を
/// leak させないための cleanup guard。`Drop` で best-effort に:
/// 1. session を `hyoui kill --signal KILL` で畳む (= daemon 配下の子も含めて殺す)
/// 2. spawn した hyoui run プロセス自身を kill + wait (= zombie 回収)
/// 3. runtime dir を削除
struct PipeRunGuard {
    child: Option<Child>,
    session: String,
    runtime_dir: PathBuf,
}

impl PipeRunGuard {
    fn new(child: Child, session: &str, runtime_dir: &Path) -> Self {
        Self {
            child: Some(child),
            session: session.to_string(),
            runtime_dir: runtime_dir.to_path_buf(),
        }
    }

    /// spawn した hyoui run プロセスへの可変参照 (= try_wait / take 用)。
    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child already taken")
    }
}

impl Drop for PipeRunGuard {
    fn drop(&mut self) {
        // daemon 配下の session を畳む (= 子 PTY も殺す)。
        let _ = Command::new(hyoui_bin())
            .args(["kill", &self.session, "--signal", "KILL"])
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .output();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// send-eof (default): pipe stdin の close で子に EOT が届き、子が read EOF として
/// 自然 exit する。exit すれば hyoui run 自身も exit し、**exit code 0 が伝播する**
/// (= DR-0019 §5 の pipe-through 透過性)。
///
/// exit code の success assert が本質: 「exit した」だけを見ると、client 側の異常
/// 終了 (exit 1、= 例えば daemon からの frame を処理できず即死する退行) も pass して
/// しまう。子 (= cat) は EOF で 0 を返すので、非 0 は経路のどこかの異常を意味する。
#[ignore = "子 process 起動 + 実時間を使う、ローカルで --ignored 実行 (DR-0019 §5)"]
#[test]
fn pipe_send_eof_default_terminates_child() {
    let rt = unique_runtime_dir();
    let session = "stdineof-send";
    let child = Command::new(hyoui_bin())
        .args(["run", "--session", session, "--", READ_CHILD])
        .env("XDG_RUNTIME_DIR", &rt)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyoui run");
    let mut guard = PipeRunGuard::new(child, session, &rt);

    // pipe に 1 行書いて close (= EOF)。send-eof default なら子に EOT が届く。
    {
        let mut stdin = guard.child_mut().stdin.take().expect("stdin");
        stdin.write_all(b"probe\n").expect("write stdin");
        // drop で close → EOF。
    }

    // 子が exit すれば hyoui run も exit する。timeout 付き wait。
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(s) = guard.child_mut().try_wait().expect("try_wait") {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "hyoui run が send-eof default で exit しなかった (= 子が残った疑い)"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(
        status.success(),
        "hyoui run は子 (= cat) の exit 0 を伝播して success で終わるはず \
         (非 0 は EOF 伝播経路 or client の異常終了): status={status:?}"
    );
    // guard Drop が session kill + runtime dir 削除を行う。
}

/// detach: `--stdin-eof=detach` では EOF で client が切断するだけで、子 (= cat、
/// EOT を受け取らないので read で待ち続ける) は daemon 配下に残る
/// (= session が live)。
#[ignore = "子 process 起動 + 実時間を使う、ローカルで --ignored 実行 (DR-0019 §5)"]
#[test]
fn pipe_detach_leaves_child_under_daemon() {
    let rt = unique_runtime_dir();
    let session = "stdineof-detach";
    // stdout/stderr は null にする。detach では daemon (= 子を抱えたまま) が live で
    // 残るので、もし test 側 pipe を daemon が継承すると `wait_with_output` が EOF を
    // 取れず永久 block する。stdout 内容は本 test では検証しない (= list で確認する)。
    let child = Command::new(hyoui_bin())
        .args([
            "run",
            "--session",
            session,
            "--stdin-eof=detach",
            "--",
            READ_CHILD,
        ])
        .env("XDG_RUNTIME_DIR", &rt)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyoui run");
    let mut guard = PipeRunGuard::new(child, session, &rt);

    {
        let mut stdin = guard.child_mut().stdin.take().expect("stdin");
        stdin.write_all(b"1+2\n").expect("write stdin");
    }

    // detach なので EOF 観測で client (= hyoui run の exec attach) は速やかに exit する。
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if guard.child_mut().try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "detach 指定で client が exit しなかった"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // client が抜けても子は daemon 配下で live なはず。`hyoui list` に session が残る。
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
    // 後始末 (= session kill + runtime dir 削除) は guard Drop に委ねる。
}

/// C-1 再現: `hyoui run -- cat < /dev/null` で client が CPU spin せず、
/// SendEof default により子が自然 exit する。
///
/// 旧実装は stdin の revents で POLLIN しか見ておらず、macOS で `/dev/null` (chardev) を
/// POLLIN 要求すると POLLNVAL が即時返る (= POLLIN は立たない)。結果 read に到達できず
/// EOF を観測できないため、(1) EOT 未送出で子が永久に残り、(2) poll が POLLNVAL で
/// 即 return する busy loop (= 100% CPU) になった。修正後は POLLNVAL/POLLERR/POLLHUP を
/// EOF 相当として SendEof 経路に倒すので、子が終了し spin もしない。
#[ignore = "子 process 起動 + 実時間 + ps サンプリングを使う、ローカルで --ignored 実行 (C-1)"]
#[test]
fn pipe_dev_null_no_spin_and_terminates() {
    let rt = unique_runtime_dir();
    let session = "stdineof-devnull";
    let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
    let child = Command::new(hyoui_bin())
        .args(["run", "--session", session, "--", READ_CHILD])
        .env("XDG_RUNTIME_DIR", &rt)
        .stdin(Stdio::from(devnull))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyoui run");
    let pid = child.id();
    let mut guard = PipeRunGuard::new(child, session, &rt);

    // CPU spin 検証: 起動直後にプロセスがまだ live なら %cpu を 2 回サンプルする。
    // spin していれば 2 回目で高 %cpu が観測される。ただし正常な場合は EOF を即観測して
    // 速やかに exit するため、その場合は spin チェックを skip して exit 検証に進む。
    std::thread::sleep(Duration::from_millis(300));
    if guard.child_mut().try_wait().expect("try_wait").is_none() {
        // まだ live → 1 秒後に %cpu をサンプル。
        std::thread::sleep(Duration::from_secs(1));
        if let Some(cpu) = sample_cpu_percent(pid) {
            assert!(
                cpu < 50.0,
                "client が CPU spin している疑い (= POLLNVAL busy loop): %cpu={cpu}"
            );
        }
    }

    // SendEof default なので子は EOT を read EOF として exit する → hyoui run も exit。
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if guard.child_mut().try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hyoui run が `< /dev/null` で exit しなかった (= EOF 未観測 / 子が残った疑い)"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // 後始末は guard Drop に委ねる。
}

/// `ps -o %cpu= -p <pid>` で 1 プロセスの瞬間 CPU 使用率を取る。取得失敗 (= 既に exit
/// 等) は `None`。
fn sample_cpu_percent(pid: u32) -> Option<f64> {
    let out = Command::new("ps")
        .args(["-o", "%cpu=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<f64>().ok()
}
