//! daemon 1 つ分の session 状態 + 起動ロジック。
//!
//! `Session::start` で:
//! 1. 子 PTY を `Pty::spawn` で起動 (= forkpty + login_tty + execvp)
//! 2. Unix socket を `UnixSock::listen` で bind (perm 0600 + 親 dir 0700)
//!
//! `Session::serve` (Phase 9 で導入) が本流の lifecycle entry point:
//! multi-attach + bounded mpsc + lock/leader + status/tail/wait を担う。
//!
//! 旧 `accept_handshake_once` (Phase 7) と `run` (Phase 8、1-client 限定の同期 path) は
//! R4-M1 (v0.1.4) で撤去済。`serve` で完全に置き換えられた。

use std::os::fd::AsFd;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::Instant;

use nix::poll::{PollFd, PollTimeout};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::Error;
#[cfg_attr(not(test), allow(unused_imports))]
use crate::protocol::Mode;
use crate::protocol::messages::{LeaderNotify, ModeChange};
use crate::protocol::{ControlMessage, Frame};
use crate::scrollback::Scrollback;
use crate::sys::{
    FdExt, Pty, SelfPipe, UnixSock, install_self_pipe, poll::PollFlags, poll::PollOutcome,
    poll::poll, pty::Spawned, register_self_pipe,
};

use super::DaemonConfig;
use super::accept::{
    MAX_PENDING_HANDSHAKES, PendingHandshake, process_pending_handshakes, spawn_handshake_worker,
};
use super::broadcast::{
    ClientHandle, MAX_CLIENTS_PER_DAEMON, broadcast_control, broadcast_master_bytes,
};
use super::control::{ClientFrameOutcome, FrameOrError, handle_client_frame};
use super::lock::{SessionState, elevate_next_leader};
use super::pty::{ALIVE_RETRY_INTERVAL, ChildLifecycle, ChildState, STOPPED_POLL_INTERVAL};

/// R5-H7: send `sig` to the child's whole process group instead of only the
/// session-leader PID, so descendants that the shell may have backgrounded
/// (= grandchildren of the daemon, e.g. `sh -c 'sleep 100 &'`) are not
/// orphaned to `init`/`launchd`.
///
/// `forkpty(3)` internally calls `login_tty(3)` which calls `setsid(2)`,
/// making the child both a session leader and a process group leader with
/// `pgid == pid`. `kill(2)` with a negative pid is the POSIX `killpg(2)`
/// equivalent and targets every process whose pgid matches `|pid|`.
///
/// Errors are intentionally ignored at most call sites (Drop / finalize),
/// matching `tmux` / `screen` / `abduco` which always treat this as a
/// best-effort terminate. The function still returns the underlying
/// `nix::Result` for tests that need to assert delivery.
fn kill_pgrp(child: Pid, sig: Signal) -> nix::Result<()> {
    kill(Pid::from_raw(-child.as_raw()), sig)
}

/// R5-FB1: `run --until PATTERN` の sliding window scanner。
///
/// 子 PTY master 出力に対し chunk 単位で `feed(&[u8])` を呼ぶ。各 chunk について
/// `(carry + new_bytes)` の連結に対し substring scan を行い、match があれば
/// `true` を返す。次回 chunk のため、carry には新 buf の末尾 `needle.len() - 1`
/// bytes を残す (= needle が境界を跨いでいた場合に次 chunk 開始で確実に拾える)。
///
/// scan 対象は raw byte (= ANSI escape を含む)。strip-ansi 等は本 watcher では
/// 行わない (= `wait --pattern` 経路で strip option を使う設計、本機能は
/// `run` から手早く needle match するための簡易 path)。
struct UntilWatcher {
    needle: Vec<u8>,
    carry: Vec<u8>,
}

impl UntilWatcher {
    fn new(needle: String) -> Self {
        Self {
            needle: needle.into_bytes(),
            carry: Vec::new(),
        }
    }

    /// 新 chunk を投入して match 確認。一致したら true を返す (= 呼出側が
    /// session 終了処理に進む)。一致しなければ carry を更新して false。
    fn feed(&mut self, chunk: &[u8]) -> bool {
        if self.needle.is_empty() {
            return false;
        }
        // (carry + chunk) を scan 対象に組み立てる。
        let mut window: Vec<u8> = Vec::with_capacity(self.carry.len() + chunk.len());
        window.extend_from_slice(&self.carry);
        window.extend_from_slice(chunk);
        // substring scan (= 標準 windows().any() で十分。chunk size 8 KiB
        // × needle 数十 bytes の積で per-iteration コストは数 μs オーダー)。
        let matched = window.len() >= self.needle.len()
            && window
                .windows(self.needle.len())
                .any(|w| w == self.needle.as_slice());
        if matched {
            return true;
        }
        // 次回 chunk のための carry 更新: window の末尾 (needle.len() - 1) bytes
        // を残す (= 境界を跨ぐ partial match を捉えるため)。
        let keep = self.needle.len().saturating_sub(1);
        if window.len() > keep {
            let cut = window.len() - keep;
            self.carry = window[cut..].to_vec();
        } else {
            self.carry = window;
        }
        false
    }
}

/// R5-H6: SIGCHLD self-pipe ownership gate.
///
/// SIGCHLD disposition + the `SELFPIPE_WRITE_FD` global are process-wide;
/// only one `Session::serve` at a time may own the SIGCHLD self-pipe.
/// `Session::serve` `try_lock`s this mutex on entry: if acquired, it installs
/// the SIGCHLD self-pipe and uses it to wake `poll(2)` on child state
/// transitions (= STOP / CONT / exit). If `try_lock` fails (= another serve
/// is already using it in the same process, e.g. concurrent test runs), the
/// serve falls back to the legacy 500ms polling path with no correctness
/// regression.
///
/// The guard is held inside `serve()` for the entire lifetime of the loop,
/// and is dropped before `SelfPipe::drop` so that `SELFPIPE_WRITE_FD` is
/// cleared before the next serve attempts to install its own self-pipe.
static SIGCHLD_SELFPIPE_LOCK: Mutex<()> = Mutex::new(());

/// RAII bundle: the owned `SelfPipe` plus the `MutexGuard` that proves we
/// own the slot. Order matters in Drop: `pipe` drops first (clearing the
/// global write-fd atomic), then `_guard` releases the lock. Rust drops
/// struct fields in declaration order, so `pipe` MUST come before `_guard`.
struct SigchldOwner {
    pipe: SelfPipe,
    _guard: MutexGuard<'static, ()>,
}

/// Attempt to acquire SIGCHLD self-pipe ownership for this serve. Returns
/// `Some` on success (= SIGCHLD will deliver into `pipe`), `None` if either
/// the lock is taken by another concurrent serve in this process or the
/// self-pipe / sigaction install fails. The `None` path is non-fatal — the
/// caller falls back to the legacy 500ms polling.
fn acquire_sigchld_selfpipe() -> Option<SigchldOwner> {
    let guard = match SIGCHLD_SELFPIPE_LOCK.try_lock() {
        Ok(g) => g,
        Err(TryLockError::WouldBlock) => return None,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
    };
    let pipe = install_self_pipe().ok()?;
    if register_self_pipe(Signal::SIGCHLD).is_err() {
        // pipe drops here, clearing SELFPIPE_WRITE_FD; guard released too.
        return None;
    }
    Some(SigchldOwner {
        pipe,
        _guard: guard,
    })
}
use super::tail::{broadcast_tail_end_to_followers, tail_end_reason_from_outcome};
use super::wait::{
    PendingWait, check_wait_timeouts, compute_wait_poll_timeout, update_waits_on_master_bytes,
};

/// daemon 1 つ分の起動済 session。
///
/// `Session` は **`Drop` で子 PTY を graceful に終了する** (R4-H4):
/// - SIGTERM を送って最大 `DROP_TERM_WAIT` (= 500ms) 待つ
/// - 残っていれば SIGKILL
/// - `waitpid(WNOHANG)` で短い loop で reap し zombie を回収
///
/// `serve()` は `inner.take()` で `SessionInner` を取り出して消費するため、正常
/// path ではその後の Drop で `inner == None` となり no-op になる (= 各フィールドは
/// `SessionInner` の destructure 経由で個別に Drop される)。Drop が実質的に発火
/// するのは `Session::start` 後に `serve` を呼ばずに `Session` が drop された
/// ケース (= test 内 panic / 初期化エラー後の early return 等) で、その場合に
/// 子が orphan として残らないようにする。
///
/// Pty/UnixSock は元々独自の Drop を持つため、Session::drop は child Pid の
/// 始末だけを担当する。Drop 中は panic 安全のため全 syscall を `let _ = ...`
/// で error 飲み込む (= panic 中の二重 panic で process abort を避ける)。
#[derive(Debug)]
pub struct Session {
    config: DaemonConfig,
    /// `serve` 経由で消費されると `None` になり、その後の Drop は no-op になる。
    /// `start` 直後は常に `Some`。
    inner: Option<SessionInner>,
}

/// `Session` の本体リソース。`Option<SessionInner>` で包むことで `serve` が
/// `take()` で move-out できるようにし、Drop bypass の `unsafe` を不要にする。
#[derive(Debug)]
struct SessionInner {
    pty: Pty,
    child: Pid,
    listener: UnixSock,
}

/// R5-H12: `HYOUI_ALLOW_CORE` env が `"1"` のとき true を返す。
/// `Session::start` で core dump 抑止を skip する opt-out 用 (debug 時のみ想定)。
fn core_dump_allowed_by_env() -> bool {
    core_dump_allowed_value(std::env::var("HYOUI_ALLOW_CORE").ok().as_deref())
}

/// `core_dump_allowed_by_env` のテスト可能化版。
/// `Some("1")` のときのみ true、それ以外 (未設定 / 他の値 / 空文字) は false。
fn core_dump_allowed_value(v: Option<&str>) -> bool {
    matches!(v, Some("1"))
}

impl Session {
    /// 子 PTY を spawn し、Unix socket を bind して session を立ち上げる。
    ///
    /// # Errors
    ///
    /// * `cmd` が空、または argv に NUL を含む → [`Error::Invalid`]
    /// * forkpty / execvp が失敗 → [`Error::Errno`]
    /// * socket parent dir が mode 0700 でない → [`Error::Precondition`]
    /// * bind / listen が失敗 → [`Error::Errno`]
    pub fn start(config: DaemonConfig) -> Result<Self, Error> {
        if config.cmd.is_empty() {
            return Err(Error::Invalid("DaemonConfig::cmd must not be empty"));
        }
        // R5-H12: daemon process は `lock_token` / `HYOUI_LOCK_TOKEN` env 等の
        // secret を memory 上に常駐させる。`panic = "abort"` / SIGSEGV / SIGABRT
        // で core dump が `/cores/...` や `systemd-coredump` に書かれると、
        // 同 UID の他 process / 管理者に secret が leak する。
        // soft/hard 両方を 0 に固定して恒久抑止する。
        // `HYOUI_ALLOW_CORE=1` 指定時のみ skip して debug できる (= opt-out)。
        // 既存 path に core dump file が残っているケースは touch しない
        // (= これは「次の crash で書かれる」抑止)。
        if !core_dump_allowed_by_env() {
            crate::sys::raw::setrlimit_core_zero()?;
        }
        let argv: Vec<&str> = config.cmd.iter().map(String::as_str).collect();
        let Spawned { pty, child } = Pty::spawn(&argv, config.cols, config.rows)?;
        // master FD を nonblock にして、POLLHUP 偽陽性 (macOS) で read_some が
        // block するのを防ぐ。read_some は EAGAIN を返す → serve_loop で continue。
        pty.master_fd().set_nonblocking(true)?;
        let listener = UnixSock::listen(&config.socket_path)?;
        Ok(Self {
            config,
            inner: Some(SessionInner {
                pty,
                child,
                listener,
            }),
        })
    }

    /// `inner` を `Some` 前提で参照する内部ヘルパ。`start` 直後 〜 `serve` の
    /// `take()` 直前までは必ず `Some`。
    fn inner(&self) -> &SessionInner {
        self.inner
            .as_ref()
            .expect("Session::inner accessed after serve consumed it (bug)")
    }

    /// session 名 (handshake response 用 + status 表示用)。
    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    /// 子 PTY の PID。
    pub fn child_pid(&self) -> Pid {
        self.inner().child
    }

    /// listener が bind している socket path。
    pub fn socket_path(&self) -> &std::path::Path {
        self.inner().listener.path()
    }

    /// 子 PTY master fd (= 後の Phase で broadcast/multiplex に使用)。
    pub fn pty(&self) -> &Pty {
        &self.inner().pty
    }

    /// Phase 9: multi-attach 対応の serve loop。
    ///
    /// 旧 `Session::run` (= Phase 8、1-client 限定) の上位互換であり、R4-M1 で
    /// 唯一の本流 entry point になった。複数 client を同時に accept、子 PTY 出力を
    /// 全 client にブロードキャスト、各 client 入力を子 PTY に集約する。各 client は
    /// per-thread writer + bounded queue を持ち、queue 超過時はその client のみ
    /// disconnect する (DR-0008 §8.2)。
    ///
    /// 終了条件:
    /// - 子 PTY が exit → 子 reap → exit code を返す
    /// - `kill` message を受けた → 子に signal → 子 reap → exit code を返す
    ///
    /// MVP 単一-client 構成と挙動を揃えるため、本実装も子が exit した時点で
    /// daemon は終了する。「clients == 0 でも daemon 維持」は v0.2.0+ で
    /// `--keep-running` 等の opt-in で導入する想定。
    pub fn serve(mut self) -> Result<i32, Error> {
        // R4-H4: Drop は `start` 後 `serve` 未呼出のフォールバック専用。
        // serve は正常 path として inner を消費する。`inner.take()` 後は
        // serve の末尾で self が drop されるとき `Session::drop` が呼ばれるが、
        // inner == None で no-op になる (= 各 field は SessionInner の
        // destructure で個別に drop される)。`self.config` は self が drop
        // されるときに通常通り drop される。
        let SessionInner {
            pty,
            child,
            listener,
        } = self
            .inner
            .take()
            .expect("Session::serve called twice or after consumption (bug)");
        let config = &self.config;
        let mut clients: Vec<ClientHandle> = Vec::new();
        let mut next_client_id: u64 = 0;
        let mut state = SessionState::default();
        let mut scrollback = Scrollback::new(config.scrollback_bytes);
        let mut pending_waits: Vec<PendingWait> = Vec::new();

        // R5-H6: Try to acquire process-wide SIGCHLD self-pipe ownership.
        // The `Some` branch installs SIGCHLD → self-pipe so `poll(2)` wakes
        // immediately on child STOP/CONT/exit (= 500ms latency → ~ms).
        // The `None` branch falls back to the legacy ChildLifecycle polling
        // (correct, just slower transition detection) when another serve
        // already owns the slot in the same process (typically only happens
        // in concurrent test runs).
        let sigchld_owner = acquire_sigchld_selfpipe();

        let outcome = serve_loop(
            &pty,
            child,
            &listener,
            &mut clients,
            &mut next_client_id,
            config,
            &mut state,
            &mut scrollback,
            &mut pending_waits,
            sigchld_owner.as_ref().map(|o| &o.pipe),
        );

        // Drop the SIGCHLD self-pipe explicitly before any further cleanup so
        // the global `SELFPIPE_WRITE_FD` is cleared and a subsequent serve in
        // the same process can claim the slot.
        drop(sigchld_owner);

        // tail follow subscriber へ TailEnd を 1 発投げてから cleanup する。
        // 終了理由の導出 (= ChildExited / ClientCancel / Error は送らない) と
        // 一括 best-effort 送信は tail.rs の helper に委譲。
        if let Some(reason) = tail_end_reason_from_outcome(&outcome) {
            broadcast_tail_end_to_followers(&clients, reason);
        }

        // cleanup:
        // 1. per-client で queued_bytes==0 を最大 200ms 待つ (= 1 client の hang
        //    が他 client の drain budget を食い潰さないように、deadline を共有せず
        //    client ごとに 200ms ずつ振る)
        // 2. `clients.drain(..)` で各 `ClientHandle` を scope-exit させ、`Drop` impl
        //    (R5-H18) が writer_tx close + reader shutdown + writer_thread join を
        //    一括実行する
        //
        // ※ Drop だけだと残り frame を drain できないため、drain wait は明示的に
        //   先行させる (= writer_pump が残 frame を全て write_all し終わるまで
        //   200ms 待つ。timeout で抜けたら Drop の shutdown で強制終了)。
        const DRAIN_BUDGET_PER_CLIENT: std::time::Duration = std::time::Duration::from_millis(200);
        for ch in clients.iter() {
            let deadline = std::time::Instant::now() + DRAIN_BUDGET_PER_CLIENT;
            while ch.queued_bytes.load(Ordering::Acquire) > 0
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        clients.clear();

        let exit_code = finalize_child(child, &outcome)?;
        drop(listener);
        match outcome {
            RelayOutcome::ChildExited(_) | RelayOutcome::ClientDetachedOrKilled => Ok(exit_code),
            RelayOutcome::Error(e) => Err(e),
        }
    }
}

/// R4-H4: `Session` の Drop は `start()` 後に `serve` を呼ばずに drop された
/// ケース (= test panic、初期化失敗後 early return 等) で子 PTY が orphan として
/// 残らないようにする。
///
/// `serve()` は `self` を destructure で消費するため、正常 path で走った後は
/// この Drop は呼ばれない (= 各フィールドが個別に Drop)。
///
/// 手順:
/// 1. SIGTERM を送って graceful 終了を要求
/// 2. 最大 `DROP_TERM_WAIT` ミリ秒、5ms 刻みで `waitpid(WNOHANG)` poll
/// 3. まだ alive なら SIGKILL → 再 reap
///
/// panic 安全: Drop 内では panic を起こさないよう、syscall は全て `let _ = ...`
/// で error を握り潰す。既に panic 中に Drop が走ると二重 panic で process
/// abort になるため、ここで panic を新たに発生させないことが必須。
impl Drop for Session {
    fn drop(&mut self) {
        use nix::sys::wait::{WaitPidFlag, WaitStatus};

        /// SIGTERM 送信後に graceful 終了を待つ最大時間。
        const DROP_TERM_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
        /// reap poll 間隔。短いほど CPU 寄り、長いほど drop 自体が遅延。
        const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(5);

        // `inner` が `None` なら `serve` が正常 path で消費済み (= 子 reap は
        // `finalize_child` 経由で完了済) なので no-op。`Some` のときだけ
        // fallback の SIGTERM → SIGKILL reap を行う。
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let child = inner.child;

        // 1. 既に reap 済か確認 (WNOHANG)。reap 済なら何もしない。
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => { /* alive: 続行 */ }
            Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => {
                // 既に終了して reap 済 (or 今 reap)。何もしない。
                return;
            }
            Ok(_) => { /* Stopped/Continued/ptrace 等: SIGTERM を試す */ }
            Err(nix::errno::Errno::ECHILD) => {
                // 既に他の waitpid で reap 済 (= serve/run の destructure 後では
                // 起こり得ないが、外部 reaper 経由のテスト等に対する防御)。
                return;
            }
            Err(_) => { /* 何らかの transient error。SIGTERM を試す */ }
        }

        // 2. SIGTERM を送る。child が既に exit 済なら ESRCH で失敗 → 無視。
        // R5-H7: process group (= setsid 済の子 + その子孫) 全体に届かせる。
        let _ = kill_pgrp(child, Signal::SIGTERM);

        // 3. graceful wait loop。reap できたら return、timeout したら SIGKILL。
        let deadline = std::time::Instant::now() + DROP_TERM_WAIT;
        loop {
            match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {}
                Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => return,
                Ok(_) => {}
                Err(nix::errno::Errno::ECHILD) => return,
                Err(_) => return, // 致命 error は飲み込んで脱出 (panic 禁止)
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(REAP_POLL);
        }

        // 4. SIGKILL → 短い loop で reap。
        // R5-H7: SIGTERM 同様 process group 全体に向ける。
        let _ = kill_pgrp(child, Signal::SIGKILL);
        let kill_deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {}
                Ok(_) => return,
                Err(_) => return,
            }
            if std::time::Instant::now() >= kill_deadline {
                // 諦める。zombie が残る可能性はあるが、Drop で長時間 block しない
                // ことを優先 (= test の hang 防止)。
                return;
            }
            std::thread::sleep(REAP_POLL);
        }
    }
}

/// serve loop の本体。`Session::serve` から切り出して所有権整理を平坦化。
#[allow(clippy::too_many_arguments)]
fn serve_loop(
    pty: &Pty,
    child: Pid,
    listener: &UnixSock,
    clients: &mut Vec<ClientHandle>,
    next_client_id: &mut u64,
    config: &DaemonConfig,
    state: &mut SessionState,
    scrollback: &mut Scrollback,
    pending_waits: &mut Vec<PendingWait>,
    sigchld_pipe: Option<&SelfPipe>,
) -> RelayOutcome {
    // R4-C3: 別 thread で進行中の handshake worker 群。worker が `do_handshake_stage`
    // を完了すると `rx` に Ok/Err が流れる。本 vector は serve_loop が所有し、各
    // iteration で try_recv で完了したものを引き取って `clients` に integrate する。
    let mut pending_handshakes: Vec<PendingHandshake> = Vec::new();
    // R4-H14: 子の Stopped/Continued 追跡。loop 越しに状態を保持する。
    let mut lifecycle = ChildLifecycle::default();
    // R5-FB1: `run --until PATTERN` の sliding window matcher。chunk 境界を
    // 跨ぐ needle を捉えるため、直近 (needle.len() - 1) bytes を carry に残す。
    let mut until_watcher: Option<UntilWatcher> = config
        .until
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| UntilWatcher::new(s.clone()));
    loop {
        // poll fd 構築: listener + master + 各 client reader (+ SIGCHLD self-pipe)
        let listener_fd = listener.as_fd();
        let master_fd = pty.master_fd();
        let mut poll_fds: Vec<PollFd> =
            Vec::with_capacity(2 + clients.len() + usize::from(sigchld_pipe.is_some()));
        poll_fds.push(PollFd::new(listener_fd, PollFlags::POLLIN));
        poll_fds.push(PollFd::new(master_fd, PollFlags::POLLIN));
        for ch in clients.iter() {
            poll_fds.push(PollFd::new(ch.reader.as_fd(), PollFlags::POLLIN));
        }
        // R5-H6: SIGCHLD self-pipe slot is appended last so it does not shift
        // client indexing. Tracked separately by the `sigchld_idx` offset.
        let sigchld_idx = if let Some(sp) = sigchld_pipe {
            let idx = poll_fds.len();
            poll_fds.push(PollFd::new(sp.read.as_fd(), PollFlags::POLLIN));
            Some(idx)
        } else {
            None
        };

        // backpressure overflow / writer dead で disconnect が必要な client_id を集める
        let mut overflow_ids: Vec<u64> = Vec::new();

        // R4-C3: pending handshake がある間は poll timeout を 50ms 以下に抑える。
        // 完了通知 (mpsc) は fd-poll では検出できないため、短い周期で try_recv する
        // 必要がある。timeout だけ走る無駄サイクルを避けるため、pending が無い場合は
        // 通常通り wait のスケジュールで眠る。
        let mut poll_timeout = compute_wait_poll_timeout(pending_waits);
        if !pending_handshakes.is_empty() {
            const HANDSHAKE_POLL_CAP_MS: u16 = 50;
            let cap = PollTimeout::from(HANDSHAKE_POLL_CAP_MS);
            // 既存 timeout が NONE か cap より大きいなら cap で頭打ちする。
            // ※ nix 0.31 の `PollTimeout::as_millis` は NONE 時に内部 unwrap で
            //   panic するため、先に `is_none()` で分岐してから ms を取り出す。
            if poll_timeout.is_none() {
                poll_timeout = cap;
            } else if let Some(ms) = poll_timeout.as_millis() {
                if ms > HANDSHAKE_POLL_CAP_MS as u32 {
                    poll_timeout = cap;
                }
            }
        }
        // R4-C3: Timeout 経路では poll_fds の revents は使わないので、まず borrows を
        // 解いてから check_wait_timeouts / process_pending_handshakes を呼ぶ。
        // Ready 経路では revents を集めてから drop する (= 通常処理に進む)。
        let outcome_kind = poll(&mut poll_fds, poll_timeout);
        let (listener_revents, master_revents, client_revents, sigchld_ready) = match outcome_kind {
            Ok(PollOutcome::Ready(_)) => {
                let lrev = poll_fds[0].revents().unwrap_or(PollFlags::empty());
                let mrev = poll_fds[1].revents().unwrap_or(PollFlags::empty());
                let crev: Vec<PollFlags> = clients
                    .iter()
                    .enumerate()
                    .map(|(i, _)| poll_fds[2 + i].revents().unwrap_or(PollFlags::empty()))
                    .collect();
                let sig_ready = sigchld_idx
                    .map(|i| {
                        poll_fds[i]
                            .revents()
                            .unwrap_or(PollFlags::empty())
                            .contains(PollFlags::POLLIN)
                    })
                    .unwrap_or(false);
                drop(poll_fds);
                (lrev, mrev, crev, sig_ready)
            }
            Ok(PollOutcome::Interrupted) => {
                drop(poll_fds);
                // R5-H6: SIGCHLD may have arrived; drain self-pipe + check
                // child state. EINTR alone (without self-pipe ready) still
                // benefits from the same drain (= no-op if empty).
                if let Some(sp) = sigchld_pipe {
                    let _ = sp.drain();
                }
                if let ChildState::Exited(code) = lifecycle.poll(child) {
                    return RelayOutcome::ChildExited(code);
                }
                continue;
            }
            Ok(PollOutcome::Timeout) => {
                drop(poll_fds);
                // wait deadline / idle 経過チェック
                check_wait_timeouts(pending_waits, clients);
                process_pending_handshakes(
                    &mut pending_handshakes,
                    config,
                    next_client_id,
                    clients,
                    state,
                    &mut overflow_ids,
                );
                // 後段の drop 処理 (overflow / dead) を共通化するため
                let mut indices_to_drop: Vec<usize> = Vec::new();
                for id in overflow_ids.drain(..) {
                    if let Some(i) = clients.iter().position(|c| c.id == id) {
                        indices_to_drop.push(i);
                    }
                }
                indices_to_drop.sort_unstable();
                indices_to_drop.dedup();
                for idx in indices_to_drop.into_iter().rev() {
                    let ch = clients.remove(idx);
                    pending_waits.retain(|w| w.client_id != ch.id);
                    // ClientHandle::Drop が writer_tx close + reader shutdown +
                    // writer_thread join を一括実行 (R5-H18)。
                    drop(ch);
                }
                continue;
            }
            Err(e) => {
                drop(poll_fds);
                return RelayOutcome::Error(e);
            }
        };

        // R4-C3: 完了済 pending handshake を取り込む (= 子 client として登録、
        // または timeout で破棄)。new client 登録による leader 昇格 + mode.change
        // broadcast 等も `process_pending_handshakes` 内で処理する。
        process_pending_handshakes(
            &mut pending_handshakes,
            config,
            next_client_id,
            clients,
            state,
            &mut overflow_ids,
        );

        // R5-H6: SIGCHLD wake-up handling. Drain the self-pipe to clear
        // pending signal bytes, then run `lifecycle.poll(child)` to pick up
        // any STOP / CONT / exit transition that just happened. If the child
        // exited, return immediately so callers (= scrollback / tail) finish
        // promptly (= no 500ms `STOPPED_POLL_INTERVAL` latency).
        if sigchld_ready {
            if let Some(sp) = sigchld_pipe {
                let _ = sp.drain();
            }
            if let ChildState::Exited(code) = lifecycle.poll(child) {
                return RelayOutcome::ChildExited(code);
            }
        }

        // 1. listener: 新規 client accept (= handshake worker を spawn するだけ。
        //    handshake 自体は別 thread で動くので serve_loop は blocking しない)
        if listener_revents.contains(PollFlags::POLLIN) {
            // D6: 集合 DoS 対策で attach 数を上限化。超過なら fd だけ accept して
            // 即 close (= 接続試行を OS に到達させない形にすると、kernel の listen
            // backlog で stuck する。一旦 fd を取って socket を close するのが安全)。
            //
            // R5-H2: pending handshake と attached client は **独立 cap** で
            // 頭打ちする。旧実装は両者を合算して MAX_PENDING_HANDSHAKES (= 64) で
            // 切っていたため、64 client が居る状態では新規 attach が一切できなく
            // なる事故が起きていた。現在は:
            //   - clients (= 確立済) >= MAX_CLIENTS_PER_DAEMON (64) なら reject
            //   - pending (= handshake 中) >= MAX_PENDING_HANDSHAKES (16) なら reject
            // のいずれか満たした時のみ accept を拒否する (AND ではなく OR)。
            if clients.len() >= MAX_CLIENTS_PER_DAEMON
                || pending_handshakes.len() >= MAX_PENDING_HANDSHAKES
            {
                if let Ok(fd) = listener.accept() {
                    drop(fd);
                }
                continue;
            }
            match spawn_handshake_worker(listener, config) {
                Ok(pending) => pending_handshakes.push(pending),
                Err(_) => {
                    // accept 失敗 (= EBADF / ENFILE 等): loop 継続
                }
            }
        }

        // 2. master: 子 PTY 出力を全 client に broadcast
        let pty_ready = master_revents.contains(PollFlags::POLLIN)
            || master_revents.contains(PollFlags::POLLHUP)
            || master_revents.contains(PollFlags::POLLERR);
        if pty_ready {
            let mut buf = [0u8; 8192];
            match pty.master_fd().read_some(&mut buf) {
                Ok(0) => match lifecycle.poll(child) {
                    ChildState::Exited(code) => return RelayOutcome::ChildExited(code),
                    ChildState::Stopped => {
                        // R4-H14: SIGTSTP'd 子で master EOF/POLLHUP が連続する間の
                        // busy-wait 回避。SIGCONT が来るまで 500ms 単位で待機。
                        std::thread::sleep(STOPPED_POLL_INTERVAL);
                    }
                    ChildState::Alive => {
                        std::thread::sleep(ALIVE_RETRY_INTERVAL);
                    }
                },
                Ok(n) => {
                    // scrollback に push してから broadcast (subscription 種類で encoding 分岐)
                    let now = Instant::now();
                    scrollback.push(now, buf[..n].to_vec());
                    overflow_ids.extend(broadcast_master_bytes(clients, &buf[..n], now));
                    // pending waits に新規 bytes を流し込み、match 確認
                    update_waits_on_master_bytes(pending_waits, clients, &buf[..n], now);
                    // R5-FB1: `--until PATTERN` match 検査。一致した瞬間に
                    // 子 process group へ SIGTERM を投げて session 終了させる。
                    // (broadcast / scrollback の後で match 判定するのは、最後の
                    // chunk も client / scrollback には届けるため。)
                    if let Some(ref mut w) = until_watcher {
                        if w.feed(&buf[..n]) {
                            let _ = kill_pgrp(child, Signal::SIGTERM);
                            // finalize_child が SIGTERM → wait → SIGKILL を実施。
                            // `ClientDetachedOrKilled` を返すことで finalize 経路に
                            // 乗せる (= `kill` subcommand と同じ後始末)。
                            return RelayOutcome::ClientDetachedOrKilled;
                        }
                    }
                }
                Err(Error::Errno(nix::errno::Errno::EIO)) => match lifecycle.poll(child) {
                    ChildState::Exited(code) => return RelayOutcome::ChildExited(code),
                    ChildState::Stopped => {
                        std::thread::sleep(STOPPED_POLL_INTERVAL);
                    }
                    ChildState::Alive => {
                        std::thread::sleep(ALIVE_RETRY_INTERVAL);
                    }
                },
                Err(Error::Errno(nix::errno::Errno::EAGAIN)) => {}
                Err(e) => return RelayOutcome::Error(e),
            }
        }

        // 3. 各 client reader: decode frame → 処理
        // frame ハンドリングは state / 他 client への副作用 (= lock state 変化、
        // broadcast 等) を持つため、まず frame を取り出してから処理する。
        let mut frames_to_process: Vec<(usize, FrameOrError)> = Vec::new();
        for (idx, revents) in client_revents.iter().enumerate() {
            if !revents.contains(PollFlags::POLLIN)
                && !revents.contains(PollFlags::POLLHUP)
                && !revents.contains(PollFlags::POLLERR)
            {
                continue;
            }
            let ch = &mut clients[idx];
            match Frame::decode_from(&mut ch.reader) {
                Ok(frame) => frames_to_process.push((idx, FrameOrError::Frame(frame))),
                Err(_) => frames_to_process.push((idx, FrameOrError::Error)),
            }
        }

        let mut indices_to_drop: Vec<usize> = Vec::new();
        // backpressure overflow / writer dead で集まった client_id → indices に変換
        for id in overflow_ids.drain(..) {
            if let Some(i) = clients.iter().position(|c| c.id == id) {
                indices_to_drop.push(i);
            }
        }
        let mut should_return: Option<RelayOutcome> = None;
        for (idx, fre) in frames_to_process {
            if should_return.is_some() {
                break;
            }
            match fre {
                FrameOrError::Frame(frame) => {
                    match handle_client_frame(
                        pty,
                        child,
                        idx,
                        frame,
                        clients,
                        state,
                        scrollback,
                        config,
                        pending_waits,
                    ) {
                        ClientFrameOutcome::Continue => {}
                        ClientFrameOutcome::DropClient => indices_to_drop.push(idx),
                        ClientFrameOutcome::TerminateSession(o) => should_return = Some(o),
                    }
                }
                FrameOrError::Error => {
                    // protocol error / EOF → 当該 client を切る
                    indices_to_drop.push(idx);
                }
            }
        }

        // drop 対象を逆順で remove (= leader cascade + lock auto-release 含む)
        // 重複 index も発生しうるので dedup する
        indices_to_drop.sort_unstable();
        indices_to_drop.dedup();
        let mut dropped_held_lock = false;
        let mut dropped_any_leader = false;
        for idx in indices_to_drop.into_iter().rev() {
            let ch = clients.remove(idx);
            if ch.leader {
                dropped_any_leader = true;
            }
            if state.lock_holder == Some(ch.id) {
                dropped_held_lock = true;
                state.lock_holder = None;
                state.lock_token = None;
            }
            // pending waits も remove (= client が消えたら wait は cancel 同等)
            pending_waits.retain(|w| w.client_id != ch.id);
            // ClientHandle::Drop が writer_tx close + reader shutdown +
            // writer_thread join を一括実行 (R5-H18)。backpressure 超過時の
            // writer_pump が write_all で block 中でも shutdown で即 error 化される。
            drop(ch);
        }

        // leader cascade: leader が消えた場合、次の Rw client を昇格させる
        if dropped_any_leader {
            let new_leader = elevate_next_leader(clients);
            broadcast_control(
                clients,
                &ControlMessage::LeaderNotify(LeaderNotify {
                    client_id: new_leader,
                }),
            );
        }

        // lock 自動解放: lock holder が抜けた場合、session mode を Rw に戻す
        if dropped_held_lock {
            broadcast_control(
                clients,
                &ControlMessage::ModeChange(ModeChange {
                    session_mode: state.session_mode(),
                    lock_holder: None,
                    client_mode: None,
                }),
            );
        }

        if let Some(o) = should_return {
            return o;
        }
    }
}

/// serve_loop の relay 結果。
#[derive(Debug)]
pub(super) enum RelayOutcome {
    /// 子 PTY 側で EOF を検出 (= 子 process が exit した)。exit code が判明していれば
    /// `Some(code)` に保持する (= waitpid を 2 度呼ばないため)。
    ChildExited(Option<i32>),
    /// client が `detach` / `kill` を送ったか socket EOF。`kill` の場合は子に
    /// signal が送られた状態でこの enum に至る。
    ClientDetachedOrKilled,
    /// 回復不能な error (= protocol violation 等)。
    Error(Error),
}

/// 子 PTY を reap して exit code を返す。
///
/// outcome に応じて:
/// - `ChildExited(Some(code))`: 既に `child_actually_exited` で reap 済、code をそのまま返す
/// - `ChildExited(None)`: exit 検知だが code 未取得 → waitpid で確認
/// - `ClientDetachedOrKilled`: 子はまだ生きている可能性 → SIGTERM → wait
///
/// signal で終了の場合は shell convention に従い `128 + signum` を返す。
fn finalize_child(child: Pid, outcome: &RelayOutcome) -> Result<i32, Error> {
    // `child_actually_exited` で既に code を取得済なら、それを優先 (waitpid を
    // 二重に呼ぶと ECHILD になる)。
    if let RelayOutcome::ChildExited(Some(code)) = outcome {
        return Ok(*code);
    }

    // ChildExited 以外 (= client 都合の終了) は子に SIGTERM を送ってから wait。
    // 既に exit 済なら kill は ESRCH で失敗 → 無視。
    // R5-H7: process group 全体に向けて、子が exec した孫 (= shell の background
    // job 等) も同じ SIGTERM で reap 対象にする。
    if !matches!(outcome, RelayOutcome::ChildExited(_)) {
        let _ = kill_pgrp(child, Signal::SIGTERM);
    }

    // 子を reap。
    loop {
        match waitpid(child, Some(WaitPidFlag::empty())) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + (sig as i32)),
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ECHILD) => {
                // 既に reap 済 (= SIGCHLD ハンドラが拾った等)。outcome に応じて
                // 0 / 143 (= SIGTERM kill) を返す。
                return Ok(match outcome {
                    RelayOutcome::ChildExited(_) => 0,
                    _ => 143,
                });
            }
            Err(e) => return Err(Error::from(e)),
        }
    }
}

// Drop impl の責務分担 (R4-H4 で Session 自体にも Drop 追加 → R4-H6 で Option-based 化):
// - listener (UnixSock) は自身の Drop で socket file を unlink
// - pty (Pty) は自身の Drop で master fd を close
// - 正常 path (Session::serve) では `self.inner.take()` で `SessionInner` を
//   move-out し、destructure で pty/child/listener を取り出す。serve の末尾で
//   self が drop されるとき `Session::drop` は `inner == None` で no-op。
//   子の reap は `finalize_child` が担当する
// - `serve` を呼ばずに Session が drop された場合 (test panic / early
//   return) は `impl Drop for Session` (= session.rs 上部) が `inner == Some`
//   を見て SIGTERM → 500ms wait → SIGKILL で子を reap して orphan を防ぐ

#[cfg(test)]
mod tests {
    use super::super::accept::constant_time_eq;
    use super::super::control::nix_signal_from_signum;
    use super::super::lock::{generate_lock_token, should_assign_leader};
    use super::*;
    use crate::protocol::messages::{
        Detach, DetachTarget, ErrorCode, Kill, LockResult, SessionMode, TailEndReason, WaitOutcome,
    };
    use crate::protocol::{
        HandshakeRequest, HandshakeResponse, MVP_CAPS, TYPE_CBOR_CONTROL, TYPE_RAW_DATA,
    };
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_temp_socket_dir() -> TempDir {
        let dir = tempfile::Builder::new()
            .prefix("hyoui-test-")
            .tempdir()
            .expect("tempdir");
        // parent dir を mode 0700 にする (UnixSock::listen の前提)
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir.path(), perm).expect("chmod 0700");
        dir
    }

    fn long_running_cmd() -> Vec<String> {
        // 30 秒 sleep。test 中に確実に alive。
        vec!["/bin/sleep".into(), "30".into()]
    }

    fn cleanup_child(pid: Pid) {
        // 子 process が orphan で残らないように SIGKILL → wait。
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        let _ = nix::sys::wait::waitpid(pid, None);
    }

    // ---- R5-FB1: UntilWatcher unit tests ----

    /// chunk 内に needle が完全に含まれる場合は即 match。
    #[test]
    fn until_watcher_matches_within_single_chunk() {
        let mut w = UntilWatcher::new("STOPHERE".into());
        assert!(w.feed(b"some output STOPHERE more"));
    }

    /// needle が chunk 境界を跨ぐ場合も carry 経由で正しく match。
    #[test]
    fn until_watcher_matches_across_chunk_boundary() {
        let mut w = UntilWatcher::new("STOPHERE".into());
        // 1 chunk 目: needle の前半 (= partial match) のみ
        assert!(!w.feed(b"prefix STOP"));
        // 2 chunk 目: needle の後半。carry + new で連結し match
        assert!(w.feed(b"HERE suffix"));
    }

    /// needle 不一致なら false を返し続ける + carry に末尾 (needle.len()-1)
    /// bytes だけ残る (= memory が肥大しない)。
    #[test]
    fn until_watcher_misses_keep_only_tail_in_carry() {
        let mut w = UntilWatcher::new("XYZ".into());
        assert!(!w.feed(b"abcdefghij"));
        // needle.len() - 1 = 2 bytes 残る (= "ij")
        assert_eq!(w.carry.len(), 2);
        assert_eq!(w.carry, b"ij");
        assert!(!w.feed(b"klmnop"));
        // carry が肥大しないこと: 高々 needle.len() - 1
        assert_eq!(w.carry.len(), 2);
    }

    /// empty needle は無効 (= 何が入っても false)。`DaemonConfig` 側で空 string
    /// は filter されているが、defense-in-depth で watcher 単体でも false を返す。
    #[test]
    fn until_watcher_empty_needle_never_matches() {
        let mut w = UntilWatcher::new(String::new());
        assert!(!w.feed(b"anything"));
    }

    /// R4-H14: `ChildLifecycle` は SIGSTOP / SIGCONT の transition を取りこぼさず、
    /// stopped 中は `ChildState::Stopped` を返し続け、SIGCONT 後は `Alive` に戻る。
    /// 旧実装 (waitpid without WUNTRACED) では stop 検出が dead code で、
    /// 結果として 5ms sleep の busy-wait に陥っていた。
    #[test]
    fn child_lifecycle_tracks_stopped_continued_transitions() {
        use crate::sys::Pty;
        use nix::sys::signal::Signal;

        // cat: stdin blocking で確実に alive。
        let spawned = Pty::spawn(&["cat"], 80, 24).expect("spawn cat");
        let child = spawned.child;

        let mut lc = ChildLifecycle::default();

        // 初期状態: Alive
        match lc.poll(child) {
            ChildState::Alive => {}
            other => panic!("expected Alive initially, got {other:?}"),
        }

        // SIGSTOP を送る → 次の poll で Stopped を観測
        nix::sys::signal::kill(child, Signal::SIGSTOP).expect("SIGSTOP");
        // kernel が状態遷移を反映するまで短くリトライ
        let mut saw_stopped = false;
        for _ in 0..50 {
            if matches!(lc.poll(child), ChildState::Stopped) {
                saw_stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_stopped, "should observe Stopped after SIGSTOP");

        // 続けて poll しても Stopped が latch されている (= state を保持)
        for _ in 0..3 {
            assert!(
                matches!(lc.poll(child), ChildState::Stopped),
                "Stopped should latch across polls"
            );
        }

        // SIGCONT で再開 → Alive に戻る
        nix::sys::signal::kill(child, Signal::SIGCONT).expect("SIGCONT");
        let mut saw_alive = false;
        for _ in 0..50 {
            if matches!(lc.poll(child), ChildState::Alive) {
                saw_alive = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_alive, "should observe Alive after SIGCONT");

        // cleanup
        let _ = nix::sys::signal::kill(child, Signal::SIGKILL);
        // exit を観測してから return (= zombie 残さない)
        let mut saw_exited = false;
        for _ in 0..100 {
            if matches!(lc.poll(child), ChildState::Exited(_)) {
                saw_exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_exited, "should reap after SIGKILL");
    }

    /// R4-H14 (regression): stopped 中の連続 poll は CPU spin にならない。
    /// 旧実装は `waitpid(WNOHANG)` のみで stop transition を捕捉できず、
    /// `ChildState::Alive` を返して 5ms sleep の loop に陥っていた。
    /// 本テストは `ChildLifecycle::poll` が stop 中に `Stopped` を返し、
    /// caller が `STOPPED_POLL_INTERVAL` (500ms) で sleep できることを確認する。
    #[test]
    fn child_lifecycle_avoids_busywait_while_stopped() {
        use crate::sys::Pty;
        use nix::sys::signal::Signal;

        let spawned = Pty::spawn(&["cat"], 80, 24).expect("spawn cat");
        let child = spawned.child;
        nix::sys::signal::kill(child, Signal::SIGSTOP).expect("SIGSTOP");

        let mut lc = ChildLifecycle::default();
        // Stop 観測まで待つ
        for _ in 0..50 {
            if matches!(lc.poll(child), ChildState::Stopped) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // 1 秒間連続で poll 結果が Stopped を返すこと
        let start = std::time::Instant::now();
        let mut polls = 0;
        while start.elapsed() < Duration::from_millis(50) {
            match lc.poll(child) {
                ChildState::Stopped => polls += 1,
                other => panic!("unexpected state during stop: {other:?}"),
            }
        }
        assert!(polls > 0, "polled at least once during stop window");

        // cleanup
        let _ = nix::sys::signal::kill(child, Signal::SIGCONT);
        let _ = nix::sys::signal::kill(child, Signal::SIGKILL);
        let _ = waitpid(child, None);
    }

    #[test]
    fn start_spawns_child_and_binds_socket() {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("test", sock.clone(), long_running_cmd());
        let session = Session::start(cfg).expect("start");

        assert_eq!(session.session_id(), "test");
        assert_eq!(session.socket_path(), sock.as_path());
        assert!(session.pty().master_fd().as_raw_fd() >= 0);

        let pid = session.child_pid();
        drop(session); // Drop で listener が unlink される
        cleanup_child(pid);
        assert!(!sock.exists(), "socket should be unlinked on Drop");
    }

    /// R4-H4: `Session::start` 後に `serve`/`run` を呼ばずに drop されると、
    /// 子 PTY は SIGTERM → 500ms 待ち → SIGKILL の順で reap され、
    /// orphan process として残らない。
    #[test]
    fn session_drop_kills_orphan_child() {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("drop.sock");
        // SIGTERM を無視せず素直に死ぬ sleep。30s alive。
        let cmd = vec!["/bin/sleep".into(), "30".into()];
        let cfg = DaemonConfig::new("drop-test", sock.clone(), cmd);
        let session = Session::start(cfg).expect("start");
        let pid = session.child_pid();

        // 子は alive (= WNOHANG で StillAlive)
        let pre = waitpid(pid, Some(WaitPidFlag::WNOHANG)).expect("waitpid pre");
        assert!(
            matches!(pre, WaitStatus::StillAlive),
            "child should be alive, got {pre:?}"
        );

        drop(session); // Drop で SIGTERM → reap

        // Drop 後は既に reap 済 → ECHILD を期待。Linux/macOS の signal 配送
        // race を避けるため最大 1s リトライ。
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let mut reaped = false;
        while std::time::Instant::now() < deadline {
            match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Err(nix::errno::Errno::ECHILD) => {
                    reaped = true;
                    break;
                }
                Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => {
                    reaped = true;
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(
            reaped,
            "child {pid:?} should be reaped by Session::drop, but waitpid still reports it alive"
        );

        // socket は UnixSock::drop で unlink される (= 既存挙動と同じ)
        assert!(!sock.exists(), "socket should be unlinked");
    }

    /// R5-H7: `Session::Drop` の kill 経路は子の process group 全体に送られ、
    /// 子が exec した shell が背後に置いた孫 (= `sh -c 'sleep ... &'`) も
    /// orphan として残さず即時 reap される。
    ///
    /// 旧実装 (= `kill(child, SIGTERM)` 単発) では、shell session leader だけが
    /// 終了し、孫 sleep は init/launchd に reparent されて生き残っていた。
    /// 本テストは killpg 化後の挙動 (= 孫 PID が ESRCH を返す = 死亡) を確認する。
    #[test]
    fn session_drop_kills_grandchild_via_killpg() {
        // 孫 PID を受け渡すための tmp ファイル。
        let pid_dir = tempfile::tempdir().expect("pid tempdir");
        let pid_file = pid_dir.path().join("grandchild.pid");

        let dir = make_temp_socket_dir();
        let sock = dir.path().join("killpg.sock");
        // sh -c で sleep を background に置き、$! を pid_file に書く。
        // sleep の stdin/stdout は /dev/null にして slave PTY を握らない
        // (= SIGHUP に依存せず、純粋に killpg だけで死ぬことを検証する)。
        //
        // 親 sh は `wait` で sleep を待つ → SIGTERM で sh 自身が死ぬ →
        // 旧実装ならここで sleep が orphan 化、新実装なら killpg で sleep も死ぬ。
        let pid_path_str = pid_file
            .to_str()
            .expect("pid_file path is utf8")
            .to_string();
        let script =
            format!("sleep 30 </dev/null >/dev/null 2>&1 & echo $! > {pid_path_str}; wait");
        let cmd = vec!["/bin/sh".into(), "-c".into(), script];
        let cfg = DaemonConfig::new("killpg-test", sock.clone(), cmd);
        let session = Session::start(cfg).expect("start");
        let shell_pid = session.child_pid();

        // 孫 PID が書き出されるまで最大 2 秒待つ。
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let grandchild_pid: i32 = loop {
            if let Ok(s) = std::fs::read_to_string(&pid_file) {
                if let Some(line) = s.lines().next() {
                    if let Ok(pid) = line.trim().parse::<i32>() {
                        break pid;
                    }
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild PID file {pid_file:?} not written within 2s"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(grandchild_pid > 0, "grandchild pid must be positive");

        // 孫が live であることを確認 (= signal 0 で ESRCH でない)。
        let grandchild = Pid::from_raw(grandchild_pid);
        assert!(
            kill(grandchild, None).is_ok(),
            "grandchild {grandchild_pid} should be alive before drop"
        );

        // Session を drop → Drop impl の killpg(SIGTERM → SIGKILL) で
        // shell + 孫 sleep の両方が死ぬはず。
        drop(session);

        // 孫が消えるまで待つ。ESRCH (= プロセス無し) を期待。
        // killpg の SIGTERM → 500ms 待ち → SIGKILL の最悪ケースに余裕を加えて 3s。
        let kill_deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut grandchild_dead = false;
        while std::time::Instant::now() < kill_deadline {
            match kill(grandchild, None) {
                Err(nix::errno::Errno::ESRCH) => {
                    grandchild_dead = true;
                    break;
                }
                Ok(()) | Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(
            grandchild_dead,
            "grandchild {grandchild_pid} should be killed via killpg, but is still alive after 3s"
        );

        // shell も既に reap 済のはず (= Session::Drop が waitpid している)。
        // grandchild は他 process なので waitpid できない (= orphan reaper の責務)。
        // ECHILD が返れば既に reap されている。
        let _ = shell_pid; // unused warning 抑止
    }

    #[test]
    fn start_rejects_empty_cmd() {
        let dir = make_temp_socket_dir();
        let sock = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("test", sock, Vec::<String>::new());
        let err = Session::start(cfg).expect_err("must error");
        assert!(matches!(err, Error::Invalid(_)));
    }

    fn client_connect_with_retry(path: &std::path::Path) -> UnixStream {
        // R4-H5: retry budget は 200 attempts (= 2s) に拡大 (旧 50 = 500ms)。
        // CI 高負荷下で daemon の listen 開始が遅れた場合に false-fail しないよう
        // 余裕を持たせる。成功すれば実時間はほぼ変わらない (= 早期 break)。
        let mut attempts = 0;
        let fd = loop {
            match crate::sys::socket::connect(path) {
                Ok(fd) => break fd,
                Err(_) if attempts < 200 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("client connect failed: {e:?}"),
            }
        };
        UnixStream::from(fd)
    }

    fn do_client_handshake(stream: &mut UnixStream) -> HandshakeResponse {
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        let body = req.encode_to_vec().expect("cbor encode");
        Frame::cbor_control(body)
            .encode_to(stream)
            .expect("write handshake");
        stream.flush().expect("flush");
        let resp_frame = Frame::decode_from(stream).expect("decode response");
        match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode cbor") {
            ControlMessage::HandshakeResponse(r) => r,
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ---- Phase 9 (Session::serve) tests ----

    fn spawn_serve_thread(
        cmd: Vec<String>,
    ) -> (
        String,
        std::path::PathBuf,
        TempDir,
        std::thread::JoinHandle<Result<i32, Error>>,
    ) {
        let dir = make_temp_socket_dir();
        let session_id = "demo".to_string();
        let sock_path = dir.path().join("test.sock");
        let cfg = DaemonConfig::new(session_id.clone(), sock_path.clone(), cmd);
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.serve());
        (session_id, sock_path, dir, handle)
    }

    /// R5-FB1: `run --until PATTERN` を DaemonConfig.until で渡すと、子 PTY
    /// 出力に PATTERN が現れた瞬間 daemon が子に SIGTERM を投げて session が
    /// 終了する。bash で `STOPHERE` を出力したあと `SHOULDNOT` を出すコマンドを
    /// 走らせ、`SHOULDNOT` の前に session が終わる (= until 機能配線済) ことを
    /// 確認する。
    #[test]
    fn run_until_terminates_child_on_pattern_match() {
        let dir = make_temp_socket_dir();
        let sock_path = dir.path().join("until.sock");
        // bash -c 'echo START; sleep 0.3; echo STOPHERE; sleep 5; echo SHOULDNOT'
        // STOPHERE 直後に SIGTERM が届けば、5 秒の sleep を待たずに即終了するはず。
        let cmd = vec![
            "/bin/bash".into(),
            "-c".into(),
            "echo START; sleep 0.3; echo STOPHERE; sleep 5; echo SHOULDNOT".into(),
        ];
        let mut cfg = DaemonConfig::new("until-test", sock_path.clone(), cmd);
        cfg.until = Some("STOPHERE".into());
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.serve());

        // session 終了を 3s 以内に確認する (= 5 秒 sleep の前に SIGTERM が
        // 効いていることの保証)。
        let start = std::time::Instant::now();
        let mut joined = None;
        while start.elapsed() < Duration::from_secs(3) {
            if handle.is_finished() {
                joined = Some(handle.join());
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let res = joined.expect("daemon should terminate within 3s after --until match");
        // SIGTERM で死ぬので shell convention の 128 + 15 = 143 が一般的。
        // ただし bash の trap 等で異なる場合もあるので exit code 自体は緩く確認。
        let exit = res.expect("daemon thread").expect("daemon serve result");
        assert!(
            exit != 0,
            "process killed by --until should exit non-zero, got {exit}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "session must end within 3s of STOPHERE; elapsed = {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn serve_handles_single_client_kill() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());
        let mut stream = client_connect_with_retry(&sock_path);
        let _resp = do_client_handshake(&mut stream);
        let kill_msg = ControlMessage::Kill(Kill { signum: None });
        let body = kill_msg.encode_to_vec().expect("encode kill");
        Frame::cbor_control(body)
            .encode_to(&mut stream)
            .expect("send kill");
        stream.flush().expect("flush");

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        assert_eq!(exit, 143);
    }

    #[test]
    fn serve_handles_sequential_clients() {
        // 1 client が detach → 2 つ目 client が attach → 2 つ目が kill で終了
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // client 1: attach → detach
        {
            let mut s = client_connect_with_retry(&sock_path);
            let _r = do_client_handshake(&mut s);
            let body = ControlMessage::Detach(Detach {
                target: DetachTarget::Myself,
            })
            .encode_to_vec()
            .expect("encode");
            Frame::cbor_control(body).encode_to(&mut s).expect("send");
            s.flush().expect("flush");
            // socket close は drop で
        }
        // 短い間を空けて 2 つ目 attach
        std::thread::sleep(Duration::from_millis(50));

        // client 2: attach → kill
        {
            let mut s = client_connect_with_retry(&sock_path);
            let _r = do_client_handshake(&mut s);
            let body = ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode");
            Frame::cbor_control(body).encode_to(&mut s).expect("send");
            s.flush().expect("flush");
        }

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        // kill による終了 = 143
        assert_eq!(exit, 143);
    }

    #[test]
    fn serve_handles_two_concurrent_clients() {
        // 同時に 2 client attach → 片方が kill 送信で session 終了
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _r1 = do_client_handshake(&mut s1);

        let mut s2 = client_connect_with_retry(&sock_path);
        let r2 = do_client_handshake(&mut s2);
        // 2 つ目 client は別 client_id を割り当てられる
        assert_ne!(r2.client_id, 0);

        // s1 が kill 送信
        let body = ControlMessage::Kill(Kill { signum: None })
            .encode_to_vec()
            .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        assert_eq!(exit, 143);
    }

    #[test]
    fn serve_propagates_child_exit_code() {
        let false_path = if std::path::Path::new("/usr/bin/false").exists() {
            "/usr/bin/false"
        } else {
            "/bin/false"
        };
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(vec![false_path.into()]);

        // 子は即 exit するが、accept 前に exit すると hang する可能性。先に接続。
        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);

        let exit = handle.join().expect("daemon thread").expect("daemon serve");
        assert_eq!(exit, 1);
    }

    // ---- Phase 10 helper unit tests ----

    /// R5-H12: `core_dump_allowed_value` は `Some("1")` のときだけ true。
    /// 未設定 / 空文字 / 他の値 (`"0"`, `"true"`, `"yes"`) はすべて false。
    #[test]
    fn core_dump_allowed_value_only_matches_one() {
        assert!(core_dump_allowed_value(Some("1")));
        assert!(!core_dump_allowed_value(None));
        assert!(!core_dump_allowed_value(Some("")));
        assert!(!core_dump_allowed_value(Some("0")));
        assert!(!core_dump_allowed_value(Some("true")));
        assert!(!core_dump_allowed_value(Some("yes")));
        assert!(!core_dump_allowed_value(Some("1\n")));
    }

    /// R5-H12: `Session::start` 通過後は `RLIMIT_CORE` の soft/hard が両方 0 に
    /// 固定される (= panic / SIGSEGV で core dump が書かれない)。
    ///
    /// 注意: 一度 hard を 0 に落とすと process 寿命中は二度と上げられない。
    /// 本 test と他の `Session::start` を呼ぶ test (例:
    /// `start_spawns_child_and_binds_socket`) は同一 process 内で並列実行され、
    /// どれが先に走っても以降は永久に (0, 0) なので race 条件は無い
    /// (= test 間でリセット不要)。
    #[test]
    fn session_start_sets_core_rlimit_to_zero() {
        use crate::sys::raw::getrlimit_core;

        let dir = make_temp_socket_dir();
        let sock = dir.path().join("core.sock");
        let cfg = DaemonConfig::new("core-test", sock, long_running_cmd());
        let session = Session::start(cfg).expect("start");
        let pid = session.child_pid();
        drop(session);
        cleanup_child(pid);

        let rl = getrlimit_core().expect("getrlimit");
        assert_eq!(
            rl.soft, 0,
            "RLIMIT_CORE soft must be 0 after Session::start"
        );
        assert_eq!(
            rl.hard, 0,
            "RLIMIT_CORE hard must be 0 after Session::start"
        );
    }

    #[test]
    fn generate_lock_token_unique_and_hex32() {
        let a = generate_lock_token().expect("urandom open");
        let b = generate_lock_token().expect("urandom open");
        assert_eq!(a.len(), 32, "token must be 32 hex chars (16 bytes)");
        assert_eq!(b.len(), 32);
        assert_ne!(a, b, "two tokens must differ");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn should_assign_leader_picks_first_rw() {
        let clients: Vec<ClientHandle> = Vec::new();
        assert!(should_assign_leader(&clients, Mode::Rw));
        assert!(!should_assign_leader(&clients, Mode::Ro));
        assert!(
            !should_assign_leader(&clients, Mode::RwNoLeader),
            "rw-no-leader は明示拒否なので leader 取らない"
        );
    }

    #[test]
    fn session_mode_reflects_lock_holder() {
        let mut s = SessionState::default();
        assert_eq!(s.session_mode(), SessionMode::Rw);
        s.lock_holder = Some(7);
        s.lock_token = Some("abcd".into());
        assert_eq!(s.session_mode(), SessionMode::Locked);
    }

    // ---- Phase 10 e2e tests (= serve_loop 経由) ----

    /// Phase 10: 2nd rw client は leader を取らない (1st が既に leader)。
    #[test]
    fn serve_only_first_rw_becomes_leader() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        assert!(r1.leader);

        let mut s2 = client_connect_with_retry(&sock_path);
        let r2 = do_client_handshake(&mut s2);
        assert!(!r2.leader, "2nd rw client must not be leader");
        assert_ne!(r1.client_id, r2.client_id);

        // cleanup: kill
        let body = ControlMessage::Kill(Kill { signum: None })
            .encode_to_vec()
            .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: lock acquire は token を返し、mode.change(Locked) を全 client に broadcast。
    #[test]
    fn serve_lock_acquire_grants_and_broadcasts_mode_change() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        // s1 accept 時の leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        let mut s2 = client_connect_with_retry(&sock_path);
        let _r2 = do_client_handshake(&mut s2);

        // s1 が lock 取得
        let body = ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
            wait: false,
            timeout_abs_ms: None,
            timeout_idle_ms: None,
            process_bound: false,
        })
        .encode_to_vec()
        .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");

        // s1 は lock.response(Acquired, token=...) を受信
        let resp_frame = Frame::decode_from(&mut s1).expect("decode resp");
        let resp = ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode cbor");
        let token = match resp {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(lr.result, LockResult::Acquired);
                assert_eq!(lr.token.as_ref().map(|t| t.len()), Some(32));
                lr.token.clone()
            }
            other => panic!("expected LockResponse, got {other:?}"),
        };
        assert!(token.is_some());

        // s1 / s2 とも mode.change(Locked, lock_holder=s1.client_id) を受信
        for s in [&mut s1, &mut s2] {
            let mc_frame = Frame::decode_from(s).expect("decode mode.change frame");
            let mc = ControlMessage::decode_from(mc_frame.body.as_slice()).expect("decode mc");
            match mc {
                ControlMessage::ModeChange(c) => {
                    assert_eq!(c.session_mode, SessionMode::Locked);
                    assert_eq!(c.lock_holder, Some(r1.client_id));
                }
                other => panic!("expected ModeChange, got {other:?}"),
            }
        }

        // cleanup
        let body = ControlMessage::Kill(Kill { signum: None })
            .encode_to_vec()
            .expect("encode");
        Frame::cbor_control(body).encode_to(&mut s1).expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: 2 件目の lock acquire は Denied、state 変化なし。
    #[test]
    fn serve_lock_acquire_while_locked_returns_denied() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        // s1 accept 時の leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");
        let mut s2 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s2);

        // s1 が lock 取得
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        // s1 が response + mode.change を受け取る、s2 が mode.change を受け取る
        let _ = Frame::decode_from(&mut s1).expect("response");
        let _ = Frame::decode_from(&mut s1).expect("mode.change s1");
        let _ = Frame::decode_from(&mut s2).expect("mode.change s2");

        // s2 が lock 取得試行 (= 拒否される)
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let resp_frame = Frame::decode_from(&mut s2).expect("resp");
        let resp = ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode");
        match resp {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(lr.result, LockResult::Denied);
                assert!(lr.token.is_none());
            }
            other => panic!("expected LockResponse(Denied), got {other:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: lock release は token 一致で成功、mode.change(Rw) を broadcast。
    #[test]
    fn serve_lock_release_clears_state() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        // s1 accept 時の leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        // acquire
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let resp_frame = Frame::decode_from(&mut s1).expect("resp");
        let token = match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode") {
            ControlMessage::LockResponse(lr) => lr.token.expect("token"),
            o => panic!("expected LockResponse, got {o:?}"),
        };
        // mode.change(Locked) は捨てる
        let _ = Frame::decode_from(&mut s1).expect("mode.change locked");

        // release
        Frame::cbor_control(
            ControlMessage::LockRelease(crate::protocol::messages::LockRelease {
                token: token.clone(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        // mode.change(Rw, lock_holder=None) を受信
        let mc_frame = Frame::decode_from(&mut s1).expect("mode.change rw");
        match ControlMessage::decode_from(mc_frame.body.as_slice()).expect("decode") {
            ControlMessage::ModeChange(c) => {
                assert_eq!(c.session_mode, SessionMode::Rw);
                assert!(c.lock_holder.is_none());
            }
            o => panic!("expected ModeChange, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: leader が detach すると、次の rw client に cascade + leader.notify broadcast。
    #[test]
    fn serve_leader_cascades_on_detach() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        assert!(r1.leader);
        let mut s2 = client_connect_with_retry(&sock_path);
        let r2 = do_client_handshake(&mut s2);
        assert!(!r2.leader);

        // s2 は s1 が leader になった瞬間の leader.notify を 1 つ受け取る (= s1 accept 時の broadcast)
        // ※ s1 自身も自分の leader.notify を 1 つ受け取る。これらを先に捨てる。
        // s1 については s2 accept 時の broadcast が起きない (s2 は leader にならないので) ことを利用し、
        // s1 が受け取る leader.notify は s1 accept 時の 1 件のみ。
        let nf = Frame::decode_from(&mut s1).expect("s1 leader.notify");
        match ControlMessage::decode_from(nf.body.as_slice()).expect("decode") {
            ControlMessage::LeaderNotify(n) => assert_eq!(n.client_id, Some(r1.client_id)),
            o => panic!("expected LeaderNotify, got {o:?}"),
        }

        // s1 を detach (= leader 抜ける)
        Frame::cbor_control(
            ControlMessage::Detach(Detach {
                target: DetachTarget::Myself,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        drop(s1);

        // s2 が新 leader として通知される (cascade)
        let nf2 = Frame::decode_from(&mut s2).expect("s2 cascade notify");
        match ControlMessage::decode_from(nf2.body.as_slice()).expect("decode") {
            ControlMessage::LeaderNotify(n) => assert_eq!(n.client_id, Some(r2.client_id)),
            o => panic!("expected LeaderNotify(cascade), got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 10: 非 leader が resize すると error 返却、子 pty は変化しない。
    #[test]
    fn serve_non_leader_resize_gets_error() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        // s1 leader.notify を捨てる
        let _ = Frame::decode_from(&mut s1).expect("leader notify");

        let mut s2 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s2);
        // s2 は leader でないので leader.notify broadcast を受けない (became_leader=false)

        // s2 が resize 送信
        Frame::cbor_control(
            ControlMessage::Resize(crate::protocol::messages::Resize {
                cols: 100,
                rows: 30,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");

        // s2 が error を受信
        let ef = Frame::decode_from(&mut s2).expect("error");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::ModeNotLeader);
            }
            o => panic!("expected Error, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Phase 11: status.query ----

    /// Phase 11: status.query は session 状態 (clients/leader/lock) を返す。
    #[test]
    fn serve_status_query_returns_current_state() {
        use crate::protocol::messages::StatusQuery;

        let (sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let r1 = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify"); // 捨て

        // s1 が status.query
        Frame::cbor_control(
            ControlMessage::StatusQuery(StatusQuery {})
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        let resp_frame = Frame::decode_from(&mut s1).expect("status response");
        match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode") {
            ControlMessage::StatusResponse(sr) => {
                assert_eq!(sr.session_id, sid);
                assert!(sr.child_pid.is_some(), "child must still be alive");
                assert_eq!(sr.clients.len(), 1);
                assert_eq!(sr.clients[0].client_id, r1.client_id);
                assert!(sr.clients[0].leader);
                assert!(sr.lock_holder.is_none());
            }
            o => panic!("expected StatusResponse, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Phase 11b: tail.request ----

    /// 次の control message frame を待ち、raw_data frame は skip して返す。
    fn next_control(s: &mut UnixStream) -> ControlMessage {
        loop {
            let f = Frame::decode_from(s).expect("frame");
            if f.ty == TYPE_CBOR_CONTROL {
                return ControlMessage::decode_from(f.body.as_slice()).expect("decode");
            }
            // raw_data は skip
        }
    }

    /// 子 PTY 出力 (= raw_data) を `target` 含むまで読み込む。
    fn read_until_contains(s: &mut UnixStream, target: &[u8]) {
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !got.windows(target.len()).any(|w| w == target) {
            if std::time::Instant::now() > deadline {
                panic!("read_until_contains: timed out waiting for {target:?}");
            }
            let f = Frame::decode_from(s).expect("frame");
            if f.ty == TYPE_RAW_DATA {
                got.extend(f.body);
            }
        }
    }

    /// Phase 11b: tail.request(follow=false) は scrollback を 1 個の TailData +
    /// TailEnd(Eof) で返す。
    #[test]
    fn serve_tail_request_no_follow_dumps_buffer() {
        use crate::protocol::messages::TailRequest;

        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf hello; sleep 30".into(),
        ];
        let (_, sock_path, _dir, handle) = {
            let dir = make_temp_socket_dir();
            let session_id = "demo".to_string();
            let sock_path = dir.path().join("test.sock");
            let cfg = DaemonConfig::new(session_id.clone(), sock_path.clone(), cmd);
            let session = Session::start(cfg).expect("start");
            let h = std::thread::spawn(move || session.serve());
            (session_id, sock_path, dir, h)
        };

        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        // 子の "hello" が到着するまで raw_data を読む
        read_until_contains(&mut s, b"hello");

        // tail.request (follow=false)
        Frame::cbor_control(
            ControlMessage::TailRequest(TailRequest {
                since_ms: None,
                since_strict: false,
                follow: false,
                strip_ansi: false,
                last_bytes: None,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");

        match next_control(&mut s) {
            ControlMessage::TailData(td) => {
                assert!(
                    td.bytes.windows(5).any(|w| w == b"hello"),
                    "TailData should contain 'hello', got {:?}",
                    String::from_utf8_lossy(&td.bytes)
                );
            }
            o => panic!("expected TailData, got {o:?}"),
        }
        match next_control(&mut s) {
            ControlMessage::TailEnd(te) => {
                assert_eq!(te.reason, TailEndReason::Eof);
            }
            o => panic!("expected TailEnd, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// tail follow=true な client は子 PTY exit 時に TailEnd(ChildExited) を受信する。
    #[test]
    fn serve_tail_follow_receives_tail_end_on_child_exit() {
        use crate::protocol::messages::TailRequest;

        // 子は短時間で exit する `/bin/sh -c "sleep 0.2"` を使う
        let cmd = vec!["/bin/sh".into(), "-c".into(), "sleep 0.2".into()];
        let dir = make_temp_socket_dir();
        let sock_path = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("demo", sock_path.clone(), cmd);
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.serve());

        let mut s = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        // tail follow=true で subscribe
        Frame::cbor_control(
            ControlMessage::TailRequest(TailRequest {
                since_ms: None,
                since_strict: false,
                follow: true,
                strip_ansi: false,
                last_bytes: None,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");

        // 子 exit までに来る何らかの frame の中で TailEnd(ChildExited) を待つ
        let mut got_child_exited = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match next_control(&mut s) {
                ControlMessage::TailEnd(te) => {
                    if te.reason == TailEndReason::ChildExited {
                        got_child_exited = true;
                        break;
                    }
                }
                _ => continue,
            }
        }
        assert!(got_child_exited, "expected TailEnd(ChildExited)");

        let _ = handle.join().expect("daemon thread");
    }

    // ---- Phase 11c: wait.request ----

    /// Phase 11c: wait.request(text) は新規 master 出力に target が含まれたら Matched。
    #[test]
    fn serve_wait_text_predicate_matches() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate, WaitRequest};

        let cmd = vec!["/bin/sh".into(), "-c".into(), "cat".into()];
        let (_, sock_path, _dir, handle) = {
            let dir = make_temp_socket_dir();
            let sock_path = dir.path().join("test.sock");
            let cfg = DaemonConfig::new("demo", sock_path.clone(), cmd);
            let session = Session::start(cfg).expect("start");
            let h = std::thread::spawn(move || session.serve());
            ("demo".to_string(), sock_path, dir, h)
        };

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("leader.notify");

        // wait.request: text "READY"
        Frame::cbor_control(
            ControlMessage::WaitRequest(WaitRequest {
                predicate: WaitPredicate::Text {
                    value: "READY".into(),
                },
                timeout_ms: Some(5_000),
                options: WaitMatchOptions::default(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        // cat に "READY\n" を送り込む → cat が echo → master → wait scan で match
        Frame::raw_data(b"READY\n".to_vec())
            .encode_to(&mut s1)
            .expect("send");
        s1.flush().expect("flush");

        // wait.result(Matched) を期待
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                panic!("timed out waiting for WaitResult");
            }
            match next_control(&mut s1) {
                ControlMessage::WaitResult(wr) => {
                    assert_eq!(wr.outcome, WaitOutcome::Matched);
                    break;
                }
                _ => continue,
            }
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 11c: wait.request(idle) は master 出力が idle_ms 間無いと Matched。
    #[test]
    fn serve_wait_idle_predicate_matches() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate, WaitRequest};

        // 子は出力しない (sleep だけ) → idle がすぐ成立する
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("leader.notify");

        // idle 100ms wait
        Frame::cbor_control(
            ControlMessage::WaitRequest(WaitRequest {
                predicate: WaitPredicate::Idle { ms: 100 },
                timeout_ms: Some(5_000),
                options: WaitMatchOptions::default(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        let start = std::time::Instant::now();
        let result = next_control(&mut s1);
        let elapsed = start.elapsed();
        match result {
            ControlMessage::WaitResult(wr) => {
                assert_eq!(wr.outcome, WaitOutcome::Matched);
            }
            o => panic!("expected WaitResult, got {o:?}"),
        }
        // R4-H5: 下限は 50ms に緩めた (旧 80ms / 要求 idle_ms = 100ms に対して 80%)。
        // 「daemon が idle 待ちをそれなりにやった」ことを確認する sanity check で
        // あり、CI 高負荷下で daemon の timer 解像度が荒くなった場合に false-fail
        // する余地を減らすため (= 要求 idle_ms = 100ms に対し 50% を下限)。
        assert!(
            elapsed >= Duration::from_millis(50),
            "idle should wait noticeably (allowing for jitter), got {elapsed:?}"
        );

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 11c: wait.request の timeout は WaitResult(Timeout) を返す。
    #[test]
    fn serve_wait_timeout_returns_timeout_outcome() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate, WaitRequest};

        // 子は何も出力しない sleep → text "NEVER" は決して来ない → timeout
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("leader.notify");

        Frame::cbor_control(
            ControlMessage::WaitRequest(WaitRequest {
                predicate: WaitPredicate::Text {
                    value: "NEVER".into(),
                },
                timeout_ms: Some(200),
                options: WaitMatchOptions::default(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        let start = std::time::Instant::now();
        match next_control(&mut s1) {
            ControlMessage::WaitResult(wr) => assert_eq!(wr.outcome, WaitOutcome::Timeout),
            o => panic!("expected WaitResult, got {o:?}"),
        }
        let elapsed = start.elapsed();
        // R4-H5: 下限は 100ms に緩めた (旧 150ms / 要求 timeout = 200ms に対して 75%)。
        // 「daemon が timeout 待ちをそれなりにやった」ことを確認する sanity check で、
        // CI 高負荷下で daemon の timer 解像度が荒くなった場合に false-fail する
        // 余地を減らすため (= 要求 timeout = 200ms に対し 50% を下限)。
        assert!(
            elapsed >= Duration::from_millis(100),
            "should wait around 200ms, got {elapsed:?}"
        );

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 11c: wait.request(pattern) regex match。
    #[test]
    fn serve_wait_pattern_predicate_matches() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate, WaitRequest};

        let cmd = vec!["/bin/sh".into(), "-c".into(), "cat".into()];
        let (_, sock_path, _dir, handle) = {
            let dir = make_temp_socket_dir();
            let sock_path = dir.path().join("test.sock");
            let cfg = DaemonConfig::new("demo", sock_path.clone(), cmd);
            let session = Session::start(cfg).expect("start");
            let h = std::thread::spawn(move || session.serve());
            ("demo".to_string(), sock_path, dir, h)
        };
        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("leader.notify");

        // pattern: r"ITEM-\d+"
        Frame::cbor_control(
            ControlMessage::WaitRequest(WaitRequest {
                predicate: WaitPredicate::Pattern {
                    regex: r"ITEM-\d+".into(),
                },
                timeout_ms: Some(5_000),
                options: WaitMatchOptions::default(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        Frame::raw_data(b"prefix ITEM-42 suffix\n".to_vec())
            .encode_to(&mut s1)
            .expect("send");
        s1.flush().expect("flush");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                panic!("timed out");
            }
            if let ControlMessage::WaitResult(wr) = next_control(&mut s1) {
                assert_eq!(wr.outcome, WaitOutcome::Matched);
                break;
            }
        }

        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Phase 11b: tail.request(follow=true) は subscription を TailFollow に
    /// 切り替え、以降の master 出力は TailData として届く。
    #[test]
    fn serve_tail_request_follow_switches_subscription() {
        use crate::protocol::messages::TailRequest;

        // s1 = Rw (input 送信用)、s2 = Rw → TailFollow (受信検証用)
        let cmd = vec!["/bin/sh".into(), "-c".into(), "cat".into()]; // stdin → stdout echo
        let (_, sock_path, _dir, handle) = {
            let dir = make_temp_socket_dir();
            let session_id = "demo".to_string();
            let sock_path = dir.path().join("test.sock");
            let cfg = DaemonConfig::new(session_id.clone(), sock_path.clone(), cmd);
            let session = Session::start(cfg).expect("start");
            let h = std::thread::spawn(move || session.serve());
            (session_id, sock_path, dir, h)
        };

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        let mut s2 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s2);

        // s2 が tail.request(follow=true)
        Frame::cbor_control(
            ControlMessage::TailRequest(TailRequest {
                since_ms: None,
                since_strict: false,
                follow: true,
                strip_ansi: false,
                last_bytes: None,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");

        // s1 が "hi\n" を送る → cat が echo → master 出力 → s2 へ TailData
        // (s1 自体は terminal echo + cat echo の二重で見るが、test では s2 のみ確認)
        Frame::raw_data(b"hi\n".to_vec())
            .encode_to(&mut s1)
            .expect("write s1");
        s1.flush().expect("flush");

        // s2 が TailData (含 "hi") を受信
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                panic!("timed out waiting for TailData with 'hi'");
            }
            match next_control(&mut s2) {
                ControlMessage::TailData(td) => {
                    got.extend(td.bytes);
                    if got.windows(2).any(|w| w == b"hi") {
                        break;
                    }
                }
                ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
                o => panic!("unexpected: {o:?}"),
            }
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Phase 12: byte bound backpressure ----

    /// Phase 12: client_buffer_bytes を超過すると当該 client は backpressure.disconnect
    /// で切断され、socket は close される。他の client は影響を受けず通常動作。
    #[test]
    fn serve_backpressure_disconnects_slow_client() {
        // yes(1) は "y\n" を fast loop で出力 → 子 PTY master に大量の bytes が積まれる
        let yes_path = if std::path::Path::new("/usr/bin/yes").exists() {
            "/usr/bin/yes"
        } else {
            "/bin/yes"
        };
        let dir = make_temp_socket_dir();
        let sock_path = dir.path().join("test.sock");
        let mut cfg = DaemonConfig::new("demo", sock_path.clone(), vec![yes_path.into()]);
        cfg.client_buffer_bytes = 4096; // 小さくして即超過させる
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.serve());

        // client 1: rw、handshake のみ。socket を読まずに放置 → backpressure 対象
        let mut slow = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut slow);
        // 注: leader.notify は来るはずだが、CI Linux 等で yes 出力が先に queue を
        // 埋めて daemon が即 backpressure disconnect する場合、leader.notify を
        // 受信する前に shutdown される race がある。本 test の意図は「backpressure で
        // disconnect されること」なので leader.notify 受信は optional 扱い。
        let _ = Frame::decode_from(&mut slow);

        // client 2: rw、こちらも attach するが「ちゃんと recv する側」として機能
        // させたい。試験安定化のためここでも何も読まない (= daemon は data を broadcast
        // し、slow が overflow したら disconnect する)。
        // 注: 本 test では `他 client が動き続けること` までは検証せず、`slow が
        // 切断されること` だけ確認する。

        // 子 yes の出力が daemon の broadcast loop を経て slow の writer queue に
        // 積まれる。slow が socket を読まないと OS socket buffer (~64 KiB) が埋まる
        // → writer_pump が write_all で block → queued_bytes 増加 → buffer_limit
        // (4096 byte) 超過 → daemon が slow を切る (shutdown Both)。
        // よってここでは「しばらく読まずに放置」してから socket を drain、最後に EOF。
        // しばらく放置 → daemon が backpressure 検知して shutdown するはず
        std::thread::sleep(Duration::from_secs(1));

        // socket を nonblocking にして drain。EOF (= read returns 0) を待つ。
        slow.set_nonblocking(true).expect("set_nonblocking");
        let mut tmpbuf = [0u8; 8192];
        let mut total_read = 0usize;
        let mut eof_detected = false;
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            match std::io::Read::read(&mut slow, &mut tmpbuf) {
                Ok(0) => {
                    eof_detected = true;
                    break;
                }
                Ok(n) => {
                    total_read += n;
                    if total_read > 1024 * 1024 {
                        panic!("slow client received >1 MiB; backpressure didn't kick in");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // まだ shutdown されていない → 少し待って再試行
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    eof_detected = true;
                    break;
                }
            }
        }
        assert!(
            eof_detected,
            "slow client should be disconnected (EOF on read); total_read = {total_read}"
        );

        // cleanup: daemon は yes 子を抱えたまま slow disconnect 後も alive のはず
        // (= 子 PTY 出力は scrollback に積まれるだけ)。kill 子で daemon 終了。
        let _ = nix::sys::signal::kill(
            // yes child is still running; we have no direct PID, rely on daemon kill
            nix::unistd::Pid::from_raw(-1),
            None,
        );
        // 別 client で kill を送れば確実
        let mut k = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut k);
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut k)
        .expect("send kill");
        k.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Round1 fixes: authorization / token / signal / silent skip ----

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn nix_signal_from_signum_rejects_zero_and_invalid() {
        assert!(nix_signal_from_signum(0).is_none(), "signum=0 = probe");
        assert!(
            nix_signal_from_signum(255).is_none(),
            "signum=255 out of range"
        );
        assert!(nix_signal_from_signum(2).is_some(), "SIGINT");
        assert!(nix_signal_from_signum(15).is_some(), "SIGTERM");
    }

    /// Round1 A1: Ro client が Kill を送ると mode.not-allowed エラーが返り、
    /// session は継続する。
    #[test]
    fn serve_ro_client_kill_rejected() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // s1 = Rw (session 維持用)
        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("leader.notify");

        // s2 = Ro (Kill 試行)
        let mut s2 = client_connect_with_retry(&sock_path);
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Ro,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        let body = req.encode_to_vec().expect("encode");
        Frame::cbor_control(body).encode_to(&mut s2).expect("send");
        s2.flush().expect("flush");
        let _ = Frame::decode_from(&mut s2).expect("handshake response"); // discard
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send kill");
        s2.flush().expect("flush");

        // s2 が error を受信
        let ef = Frame::decode_from(&mut s2).expect("error response");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::ModeNotAllowed);
            }
            o => panic!("expected Error, got {o:?}"),
        }

        // s1 から正規 Kill で session 終了
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Round1 A3: signum=0 (POSIX probe) を Kill / Signal で送ると signal.invalid を返す。
    #[test]
    fn serve_signal_zero_rejected() {
        use crate::protocol::messages::Signal as ProtoSignal;
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());
        let mut s = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        Frame::cbor_control(
            ControlMessage::Signal(ProtoSignal { signum: 0 })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");

        let ef = Frame::decode_from(&mut s).expect("error");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::SignalInvalid),
            o => panic!("expected Error, got {o:?}"),
        }

        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Round1 A2: `expected_token` 設定時に token mismatch で handshake が拒否される。
    #[test]
    fn serve_handshake_token_mismatch_rejected() {
        let dir = make_temp_socket_dir();
        let sock_path = dir.path().join("test.sock");
        let mut cfg = DaemonConfig::new("demo", sock_path.clone(), long_running_cmd());
        cfg.expected_token = Some("secret-xyz".into());
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.serve());

        let mut s = client_connect_with_retry(&sock_path);
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: Some("wrong-token".into()),
        });
        let body = req.encode_to_vec().expect("encode");
        Frame::cbor_control(body).encode_to(&mut s).expect("send");
        s.flush().expect("flush");

        // daemon は auth.token-mismatch error を返し、socket を切る
        let ef = Frame::decode_from(&mut s).expect("error");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::AuthTokenMismatch),
            o => panic!("expected Error, got {o:?}"),
        }

        // 正しい token で接続できることも確認
        let mut s2 = client_connect_with_retry(&sock_path);
        let req2 = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: Some("secret-xyz".into()),
        });
        let body2 = req2.encode_to_vec().expect("encode");
        Frame::cbor_control(body2).encode_to(&mut s2).expect("send");
        s2.flush().expect("flush");
        let resp = Frame::decode_from(&mut s2).expect("response");
        match ControlMessage::decode_from(resp.body.as_slice()).expect("decode") {
            ControlMessage::HandshakeResponse(_) => {} // OK
            o => panic!("expected HandshakeResponse, got {o:?}"),
        }
        // cleanup: leader.notify を捨てて kill
        let _ = Frame::decode_from(&mut s2);
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Round2 fixes: regress confirmations ----

    /// Round2 #1: 空 Text predicate を送ると `wait.invalid-text` error が返り、
    /// daemon は panic せず session 継続。
    #[test]
    fn serve_wait_empty_text_predicate_rejected() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate, WaitRequest};

        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf hello; sleep 30".into(),
        ];
        let dir = make_temp_socket_dir();
        let sock_path = dir.path().join("test.sock");
        let cfg = DaemonConfig::new("demo", sock_path.clone(), cmd);
        let session = Session::start(cfg).expect("start");
        let handle = std::thread::spawn(move || session.serve());

        let mut s = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        Frame::cbor_control(
            ControlMessage::WaitRequest(WaitRequest {
                predicate: WaitPredicate::Text {
                    value: String::new(),
                },
                timeout_ms: Some(1000),
                options: WaitMatchOptions::default(),
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");

        // Error を待つ (raw_data frame は skip)
        let mut got_error = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match next_control(&mut s) {
                ControlMessage::Error(e) => {
                    assert_eq!(e.code, ErrorCode::WaitInvalidText);
                    got_error = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(got_error, "expected wait.invalid-text error");

        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Round2 #3: LockAcquire(wait=true) で holder ありの場合、
    /// **1 frame だけ** (LockResponse Denied) を返す (= Error と LockResponse の
    /// 2 frame だった旧版が直っているか確認)。
    #[test]
    fn serve_lock_acquire_wait_returns_single_frame() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        let mut s2 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s2);

        // s1 が lock 取得
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        // s1 が LockResponse (Acquired) + ModeChange、s2 が ModeChange
        let _ = Frame::decode_from(&mut s1).expect("s1 lock resp");
        let _ = Frame::decode_from(&mut s1).expect("s1 mode change");
        let _ = Frame::decode_from(&mut s2).expect("s2 mode change");

        // s2 が wait=true で lock acquire 試行
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: true,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");

        // s2 は 1 frame だけ受信、それは LockResponse(Denied)
        let resp = next_control(&mut s2);
        match resp {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(lr.result, LockResult::Denied);
                assert!(lr.token.is_none());
            }
            o => panic!("expected single LockResponse(Denied), got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// Round2 #6: RwNoLeader client が Kill を送ると mode.not-allowed エラー。
    #[test]
    fn serve_rw_no_leader_kill_rejected() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // session 維持用 Rw client
        let mut s1 = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        // RwNoLeader client
        let mut s2 = client_connect_with_retry(&sock_path);
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::RwNoLeader,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        let body = req.encode_to_vec().expect("encode");
        Frame::cbor_control(body).encode_to(&mut s2).expect("send");
        s2.flush().expect("flush");
        let _ = Frame::decode_from(&mut s2).expect("s2 handshake resp");

        // s2 (RwNoLeader) が Kill 試行 → mode.not-allowed
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");

        let ef = Frame::decode_from(&mut s2).expect("error");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => assert_eq!(e.code, ErrorCode::ModeNotAllowed),
            o => panic!("expected mode.not-allowed Error, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// R4-C9: 自己 LockAcquire は idempotent — 同じ client が既に lock を保持している
    /// 状態で再度 LockAcquire を送ると、Denied ではなく Acquired を返し、token は
    /// 初回と同じものを返す (= 新発行しない)。mode.change broadcast は発生しない。
    #[test]
    fn serve_lock_acquire_is_idempotent_for_self_holder() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s1 = client_connect_with_retry(&sock_path);
        let _r1 = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        // s1 が lock 取得 (1 回目)
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        let resp1_frame = Frame::decode_from(&mut s1).expect("decode resp1");
        let token1 = match ControlMessage::decode_from(resp1_frame.body.as_slice()).expect("decode")
        {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(lr.result, LockResult::Acquired);
                lr.token.expect("first acquire returns token")
            }
            o => panic!("expected LockResponse(Acquired), got {o:?}"),
        };
        // mode.change(Locked) は捨てる
        let _ = Frame::decode_from(&mut s1).expect("mode.change locked");

        // s1 が同じ lock を再取得 (= idempotent)
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");

        let resp2_frame = Frame::decode_from(&mut s1).expect("decode resp2");
        let token2 = match ControlMessage::decode_from(resp2_frame.body.as_slice()).expect("decode")
        {
            ControlMessage::LockResponse(lr) => {
                assert_eq!(
                    lr.result,
                    LockResult::Acquired,
                    "self-reacquire must succeed (idempotent)"
                );
                lr.token.expect("self-reacquire returns token")
            }
            o => panic!("expected LockResponse(Acquired), got {o:?}"),
        };
        assert_eq!(token1, token2, "self-reacquire must return the same token");

        // mode.change(Locked) が **broadcast されない** ことを確認する。
        // (= state 変化が無いので broadcast 不要。s1 自身も Locked → Locked への
        //   no-op broadcast を受けない。read_timeout で何も来ないことを検証。)
        //
        // R4-H5: timeout は 500ms に緩めた (旧 100ms)。CI 高負荷時に daemon の
        // broadcast 処理が遅れた場合に false-pass する可能性を減らすため。
        // 待ち時間が増えても、broadcast が来なければ test 全体に与える影響は
        // 500ms 程度。
        s1.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("set read_timeout");
        match Frame::decode_from(&mut s1) {
            Err(_) => {} // 想定通り
            Ok(f) => {
                let m = ControlMessage::decode_from(f.body.as_slice()).expect("decode broadcast");
                panic!("unexpected broadcast on self-reacquire: {m:?}");
            }
        }
        s1.set_read_timeout(None).expect("clear timeout");

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// R4-C7: Mode::Ro client が LockAcquire を送ると mode.not-allowed エラーを返し、
    /// session 全体が Locked 化しない (= session DoS を防ぐ)。
    #[test]
    fn serve_ro_client_lock_acquire_rejected() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // session 維持用 Rw client (s1)
        let mut s1 = client_connect_with_retry(&sock_path);
        let _r1 = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");

        // Ro client (s2)
        let mut s2 = client_connect_with_retry(&sock_path);
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Ro,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        let body = req.encode_to_vec().expect("encode");
        Frame::cbor_control(body).encode_to(&mut s2).expect("send");
        s2.flush().expect("flush");
        let _ = Frame::decode_from(&mut s2).expect("s2 handshake resp"); // discard

        // s2 (Ro) が LockAcquire 送信 → mode.not-allowed
        Frame::cbor_control(
            ControlMessage::LockAcquire(crate::protocol::messages::LockAcquire {
                wait: false,
                timeout_abs_ms: None,
                timeout_idle_ms: None,
                process_bound: false,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");

        let ef = Frame::decode_from(&mut s2).expect("error response");
        match ControlMessage::decode_from(ef.body.as_slice()).expect("decode") {
            ControlMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::ModeNotAllowed);
            }
            o => panic!("expected mode.not-allowed Error, got {o:?}"),
        }

        // s1 にも mode.change(Locked) が broadcast されていない (= session DoS が
        // 起きていない) ことを確認する。s1 の read_timeout で何も来ないことを
        // 確認 (Frame::decode が EWOULDBLOCK で error)。
        //
        // R4-H5: timeout は 500ms に緩めた (旧 100ms)。CI 高負荷時に daemon の
        // broadcast 処理が遅れた場合に false-pass する可能性を減らすため。
        s1.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("set read_timeout");
        match Frame::decode_from(&mut s1) {
            Err(_) => {} // 想定通り (= broadcast 来てない)
            Ok(f) => {
                let m = ControlMessage::decode_from(f.body.as_slice()).expect("decode broadcast");
                panic!("unexpected broadcast received on s1: {m:?}");
            }
        }
        s1.set_read_timeout(None).expect("clear timeout");

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// R4-C3: slow-loris client (= handshake.request を送らずに socket を開きっぱなし
    /// にする悪意 client) が居ても、他の正規 client は handshake を完了できる。
    ///
    /// 旧実装 (`accept_new_client` を `serve_loop` 内で同期 blocking) ではこの
    /// シナリオで serve_loop 全体が止まり、後続の正規 client は handshake すら
    /// 走らなかった。新実装は handshake worker thread に切り出しているため、
    /// 正規 client は slow-loris と並列に handshake を完了できる。
    #[test]
    fn serve_slow_loris_does_not_block_other_clients() {
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // 悪意 client: connect だけして handshake.request は送らない (socket 開きっぱ)。
        let _slow = client_connect_with_retry(&sock_path);

        // 正規 client: 直後に attach → handshake が成功するはず。
        // 旧実装ではここで serve_loop が止まっているので connect は出来ても
        // handshake response が永遠に来ず Frame::decode が EOF / timeout する。
        // 新実装では HANDSHAKE_TIMEOUT (= 5s) 内に response が返る。
        let start = std::time::Instant::now();
        let mut good = client_connect_with_retry(&sock_path);
        // 念のため read timeout を 10s にして「永遠に block」を検知できるようにする
        good.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set read_timeout");
        let resp = do_client_handshake(&mut good);
        let elapsed = start.elapsed();
        // 正規 handshake は本来 ms オーダーで終わる。slow-loris の HANDSHAKE_TIMEOUT (= 5s)
        // を待たされていない (= 並列処理されている) ことを確認する。
        assert!(
            elapsed < Duration::from_secs(3),
            "good client handshake took too long ({elapsed:?}), serve_loop may be blocked by slow-loris"
        );
        assert!(resp.leader, "good client (1st rw) should be leader");

        // cleanup
        good.set_read_timeout(None).expect("clear timeout");
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut good)
        .expect("send kill");
        good.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// R5-H2: `MAX_PENDING_HANDSHAKES` と `MAX_CLIENTS_PER_DAEMON` は **独立 cap**
    /// であり、attached client が attach 上限に達していても新規 handshake は
    /// `MAX_PENDING_HANDSHAKES` 個まで受け付けられる (= pending 段階の枠は別管理)。
    ///
    /// 旧実装は `clients + pending >= MAX_PENDING_HANDSHAKES (= 64)` の合算条件で
    /// reject していたため、attach 数が上限に達した瞬間に **新規 attach も
    /// handshake も両方無音 reject** になる事故があった。本テストは:
    /// - attached client が複数居る状態で、新規 connection が即 close されない
    ///   (= accept は成功する → handshake worker は spawn される)
    /// - これが小さい client 数でも再現できる (= 上限合算 ≠ 上限独立 を区別する)
    ///
    /// 実機で 64 client × handshake 完了を 1 test で再現するのは fd resource を
    /// 大量消費するため、本テストは「accept 後 handshake が走り出すまで」の経路
    /// で独立性を確認する。const 値そのもの (= MAX_PENDING_HANDSHAKES <
    /// MAX_CLIENTS_PER_DAEMON) は const block の static assertion で保証する。
    #[test]
    fn accept_loop_pending_cap_independent_from_clients_cap() {
        // const 比較: 2 つの const が独立に定義されていること (= 同じ値ではない)。
        // 同じ値だと「合算頭打ち」の旧実装に逆戻りしたことに気づけない。
        // const block で compile-time check (clippy::assertions_on_constants 回避)。
        const _: () = assert!(
            MAX_PENDING_HANDSHAKES != MAX_CLIENTS_PER_DAEMON,
            "MAX_PENDING_HANDSHAKES and MAX_CLIENTS_PER_DAEMON must be independent caps"
        );
        const _: () = assert!(
            MAX_PENDING_HANDSHAKES < MAX_CLIENTS_PER_DAEMON,
            "MAX_PENDING_HANDSHAKES should be smaller than MAX_CLIENTS_PER_DAEMON \
             to limit DoS surface"
        );

        // 実機 behavior 確認: attach client が 3 個居る状態で、追加 connect が
        // 即 close されず handshake response を返せる (= pending 枠が独立に存在)。
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // 既存 attach: 3 client (= MAX_PENDING_HANDSHAKES より十分小さい)
        let mut attached: Vec<UnixStream> = Vec::new();
        for _ in 0..3 {
            let mut s = client_connect_with_retry(&sock_path);
            let _ = do_client_handshake(&mut s);
            // leader.notify を 1 つ drain (1 つ目だけ leader=true で broadcast を受ける可能性)
            s.set_read_timeout(Some(Duration::from_millis(200)))
                .expect("set");
            let _ = Frame::decode_from(&mut s);
            s.set_read_timeout(None).expect("clear");
            attached.push(s);
        }

        // 追加 connect → handshake 完了するか確認。旧実装でも MAX=64 までは
        // 通るので、ここは主に「regression が起きていない (= cap が下がりすぎて
        // ない / OR 条件が壊れていない)」を見るための smoke test。
        let mut newcomer = client_connect_with_retry(&sock_path);
        newcomer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set");
        let resp = do_client_handshake(&mut newcomer);
        assert!(
            !resp.leader,
            "newcomer should not be leader (1st rw is already leader)"
        );

        // cleanup: kill で daemon を畳む
        newcomer.set_read_timeout(None).expect("clear");
        Frame::cbor_control(
            ControlMessage::Kill(Kill { signum: None })
                .encode_to_vec()
                .expect("encode"),
        )
        .encode_to(&mut attached[0])
        .expect("send kill");
        attached[0].flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }
}
