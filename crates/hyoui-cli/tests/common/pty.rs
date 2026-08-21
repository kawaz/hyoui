//! hyoui-cli を PTY 内で spawn して bytes / signal / screen dump を観測する
//! test harness (DR-0014 §マトリクス検証の base)。
//!
//! ## 設計方針
//!
//! - **介入しない**: harness は hyoui の挙動を **観測する目的のみ**。test 都合で
//!   hyoui 本体を改造しない (= 既存 socket 解決 / TTY mode / WINCH 配送をそのまま使う)
//! - **隔離**: 各 `HyouiTestRunner` は `tempfile::TempDir` を持ち、socket は
//!   `<runtime_dir>/<session>.sock` に明示配置 (= `--socket=<path>` 経由)。
//!   `XDG_RUNTIME_DIR` / `TMPDIR` 等 env を test 間で共有しない
//! - **PTY size 等の default は hyoui 既存挙動と整合**: `Size::new(24, 80)` (=
//!   24 行 80 列、hyoui-cli の `--cols` / `--rows` のデフォルトと一致)。
//!   master 側の termios は触らない (= hyoui-cli が attach 時に raw 化する)
//! - **同期 token を優先**: timeout ベースの sleep でなく、bytes pattern 一致で
//!   進行同期する (rexpect `wait_for_prompt` / pty-process `WINCH` echo pattern)
//!
//! ## API 概要
//!
//! ```ignore
//! let runner = HyouiTestRunner::new();
//! let mut h = runner.spawn_hyoui(&["run", "--", "/bin/echo", "hello"]);
//! let out = h.wait_for("hello", Duration::from_secs(5))?;
//! h.kill().ok();
//! ```

#![allow(dead_code)] // 各 test は subset しか使わないため

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use pty_process::Size;
use pty_process::blocking::{Command as PtyCommand, Pty, open as pty_open};
use tempfile::TempDir;

/// `target/debug/hyoui-cli` の path を cargo 経由で取得。
fn hyoui_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hyoui"))
}

/// 子プロセス helper の実行に deadline を被せる (= issue 2026-06-11)。
///
/// `hyoui screen dump` / `hyoui status` 等の CLI helper は client 側 socket read
/// timeout が未配線 (= 別 task)。daemon が wedge していると `Command::output()` が
/// 無期限ブロックし、test 全体のハングに化ける。deadline 超過時は子を SIGKILL して
/// `TimedOut` エラーを返す (= 呼出側の待ちループが fail で抜けられる)。
///
/// 注: stdout が pipe バッファ (~64 KiB) を超える子には使えない (= try_wait poll 中は
/// pipe を読まないため子が write で詰まる)。screen dump / status は数 KB なので対象内。
pub fn capture_with_deadline(
    cmd: &mut Command,
    deadline: Duration,
) -> std::io::Result<std::process::Output> {
    let mut child = cmd.spawn()?;
    let dl = Instant::now() + deadline;
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if Instant::now() >= dl => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("subprocess deadline {deadline:?} exceeded (killed)"),
                ));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// deadline 付き thread join (= issue 2026-06-11)。
///
/// daemon serve 等を `std::thread::spawn` した test が素の `JoinHandle::join` を
/// 呼ぶと、daemon が wedge した場合に **無限ハング**する (= 2026-05-28 に ubuntu CI
/// で 6h timeout を観測した構造)。watcher thread 経由で join し `recv_timeout` で
/// deadline を被せる。timeout 時は message 付き panic で fail させる
/// (= watcher / 対象 thread は leak するが、test process 終了で回収される)。
pub fn join_with_deadline<T: Send + 'static>(
    handle: std::thread::JoinHandle<T>,
    timeout: Duration,
    what: &str,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => panic!("{what}: joined thread panicked"),
        Err(_) => panic!(
            "{what}: thread did not finish within {timeout:?} (= 無限ハング防止のため deadline fail)"
        ),
    }
}

/// test 用 hyoui-cli runner。runtime_dir (= socket / runtime files の隔離先)
/// を 1 つ持ち、その下で複数 session を spawn / attach できる。
///
/// `Drop` で runtime_dir が unlink される (= 各 test 終了で完全 cleanup)。
pub struct HyouiTestRunner {
    /// `<runtime_dir>/<session>.sock` で socket を切る base dir。
    /// mode 0700 で初期化される (= hyoui の `ensure_socket_dir` 要件と整合)。
    runtime_dir: TempDir,
}

impl HyouiTestRunner {
    /// 新規 runner。`tempfile::Builder` で `prefix="hyoui-test-"` の
    /// `TempDir` を作り、mode 0700 を設定する。
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let runtime_dir = tempfile::Builder::new()
            .prefix("hyoui-test-")
            .tempdir()
            .expect("create runtime_dir");
        let perm = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(runtime_dir.path(), perm).expect("chmod 0700 on runtime_dir");
        Self { runtime_dir }
    }

    /// runtime_dir (= base path) を返す。test 内で socket path を組み立てる用。
    pub fn runtime_dir(&self) -> &Path {
        self.runtime_dir.path()
    }

    /// 指定 session_id に対応する socket path を返す
    /// (= `<runtime_dir>/<session>.sock`)。
    pub fn socket_path(&self, session: &str) -> PathBuf {
        self.runtime_dir.path().join(format!("{session}.sock"))
    }

    /// hyoui-cli を PTY 内で spawn して `SpawnedHyoui` を返す。
    ///
    /// 内部で `--socket=<runtime_dir>/<session>.sock` を **第 2 引数の直後**に
    /// 注入する。第 1 引数は subcommand (= `run` / `attach` / ...) を想定。
    /// 既に `args` に `--socket` が含まれている場合は **注入しない** (= caller が
    /// override する余地を残す)。
    ///
    /// PTY size は 24x80 (= hyoui-cli の `--cols/--rows` default と一致)。
    pub fn spawn_hyoui(&self, session: &str, args: &[&str]) -> SpawnedHyoui {
        self.spawn_hyoui_inner(session, args, None, Some(Size::new(24, 80)))
    }

    /// PTY のサイズを指定せずに (= `openpty(3)` 直後の winsize 未設定 = 0x0 のまま)
    /// hyoui-cli を spawn する。
    ///
    /// `TIOCSWINSZ` を一度も呼ばない PTY を再現するための harness (= CI harness /
    /// `script -q /dev/null` / `pty.openpty()` 経由の起動、issue 2026-06-11 /
    /// 2026-07-30)。通常の test は [`Self::spawn_hyoui`] を使う。
    pub fn spawn_hyoui_unsized_pty(&self, session: &str, args: &[&str]) -> SpawnedHyoui {
        self.spawn_hyoui_inner(session, args, None, None)
    }

    /// runner 固有の XDG config を適用して hyoui-cli を spawn する。
    ///
    /// process-global env を変更せず、他 test と config を共有しない。設定は
    /// `<runtime_dir>/xdg-config/hyoui/config.toml` に置き、spawn した process だけへ
    /// `XDG_CONFIG_HOME` を渡す。
    pub fn spawn_hyoui_with_config(
        &self,
        session: &str,
        args: &[&str],
        config_toml: &str,
    ) -> SpawnedHyoui {
        let config_home = self.runtime_dir.path().join("xdg-config");
        let config_dir = config_home.join("hyoui");
        std::fs::create_dir_all(&config_dir).expect("create runner config dir");
        std::fs::write(config_dir.join("config.toml"), config_toml).expect("write runner config");
        self.spawn_hyoui_inner(session, args, Some(&config_home), Some(Size::new(24, 80)))
    }

    fn spawn_hyoui_inner(
        &self,
        session: &str,
        args: &[&str],
        config_home: Option<&Path>,
        pty_size: Option<Size>,
    ) -> SpawnedHyoui {
        let socket = self.socket_path(session);
        let socket_arg = format!("--socket={}", socket.display());

        // args の中に既に --socket が含まれているかを判定
        let has_socket = args
            .iter()
            .any(|a| *a == "--socket" || a.starts_with("--socket="));

        // 構築: <hyoui_bin> <subcommand> --socket=<sock> <rest...>
        let (pty, pts) = pty_open().expect("pty_open");
        // `pty_size = None` は winsize 未設定 (= 0x0) のまま起動する意図
        // (= `spawn_hyoui_unsized_pty`)。resize を呼ばないことが仕様。
        if let Some(size) = pty_size {
            pty.resize(size).expect("pty resize");
        }

        // pty_process::blocking::Command は std::process::Command 互換 + spawn(pts) で
        // std::process::Child を返す。stdin/stdout/stderr は自動で pts に bind される。
        let mut cmd = PtyCommand::new(hyoui_bin());
        // subcommand を最初に
        let mut iter = args.iter();
        if let Some(subcmd) = iter.next() {
            cmd = cmd.arg(subcmd);
            if !has_socket {
                cmd = cmd.arg(&socket_arg);
            }
            for rest in iter {
                cmd = cmd.arg(rest);
            }
        }

        // env: テストの独立性のため runtime_dir を override (= 万が一 hyoui-cli が
        // env を読む経路があっても test の隔離を破らない)
        cmd = cmd
            .env("XDG_RUNTIME_DIR", self.runtime_dir.path())
            .env("TMPDIR", self.runtime_dir.path())
            // HYOUI_LOCK_TOKEN は test ごとに無効化したい (= 既存 hyoui-cli が
            // 環境変数から authenticate token を拾うので、隔離 dir でも干渉回避)
            .env_remove("HYOUI_LOCK_TOKEN")
            // DR-0020: test 自体が hyoui 配下で動いている環境 (= dogfooding) では
            // HYOUI_SESSION_ID / HYOUI_NAMESPACE が継承され、session 省略形の解決や
            // attach の self 判定が外側 session に向かう。test の隔離のため常に外す。
            .env_remove("HYOUI_SESSION_ID")
            .env_remove("HYOUI_NAMESPACE");
        if let Some(config_home) = config_home {
            cmd = cmd.env("XDG_CONFIG_HOME", config_home);
        }

        let child = cmd.spawn(pts).expect("spawn hyoui in pty");
        let pid = Pid::from_raw(child.id() as i32);

        SpawnedHyoui {
            pty,
            child,
            pid,
            session: session.to_string(),
            socket,
            output_buf: Vec::new(),
        }
    }

    /// 既存 session に attach する (= 別 process / 別 PTY)。
    ///
    /// `spawn_hyoui` が daemon を持つ session を作った後、別 client として
    /// attach する flow に使う。
    pub fn attach(&self, session: &str) -> SpawnedHyoui {
        self.spawn_hyoui(session, &["attach"])
    }
}

impl Default for HyouiTestRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// PTY 内で動いている hyoui-cli 1 プロセス。
///
/// `Drop` で best-effort kill + wait する (= test panic 時の child leak 防止)。
pub struct SpawnedHyoui {
    pty: Pty,
    child: Child,
    pid: Pid,
    session: String,
    socket: PathBuf,
    /// `wait_for` で読み取った bytes の蓄積。重複しないよう pattern match した
    /// 後も残し、`drain_output` で全部取れるようにする。
    output_buf: Vec<u8>,
}

impl SpawnedHyoui {
    /// 子プロセスの pid (= hyoui-cli 自身、PTY 子ではない)。
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// session 名 (= spawn 時に渡したもの)。
    pub fn session(&self) -> &str {
        &self.session
    }

    /// socket path (= `<runtime_dir>/<session>.sock`)。
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// PTY master 経由で child の stdin に bytes を送る (= line discipline 経由なので、
    /// `b"\x1a"` は ^Z, `b"\x03"` は ^C, `b"\x04"` は EOF として解釈される)。
    pub fn send_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.pty.write_all(bytes)?;
        self.pty.flush()
    }

    /// PTY 出力を一定時間 poll し、`pattern` (= raw substring) が **累積 output 中に**
    /// 出現するまで block する。timeout 内に見つからなければ `Err`。
    ///
    /// 戻り値は **timeout 時点までの累積 output 全文** (= debug しやすさ優先)。
    /// 一度マッチした pattern も `output_buf` に残るため、次の `wait_for` で
    /// 「過去の出力」も含めた判定になる点に注意。新規バイトのみを見たい場合は
    /// 事前に `drain_output` で buffer を空にする。
    pub fn wait_for(&mut self, pattern: &str, timeout: Duration) -> std::io::Result<String> {
        let deadline = Instant::now() + timeout;
        // 既に buffer に pattern が含まれていれば即 return
        if find_pattern(&self.output_buf, pattern.as_bytes()).is_some() {
            return Ok(String::from_utf8_lossy(&self.output_buf).into_owned());
        }
        let mut tmp = [0u8; 4096];
        loop {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(r) => r,
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "wait_for({pattern:?}) timed out after {:?}; output so far ({} bytes): {:?}",
                            timeout,
                            self.output_buf.len(),
                            String::from_utf8_lossy(&self.output_buf)
                        ),
                    ));
                }
            };
            // PTY master fd を poll。pty-process は `AsFd` 実装を持つので
            // safe な `as_fd()` 経由で BorrowedFd を取得できる (= unsafe 不要)。
            let borrowed = self.pty.as_fd();
            let mut fds = [PollFd::new(borrowed, PollFlags::POLLIN)];
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let n = match nix::poll::poll(&mut fds, PollTimeout::from(timeout_ms as u16)) {
                Ok(n) => n,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(std::io::Error::other(format!("poll: {e}"))),
            };
            if n == 0 {
                continue; // timeout の判定は次 iteration の checked_duration_since で
            }
            let revents = fds[0].revents().unwrap_or(PollFlags::empty());
            if revents.contains(PollFlags::POLLIN) {
                match self.pty.read(&mut tmp) {
                    Ok(0) => {
                        // EOF
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            format!(
                                "PTY EOF while waiting for {pattern:?}; output ({} bytes): {:?}",
                                self.output_buf.len(),
                                String::from_utf8_lossy(&self.output_buf)
                            ),
                        ));
                    }
                    Ok(n) => {
                        self.output_buf.extend_from_slice(&tmp[..n]);
                        if find_pattern(&self.output_buf, pattern.as_bytes()).is_some() {
                            return Ok(String::from_utf8_lossy(&self.output_buf).into_owned());
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) => return Err(e),
                }
            } else if revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
                // 子側の close は EIO になることもある。次 read で確定させる。
                match self.pty.read(&mut tmp) {
                    Ok(0) | Err(_) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            format!(
                                "PTY hangup while waiting for {pattern:?}; output ({} bytes): {:?}",
                                self.output_buf.len(),
                                String::from_utf8_lossy(&self.output_buf)
                            ),
                        ));
                    }
                    Ok(n) => {
                        self.output_buf.extend_from_slice(&tmp[..n]);
                        if find_pattern(&self.output_buf, pattern.as_bytes()).is_some() {
                            return Ok(String::from_utf8_lossy(&self.output_buf).into_owned());
                        }
                    }
                }
            }
        }
    }

    /// `output_buf` の現在の中身を取り出し、buffer を空にする。
    pub fn drain_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output_buf)
    }

    /// PTY master に今読める bytes があれば全部読んで `output_buf` に蓄積する
    /// (= 非ブロッキング pump)。読んだ byte 数を返す。
    ///
    /// **用途 (= issue 2026-06-11)**: 子 (hyoui attach) が `tcsetattr(TCSAFLUSH)`
    /// 等で「外側端末への出力 drain」を待つ場面で、master を誰も読まないと子が
    /// ioctl で永久ブロックする (= 実端末なら端末エミュレータが常時読むので
    /// 起きない、test 環境特有の相互待ち)。process state を ps で poll する
    /// 待ちループ内で本 method を毎周呼び、master を吸い続けること。
    pub fn pump_pty(&mut self) -> usize {
        let mut total = 0usize;
        let mut tmp = [0u8; 4096];
        loop {
            let borrowed = self.pty.as_fd();
            let mut fds = [PollFd::new(borrowed, PollFlags::POLLIN)];
            // timeout 0 = 即時判定 (= 読めるものだけ読む)
            match nix::poll::poll(&mut fds, PollTimeout::ZERO) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            let revents = fds[0].revents().unwrap_or(PollFlags::empty());
            if !revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
                break;
            }
            match self.pty.read(&mut tmp) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    self.output_buf.extend_from_slice(&tmp[..n]);
                    total += n;
                }
                Err(_) => break, // EIO (= 子側 close) 等は pump 終了
            }
        }
        total
    }

    /// 子プロセス (= hyoui-cli 自身) に signal を送る。
    ///
    /// `kill(pid, sig)` 1 発。kernel 側で配送されるので timing は呼出側責任。
    pub fn signal(&self, sig: Signal) -> nix::Result<()> {
        nix::sys::signal::kill(self.pid, sig)
    }

    /// hyoui-cli (= 自プロセス) が exit するのを deadline まで待ち、exit code を返す。
    ///
    /// `hyoui run` / `hyoui attach` は exec で attach client に化けるので、本 pid を
    /// reap すれば attach client の exit code が取れる (= issue 2026-06-11 優先1 の
    /// exit code semantics 検証用)。
    ///
    /// 戻り値:
    /// - `Some(code)`: 正常 exit (= WIFEXITED) の exit code
    /// - `None`: deadline 超過 or signal 死 (= WIFSIGNALED)。test 側で None を fail
    ///   扱いにする
    pub fn wait_exit_code(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            // pump_pty で PTY の出力を吸い続けないと、子が write block して exit
            // できないことがある (= PTY buffer full)。
            self.pump_pty();
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return None,
            }
        }
        None
    }

    /// この session の daemon process pid を返す (= socket を listen している process)。
    ///
    /// daemon は `__daemonize-run` で起動後 ps の argv を user-facing に書き換えるため、
    /// ps の command 文字列に socket path が出ない。確実に特定するため、`lsof` で socket
    /// file を開いている process を引く (= listen 中の daemon が必ず 1 つ持つ)。
    pub fn daemon_pid(&self) -> Option<i32> {
        let out = std::process::Command::new("lsof")
            .args(["-t", &self.socket.display().to_string()])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        // `lsof -t` は pid を 1 行 1 つで返す。複数あれば最初の 1 つ (= daemon)。
        for line in text.lines() {
            if let Ok(pid) = line.trim().parse::<i32>()
                && pid != self.pid.as_raw()
            {
                return Some(pid);
            }
        }
        None
    }

    /// 子の process state を取得する (= `ps -o pid,ppid,pgid,stat,comm` 相当)。
    ///
    /// macOS / Linux 両対応のため、`ps` コマンドを subprocess で呼ぶ
    /// (= /proc は macOS にない、`ps` は POSIX で利用可能)。
    ///
    /// **注**: `sid` (session id) は macOS の `ps` keyword では非対応のため
    /// 含めない (= 既存 keyword set で両 OS 動作確認済)。session leader 判定が
    /// 必要な場面は `state.pgid == state.pid` で代替 (= DR-0001 §実装ノート
    /// 「子は独立セッションリーダーなので 子の pgid == 子の pid」)。
    pub fn process_state(&self) -> std::io::Result<ProcessState> {
        process_state_of(self.pid.as_raw())
    }

    /// `hyoui screen dump --socket=<sock> --format=ansi` を別 process で呼んで
    /// stdout bytes を取得する。
    ///
    /// daemon が `session` に対応する socket を listen している前提。
    /// session 引数を取るのは将来 1 runner で複数 session を扱う拡張を想定してで、
    /// 本実装では `self.socket` を使う (= 同じ runner / 同じ session)。
    pub fn screen_dump(&self, _session: &str) -> std::io::Result<Vec<u8>> {
        let socket_arg = format!("--socket={}", self.socket.display());
        // CLI helper には socket read timeout が無い (= 別 task)。wedged daemon で
        // test がハングしないよう deadline 付き実行 (= issue 2026-06-11)。
        let out = capture_with_deadline(
            Command::new(hyoui_bin())
                .args(["screen", "dump", &socket_arg, "--format=ansi"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            Duration::from_secs(10),
        )?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "hyoui screen dump failed (status={}): stderr={:?}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(out.stdout)
    }

    /// `hyoui status --socket=<sock>` の stdout を取得する (= daemon が listen 中の前提)。
    pub fn status_text(&self) -> std::io::Result<String> {
        let socket_arg = format!("--socket={}", self.socket.display());
        // deadline 付き実行: screen_dump と同理由 (= issue 2026-06-11)。
        let out = capture_with_deadline(
            Command::new(hyoui_bin())
                .args(["status", &socket_arg])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            Duration::from_secs(10),
        )?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "hyoui status failed (status={}): stderr={:?}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `hyoui screen snapshot --socket=<sock> --include=WindowSize --format=json` の
    /// stdout を取得する (= viewport サイズの実測用)。
    pub fn screen_snapshot_text(&self) -> std::io::Result<String> {
        let socket_arg = format!("--socket={}", self.socket.display());
        // deadline 付き実行: status_text と同理由 (= issue 2026-06-11)。
        let out = capture_with_deadline(
            Command::new(hyoui_bin())
                .args([
                    "screen",
                    "snapshot",
                    &socket_arg,
                    "--include=WindowSize",
                    "--format=json",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            Duration::from_secs(10),
        )?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "hyoui screen snapshot failed (status={}): stderr={:?}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// attach client (= leader) が daemon に接続済みになるまで待つ。
    ///
    /// 子 process は attach handshake より先に起動するため、子の出現だけでは client 側の
    /// stopped-child 処理が利用可能とは限らない。`SessionChildStoppedNotify` の受信、
    /// DR-0030 の resume 要求、DR-0029 の停止通知描画を検証する test は、先に rw leader
    /// 登録の完了をこの helper で確認する。
    ///
    /// `hyoui status` の clients に `leader` 付き client が現れるまで poll する
    /// (= status 自身も一時 client を張るが、leader は attach client のみ)。timeout で
    /// `false` を返す (= 呼出側で deadline 付き panic にする)。
    pub fn wait_for_leader_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(text) = self.status_text()
                && text.contains("leader")
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 正規経路 (`hyoui kill --socket=<sock>`) で daemon を畳む。
    ///
    /// detach key テスト等、client が抜けた後も daemon + 子が残るケースで使う。daemon
    /// は `__daemonize-run` で起動後 ps の argv を user-facing に書き換えるため、ps の
    /// command 文字列に socket path が出ない (= `daemon_pid` で socket 一致が効かない)。
    /// 確実に畳むため CLI の kill subcommand を叩く。best-effort (= 失敗は無視)。
    pub fn kill_daemon_via_cli(&self) {
        let socket_arg = format!("--socket={}", self.socket.display());
        let _ = std::process::Command::new(hyoui_bin())
            .args(["kill", &socket_arg])
            .env(
                "XDG_RUNTIME_DIR",
                self.socket.parent().unwrap_or(Path::new("/tmp")),
            )
            .output();
    }

    /// child + session subtree (= daemon / 子 PTY) を kill して PTY を閉じる。
    ///
    /// 既に exit している場合は `Ok(())`。SIGTERM → 200ms → SIGKILL の段階的
    /// shutdown (= TUI app の cleanup hook を尊重しつつ、blocking を最小化)。
    ///
    /// **deadline 必須**: DR-0015/0017 構造では `hyoui run` は detached daemon を
    /// 別 session に fork し、自身は `attach` に exec する。子 PTY を STOPPED の
    /// まま放置すると (= follow test の self-stop シナリオ)、session leader が
    /// controlling tty cleanup で exit しきれず `child.wait()` が **無限 block** する
    /// (= issue 2026-06-11 のハング根因)。そこで:
    ///   1. session subtree (daemon / 子) を `find_descendants` で列挙し、
    ///      **SIGCONT → SIGKILL** で起こしてから確実に殺す (= stopped child を残さない)。
    ///   2. 最終 reap は blocking `wait()` ではなく `try_wait` を deadline 付き poll で
    ///      回す (= 万一 reap できなくても無限 block しない)。
    pub fn kill(&mut self) -> std::io::Result<()> {
        // try_wait で既に終わってないか確認
        if let Ok(Some(_)) = self.child.try_wait() {
            return Ok(());
        }
        // 1. session subtree (daemon + 子 PTY) を起こしてから皆殺し。
        //    stopped (T) のまま残すと session leader (= self.pid) が exit しきれず
        //    後段の reap が無限 block するため、SIGCONT を先送りする。
        self.kill_session_subtree();

        let _ = self.signal(Signal::SIGCONT);
        let _ = self.signal(Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e),
            }
        }
        // まだ生きてたら SIGKILL + subtree 再掃除 (= exec 後に現れた子も拾う)。
        let _ = self.signal(Signal::SIGCONT);
        let _ = self.signal(Signal::SIGKILL);
        self.kill_session_subtree();

        // 2. blocking wait() は使わない (= 無限 block 回避)。deadline 付き try_wait poll。
        let reap_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < reap_deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e),
            }
        }
        // deadline 超過: reap できなくても test を無限 block させない。
        // (= zombie / orphan が残る可能性はあるが、それは leak 検出の領分。
        //  本 harness は「ハングしない」ことを最優先する。)
        Ok(())
    }

    /// session subtree (= daemon / 子 PTY / その孫) を SIGCONT で起こしてから
    /// SIGKILL で確実に殺す。`find_descendants` で `self.pid` 配下を BFS する。
    ///
    /// DR-0015/0017 では daemon は別 session に setsid 済だが、exec 前の段階では
    /// `self.pid` の子として現れる。stopped child を残すと session cleanup が
    /// 詰まるため、まず CONT で全員起こす。
    fn kill_session_subtree(&self) {
        if let Ok(descendants) = find_descendants(self.pid.as_raw()) {
            // まず全員を起こす (= stopped のままだと SIGTERM/KILL 以外は配送保留)。
            for p in &descendants {
                let _ = nix::sys::signal::kill(Pid::from_raw(p.pid), Signal::SIGCONT);
            }
            for p in &descendants {
                let _ = nix::sys::signal::kill(Pid::from_raw(p.pid), Signal::SIGKILL);
            }
        }
    }
}

impl Drop for SpawnedHyoui {
    fn drop(&mut self) {
        // best-effort cleanup. 個別 test の `kill()` を呼び忘れても leak しない。
        let _ = self.kill();
    }
}

/// `ps` 出力から抽出した process state。
///
/// session leader 判定は `pgid == pid` で行う (= macOS の `ps -o sid=` 非対応の
/// workaround)。
#[derive(Debug, Clone)]
pub struct ProcessState {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// `ps` の stat field (= "R", "S", "T", "Z+", "Ss" 等)。
    /// 1 文字目が state 主分類:
    /// - `R`: Running
    /// - `S`: Sleeping (interruptible)
    /// - `T`: Stopped (= SIGSTOP/SIGTSTP)
    /// - `Z`: Zombie
    /// - `I`: Idle (macOS, BSD)
    pub stat: String,
    /// `ps` の comm (= 短い process 名)。macOS は `/full/path/to/binary args...`
    /// が出る場合もあるので、test 側で startsWith / contains で比較する想定。
    pub comm: String,
}

impl ProcessState {
    /// `stat` の主分類が指定文字に一致するかを返す (= 例: `is_state('T')` で
    /// Stopped 判定)。`ps` の stat は "Ss" / "S+" 等の suffix を含む場合があるため、
    /// 1 文字目だけを見る。
    pub fn is_state(&self, primary: char) -> bool {
        self.stat.starts_with(primary)
    }
}

/// 指定 PID の process state を取得する (= `ps -p <pid> -o ...`)。
///
/// macOS/Linux 両対応の最小 keyword set (`pid,ppid,pgid,stat,comm`) を使う。
/// プロセスが既に消えている場合は `ps` が空 stdout + status=1 を返すので
/// `NotFound` 系 err として伝播。
pub fn process_state_of(pid: i32) -> std::io::Result<ProcessState> {
    let pid_str = pid.to_string();
    let out = Command::new("ps")
        .args(["-o", "pid=,ppid=,pgid=,stat=,comm=", "-p", &pid_str])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "ps -p {pid_str} failed (status={}): stderr={:?}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        ));
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if line.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("ps -p {pid_str}: empty output (process gone?)"),
        ));
    }
    parse_ps_line(&line)
}

/// `ps -A -o pid=,ppid=,pgid=,stat=,comm=` の出力を全 process 分 parse する。
///
/// 内部 helper、test 側からは `find_children` / `find_descendants` 経由で使う。
fn all_processes() -> std::io::Result<Vec<ProcessState>> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,comm="])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "ps -A failed (status={}): stderr={:?}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let mut result = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(ps) = parse_ps_line(line) {
            result.push(ps);
        }
        // parse 失敗行は skip (= comm に space を含む行で 5 field を割らない
        // ケースは parse_ps_line の堅牢化で吸収される)
    }
    Ok(result)
}

/// `ps -o pid=,ppid=,pgid=,stat=,comm=` の 1 行を parse する。
///
/// `comm` (5 番目以降) は空白を含むことがある (= macOS は full path + args が
/// 出る場合) ので、最初の 4 field を `split_whitespace` で取り、残りを `comm`
/// に join する。
fn parse_ps_line(line: &str) -> std::io::Result<ProcessState> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 {
        return Err(std::io::Error::other(format!(
            "ps line unparseable (need 5+ fields): {line:?}"
        )));
    }
    let parse_i32 = |s: &str| -> std::io::Result<i32> {
        s.parse::<i32>()
            .map_err(|e| std::io::Error::other(format!("parse {s:?}: {e}")))
    };
    Ok(ProcessState {
        pid: parse_i32(parts[0])?,
        ppid: parse_i32(parts[1])?,
        pgid: parse_i32(parts[2])?,
        stat: parts[3].to_string(),
        comm: parts[4..].join(" "),
    })
}

/// 指定 PID を親に持つ直接の子プロセス一覧を返す (= ppid == parent_pid)。
pub fn find_children(parent_pid: i32) -> std::io::Result<Vec<ProcessState>> {
    let all = all_processes()?;
    Ok(all.into_iter().filter(|p| p.ppid == parent_pid).collect())
}

/// 直接の子を 1 つだけ返す (= 複数いれば最初の 1 つ、いなければ `NotFound`)。
pub fn find_child_of(parent_pid: i32) -> std::io::Result<ProcessState> {
    let children = find_children(parent_pid)?;
    children.into_iter().next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no child found for ppid={parent_pid}"),
        )
    })
}

/// 指定 PID の子孫 (= 子・孫・ひ孫 ...) 全部を返す。
///
/// 単純な BFS で `ppid` を辿る。`root_pid` 自身は含めない。
pub fn find_descendants(root_pid: i32) -> std::io::Result<Vec<ProcessState>> {
    let all = all_processes()?;
    let mut result = Vec::new();
    let mut frontier: Vec<i32> = vec![root_pid];
    while let Some(parent) = frontier.pop() {
        for p in &all {
            if p.ppid == parent && !result.iter().any(|r: &ProcessState| r.pid == p.pid) {
                result.push(p.clone());
                frontier.push(p.pid);
            }
        }
    }
    Ok(result)
}

/// `haystack` 内に `needle` が初めて現れる index を返す (= byte-level substring search)。
fn find_pattern(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_pattern_basic() {
        assert_eq!(find_pattern(b"hello world", b"world"), Some(6));
        assert_eq!(find_pattern(b"hello", b"xyz"), None);
        assert_eq!(find_pattern(b"abc", b""), Some(0));
        assert_eq!(find_pattern(b"ab", b"abc"), None);
    }
}
