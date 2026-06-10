//! `unsafe` boundary #1 — direct libc calls and `unsafe` `nix` calls that
//! cannot be expressed through `nix`'s safe API alone.
//!
//! Every `unsafe` block in the crate (outside [`super::signal`]) lives here.
//! Functions exported from this module are safe to call from elsewhere
//! because each `unsafe` body is paired with a `SAFETY:` justification.
//!
//! Contents:
//!
//! * [`ioctl_get_winsize`] / [`ioctl_set_winsize`] — `TIOCGWINSZ` /
//!   `TIOCSWINSZ` against any fd.
//!
//! * [`openpty_fork_anchor_exec`] — DR-0017 session anchor 化。`openpty(3)` +
//!   手動 `fork(2)` + `execvp(3)`。daemon (= setsid 済 session leader) が
//!   slave を `TIOCSCTTY` で controlling tty にし、child を **同 session・別
//!   pgrp・foreground** で起動する。child の process group が orphan でなくなり、
//!   SIGTSTP が本来のセマンティクスで動く。child の fork→exec 区間は
//!   async-signal-safe (no allocation, no locks, no Rust destructors;
//!   `execvp` 失敗時は `_exit(127)`)。
//!
//! * [`borrow_raw_fd`] and [`own_raw_fd`] — thin wrappers around
//!   `BorrowedFd::borrow_raw` / `OwnedFd::from_raw_fd` so the rest of the
//!   crate never spells those operations directly.
//!
//! * [`setrlimit_core_zero`] / [`getrlimit_core`] — `setrlimit(RLIMIT_CORE)` /
//!   `getrlimit(RLIMIT_CORE)`. R5-H12 で daemon の core dump 抑止に使う。

use std::ffi::CString;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use nix::errno::Errno;
use nix::pty::{ForkptyResult, OpenptyResult, Winsize};
use nix::unistd::{self, Pid};

use super::error::{Error, Result};

// ---------------------------------------------------------------------------
// ioctl(TIOC{G,S}WINSZ)
// ---------------------------------------------------------------------------

/// Read the current window size for `fd`.
pub fn ioctl_get_winsize(fd: BorrowedFd<'_>) -> Result<libc::winsize> {
    // SAFETY: `ws` is fully overwritten by `ioctl` on success; on failure we
    // never read it. `fd` is borrowed valid by the type system.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
    if r == -1 {
        return Err(Error::Errno(Errno::last()));
    }
    Ok(ws)
}

/// Apply the given (`cols`, `rows`) to the terminal behind `fd`.
pub fn ioctl_set_winsize(fd: BorrowedFd<'_>, cols: u16, rows: u16) -> Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `&ws` outlives the call; `fd` is borrowed valid; the ioctl
    // number is the canonical `TIOCSWINSZ`.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    if r == -1 {
        return Err(Error::Errno(Errno::last()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// openpty + manual fork + execvp (DR-0017 session anchor)
// ---------------------------------------------------------------------------

/// Result of [`openpty_fork_anchor_exec`] in the parent process.
#[derive(Debug)]
pub struct ForkedChild {
    /// PID of the new child process.
    pub child: Pid,
    /// PTY master fd owned by the parent.
    pub master: OwnedFd,
}

/// DR-0017 session anchor: `openpty(3)` + 手動 `fork(2)` + (child で)
/// `execvp(3)`。
///
/// `forkpty(3)` (= 内部で `login_tty` → `setsid` を呼び child を独立 session
/// leader にする) を **やめる**。代わりに以下の構造を取る:
///
/// 1. **parent (= daemon, setsid 済 = session leader かつ ctty 無し)** が
///    `openpty` で master/slave を取り、`ioctl(slave, TIOCSCTTY, 0)` で slave を
///    自分の controlling tty にする (= daemon が session の anchor になる)。
/// 2. `fork`。child は **同 session のまま** `setpgid(0,0)` で新 pgrp になり、
///    `tcsetpgrp(slave, getpid())` で foreground 化、slave を fd 0/1/2 に dup2
///    して `execvp`。
/// 3. parent でも race 対策に `setpgid(child, child)` + `SIGTTOU` ignore の上で
///    `tcsetpgrp(slave, child)`。その後 parent 側 slave fd は close する
///    (= child exit 時の master EOF 検出を旧 forkpty と同じに保つため、下記参照)。
///
/// 結果、child の pgrp は「同 session 内に別 pgrp の親 (daemon) がいる」状態に
/// なり orphan でなくなる。SIGTSTP が catch 可能な本来のセマンティクスで動く。
///
/// # Preconditions
///
/// **呼び出しプロセスが session leader かつ controlling tty を持たないこと**
/// (= daemonize 経路で `setsid()` 済の daemon child)。この前提を満たさないと
/// `TIOCSCTTY` が `EPERM` / `ENOTTY` で失敗する。production の `Session::start`
/// は必ず daemonize 経路 (setsid 済) で走るためこの前提を満たす。前提を満たさない
/// 呼び出し (= test が `Session::start` を直接呼ぶ等) では `TIOCSCTTY` 失敗を
/// **明示エラー (`Error::Precondition`)** として返す (= サイレント fallback しない、
/// DR-0017 §柱1)。
///
/// # async-signal-safety
///
/// child の fork→exec 区間は alloc / lock / Rust destructor を一切走らせない。
/// argv の C ポインタ配列は **fork 前に** 構築し (= `argv_ptrs`)、child では
/// `libc::execvp` を直接呼ぶ (= nix の `execvp` は post-fork に Vec を組むため
/// 使わない)。失敗時は `_exit(127)`。
pub fn openpty_fork_anchor_exec(argv: &[CString], cols: u16, rows: u16) -> Result<ForkedChild> {
    if argv.is_empty() {
        return Err(Error::Invalid("argv must not be empty"));
    }
    let ws = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // 1. openpty で master/slave を取得 (= fork はまだしない)。
    let OpenptyResult { master, slave } = nix::pty::openpty(&ws, None).map_err(Error::from)?;
    let master_raw = master.as_raw_fd();
    let slave_raw = slave.as_raw_fd();

    // 2. parent 側で slave を controlling tty にする (= daemon が anchor になる)。
    //    呼び出しプロセスが session leader かつ ctty 無しでないと EPERM/ENOTTY。
    // SAFETY: `slave_raw` は直前の openpty で得た有効な fd。`TIOCSCTTY` は
    // controlling tty 取得の canonical ioctl で、第 3 引数 0 (= force しない)。
    let rc = unsafe { libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) };
    if rc == -1 {
        // master / slave は OwnedFd の Drop で close される。
        return Err(Error::Precondition(
            "TIOCSCTTY failed: caller must be a session leader without a controlling tty \
             (= daemonize 経路で setsid 済の daemon child である前提、DR-0017 §柱1)",
        ));
    }

    // 3. argv の C ポインタ配列を **fork 前に** 構築する (= post-fork alloc 禁止)。
    //    各 CString の ptr + 末尾 null。`argv` (= 借用元) が本関数 scope 内で
    //    生存するため、ptr は fork→exec 区間で有効。
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let exec_path = argv[0].as_ptr();
    let argv_ptr = argv_ptrs.as_ptr();

    // 4. fork。
    // SAFETY: `fork(2)`。child path では async-signal-safe な操作のみ
    // (setpgid / tcsetpgrp / dup2 / close / execvp / _exit)。alloc / lock /
    // Rust destructor を一切走らせない。argv ポインタ配列は fork 前に構築済。
    let pid = unsafe { libc::fork() };
    if pid == -1 {
        let err = Errno::last();
        // master / slave は OwnedFd の Drop で close される。
        return Err(Error::from(err));
    }

    if pid == 0 {
        // ===== child path (async-signal-safe のみ) =====
        // SAFETY: 以下はすべて async-signal-safe な libc 直呼び。
        unsafe {
            // 同 session のまま新 pgrp の leader になる。
            libc::setpgid(0, 0);
            // 自 pgrp を controlling tty の foreground に。
            libc::tcsetpgrp(slave_raw, libc::getpid());
            // slave を std fd 0/1/2 に複製。
            libc::dup2(slave_raw, 0);
            libc::dup2(slave_raw, 1);
            libc::dup2(slave_raw, 2);
            // master は child では不要なので close。slave も >2 なら元 fd を close
            // (= dup2 済なので 0/1/2 だけ残ればよい)。
            libc::close(master_raw);
            if slave_raw > 2 {
                libc::close(slave_raw);
            }
            // execvp は失敗時のみ戻る。
            libc::execvp(exec_path, argv_ptr);
            // exec 失敗。Rust destructor を絶対に走らせず即終了。
            libc::_exit(127);
        }
    }

    // ===== parent path (= daemon) =====
    // race 対策: child の `setpgid(0,0)` と競合しても結果が同じになるよう、
    // parent からも `setpgid(child, child)` を試みる。EACCES は child が既に
    // exec 済の印 (= setpgid は exec 後の子に対して EACCES) なので無視可。
    // SAFETY: `setpgid` は async でない通常 syscall。
    let _ = unsafe { libc::setpgid(pid, pid) };

    // parent は background pgrp になり得るため、`tcsetpgrp` で SIGTTOU を浴びて
    // 自分が止まらないよう SIGTTOU を ignore してから呼ぶ。
    // SAFETY: `signal(SIGTTOU, SIG_IGN)` は disposition 設定の通常 syscall。
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::tcsetpgrp(slave_raw, pid);
    }

    // parent 側の slave fd は **ここで close する** (= caller には返さない)。
    //
    // Design rationale: anchor 性 (= child の pgrp が orphan でないこと) は
    // 「daemon が同 session に居て controlling tty を握っている」という
    // session/pgrp の構造で成立する。controlling tty 関連付けは session 単位で
    // 確立済 (TIOCSCTTY) で、parent の slave fd を close しても master が生きて
    // いる限り解除されない。一方、parent が slave を開いたままだと **child exit
    // 時に master が EOF を返さなくなり** (= slave を誰かが開いている)、既存の
    // 「master EOF / EIO → 子 exit 検出」path の挙動が変わる。daemon は spawn 後に
    // slave へ `tcsetpgrp` / `tcsetattr` を呼ぶ必要が現状ないため、slave を保持
    // するメリットがなく、close して旧 forkpty と同じ fd 構成 (= slave は child
    // のみ保持) を維持する。
    //
    // `slave` (OwnedFd) を drop することで close される。
    drop(slave);

    // master は OwnedFd のまま caller (= Pty) に所有権を渡す。
    Ok(ForkedChild {
        child: Pid::from_raw(pid),
        master,
    })
}

/// **テスト経由のみ** で使う fallback: 旧 `forkpty(3)` + `login_tty` 構造で child を
/// 独立 session leader にして起動する (= DR-0017 以前の構造)。
///
/// DR-0017 §柱1: production の `Session::start` は必ず daemonize 経路 (= setsid 済)
/// で走るため [`openpty_fork_anchor_exec`] の `TIOCSCTTY` 前提を満たす。一方、
/// 統合/単体テストが `Session::start` / `Pty::spawn` を **直接** 呼ぶ場合、テスト
/// プロセスは session leader でないか既に ctty を持つため anchor 化できない。
///
/// その場合のみ本 fallback に落ちる。child は session leader (= orphan pgrp) になる
/// ため **^Z は止まらない** が、テストは PTY I/O / resize / exit 検出など anchor と
/// 無関係な性質を確認するものが大半で、実害がない。anchor 性そのものを検証する
/// テストは fork + setsid 済のハーネス内で `openpty_fork_anchor_exec` を直接呼ぶ
/// (= `raw.rs` の `#[ignore]` test 群)。
///
/// サイレントではなく、呼び出し側 ([`super::pty::Pty::spawn`]) が **明示 warning を
/// stderr に出した上で** 本 fallback を使う。
pub fn forkpty_then_exec_legacy(argv: &[CString], cols: u16, rows: u16) -> Result<ForkedChild> {
    if argv.is_empty() {
        return Err(Error::Invalid("argv must not be empty"));
    }
    let ws = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: `nix::pty::forkpty` is documented `unsafe` because in the
    // child path only async-signal-safe code may run. Between fork and exec
    // we only call `execvp` (async-signal-safe) and on its failure
    // `_exit(127)` (async-signal-safe). No allocation, no locks, no Rust
    // destructors of our own.
    let result = unsafe { nix::pty::forkpty(&ws, None) }.map_err(Error::from)?;
    match result {
        ForkptyResult::Parent { child, master } => Ok(ForkedChild { child, master }),
        ForkptyResult::Child => {
            // execvp returns only on failure.
            let _ = unistd::execvp(&argv[0], argv);
            // SAFETY: `_exit` is async-signal-safe and never returns; we
            // must NOT run Rust destructors in the child.
            unsafe { libc::_exit(127) };
        }
    }
}

// ---------------------------------------------------------------------------
// setrlimit / getrlimit (RLIMIT_CORE) — R5-H12
// ---------------------------------------------------------------------------

/// Soft / hard limit pair for [`getrlimit_core`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RlimitPair {
    /// `rlim_cur` (= soft limit).
    pub soft: u64,
    /// `rlim_max` (= hard limit).
    pub hard: u64,
}

/// Force `RLIMIT_CORE` to `(0, 0)` so that `panic = "abort"` / SIGSEGV /
/// SIGABRT do **not** produce a core dump.
///
/// R5-H12: daemon process memory に `lock_token` や `HYOUI_LOCK_TOKEN` 環境変数の
/// plain-text 値が常駐するため、abort 時に `/cores/...` や `systemd-coredump` で
/// 同 UID の他 process / 管理者にこれら secret が leak する。
/// daemon 起動直後 (`Session::start`) に soft/hard 両方を 0 に固定して core dump
/// 生成を恒久抑止する。
///
/// 既存 path に core dump file が落ちているケース (= 過去の crash の残骸) は
/// 触らない — これは「次の crash で書かれる core dump」を抑止する操作。
pub fn setrlimit_core_zero() -> Result<()> {
    let rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `&rlim` outlives the syscall; `RLIMIT_CORE` is a valid constant.
    // `setrlimit` writes nothing through the pointer (read-only argument).
    let r = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &rlim) };
    if r == -1 {
        return Err(Error::Errno(Errno::last()));
    }
    Ok(())
}

/// Read the current `RLIMIT_CORE` soft/hard pair. R5-H12 test 用。
pub fn getrlimit_core() -> Result<RlimitPair> {
    // SAFETY: `rlim` is fully overwritten on success; on failure we never read
    // it. `RLIMIT_CORE` is a valid resource constant.
    let mut rlim: libc::rlimit = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut rlim) };
    if r == -1 {
        return Err(Error::Errno(Errno::last()));
    }
    // `libc::rlim_t` は macOS / Linux で `u64` (= `RlimitPair` の field と同型)。
    // 暗黙の同型代入で OK。
    Ok(RlimitPair {
        soft: rlim.rlim_cur,
        hard: rlim.rlim_max,
    })
}

// ---------------------------------------------------------------------------
// fd-wrapping helpers
// ---------------------------------------------------------------------------

/// Take ownership of a raw fd. Caller must guarantee `raw` is a valid,
/// currently-open fd with no other Rust owner.
///
/// `unsafe` を呼び出し側に伝播させないための薄い wrapper。`hyoui-cli` の
/// `forbid(unsafe_code)` 下で daemon の parent → child fd 継承を扱う等に使う。
pub fn own_raw_fd(raw: RawFd) -> OwnedFd {
    // SAFETY: delegated to the caller's contract.
    unsafe { OwnedFd::from_raw_fd(raw) }
}

/// Borrow a fd by raw integer. Caller must guarantee the fd outlives the
/// returned `BorrowedFd`. Used by tests that exercise error paths on
/// closed fds.
#[allow(dead_code)] // only consumed by `#[cfg(test)]` callers
pub(crate) fn borrow_raw_fd<'a>(raw: RawFd) -> BorrowedFd<'a> {
    // SAFETY: delegated to the caller's contract.
    unsafe { BorrowedFd::borrow_raw(raw) }
}

/// Redirect `stdin` (= fd 0) to `/dev/null` (= daemon 化慣例の subset)。
///
/// `dup2(devnull, 0)` で fd 0 を /dev/null に置換する。`hyoui-cli` 等の
/// `forbid(unsafe_code)` crate から safe に呼べるよう本 module 経由で wrap する。
///
/// nix 0.31 の `dup2(src: BorrowedFd, dst: &mut OwnedFd)` は fd 0 を OwnedFd 化
/// できない (= close 時に std stdin が無効化される) ため、libc::dup2 を直接 unsafe で
/// 呼ぶ。本 module は `unsafe` boundary なので OK。
///
/// # Errors
///
/// `/dev/null` を open できない / `dup2` syscall が失敗した場合に [`Error`] を返す。
pub fn redirect_stdin_to_devnull() -> Result<()> {
    let devnull = std::fs::File::open("/dev/null").map_err(Error::from)?;
    let devnull_fd = devnull.as_raw_fd();
    // SAFETY: libc::dup2 は raw fd を取る同期 syscall。devnull_fd は本関数内で生存
    // (= devnull が drop されるのは関数終了時、dup2 は scope 内同期完結)。fd 0 は
    // POSIX で stdin として available、dup2 で置換しても OS が close する責務。
    let rc = unsafe { libc::dup2(devnull_fd, 0) };
    if rc < 0 {
        return Err(Error::from(Errno::last()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DR-0017 session anchor PoC (Rust 版) — findings の C PoC の移植
// ---------------------------------------------------------------------------

#[cfg(test)]
mod anchor_tests {
    use super::*;

    /// harness 子 (= setsid 済 anchor 役) の終了コード。`_exit` で返す。
    /// async-signal-safe にするため harness 子側では panic / alloc 系を避ける。
    mod code {
        pub const OK: i32 = 0;
        pub const SPAWN_FAILED: i32 = 10;
        pub const CHILD_IS_SESSION_LEADER: i32 = 11; // grandchild が session leader (= anchor 失敗)
        pub const CHILD_PGRP_ORPHAN: i32 = 12; // grandchild の pgrp が orphan (= anchor 失敗)
        pub const TSTP_NOT_STOPPED: i32 = 13; // kill -TSTP で WUNTRACED stop しない
        pub const LDISC_NOT_STOPPED: i32 = 14; // master への 0x1a write で stop しない
    }

    /// harness 子 process で走る本体。setsid → anchor spawn → 検証。
    /// 成功なら `code::OK`、失敗は具体的な code を `_exit` で返す。
    ///
    /// この関数は fork 後の child で **単独 thread** として走る (= test runner の
    /// 他 thread は fork で複製されない)。よって libc::setsid 等を安全に呼べる。
    fn harness_child_body() -> ! {
        // setsid で新 session leader (= ctty 無し) になる。
        // SAFETY: fork 直後の単独 thread。setsid は session 操作の通常 syscall。
        let sid = unsafe { libc::setsid() };
        if sid == -1 {
            // 既に process group leader だと EPERM。test runner が process group
            // leader である可能性は低いが、その場合は anchor 検証不能なので
            // SPAWN_FAILED として扱う (= 上位が ignore せず明確に失敗を見る)。
            // SAFETY: _exit は async-signal-safe。
            unsafe { libc::_exit(code::SPAWN_FAILED) };
        }

        // anchor 経路で /bin/sleep を起動する (= 長生きする cooked-mode 子)。
        let argv = [
            std::ffi::CString::new("/bin/sleep").unwrap(),
            std::ffi::CString::new("60").unwrap(),
        ];
        let forked = match openpty_fork_anchor_exec(&argv, 80, 24) {
            Ok(f) => f,
            // SAFETY: _exit は async-signal-safe。
            Err(_) => unsafe { libc::_exit(code::SPAWN_FAILED) },
        };
        let child = forked.child.as_raw();
        let master = forked.master;

        // exec が終わって sleep が走り出すまで少し待つ。
        std::thread::sleep(std::time::Duration::from_millis(150));

        // 検証 1: grandchild は session leader で **ない** こと。
        // getsid(child) != child なら session leader でない。
        // SAFETY: getsid は通常 syscall。
        let child_sid = unsafe { libc::getsid(child) };
        if child_sid == child {
            unsafe { libc::_exit(code::CHILD_IS_SESSION_LEADER) };
        }

        // 検証 2: grandchild の pgrp が orphan で **ない** こと。
        // anchor 役 (= 本 harness 子) が同 session の別 pgrp の親として存在するため
        // orphan ではないはず。直接 orphan 判定の syscall は無いので、
        // 「kill -TSTP で実際に止まるか」(= 検証 3) で機能的に確認する。
        // ここでは pgrp が child 自身 (= setpgid(0,0) 成功) であることだけ確認。
        // SAFETY: getpgid は通常 syscall。
        let child_pgid = unsafe { libc::getpgid(child) };
        if child_pgid != child {
            unsafe { libc::_exit(code::CHILD_PGRP_ORPHAN) };
        }

        // 検証 3: kill -TSTP で grandchild が WUNTRACED stop すること。
        // SAFETY: kill は通常 syscall。
        unsafe { libc::kill(child, libc::SIGTSTP) };
        if !wait_stopped(child) {
            unsafe { libc::_exit(code::TSTP_NOT_STOPPED) };
        }
        // 起こしておく (次の検証のため)。
        unsafe { libc::kill(child, libc::SIGCONT) };
        // continued を回収。
        let _ = wait_continued(child);

        // 検証 4: master への 0x1a (= ^Z byte) write で line discipline が
        // SIGTSTP を生成し、grandchild が stop すること (= 実際の ^Z 経路)。
        let z: [u8; 1] = [0x1a];
        // SAFETY: master は有効 fd、write は 1 byte の通常 I/O。
        let _ = unsafe { libc::write(master.as_raw_fd(), z.as_ptr() as *const libc::c_void, 1) };
        if !wait_stopped(child) {
            unsafe { libc::_exit(code::LDISC_NOT_STOPPED) };
        }

        // cleanup: grandchild を確実に終了。
        unsafe {
            libc::kill(child, libc::SIGCONT);
            libc::kill(child, libc::SIGKILL);
        }
        let mut st: libc::c_int = 0;
        unsafe { libc::waitpid(child, &mut st, 0) };

        // SAFETY: _exit は async-signal-safe。
        unsafe { libc::_exit(code::OK) };
    }

    /// `pid` が WUNTRACED で stop するまで短く poll する。
    fn wait_stopped(pid: i32) -> bool {
        for _ in 0..100 {
            let mut st: libc::c_int = 0;
            // SAFETY: waitpid は通常 syscall。WNOHANG | WUNTRACED。
            let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG | libc::WUNTRACED) };
            if r == pid && libc::WIFSTOPPED(st) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// `pid` の Continued を回収する (= WCONTINUED)。best-effort。
    fn wait_continued(pid: i32) -> bool {
        for _ in 0..50 {
            let mut st: libc::c_int = 0;
            // SAFETY: waitpid は通常 syscall。WNOHANG | WCONTINUED。
            let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG | libc::WCONTINUED) };
            if r == pid && libc::WIFCONTINUED(st) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// DR-0017 §柱1 の PoC を Rust で再現する統合テスト。
    ///
    /// test runner プロセスは session leader でないか既に ctty を持つため、
    /// `openpty_fork_anchor_exec` を直接呼べない。そこで **fork した harness 子で
    /// `setsid()` してから** anchor spawn + 検証を行う (= findings の C PoC と同じ
    /// 構造)。harness 子は検証結果を exit code で返す。
    ///
    /// 検証項目 (= harness 子内):
    /// - grandchild が session leader で **ない** (= anchor 化された)
    /// - grandchild の pgrp が自分 (= `setpgid(0,0)` 成功、orphan でない)
    /// - `kill -TSTP` で grandchild が WUNTRACED stop する
    /// - master への `0x1a` write (= ^Z byte) で grandchild が stop する
    ///
    /// PTY + fork を使うため `#[ignore]` (= CI 安定性。ローカルで
    /// `cargo test -- --ignored` で必ず通すこと、DR-0017 検証要件)。
    #[ignore = "PTY + fork PoC、ローカルで --ignored 実行 (DR-0017 §柱1 検証)"]
    #[test]
    fn session_anchor_makes_child_stoppable() {
        // SAFETY: fork。child では async-signal-safe な harness_child_body のみ
        // (= 単独 thread、libc 直呼びと _exit のみ)。parent は waitpid で結果回収。
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            harness_child_body();
        }
        // parent: harness 子の終了コードを回収。
        let mut st: libc::c_int = 0;
        // SAFETY: waitpid は通常 syscall。
        let r = unsafe { libc::waitpid(pid, &mut st, 0) };
        assert_eq!(r, pid, "waitpid harness child");
        assert!(
            libc::WIFEXITED(st),
            "harness child did not exit normally (raw status = {st})"
        );
        let exit = libc::WEXITSTATUS(st);
        assert_eq!(
            exit,
            code::OK,
            "anchor PoC failed with code {exit} \
             (10=spawn, 11=child-is-session-leader, 12=pgrp-orphan, \
             13=TSTP-not-stopped, 14=ldisc-0x1a-not-stopped)"
        );
    }
}
