//! Pseudo-terminal pair + child spawn.
//!
//! Two flows:
//!
//! 1. [`Pty::open`] — `openpty(3)` for tests / scenarios that need a
//!    master+slave pair without forking (e.g. winsize tests).
//!
//! 2. [`Pty::spawn`] — DR-0017 session anchor: `openpty(3)` + 手動 `fork(2)` +
//!    `execvp(3)`。parent (= setsid 済 daemon) が slave を `TIOCSCTTY` で
//!    controlling tty にし、child を **同 session・別 pgrp・foreground** で
//!    起動する。child の pgrp が orphan でなくなり、SIGTSTP が本来の
//!    セマンティクスで動く (= TUI の ^Z が効く)。詳細は DR-0017。
//!
//! `Pty::spawn` は anchor を満たせない呼び出し (= session leader でない /
//! 既に ctty を持つ = テストが `Session::start` を直接呼ぶケース) を
//! `Error::Precondition` で検知し、**明示 warning を出した上で** 旧 `forkpty`
//! 構造 ([`raw::forkpty_then_exec_legacy`]) に fallback する (= テスト経由のみ)。

use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;

use nix::pty::{OpenptyResult, Winsize};

use super::error::{Error, Result};
use super::raw;
use super::signal as sig;

/// Owned PTY master, optionally with a slave kept by the parent.
///
/// * [`Pty::open`] returns one with `slave = Some(_)`.
/// * [`Pty::spawn`] returns one with `slave = None` (the child holds it via
///   `login_tty`).
///
/// Both fields are `Option` so [`Pty::into_master`] can move them out via
/// `Option::take` without needing any `unsafe`-based ManuallyDrop / ptr::read
/// trick (the `Drop` impl otherwise forbids destructuring `self`).
#[derive(Debug)]
pub struct Pty {
    master: Option<OwnedFd>,
    slave: Option<OwnedFd>,
}

/// Result of [`Pty::spawn`].
#[derive(Debug)]
pub struct Spawned {
    /// Master side of the PTY. Slave is owned by the child.
    pub pty: Pty,
    /// Child process ID.
    pub child: nix::unistd::Pid,
}

impl Pty {
    fn master_owned(&self) -> &OwnedFd {
        // Both `master` Options are `Some` for the lifetime of any Pty
        // produced by `open`/`spawn`; only `into_master` (which consumes
        // `self`) takes them out. Panicking here would indicate a bug.
        self.master.as_ref().expect("Pty master fd already taken")
    }

    /// `openpty(3)` — open a fresh master/slave pair sized `(cols, rows)`.
    /// No fork happens.
    pub fn open(cols: u16, rows: u16) -> Result<Self> {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let OpenptyResult { master, slave } = nix::pty::openpty(&ws, None).map_err(Error::from)?;
        Ok(Self {
            master: Some(master),
            slave: Some(slave),
        })
    }

    /// DR-0017 session anchor: `openpty(3)` + 手動 `fork(2)` + `execvp(3)`。
    /// parent が slave を `TIOCSCTTY` で controlling tty にし、child を同
    /// session・別 pgrp・foreground で起動する。
    ///
    /// Returns the parent-side [`Spawned`]. The child path **does not
    /// return** — it always execs or `_exit(127)`s.
    ///
    /// anchor 前提 (= caller が session leader かつ ctty 無し) を満たさない
    /// 呼び出し (= テストが直接呼ぶケース) では、`TIOCSCTTY` 失敗
    /// (`Error::Precondition`) を検知して **明示 warning を stderr に出した上で**
    /// 旧 `forkpty` 構造に fallback する (= child が独立 session leader、^Z は
    /// 効かないがテストの大半は anchor と無関係なため実害なし)。詳細は DR-0017 §柱1。
    ///
    /// `cwd = Some(dir)` の場合、子は exec 直前に `chdir(dir)` する (= `hyoui run` の
    /// 起点 dir で実コマンドを動かす透過性回復、bug fix 2026-06-11)。`None` なら
    /// 呼び出しプロセスの cwd を継承する (= 従来挙動)。
    pub fn spawn(argv: &[&str], cols: u16, rows: u16, cwd: Option<&Path>) -> Result<Spawned> {
        if argv.is_empty() {
            return Err(Error::Invalid("argv must not be empty"));
        }
        let argv_c: Vec<CString> = argv
            .iter()
            .map(|s| CString::new(*s).map_err(|_| Error::Invalid("argv contained NUL")))
            .collect::<Result<_>>()?;
        // cwd を CString 化 (= path に NUL を含むと exec 経路で使えないため reject)。
        let cwd_c: Option<CString> = match cwd {
            Some(p) => {
                use std::os::unix::ffi::OsStrExt;
                Some(
                    CString::new(p.as_os_str().as_bytes())
                        .map_err(|_| Error::Invalid("cwd path contained NUL"))?,
                )
            }
            None => None,
        };
        let forked = match raw::openpty_fork_anchor_exec(&argv_c, cols, rows, cwd_c.as_ref()) {
            Ok(f) => f,
            Err(Error::Precondition(_)) => {
                // anchor 前提を満たさない (= TIOCSCTTY 失敗)。production の daemon は
                // setsid 済なのでここに来ない (= 来たら呼び出し経路が test など非
                // daemonize)。サイレントにせず明示 warning を出してから legacy 構造で
                // 起動する (= DR-0017 §柱1 が許容する fallback)。
                eprintln!(
                    "hyoui: warning: session anchor 化不可 (= 呼び出しプロセスが session leader でない / 既に controlling tty を持つ)。\
                     旧 forkpty 構造で child を起動します (= child が独立 session leader、^Z は効きません)。\
                     production の daemon は setsid 済のためこの経路には入りません (= テスト等の直接呼び出しのみ)。"
                );
                raw::forkpty_then_exec_legacy(&argv_c, cols, rows, cwd_c.as_ref())?
            }
            Err(e) => return Err(e),
        };
        Ok(Spawned {
            pty: Self {
                master: Some(forked.master),
                slave: None,
            },
            child: forked.child,
        })
    }

    /// Borrow the master fd. Always valid while `self` lives.
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master_owned().as_fd()
    }

    /// Borrow the slave fd, if still held by the parent.
    pub fn slave_fd(&self) -> Option<BorrowedFd<'_>> {
        self.slave.as_ref().map(|fd| fd.as_fd())
    }

    /// Consume `self`, returning the owned master fd.
    pub fn into_master(mut self) -> OwnedFd {
        // Run the SIGWINCH-clear side effect before Drop strips it, so
        // a late SIGWINCH cannot target a fd that is now owned by the
        // caller (and might be closed independently).
        sig::clear_winch_if(self.master_owned().as_raw_fd());
        // Take master out; slave (if any) Drops normally via Pty::drop.
        self.master.take().expect("Pty master fd already taken")
    }

    /// Apply a new window size via `TIOCSWINSZ` on the master.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        raw::ioctl_set_winsize(self.master_fd(), cols, rows)
    }

    /// DR-0028 Phase 1: 既存 PTY master fd (= self-exec 前の親から継承した fd) から
    /// `Pty` を組み立てる。slave は既に子 process が保持しているので `None`。
    pub fn from_master_fd(master: std::os::fd::OwnedFd) -> Self {
        Self {
            master: Some(master),
            slave: None,
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(m) = self.master.as_ref() {
            sig::clear_winch_if(m.as_raw_fd());
        }
        // master and slave (Options of OwnedFd) close via their normal Drop.
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_returns_valid_handle() {
        // mirrors ffi_wbtest.mbt: "pty_open: returns valid handle"
        let pty = Pty::open(80, 24).expect("open");
        assert!(pty.master_fd().as_raw_fd() >= 0);
    }

    #[test]
    fn master_fd_is_valid() {
        // mirrors ffi_wbtest.mbt: "pty_master_fd: returns valid fd"
        let pty = Pty::open(80, 24).expect("open");
        assert!(pty.master_fd().as_raw_fd() >= 0);
    }

    #[test]
    fn drop_succeeds() {
        // mirrors ffi_wbtest.mbt: "pty_close: succeeds"
        let pty = Pty::open(80, 24).expect("open");
        drop(pty);
    }

    #[test]
    fn spawn_with_empty_argv_errors() {
        // mirrors ffi_wbtest.mbt: "pty_spawnv: empty argv returns None"
        let err = Pty::spawn(&[], 80, 24, None).expect_err("expected Invalid");
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn resize_succeeds() {
        // mirrors ffi_wbtest.mbt: "pty_resize: succeeds on valid handle"
        let pty = Pty::open(80, 24).expect("open");
        pty.resize(120, 40).expect("resize");
    }

    /// bug fix 2026-06-11: `cwd = Some(dir)` を渡すと子が exec 前に chdir し、
    /// `pwd` の出力が指定 dir になる (= 起動元 cwd 透過の回帰防止)。
    /// テスト経路は anchor 化不可で legacy forkpty fallback に落ちるが、chdir 経路は
    /// 両 path 共通なので検証になる。
    #[test]
    fn spawn_with_cwd_changes_child_directory() {
        use std::io::Read;
        let dir = tempfile::Builder::new()
            .prefix("hyoui-pty-cwd-")
            .tempdir()
            .expect("tempdir");
        // symlink (= macOS /tmp → /private/tmp 等) を解決して比較の地ならし。
        let want = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let spawned = Pty::spawn(&["pwd"], 80, 24, Some(&want)).expect("spawn pwd");
        let mut master: std::fs::File = spawned.pty.into_master().into();
        let mut out = String::new();
        // pwd は 1 行出して exit。master EOF まで read (PTY なので EIO で終わる)。
        let _ = master.read_to_string(&mut out);
        let printed = out.trim_end_matches(['\r', '\n']).trim();
        assert_eq!(
            printed,
            want.to_string_lossy(),
            "child pwd should equal requested cwd; raw output={out:?}"
        );
    }
}
