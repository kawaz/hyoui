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
        let socket = self.socket_path(session);
        let socket_arg = format!("--socket={}", socket.display());

        // args の中に既に --socket が含まれているかを判定
        let has_socket = args
            .iter()
            .any(|a| *a == "--socket" || a.starts_with("--socket="));

        // 構築: <hyoui_bin> <subcommand> --socket=<sock> <rest...>
        let (pty, pts) = pty_open().expect("pty_open");
        pty.resize(Size::new(24, 80)).expect("pty resize");

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
            .env_remove("HYOUI_LOCK_TOKEN");

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

    /// 子プロセス (= hyoui-cli 自身) に signal を送る。
    ///
    /// `kill(pid, sig)` 1 発。kernel 側で配送されるので timing は呼出側責任。
    pub fn signal(&self, sig: Signal) -> nix::Result<()> {
        nix::sys::signal::kill(self.pid, sig)
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
        let out = Command::new(hyoui_bin())
            .args(["screen", "dump", &socket_arg, "--format=ansi"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "hyoui screen dump failed (status={}): stderr={:?}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(out.stdout)
    }

    /// child を kill + wait して PTY を閉じる。
    ///
    /// 既に exit している場合は `Ok(())`。SIGTERM → 100ms → SIGKILL の段階的
    /// shutdown (= TUI app の cleanup hook を尊重しつつ、blocking を最小化)。
    pub fn kill(&mut self) -> std::io::Result<()> {
        // try_wait で既に終わってないか確認
        if let Ok(Some(_)) = self.child.try_wait() {
            return Ok(());
        }
        let _ = self.signal(Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e),
            }
        }
        // まだ生きてたら SIGKILL
        let _ = self.signal(Signal::SIGKILL);
        let _ = self.child.wait();
        Ok(())
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
