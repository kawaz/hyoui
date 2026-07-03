//! daemon 1 つ分の session 状態 + 起動ロジック。
//!
//! `Session::start` で:
//! 1. 子 PTY を `Pty::spawn` で起動 (= DR-0017 session anchor: openpty + 手動
//!    fork + execvp。daemon が controlling tty を握り、子は同 session・別 pgrp・
//!    foreground で起動)
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
// DR-0015: OnChildSuspend / OnParentSuspend は daemon 側で使わなくなった
// (= client 側で policy 発動、daemon は新 message を中継するのみ)。
#[cfg_attr(not(test), allow(unused_imports))]
use crate::protocol::Mode;
use crate::protocol::messages::{LeaderNotify, ModeChange};
use crate::protocol::{ControlMessage, Frame};
use crate::scrollback::Scrollback;
use crate::sys::clock::now_unix_ms;
use crate::sys::{
    FdExt, Pty, SelfPipe, UnixSock, install_default, install_self_pipe, poll::PollFlags,
    poll::PollOutcome, poll::poll, pty::Spawned, register_self_pipe,
};

use super::accept::{
    MAX_PENDING_HANDSHAKES, PendingHandshake, process_pending_handshakes, spawn_handshake_worker,
};
use super::broadcast::{
    ClientHandle, MAX_CLIENTS_PER_DAEMON, broadcast_control, broadcast_master_bytes, send_control,
};
use super::control::{ClientFrameOutcome, FrameOrError, handle_client_frame};
use super::lock::{LockEvent, LockMsg, SessionState, elevate_next_leader};
use super::pty::{
    ALIVE_RETRY_INTERVAL, ChildLifecycle, ChildState, ChildTransition, STOPPED_POLL_INTERVAL,
};
use super::reducer::{self, DaemonState, translate};
use super::screen::{ScreenState, StalledOutcome, check_stalled};
use super::{ChildSuspendPolicy, DaemonConfig};

/// R5-H7: send `sig` to the child's whole process group instead of only the
/// session-leader PID, so descendants that the shell may have backgrounded
/// (= grandchildren of the daemon, e.g. `sh -c 'sleep 100 &'`) are not
/// orphaned to `init`/`launchd`.
///
/// DR-0017 session anchor 構造では child は session leader **ではない** が、
/// fork 直後の `setpgid(0, 0)` で **process group leader** (= `pgid == pid`) には
/// なる。よって `kill(2)` with a negative pid (= POSIX `killpg(2)` 相当) は
/// child の pgrp 全体を変わらず狙える。
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
/// serve falls back to the legacy 500ms polling path. In that fallback the
/// serve_loop caps its `poll(2)` timeout at 500ms (see `cap_poll_timeout`
/// usage) and re-polls `ChildLifecycle` on every Timeout wake, so an idle
/// child's STOP / exit is still detected within ~500ms instead of blocking
/// forever — the only difference from the self-pipe path is detection latency
/// (ms vs ~500ms), not correctness.
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
/// `Some` on success (= SIGCHLD / SIGTSTP / SIGCONT will deliver into `pipe`),
/// `None` if either the lock is taken by another concurrent serve in this
/// process or the self-pipe / sigaction install fails. The `None` path is
/// non-fatal — the caller falls back to the legacy 500ms polling.
///
/// DR-0001 軸 1/2 配線: 同 self-pipe に **SIGTSTP / SIGCONT も register** する。
/// signal handler は signum を 1 byte 書き、serve_loop が drain 時に signum で
/// 分岐して `OnChildSuspend` / `OnParentSuspend` policy を発火させる。
/// SIGTSTP と SIGCONT のハンドラ install に失敗しても fatal にせず best-effort で
/// 進める (= 既存 SIGCHLD のみで動く既存挙動を維持)。
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
    // SIGTSTP (= 握り潰し) + SIGCONT (= stopped child の invariant 回復) を同
    // self-pipe に乗せる。SIGTSTP は handler 登録により kernel default の STOPPED を
    // 抑止し、handle_suspend_signals が何もしない (= daemon を外部 TSTP で止めさせない、
    // DR-0015 §2.3 で軸 2 廃止後の意図的挙動)。best-effort: install 失敗時は当該
    // signal の挙動が default に戻るだけで、SIGCHLD 経路 (= 軸 1 既存配線) は維持される。
    let _ = register_self_pipe(Signal::SIGTSTP);
    let _ = register_self_pipe(Signal::SIGCONT);
    // issue 2026-06-11 優先3: SIGTERM / SIGINT を同 self-pipe に乗せ、graceful
    // shutdown 経路 (= `--until` match と同じ killpg(SIGTERM) → finalize escalation
    // → SessionExitNotify → socket unlink) へ流す。handler 未登録だと daemon が即死し
    // child は SIGHUP 巻き添え死 + socket 残骸になる。best-effort install。
    let _ = register_self_pipe(Signal::SIGTERM);
    let _ = register_self_pipe(Signal::SIGINT);
    Some(SigchldOwner {
        pipe,
        _guard: guard,
    })
}

/// `SigchldOwner` を drop した後に SIGTSTP / SIGCONT の disposition を default に
/// 戻す helper。`SelfPipe` drop が `SELFPIPE_WRITE_FD` をクリアするので、handler
/// が late delivery で stale fd を触ることはないが、`sigaction` が install された
/// ままだと test 終了後にも process-wide で残留する。clean up を明示する。
fn release_suspend_signal_handlers() {
    let _ = install_default(Signal::SIGTSTP);
    let _ = install_default(Signal::SIGCONT);
    // 優先3: graceful shutdown 用に install した SIGTERM / SIGINT も default に戻す。
    let _ = install_default(Signal::SIGTERM);
    let _ = install_default(Signal::SIGINT);
}
use super::tail::{broadcast_tail_end_to_followers, tail_end_reason_from_outcome};

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
        // bug fix 2026-06-11: 子 PTY を `hyoui run` の起点 cwd で起動する (= 透過性回復、
        // DR-0005)。daemon 自身は daemonize 慣習で chdir("/") 済だが、子 (= claude 等)
        // は起動元 dir で動くべき。`config.cwd` が None (= test 経路や cwd 取得失敗) なら
        // chdir せず daemon の cwd を継承 (= 従来挙動、後方互換)。
        let Spawned { pty, child } =
            Pty::spawn(&argv, config.cols, config.rows, config.cwd.as_deref())?;
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
        // DR-0019 Update: on-child-suspend policy は runtime 変更可能 (= `hyoui set`)。
        // 起動時 config 値で初期化し、以降は SessionState 内の atomic を正本とする。
        state.init_child_suspend_policy(config.on_child_suspend);
        let mut scrollback = Scrollback::new(config.scrollback_bytes);
        // DR-0013 Phase B: daemon が screen state の正本を保持する。
        // 子 PTY bytes は本 state に流し込んでから broadcast / wait / tail に
        // 配る。attach 復元時は `build_attach_redraw` で生成した sequence を
        // 新 client に送る。
        //
        // `screen_input_log_bytes` で primary buffer 用 input log の容量を渡し、
        // resize 時の replay 救済策 (DR-0013 §7) を有効化する。byte-base scrollback
        // (= `Scrollback`) と rows-base scrollback (= vt100 内蔵 ring) は責務分離方針
        // のため両者は別 layer として並存し、rows-base 側は
        // `config.screen_vt100_scrollback_rows` で容量を指定する (= DR-0013 §8 Update
        // 配線、Phase C スコープの「scrollback 内蔵 ring を必要時に配線」を本タスクで
        // 実施)。
        let mut screen_state = ScreenState::with_input_log_capacity(
            config.rows,
            config.cols,
            config.screen_vt100_scrollback_rows,
            config.screen_input_log_bytes,
        );
        // DEC sync update 同期中に attach が発生した場合の deferred redraw 用。
        // sync が終了するまで redraw 送信を保留し、次の iteration で flush する
        // (DR-0013 §6 + alacritty `event_loop.rs:166` pattern)。
        let mut pending_redraws: Vec<u64> = Vec::new();

        // R5-H6: Try to acquire process-wide SIGCHLD self-pipe ownership.
        // The `Some` branch installs SIGCHLD → self-pipe so `poll(2)` wakes
        // immediately on child STOP/CONT/exit (= 500ms latency → ~ms).
        // The `None` branch falls back to the legacy ChildLifecycle polling
        // (correct, just slower transition detection) when another serve
        // already owns the slot in the same process (typically only happens
        // in concurrent test runs).
        let sigchld_owner = acquire_sigchld_selfpipe();

        // Issue #1 + user request: `--debug-dump=<path>` で子 PTY からの raw bytes を
        // append-only で file に書き出す。daemon process が直接 open / write し、
        // failure 時は stderr に warn 1 行のみで dump を諦める (= session は止めない)。
        let mut debug_dump_file: Option<std::fs::File> =
            config.debug_dump_path.as_ref().and_then(|p| {
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                {
                    Ok(f) => Some(f),
                    Err(e) => {
                        eprintln!(
                            "hyoui: --debug-dump open 失敗 (= path: {p:?}): {e} (dump 無効化)"
                        );
                        None
                    }
                }
            });

        let outcome = serve_loop(
            &pty,
            child,
            &listener,
            &mut clients,
            &mut next_client_id,
            config,
            &mut state,
            &mut scrollback,
            &mut screen_state,
            &mut pending_redraws,
            sigchld_owner.as_ref().map(|o| &o.pipe),
            debug_dump_file.as_mut(),
        );

        // Drop the SIGCHLD self-pipe explicitly before any further cleanup so
        // the global `SELFPIPE_WRITE_FD` is cleared and a subsequent serve in
        // the same process can claim the slot. self-pipe が消えた後は SIGTERM /
        // SIGINT / SIGTSTP / SIGCONT handler が走っても write skip (= no-op) なので、
        // handler disposition (= SIG_DFL への復帰) は shutdown シーケンス完了後まで
        // 据え置く (= 下記 `release_suspend_signal_handlers` 参照)。
        let had_owner = sigchld_owner.is_some();
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

        // DR-0015 §2.1: 子 PTY exit を契機に session.exit.notify を **全 attached
        // client に cap-aware broadcast**。buffer drain (= 上の per-client wait) の
        // 後で送ることで、子最後の出力 + exit notify が前後する order を保証する。
        // outcome から exit_status を組み立て、cap `session-exit-v1` を持つ client
        // にだけ届く (= 旧 client は skip、未対応 client への decode error 回避)。
        let exit_code = finalize_child(child, &outcome)?;
        {
            use crate::protocol::messages::SessionExitNotify;
            let notify = ControlMessage::SessionExitNotify(SessionExitNotify {
                exit_status: exit_code,
                signal: None, // finalize_child 結果は 128+signum 数値化済、signal name は補足
            });
            let _overflow = super::broadcast::broadcast_control_with_cap(
                &mut clients,
                &notify,
                "session-exit-v1",
            );
            // 送信後の overflow は client 切断扱いだが、ここは shutdown 直前なので無視。

            // 終端 drain: SessionExitNotify を writer thread が flush し切るまで wait。
            // これをしないと、enqueue 直後の `clients.clear()` で socket が閉じ、client が
            // SessionExitNotify を読む前に EOF を観測する race が起きる。issue 2026-06-11
            // 優先1 で client の socket EOF を `ConnectionLost` (= 非 0 exit) に分離した
            // ため、この race が顕在化した (= 子の正常 exit code を返すべき場面で exit 9 が
            // 漏れる)。
            //
            // budget は **per-client** で振る (= 先行の cleanup drain と同じ流儀、
            // `DRAIN_BUDGET_PER_CLIENT`)。共有 deadline 方式だと先頭 client の hang が
            // 後続 client の ExitNotify drain budget を食い潰し、複数 attach 時に
            // 後続が ExitNotify を読めず exit 9 に漏れる。per-client なら 1 client の
            // hang が他に波及しない。
            for ch in clients.iter() {
                let deadline = Instant::now() + DRAIN_BUDGET_PER_CLIENT;
                while ch.queued_bytes.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
        clients.clear();

        // DR-0015 Task 22 (linger pattern): 子 exit 直後に socket を即 unlink すると、
        // run の親 attach (= exec attach pattern) が間に合わずに ENOENT で失敗する
        // race が発生する (= 短命子 /bin/echo 等)。tmux / abduco 流の「子 exit 後も
        // 短時間 attach 待ち」を実装。linger 期間 (= 2 秒) listener から 1 client だけ
        // accept + handshake + SessionExitNotify 送信して終了。
        //
        // attach が来なくても linger 期間経過で socket close + daemon exit。既存 client
        // が attach 済の場合も 2 秒延長されるが、SessionExitNotify は既に broadcast 済
        // なので linger 中の通常 attach に対しても同じ exit_status を送る。
        if matches!(outcome, RelayOutcome::ChildExited(_)) {
            linger_for_late_attach(&listener, config, exit_code, &mut screen_state);
        }

        drop(listener);

        // DR-0001 軸 1/2 配線で install した SIGTSTP / SIGCONT / SIGTERM / SIGINT
        // handler を default に戻す。**socket unlink (= drop(listener)) を含む shutdown
        // シーケンス完了後**まで遅延させるのが要点 (= issue 2026-06-11 優先3 後退修正)。
        //
        // 早すぎる SIG_DFL 復帰は危険: SIGTERM/SIGINT 経路の shutdown は
        // killpg(SIGTERM) → finalize_child の escalation (最長 ~6s) → SessionExitNotify
        // broadcast → drain → socket unlink と続く。この間に 2 発目の SIGTERM が来た時
        // (= OS shutdown は TERM → 猶予 → 再 TERM が普通)、handler を既に SIG_DFL に
        // 戻していると daemon が即死し、escalation も socket unlink もすっ飛ぶ
        // (= child 巻き添え + socket 残骸)。handler を据え置けば 2 発目以降は
        // self-pipe write skip の no-op で握り潰され、shutdown シーケンスが完走する。
        //
        // SIGCHLD は既存配線 (R5-H6) と整合するため install_default はしない
        // (= test 終了後に同 disposition が残留することを許容、handler 自体は
        // SELFPIPE_WRITE_FD == -1 で write skip するため副作用なし)。
        if had_owner {
            release_suspend_signal_handlers();
        }

        match outcome {
            RelayOutcome::ChildExited(_) | RelayOutcome::ClientDetachedOrKilled => Ok(exit_code),
            RelayOutcome::Error(e) => Err(e),
        }
    }
}

/// DR-0015 Task 22: 子 PTY exit 後に短時間 attach を待つ linger helper。
///
/// `Session::serve` が `ChildExited` を観測した後、socket を即 unlink せず最大
/// `LINGER_DURATION` (= 2 秒) listener から accept + handshake を継続する。
/// 1 client が handshake 完了したら `SessionExitNotify` を送って即 break。
/// timeout なら何もせず終了 (= 既存挙動と同じく socket close)。
fn linger_for_late_attach(
    listener: &UnixSock,
    config: &DaemonConfig,
    exit_status: i32,
    screen_state: &mut ScreenState,
) {
    use crate::daemon::accept::{process_pending_handshakes, spawn_handshake_worker};
    use crate::protocol::messages::SessionExitNotify;

    const LINGER_DURATION: std::time::Duration = std::time::Duration::from_secs(2);
    const POLL_TIMEOUT_MS: u16 = 50;

    let deadline = Instant::now() + LINGER_DURATION;
    let mut pending_handshakes: Vec<PendingHandshake> = Vec::new();
    let mut next_client_id: u64 = 0;
    let mut clients: Vec<ClientHandle> = Vec::new();
    let mut state = SessionState::default();
    let mut overflow_ids: Vec<u64> = Vec::new();
    let mut pending_redraws: Vec<u64> = Vec::new();

    loop {
        if Instant::now() >= deadline {
            break;
        }

        // poll: listener (= 新 attach) + pending handshake 完了通知 (= mpsc は fd-poll
        // できないので短い timeout で try_recv)
        let listener_fd = listener.as_fd();
        let mut poll_fds: Vec<PollFd> = vec![PollFd::new(listener_fd, PollFlags::POLLIN)];
        let _ = poll(&mut poll_fds, PollTimeout::from(POLL_TIMEOUT_MS));

        let listener_revents = poll_fds[0].revents().unwrap_or(PollFlags::empty());
        drop(poll_fds);

        // 新 attach の accept
        if listener_revents.contains(PollFlags::POLLIN)
            && clients.len() < MAX_CLIENTS_PER_DAEMON
            && pending_handshakes.len() < MAX_PENDING_HANDSHAKES
            && let Ok(pending) = spawn_handshake_worker(listener, config)
        {
            pending_handshakes.push(pending);
        }

        // 完了済 handshake を回収
        process_pending_handshakes(
            &mut pending_handshakes,
            config,
            &mut next_client_id,
            &mut clients,
            &mut state,
            &mut overflow_ids,
            screen_state,
            &mut pending_redraws,
        );

        // 1 client でも attach 完了したら SessionExitNotify を送って break。
        if !clients.is_empty() {
            // Task 25 race 対策: process_pending_handshakes が handshake.response を
            // enqueue した直後で、writer thread が flush する前に SessionExitNotify
            // を続けて enqueue すると、CI macOS で client 側 decode タイミングが
            // 不安定になる (= `handshake.response decode failed`)。
            // handshake.response の queued_bytes が 0 になるまで wait してから
            // SessionExitNotify を送る (= 順序を sequential に強制)。
            let handshake_drain_deadline = Instant::now() + std::time::Duration::from_millis(500);
            for ch in clients.iter() {
                while ch.queued_bytes.load(std::sync::atomic::Ordering::Acquire) > 0
                    && Instant::now() < handshake_drain_deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }

            let notify = ControlMessage::SessionExitNotify(SessionExitNotify {
                exit_status,
                signal: None,
            });
            let _ = super::broadcast::broadcast_control_with_cap(
                &mut clients,
                &notify,
                "session-exit-v1",
            );
            // 終端 drain: SessionExitNotify を send 完了するまで wait (= CI race 緩和、
            // 旧 200ms → 1000ms)
            let drain_deadline = Instant::now() + std::time::Duration::from_millis(1000);
            for ch in clients.iter() {
                while ch.queued_bytes.load(std::sync::atomic::Ordering::Acquire) > 0
                    && Instant::now() < drain_deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            clients.clear();
            break;
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

/// DR-0013 §6 Phase A: DEC sync update 終了で `pending_redraws` を flush する。
///
/// `screen_state.sync_in_progress()` が false に戻った瞬間、保留中の client_id に
/// 対して `send_attach_redraw` 相当を実行する。enqueue 失敗時は当該 client_id を
/// `overflow_ids` に積み、caller が drop する。
fn flush_pending_redraws_if_sync_over(
    clients: &[ClientHandle],
    screen_state: &ScreenState,
    pending_redraws: &mut Vec<u64>,
    overflow_ids: &mut Vec<u64>,
) {
    if screen_state.sync_in_progress() || pending_redraws.is_empty() {
        return;
    }
    // sync が解けた → 全 pending を flush する
    let ids: Vec<u64> = std::mem::take(pending_redraws);
    for id in ids {
        if let Some(ch) = clients.iter().find(|c| c.id == id) {
            super::accept::send_attach_redraw(ch, screen_state, overflow_ids);
        }
        // client が既に居ない場合は黙って drop (= overflow 経路で先に切られた等)
    }
}

/// DR-0013 §5 Phase B: stalled sequence の 5s 検出と自動 reset 判定。
///
/// 動作 (DR-0013 task A-8 解消):
/// - `check_stalled` で Detected/Healthy を取得
/// - `ScreenState::note_stalled_outcome` で連続 detect counter を進める
/// - 連続 3 回 (= 15s) detect されると `StalledAction::ResetRequested` が返るので
///   `ScreenState::reset` を呼び、警告 log を出す
/// - 既存 `warned` flag は「同じ detect cycle で warn を 1 度だけ出す」用途、
///   feed 復帰 (= note が Healthy 受信で counter リセット) で再 warn 可能になる
fn detect_and_warn_stalled(screen_state: &mut ScreenState, warned: &mut bool) {
    let outcome = check_stalled(screen_state, Instant::now());
    let detected = matches!(outcome, StalledOutcome::Detected);
    let action = screen_state.note_stalled_outcome(detected);
    if detected && !*warned {
        // Hotfix (透過原則違反 bug): daemon の stderr は非 detached 起動時に
        // child PTY と同じ TTY に向いている (= 親 process 内で daemon thread が走る)
        // ため、`eprintln!` で warning を出すと attach 中 client の画面に混入する。
        // MVP では完全 silent 化して画面汚染を止める。stalled detect counter / 自動
        // reset 機構自体は維持 (= 機能は失わない)。
        // TODO: 別 channel (= XDG_STATE_HOME/hyoui/<session>.log 等の log file) に
        // warning を出す経路を整備する。
        *warned = true;
    }
    if action.is_some() {
        // DR-0013 §5 Phase B: 連続 detect 上限到達 → 自動 reset。state を捨てる
        // (= cells / cursor / mode 全消し) が、broken stream からの復旧を優先する。
        // warning 表示は同上の理由で silent (TODO: log file 経路)。
        screen_state.reset();
        // reset 後は note の counter も 0 になっている (reset 内で初期化済)。
        // warn flag も解除して、次サイクル以降の detect で新 warn を許可する。
        *warned = false;
    }
    if !detected {
        // feed 復帰時に warn flag をリセット (= 次回 stalled で改めて warn できる)
        *warned = false;
    }
}

/// DR-0001 軸 2 + invariant 回復: self-pipe から drain した signal byte 列を
/// 走査し、SIGTSTP / SIGCONT に対応する policy を発火する。
///
/// - **SIGTSTP** (= 親 daemon が外部から `kill -TSTP <pid>` 等で suspend 要求):
///   `OnParentSuspend` に従い、`Transparent` なら子 pgrp に SIGSTOP を投げて
///   から親自身に `raise(SIGSTOP)`、`Decouple` なら親だけ `raise(SIGSTOP)`。
///   `raise(SIGSTOP)` は kernel に処理させるため、handler 内ではなく serve_loop
///   コンテキスト (= 同期 path) で呼ぶ。
/// - **SIGCONT** (= 外側 `fg` 等で親が再開した):
///   DR-0001 §invariant 回復ルール: 子が STOPPED なら `killpg(child, SIGCONT)`
///   を送って復帰させる (= 「親 fg かつ子 stop」禁則の論理的解消)。
///
/// SIGCHLD バイトは本 helper では処理しない (= caller 側で `lifecycle.poll_with_transition`
/// を介して transition 判定 → `handle_child_transition` に流す)。
///
/// 戻り値 `Some(RelayOutcome)` は serve_loop を即時終了させる場合 (= issue 2026-06-11
/// 優先3 で SIGTERM / SIGINT を統合した。受信時に child pgrp へ SIGTERM を送り、
/// `RelayOutcome::ClientDetachedOrKilled` を返して `--until` match と同じ
/// finalize escalation (CONT+TERM → grace → KILL) → SessionExitNotify broadcast →
/// socket unlink 経路に乗せる)。
/// `handle_suspend_signals` の SIGCONT 経路で使うフォールバック判定。
///
/// 「daemon STOPPED 中に child が STOP → daemon に CONT」順序では SIGCHLD と
/// SIGCONT が同一 drain batch に入り、本判定の時点ではまだ child の Stopped
/// transition が `ChildLifecycle::poll_with_transition` に消費されていない (=
/// `is_stopped()` latch が false)。この window では transition がまだ kernel の
/// wait queue に残っているので、`waitpid(WNOHANG | WUNTRACED)` で直接 Stopped を
/// 拾える。`ChildLifecycle` の latch state は更新しない (= 先取りした transition
/// は後段 poll では二度と観測されないが、起こすだけが目的なので問題ない)。
///
/// Stopped 以外 (= Alive / Exited / error) は false を返す。
fn child_is_stopped_via_waitpid(child: Pid) -> bool {
    let flags = WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED;
    matches!(waitpid(child, Some(flags)), Ok(WaitStatus::Stopped(_, _)))
}

/// DR-0015 §2.3: 軸 2 (= 親 hyoui 自身の外部 SIGTSTP 経路) は廃止。
///
/// 新構成では daemon process は常駐し、attach client process が外部 SIGTSTP を
/// 受けて止まっても daemon は影響を受けない (= 別 process、socket は close されず
/// 単に「その client が応答しなくなる」だけ)。よって daemon が自身に SIGTSTP を
/// 受け取って処理する経路は本来不要。
///
/// ただし daemon 自身が `--detached` で fork され session leader として動く時、
/// 親 (= attach client) からの SIGTSTP/SIGCONT は届かない (= 別 process group)。
/// 残るのは「外部から `kill -TSTP <daemon-pid>` で daemon を直接止める」case のみで、
/// daemon は SIGTSTP の self-pipe handler を登録しているため **STOPPED に入らず
/// signal を握り潰す** (= 常駐 anchor を外部 TSTP で止めさせない意図、実機確認済み)。
///
/// よって本 helper は SIGCONT 経路 (= daemon 自身が SIGCONT で復帰した時の子の
/// invariant 回復) と SIGTERM / SIGINT (= graceful shutdown) を扱う。SIGTSTP 経路は
/// handler 登録を維持しつつ何もしない (= 握り潰す。register をやめると kernel
/// default の STOPPED に戻るため、登録は意図的に残す)。
fn handle_suspend_signals(
    drained: &[u8],
    child: Pid,
    _config: &DaemonConfig,
    lifecycle: &ChildLifecycle,
    state: &SessionState,
) -> Option<RelayOutcome> {
    for &b in drained {
        let sig_i32 = b as i32;
        if sig_i32 == Signal::SIGCONT as i32 {
            // 外部 `kill -CONT <daemon-pid>` で daemon が再開した時、子が STOPPED
            // なら一緒に起こす (= 防衛策、軸 2 廃止後も「daemon だけ動いて子 STOP」
            // 状態を残さない)。
            //
            // issue 2026-06-11 優先2: ここで自前 `waitpid(WUNTRACED)` を呼んで
            // stopped 判定すると **不発する**。child の Stopped transition は SIGCHLD
            // 経由で既に `lifecycle.poll_with_transition` が消費済で、再度 waitpid を
            // 呼んでも `StillAlive` が返る (= kernel は Stopped transition を一度しか
            // 報告しない)。そのため latch 済の `lifecycle.is_stopped()` を参照する。
            //
            // ただし latch だけでは「daemon STOPPED 中に child が STOP → daemon に
            // CONT」を取りこぼす。この順序では SIGCHLD (child STOP) と SIGCONT
            // (daemon) が同一 drain batch に入り、serve_loop は本 helper を
            // `poll_with_transition` より **先** に呼ぶため、ここに来た時点ではまだ
            // Stopped transition が消費されておらず `is_stopped()` は false (= latch
            // 未設定)。この window だけ、Stopped transition がまだ kernel の wait
            // queue に残っている (= poll 未実行) ので、自前 `waitpid(WNOHANG |
            // WUNTRACED)` が **当該ケースに限り** Stopped を拾える (= 後段の
            // `poll_with_transition` が拾うはずだった transition を先取りする形)。
            //
            // Design rationale: latch (= 通常ケース) と直接 waitpid フォールバック
            // (= 同一 batch ケース) の両建てで両ケースをカバーする。drain を poll の
            // 後ろへ移す案より既存 serve_loop 構造への影響が小さい (= drain → poll の
            // 順序、SIGTERM/SIGINT 処理位置を変えずに済む)。直接 waitpid は
            // `lifecycle` の latch state を更新しない (= &ChildLifecycle のまま) が、
            // ここで先取りした Stopped transition は後段 poll では二度と観測されない
            // ので、起こすだけで十分 (= 子は CONT 後 Continued transition を出し、
            // 後段で `record_child_continued` 経路に乗る)。
            if lifecycle.is_stopped() || child_is_stopped_via_waitpid(child) {
                let _ = kill_pgrp(child, Signal::SIGCONT);
            }
        } else if sig_i32 == Signal::SIGTERM as i32 || sig_i32 == Signal::SIGINT as i32 {
            // issue 2026-06-11 優先3: graceful shutdown。`--until` match と同じ経路
            // (= killpg(SIGTERM) → finalize escalation → SessionExitNotify → socket
            // unlink) に乗せる。handler 未登録だと daemon 即死 → child SIGHUP 巻き添え
            // 死 + socket 残骸になっていた。
            let reason = if sig_i32 == Signal::SIGINT as i32 {
                "sigint"
            } else {
                "sigterm"
            };
            state.record_registry.push_lifecycle(
                super::record::LifecycleEvent::SessionTerminatedByCondition {
                    reason: reason.to_string(),
                    ts_unix_ms: now_unix_ms(),
                },
            );
            let _ = kill_pgrp(child, Signal::SIGTERM);
            return Some(RelayOutcome::ClientDetachedOrKilled);
        }
        // SIGTSTP は意図的に無視する (= 軸 2 廃止、DR-0015 §2.3)。
        //
        // daemon は SIGTSTP を self-pipe handler 経由で受け、本 loop は何もしない。
        // この結果、外部 `kill -TSTP <daemon-pid>` を送っても daemon は STOPPED に
        // **入らない** (= signal が self-pipe へ吸い込まれて消える、実機確認済み:
        // findings 2026-06-11 §仮説2)。これは「daemon を外部 TSTP で止めさせない」
        // 意図的な挙動 (= daemon は常駐 anchor であるべきで、外部からの suspend で
        // 子もろとも止まると透過性が壊れる)。
        //
        // register をやめると kernel default の STOPPED に戻ってしまうため、handler
        // 登録は維持して「受けるが何もしない」状態を保つ。daemon を本当に止めたい
        // なら `kill -STOP` (= catch 不能) を使う。
        // SIGCHLD / 他の signum もここでは処理しない (= caller 側 lifecycle 経路)。
    }
    None
}

/// 子の **新規** `Stopped` transition を観測した瞬間に呼ばれる
/// (= `lifecycle.poll_with_transition`)。
///
/// DR-0017 §柱2: ユーザ / 端末起因の stop (SIGTSTP / SIGSTOP) は **意図的な操作**
/// なので daemon は **勝手に起こさない**。`SessionChildStoppedNotify` で leader に
/// follow 判断を委ねるのみ。leader 不在 / cap 不足でも **SIGCONT は送らず**、
/// stopped のまま残す (= 外側 API `hyoui kill --signal=CONT` で起こせるため
/// 「誰も起こせない」状況は構造的に存在しない)。`state.child_stopped` を立てて
/// status/list での可観測性を担保する。
///
/// daemon は **直接 raise(SIGSTOP) も親 termios も触らない**。
///
/// 子の self-stop だけが本 path に来る (= DR-0015 §2.3 で軸 2 廃止のため daemon
/// 自身が `killpg(child, SIGSTOP)` する経路は存在しない)。
fn notify_child_stopped(
    child: Pid,
    clients: &mut [ClientHandle],
    state: &SessionState,
    sig_observed: i32,
) -> Vec<u64> {
    use crate::protocol::messages::SessionChildStoppedNotify;

    // DR-0019 Update: policy は runtime 変更可能なので SessionState から読む
    // (= `hyoui set` で変更された最新値を反映)。
    let policy = state.child_suspend_policy();

    // 子が stopped であることを記録 (= status/list の可観測性、DR-0017 §柱2)。
    // AutoResume でも一旦立てる (= 直後の SIGCONT で Continued 観測時に下りる)。
    state.set_child_stopped(true);

    // DR-0016 §3: child-stopped-observed lifecycle event (= 4 段階の 1 段階目)。
    // WUNTRACED で stop transition を初めて観測した瞬間に push する。
    let sig_name = sig_num_to_name(sig_observed);
    state
        .record_registry
        .push_lifecycle(super::record::LifecycleEvent::ChildStoppedObserved {
            sig_name: sig_name.clone(),
            sig_num: sig_observed,
            pid: child.as_raw() as u32,
            ts_unix_ms: now_unix_ms(),
        });

    // DR-0019: auto-resume policy では daemon が即座に子を起こす。
    //
    // Design rationale: ここで `SessionChildStoppedNotify` を送ら**ない**。
    // 通知すると leader client が follow して自身を SIGSTOP した直後に、daemon が
    // 子だけ SIGCONT で復帰させてしまい、client が置き去り (= 子は動くのに人間の
    // 端末は止まったまま) になる race が生じる。auto-resume の意図は「誰も follow
    // させず、子を即復帰させる」なので、stopped event の record だけ残して通知は
    // 抑止する。子の Continued は次回 poll の `record_child_continued` 経路で記録される。
    if policy == ChildSuspendPolicy::AutoResume {
        let _ = kill_pgrp(child, Signal::SIGCONT);
        return Vec::new();
    }

    // leader を探す。複数 leader はあり得ない設計 (= broadcast.rs::elevate_next_leader)。
    let leader_idx = clients.iter().position(|ch| ch.leader);
    let Some(idx) = leader_idx else {
        // leader 不在: DR-0017 §柱2 で auto-resume fallback を廃止。SIGCONT を
        // 送らず stopped のまま残す (= 外側 API で起こせる)。notify 先がいないので
        // 何もしない。
        return Vec::new();
    };
    // cap check: leader が `child-state-v1` を持たない場合も同様に SIGCONT を送らず
    // notify もしない (= stopped のまま残す、外側 API で起こせる)。
    if !clients[idx]
        .negotiated_caps
        .iter()
        .any(|c| c == "child-state-v1")
    {
        return Vec::new();
    }
    // notify 送信 (= 単一 receiver、cap-aware broadcast helper を使うほどでもない)
    let msg = ControlMessage::SessionChildStoppedNotify(SessionChildStoppedNotify {
        pid: child.as_raw() as u32,
        signal: Some(sig_name),
    });
    if send_control(&clients[idx], msg) {
        Vec::new()
    } else {
        vec![clients[idx].id]
    }
}

/// DR-0016 §3: signal 番号から canonical な signal 名 (= "SIGTSTP" 等) を返す。
/// nix の `Signal::try_from` で reverse lookup、未知 signal は "SIG<N>" fallback。
fn sig_num_to_name(sig: i32) -> String {
    match nix::sys::signal::Signal::try_from(sig) {
        Ok(s) => s.as_str().to_string(),
        Err(_) => format!("SIG{sig}"),
    }
}

/// DR-0016 §3 4 段階の 4 段階目 (= `child-continued-observed`)。
///
/// `WaitStatus::Continued` は signal を carry しないが、kernel が continued を
/// 報告するのは SIGCONT で起きた時だけなので、固定で SIGCONT 名 + 番号で push する。
fn record_child_continued(state: &SessionState, child: Pid) {
    // DR-0017 §柱2: 子が再開したので stopped 観測フラグを下ろす (= status/list 整合)。
    state.set_child_stopped(false);
    let sig = nix::sys::signal::Signal::SIGCONT as i32;
    state
        .record_registry
        .push_lifecycle(super::record::LifecycleEvent::ChildContinuedObserved {
            sig_name: "SIGCONT".to_string(),
            sig_num: sig,
            pid: child.as_raw() as u32,
            ts_unix_ms: now_unix_ms(),
        });
}

// `handle_child_transition` は DR-0015 §2.2 で廃止 (= callback inject 経路の入り口
// だった)。新方針では `notify_child_stopped` を caller (= serve_loop) が直接呼ぶ。
// transition::Stopped 以外 (Continued / Exited) は caller 側で処理する形に統一。

/// `current` を上限 `cap_ms` で頭打ちする。`current` が `NONE` (= 無限 block)
/// または `cap_ms` より大きいときだけ `cap_ms` に縮める。それ以外 (= 既により
/// 短い timeout) はそのまま返す。
///
/// ※ nix の `PollTimeout::as_millis` は `NONE` 時に内部 unwrap で panic するため、
///   先に `is_none()` で分岐してから ms を取り出す。
fn cap_poll_timeout(current: PollTimeout, cap_ms: u16) -> PollTimeout {
    let cap = PollTimeout::from(cap_ms);
    if current.is_none() {
        cap
    } else if let Some(ms) = current.as_millis() {
        if ms > cap_ms as u32 { cap } else { current }
    } else {
        current
    }
}

/// DR-0019 §4: daemon 側終了条件 (overall `--timeout` / idle `--idle-timeout`) の
/// 発火判定。発火するなら理由文字列 (`"timeout"` / `"idle-timeout"`)、しなければ
/// `None` を返す。
///
/// - `elapsed_since_start`: serve_loop 開始からの経過 (= overall 基準)
/// - `elapsed_since_output`: 最終 master 出力からの経過 (= idle 基準)
/// - 両方発火条件を満たす場合は overall を優先 (= 理由を一意に決める。SIGTERM 手順は
///   どちらも同じなので優先順位は表示上の問題のみ)
fn eval_timeout(
    elapsed_since_start: std::time::Duration,
    elapsed_since_output: std::time::Duration,
    timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
) -> Option<&'static str> {
    if let Some(ms) = timeout_ms
        && elapsed_since_start.as_millis() >= u128::from(ms)
    {
        return Some("timeout");
    }
    if let Some(ms) = idle_timeout_ms
        && elapsed_since_output.as_millis() >= u128::from(ms)
    {
        return Some("idle-timeout");
    }
    None
}

/// DR-0019 §4: timeout / idle-timeout が有効なとき、最も近い deadline までの残り
/// ミリ秒を返す (= `poll(2)` timeout の cap に使い、deadline で確実に wake させる)。
///
/// 両方有効なら近い方 (= 小さい残り)。既に超過していたら `Some(0)` (= 即 wake)。
/// 両方無効なら `None` (= cap しない)。
fn timeout_poll_cap_ms(
    elapsed_since_start: std::time::Duration,
    elapsed_since_output: std::time::Duration,
    timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
) -> Option<u64> {
    let overall_rem = timeout_ms.map(|ms| {
        let elapsed = u64::try_from(elapsed_since_start.as_millis()).unwrap_or(u64::MAX);
        ms.saturating_sub(elapsed)
    });
    let idle_rem = idle_timeout_ms.map(|ms| {
        let elapsed = u64::try_from(elapsed_since_output.as_millis()).unwrap_or(u64::MAX);
        ms.saturating_sub(elapsed)
    });
    match (overall_rem, idle_rem) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
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
    screen_state: &mut ScreenState,
    pending_redraws: &mut Vec<u64>,
    sigchld_pipe: Option<&SelfPipe>,
    debug_dump: Option<&mut std::fs::File>,
) -> RelayOutcome {
    // debug_dump は loop 内で再借用するため局所変数に move する。
    let mut debug_dump = debug_dump;
    // DR-0013 §5: stalled sequence の 5s timeout 検出は per-loop で行う。
    // 連続して warn を撒かないよう、検出後は flag を立てて feed が来るまで
    // 黙る (= 1 度 detect したら次の feed まで再 detect しない方針)。
    let mut stalled_warned = false;
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
    // DR-0019 §4: 終了条件 (overall `--timeout` / idle `--idle-timeout`) の基準時刻。
    // overall は serve_loop 開始から、idle は最終 master 出力から計測する。起動後
    // 一度も出力が無いケースでも idle が発火するよう、`last_output` の初期値は
    // `serve_start` と同じにする。
    let serve_start = Instant::now();
    let mut last_output = serve_start;
    // DR-0025 Phase 1b 後半 (translate 併走): serve_loop の各 IO event を DaemonMsg に
    // 写して super-reducer を実走させる。現段階では全 domain reducer が stub のため
    // effect は出ず (= 各挿入点の debug_assert で担保)、既存 handler が従来通り挙動を
    // 担う (= 挙動不変)。lock の実 state は SessionState (`state.lock`、Phase 1a) 側に
    // あり、`daemon_state.lock` は未使用の空 stub のまま置く (= 二重管理して食い違わせ
    // ない、Phase 2 で SessionState.lock を DaemonState へ移設する)。
    let mut daemon_state = DaemonState::default();
    loop {
        // DR-0019 §4: 終了条件の発火判定 (= ループ冒頭で毎回チェック)。発火したら
        // `--until` match と同じ手順 (= killpg(SIGTERM) → finalize escalation) に乗せ、
        // 発火理由を lifecycle event に残す。
        if config.timeout_ms.is_some() || config.idle_timeout_ms.is_some() {
            let now = Instant::now();
            if let Some(reason) = eval_timeout(
                now.duration_since(serve_start),
                now.duration_since(last_output),
                config.timeout_ms,
                config.idle_timeout_ms,
            ) {
                state.record_registry.push_lifecycle(
                    super::record::LifecycleEvent::SessionTerminatedByCondition {
                        reason: reason.to_string(),
                        ts_unix_ms: now_unix_ms(),
                    },
                );
                let _ = kill_pgrp(child, Signal::SIGTERM);
                return RelayOutcome::ClientDetachedOrKilled;
            }
        }

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

        // poll timeout の決定。デフォルトは無限 block (= 別 fd の POLLIN /
        // self-pipe wake で起きる) で、以下の cap を順に適用して頭打ちする:
        //
        // - R4-C3: pending handshake がある間は 50ms。完了通知 (mpsc) は fd-poll
        //   できないため、短い周期で try_recv する必要がある。
        // - R5-H6 fallback: SIGCHLD self-pipe の ownership を取れず
        //   `sigchld_pipe == None` で動く serve は、poll_fds に self-pipe fd を
        //   積めない。pending_handshakes が空のとき poll が無限 block すると、
        //   idle 中 (= master 出力も client 入力も無い) の子 STOP / exit を
        //   一切検出できず、子が死んでも daemon が永久に起きない correctness
        //   regression になる。legacy polling 経路 (ChildLifecycle, ~500ms 粒度)
        //   に合わせて poll timeout を 500ms で頭打ちし、Timeout 経路の
        //   `lifecycle.poll_with_transition` で子の state 変化を拾えるようにする。
        let mut poll_timeout = PollTimeout::NONE;
        if !pending_handshakes.is_empty() {
            const HANDSHAKE_POLL_CAP_MS: u16 = 50;
            poll_timeout = cap_poll_timeout(poll_timeout, HANDSHAKE_POLL_CAP_MS);
        }
        if sigchld_pipe.is_none() {
            const NO_SELFPIPE_POLL_CAP_MS: u16 = 500;
            poll_timeout = cap_poll_timeout(poll_timeout, NO_SELFPIPE_POLL_CAP_MS);
        }
        // DR-0019 §4: overall / idle timeout が有効なら、deadline までの残りで
        // poll を cap し、deadline 到達時に確実に wake してループ冒頭の eval_timeout
        // で発火させる。u16 上限 (= 約 65s) を超える残りは 65s ごとに wake して
        // 再評価する (= 最終的に必ず発火、過剰精度は不要)。
        if config.timeout_ms.is_some() || config.idle_timeout_ms.is_some() {
            let now = Instant::now();
            if let Some(rem) = timeout_poll_cap_ms(
                now.duration_since(serve_start),
                now.duration_since(last_output),
                config.timeout_ms,
                config.idle_timeout_ms,
            ) {
                let cap = u16::try_from(rem).unwrap_or(u16::MAX);
                poll_timeout = cap_poll_timeout(poll_timeout, cap);
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
                // R5-H6 + DR-0001 軸 1/2: SIGCHLD / SIGTSTP / SIGCONT may have
                // arrived. Drain self-pipe + dispatch policy. EINTR alone
                // (without self-pipe ready) still benefits from the same drain
                // (= no-op if empty).
                if let Some(sp) = sigchld_pipe {
                    let drained = sp.drain().unwrap_or_default();
                    if let Some(outcome) =
                        handle_suspend_signals(&drained, child, config, &lifecycle, state)
                    {
                        return outcome;
                    }
                }
                let (child_state, transition) = lifecycle.poll_with_transition(child);
                if let ChildState::Exited(code) = child_state {
                    return RelayOutcome::ChildExited(code);
                }
                match transition {
                    Some(ChildTransition::Stopped { sig }) => {
                        let overflow = notify_child_stopped(child, clients, state, sig);
                        overflow_ids.extend(overflow);
                    }
                    Some(ChildTransition::Continued) => record_child_continued(state, child),
                    _ => {}
                }
                continue;
            }
            Ok(PollOutcome::Timeout) => {
                drop(poll_fds);
                // R5-H6 fallback: self-pipe を持たない serve では poll が
                // child state transition で起きないため、timeout 起床のたびに
                // legacy polling 経路で子の STOP / CONT / exit を拾う。
                // self-pipe を持つ通常 path はここで lifecycle を触らない
                // (= Interrupted / sigchld_ready 経路で処理済、二重 poll を避ける)。
                if sigchld_pipe.is_none() {
                    let (child_state, transition) = lifecycle.poll_with_transition(child);
                    if let ChildState::Exited(code) = child_state {
                        return RelayOutcome::ChildExited(code);
                    }
                    match transition {
                        Some(ChildTransition::Stopped { sig }) => {
                            let overflow = notify_child_stopped(child, clients, state, sig);
                            overflow_ids.extend(overflow);
                        }
                        Some(ChildTransition::Continued) => record_child_continued(state, child),
                        _ => {}
                    }
                }
                process_pending_handshakes(
                    &mut pending_handshakes,
                    config,
                    next_client_id,
                    clients,
                    state,
                    &mut overflow_ids,
                    screen_state,
                    pending_redraws,
                );
                // DR-0013 §5 + §6 Phase A:
                // - sync 終了で pending redraw を flush
                // - 5s stalled detect (Phase A は warn のみ)
                flush_pending_redraws_if_sync_over(
                    clients,
                    screen_state,
                    pending_redraws,
                    &mut overflow_ids,
                );
                detect_and_warn_stalled(screen_state, &mut stalled_warned);
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
                    // DR-0016 §3: client-detached lifecycle event。
                    state.record_registry.push_lifecycle(
                        super::record::LifecycleEvent::ClientDetached {
                            client_id: ch.id,
                            ts_unix_ms: now_unix_ms(),
                        },
                    );
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
            screen_state,
            pending_redraws,
        );
        // DR-0013 §5 + §6 Phase A: sync 終了で pending redraw を flush、
        // stalled detect は per-iteration で 1 回行う。
        flush_pending_redraws_if_sync_over(
            clients,
            screen_state,
            pending_redraws,
            &mut overflow_ids,
        );
        detect_and_warn_stalled(screen_state, &mut stalled_warned);

        // R5-H6 + DR-0001 軸 1/2: SIGCHLD / SIGTSTP / SIGCONT wake-up handling.
        // Drain the self-pipe + dispatch each signal byte. SIGTSTP / SIGCONT が
        // 入っていれば軸 2 / invariant 回復 policy をここで発火させる。
        // SIGCHLD については従来通り lifecycle.poll で transition を取り出し、
        // 軸 1 policy (= Stopped transition 観測時の Follow/AutoResume) を発火。
        if sigchld_ready {
            // DR-0025 Phase 1b (translate 併走): SIGCHLD self-pipe 発火を写す。waitpid
            // 経由の state 遷移解釈は Phase 3 の Child reducer が担う。
            let effects = reducer::handle(&mut daemon_state, translate::sigchld_received(child));
            debug_assert!(
                effects.is_empty(),
                "Phase 1b stub 段階では sigchld_received から effect は出ない"
            );
            if let Some(sp) = sigchld_pipe {
                let drained = sp.drain().unwrap_or_default();
                if let Some(outcome) =
                    handle_suspend_signals(&drained, child, config, &lifecycle, state)
                {
                    return outcome;
                }
            }
            let (child_state, transition) = lifecycle.poll_with_transition(child);
            if let ChildState::Exited(code) = child_state {
                return RelayOutcome::ChildExited(code);
            }
            match transition {
                Some(ChildTransition::Stopped { sig }) => {
                    let overflow = notify_child_stopped(child, clients, state, sig);
                    overflow_ids.extend(overflow);
                }
                Some(ChildTransition::Continued) => record_child_continued(state, child),
                _ => {}
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
                Ok(0) => {
                    let (child_state, transition) = lifecycle.poll_with_transition(child);
                    match transition {
                        Some(ChildTransition::Stopped { sig }) => {
                            let overflow = notify_child_stopped(child, clients, state, sig);
                            overflow_ids.extend(overflow);
                        }
                        Some(ChildTransition::Continued) => record_child_continued(state, child),
                        _ => {}
                    }
                    match child_state {
                        ChildState::Exited(code) => {
                            // DR-0025 Phase 1b (translate 併走): 子 exit を Reaped に写す。
                            // PTY master EOF は子終了の canonical な検出点。他 exit 検出
                            // 経路 (sigchld / EIO / EINTR / timeout) の集約は Phase 3 の
                            // Child reducer 化で waitpid transition を child::reduce へ
                            // 寄せる際に行う。
                            let effects =
                                reducer::handle(&mut daemon_state, translate::child_reaped(code));
                            debug_assert!(
                                effects.is_empty(),
                                "Phase 1b stub 段階では child_reaped から effect は出ない"
                            );
                            return RelayOutcome::ChildExited(code);
                        }
                        ChildState::Stopped => {
                            // R4-H14: SIGTSTP'd 子で master EOF/POLLHUP が連続する間の
                            // busy-wait 回避。SIGCONT が来るまで 500ms 単位で待機。
                            std::thread::sleep(STOPPED_POLL_INTERVAL);
                        }
                        ChildState::Alive => {
                            std::thread::sleep(ALIVE_RETRY_INTERVAL);
                        }
                    }
                }
                Ok(n) => {
                    // DR-0025 Phase 1b (translate 併走): 子 PTY 生 bytes を Layer 1 event
                    // に写して super-reducer を実走 (stub なので effect 空)。screen_state /
                    // scrollback / broadcast の既存処理は下でそのまま続ける (= 挙動不変)。
                    let effects =
                        reducer::handle(&mut daemon_state, translate::tty_master_read(&buf[..n]));
                    debug_assert!(
                        effects.is_empty(),
                        "Phase 1b stub 段階では tty_master_read から effect は出ない"
                    );
                    // Issue #1 + user request: `--debug-dump` の raw bytes 書き出し。
                    // 子 PTY からの bytes は scrollback / vt100 へ渡る **前** の生
                    // chunk なので、ここで append すれば「daemon が観測した最初の
                    // 形」が保存される (= state 経由の翻訳なし、ANSI escape も含む)。
                    if let Some(f) = debug_dump.as_mut() {
                        use std::io::Write as _;
                        if let Err(e) = f.write_all(&buf[..n]) {
                            eprintln!("hyoui: --debug-dump write 失敗: {e} (以後 dump 中止)");
                            debug_dump = None;
                        }
                    }
                    // DR-0016 §8: out event hook — 子 PTY 生 bytes を screen 加工 /
                    // broadcast の **直前** で record sink に push する (= 加工前の
                    // 「daemon が観測した最初の形」を録画する、debug_dump と同じ意図)。
                    state.record_registry.push_bytes_out(&buf[..n]);
                    // scrollback に push してから broadcast (subscription 種類で encoding 分岐)
                    let now = Instant::now();
                    // DR-0019 §4: idle-timeout は master 出力の最終時刻基準。新 bytes
                    // 受信のたびに基準を更新する (= 子が喋り続ける限り idle は発火しない)。
                    last_output = now;
                    scrollback.push(now, buf[..n].to_vec());
                    // DR-0013 §3: bytes は本 wrapper を経由してから broadcast へ。
                    // sync_in_progress / cell / cursor / mode 等の state を更新する。
                    // Phase A では既存 broadcast / wait / tail との **併存**で、
                    // 生 bytes も従来通り流す (= breaking 回避、§10)。
                    // Phase B で生 byte broadcast を state-driven に置換する。
                    screen_state.process(&buf[..n]);
                    // feed 後に stalled-warn flag を解除 (= 次回 5s 経過時に再警告可)
                    stalled_warned = false;
                    overflow_ids.extend(broadcast_master_bytes(clients, &buf[..n], now));
                    // R5-FB1: `--until PATTERN` match 検査。一致した瞬間に
                    // 子 process group へ SIGTERM を投げて session 終了させる。
                    // (broadcast / scrollback の後で match 判定するのは、最後の
                    // chunk も client / scrollback には届けるため。)
                    if let Some(ref mut w) = until_watcher
                        && w.feed(&buf[..n])
                    {
                        // DR-0019 §4: until match も timeout / idle-timeout と同じ
                        // 「daemon 側終了条件」なので lifecycle event を残す (= record.rs
                        // の SessionTerminatedByCondition doc が謳う reason "until")。
                        state.record_registry.push_lifecycle(
                            super::record::LifecycleEvent::SessionTerminatedByCondition {
                                reason: "until".to_string(),
                                ts_unix_ms: now_unix_ms(),
                            },
                        );
                        let _ = kill_pgrp(child, Signal::SIGTERM);
                        // finalize_child が SIGTERM → wait → SIGKILL を実施。
                        // `ClientDetachedOrKilled` を返すことで finalize 経路に
                        // 乗せる (= `kill` subcommand と同じ後始末)。
                        return RelayOutcome::ClientDetachedOrKilled;
                    }
                }
                Err(Error::Errno(nix::errno::Errno::EIO)) => {
                    let (child_state, transition) = lifecycle.poll_with_transition(child);
                    match transition {
                        Some(ChildTransition::Stopped { sig }) => {
                            let overflow = notify_child_stopped(child, clients, state, sig);
                            overflow_ids.extend(overflow);
                        }
                        Some(ChildTransition::Continued) => record_child_continued(state, child),
                        _ => {}
                    }
                    match child_state {
                        ChildState::Exited(code) => return RelayOutcome::ChildExited(code),
                        ChildState::Stopped => {
                            std::thread::sleep(STOPPED_POLL_INTERVAL);
                        }
                        ChildState::Alive => {
                            std::thread::sleep(ALIVE_RETRY_INTERVAL);
                        }
                    }
                }
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
                Ok(frame) => {
                    // DR-0025 Phase 1b (translate 併走): frame 受信を写す (client id のみ、
                    // frame 実体は運ばない placeholder 主義)。kind 別の認可 / 処理は下の
                    // handle_client_frame が従来通り担う (= 挙動不変)。
                    let client_id = ch.id;
                    let effects = reducer::handle(
                        &mut daemon_state,
                        translate::client_frame_received(client_id),
                    );
                    debug_assert!(
                        effects.is_empty(),
                        "Phase 1b stub 段階では client_frame_received から effect は出ない"
                    );
                    frames_to_process.push((idx, FrameOrError::Frame(frame)));
                }
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
                        screen_state,
                        config,
                    ) {
                        ClientFrameOutcome::Continue => {}
                        ClientFrameOutcome::DropClient => indices_to_drop.push(idx),
                        // DR-0020 §4: detach --target=others/all で複数 client を drop。
                        // 後段の dedup / 逆順 remove / leader cascade に乗る。
                        ClientFrameOutcome::DropClients {
                            indices,
                            cancel_pending,
                        } => {
                            indices_to_drop.extend(indices);
                            // codex review 2026-06-12: others/all は in-flight handshake
                            // (= pending) もキャンセルする。entry drop で worker 側
                            // socket が close され、成立しかけの接続は handshake 失敗
                            // として終わる (= detach をすり抜ける race を防ぐ)。
                            if cancel_pending {
                                pending_handshakes.clear();
                            }
                        }
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
            let detached_id = ch.id;
            if ch.leader {
                dropped_any_leader = true;
            }
            // DR-0025 Phase 1b (translate 併走): client detach を写す。lock auto-release /
            // leader cascade の 1 本化は Phase 2 の Client reducer が担い、現段階では下の
            // 既存処理 (lock::reduce + elevate_next_leader) が挙動を担う (= 挙動不変)。
            let effects =
                reducer::handle(&mut daemon_state, translate::client_detached(detached_id));
            debug_assert!(
                effects.is_empty(),
                "daemon_state.lock が空 stub の間は client_detached から effect は出ない \
                 (= 実 lock state は SessionState 側)。Phase 2-α2 の lock 移設で holder が\
                 立つようになった時点で、この assert は execute 呼び出しに置換必須"
            );
            // DR-0025 Phase 1a: holder client の切断による process-bound GC は lock
            // reducer に委譲する。Released (= ProcessBoundGc) が返ったら mode.change
            // broadcast の契機 (dropped_held_lock) にする。非 holder の切断は空 event。
            if super::lock::reduce(
                &mut state.lock,
                LockMsg::ClientDisconnected { client_id: ch.id },
            )
            .iter()
            .any(|e| matches!(e, LockEvent::Released { .. }))
            {
                dropped_held_lock = true;
            }
            // ClientHandle::Drop が writer_tx close + reader shutdown +
            // writer_thread join を一括実行 (R5-H18)。backpressure 超過時の
            // writer_pump が write_all で block 中でも shutdown で即 error 化される。
            drop(ch);
            // DR-0016 §3: client-detached lifecycle event。lock auto-release が
            // 起きた場合は lock-released は別途 push しない (= explicit な
            // LockRelease 経路でないため、observer は client-detached + lock_holder
            // 変化で推定する想定。dropped_held_lock を見て lock-released を発火する
            // のは将来 task)。
            state
                .record_registry
                .push_lifecycle(super::record::LifecycleEvent::ClientDetached {
                    client_id: detached_id,
                    ts_unix_ms: now_unix_ms(),
                });
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

/// 子 PTY exit を見届けて SIGTERM → SIGKILL に昇格するまでの grace 期間。
///
/// `finalize_child` の `ClientDetachedOrKilled` 経路 (= legacy `Kill { wait: true }`
/// client / `--until` match) で、SIGTERM を ignore する子のために daemon が
/// **無限 blocking waitpid で孤児化する**のを防ぐ上限。2026-06-11 の孤児 daemon
/// 騒ぎの根源がこの無限 wait だった。
///
/// 値の根拠: `Session::drop` (= panic 経路) は 500ms → SIGKILL だが、正常 terminate
/// 経路は子の後始末 (state flush 等) にもう少し余裕を持たせる。5s は対話 app の
/// graceful shutdown に十分で、daemon が「見届け中」に占有される時間としても許容範囲。
const FINALIZE_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// 子 PTY を reap して exit code を返す。**必ず有限時間で返る** (= 孤児 daemon 防止)。
///
/// outcome に応じて:
/// - `ChildExited(Some(code))`: 既に `child_actually_exited` で reap 済、code をそのまま返す
/// - `ChildExited(None)`: exit 検知だが code 未取得 → waitpid で確認
/// - `ClientDetachedOrKilled`: 子はまだ生きている可能性 → SIGCONT+SIGTERM →
///   [`FINALIZE_TERM_GRACE`] まで `waitpid(WNOHANG)` polling → 超過したら SIGKILL
///   昇格 → blocking reap (= SIGKILL 後は必ず即 reap できる)
///
/// SIGCONT 併送は shell の job control 慣行 (= stopped な子に TERM を送っても
/// pending のまま配送されないため、起こしてから効かせる)。
///
/// signal で終了の場合は shell convention に従い `128 + signum` を返す。
fn finalize_child(child: Pid, outcome: &RelayOutcome) -> Result<i32, Error> {
    // `child_actually_exited` で既に code を取得済なら、それを優先 (waitpid を
    // 二重に呼ぶと ECHILD になる)。
    if let RelayOutcome::ChildExited(Some(code)) = outcome {
        return Ok(*code);
    }

    // ChildExited (= 子は既に死んでいる、reap だけ) は blocking reap で即返る。
    if matches!(outcome, RelayOutcome::ChildExited(_)) {
        return reap_blocking(child, outcome);
    }

    // client 都合の終了 (= legacy wait:true kill / --until)。子に SIGCONT+SIGTERM を
    // 送ってから grace 付きで wait。既に exit 済なら kill は ESRCH で失敗 → 無視。
    // R5-H7: process group 全体に向けて、子が exec した孫 (= shell の background
    // job 等) も同じ SIGTERM で reap 対象にする。
    let _ = kill_pgrp(child, Signal::SIGCONT);
    let _ = kill_pgrp(child, Signal::SIGTERM);

    let deadline = Instant::now() + FINALIZE_TERM_GRACE;
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
    loop {
        let timed_out = match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + (sig as i32)),
            // StillAlive / stopped / continued の中間状態は deadline 判定して継続 polling。
            Ok(_) => Instant::now() >= deadline,
            Err(nix::errno::Errno::EINTR) => continue,
            // 既に reap 済 (= SIGCHLD ハンドラが拾った等)。SIGTERM kill の慣行 code。
            Err(nix::errno::Errno::ECHILD) => return Ok(143),
            Err(e) => return Err(Error::from(e)),
        };
        if timed_out {
            // grace 超過 = 子が SIGTERM を ignore。SIGKILL 昇格して blocking で
            // 見届ける (= SIGKILL は catch 不能、D-state 以外は即死するので有限)。
            let _ = kill_pgrp(child, Signal::SIGKILL);
            return reap_blocking(child, outcome);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 子を blocking で reap し切る。`finalize_child` の ChildExited 経路と SIGKILL
/// 昇格後の見届けで使う。
fn reap_blocking(child: Pid, outcome: &RelayOutcome) -> Result<i32, Error> {
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
    use super::super::control::signal_name_to_nix_signal;
    use super::super::lock::{LockMsg, generate_lock_token, should_assign_leader};
    use super::*;
    use crate::protocol::messages::{
        Detach, DetachTarget, ErrorCode, Kill, LockResult, SessionMode, TailEndReason,
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

    // ---- DR-0019 §4: timeout / idle-timeout 判定 helper unit tests ----

    #[test]
    fn eval_timeout_none_when_disabled() {
        // 両方 None なら発火しない (= default)。
        assert_eq!(
            eval_timeout(
                Duration::from_secs(999),
                Duration::from_secs(999),
                None,
                None
            ),
            None
        );
    }

    #[test]
    fn eval_timeout_overall_fires_at_threshold() {
        // overall: start からの経過が timeout_ms 以上で "timeout"。
        assert_eq!(
            eval_timeout(
                Duration::from_millis(1000),
                Duration::from_millis(0),
                Some(1000),
                None
            ),
            Some("timeout")
        );
        // 未達なら None。
        assert_eq!(
            eval_timeout(
                Duration::from_millis(999),
                Duration::from_millis(0),
                Some(1000),
                None
            ),
            None
        );
    }

    #[test]
    fn eval_timeout_idle_fires_on_output_gap() {
        // idle: 最終出力からの経過が idle_timeout_ms 以上で "idle-timeout"。
        assert_eq!(
            eval_timeout(
                Duration::from_secs(100),
                Duration::from_millis(500),
                None,
                Some(500)
            ),
            Some("idle-timeout")
        );
        assert_eq!(
            eval_timeout(
                Duration::from_secs(100),
                Duration::from_millis(499),
                None,
                Some(500)
            ),
            None
        );
    }

    #[test]
    fn eval_timeout_overall_takes_priority_over_idle() {
        // 両方発火条件を満たす場合は overall ("timeout") を優先する
        // (= 発火理由を一意に決める。どちらでも SIGTERM 手順は同じ)。
        assert_eq!(
            eval_timeout(
                Duration::from_millis(2000),
                Duration::from_millis(2000),
                Some(1000),
                Some(1000)
            ),
            Some("timeout")
        );
    }

    #[test]
    fn timeout_poll_cap_ms_returns_remaining_until_nearest_deadline() {
        // overall のみ: 残り = timeout - elapsed_start。
        assert_eq!(
            timeout_poll_cap_ms(
                Duration::from_millis(300),
                Duration::from_millis(0),
                Some(1000),
                None
            ),
            Some(700)
        );
        // idle のみ: 残り = idle - elapsed_output。
        assert_eq!(
            timeout_poll_cap_ms(
                Duration::from_millis(0),
                Duration::from_millis(200),
                None,
                Some(500)
            ),
            Some(300)
        );
        // 両方: より近い deadline を採る (= 小さい方)。
        assert_eq!(
            timeout_poll_cap_ms(
                Duration::from_millis(300),
                Duration::from_millis(200),
                Some(1000),
                Some(500)
            ),
            Some(300) // idle 残り 300 < overall 残り 700
        );
        // 無効なら None (= cap しない)。
        assert_eq!(
            timeout_poll_cap_ms(
                Duration::from_millis(0),
                Duration::from_millis(0),
                None,
                None
            ),
            None
        );
        // 既に超過していたら 0 (= 即 wake)。
        assert_eq!(
            timeout_poll_cap_ms(
                Duration::from_millis(2000),
                Duration::from_millis(0),
                Some(1000),
                None
            ),
            Some(0)
        );
    }

    // ---- cap_poll_timeout unit tests ----

    /// NONE (= 無限 block) は常に cap で頭打ちされる。
    /// R5-H6 fallback (self-pipe 不在 + pending_handshakes 空) で poll が
    /// 無限 block して idle 子の stop/exit を見逃す regression を塞ぐ要。
    #[test]
    fn cap_poll_timeout_caps_none_to_cap() {
        let capped = cap_poll_timeout(PollTimeout::NONE, 500);
        assert_eq!(capped.as_millis(), Some(500));
    }

    /// cap より大きい既存 timeout は cap に縮む。
    #[test]
    fn cap_poll_timeout_shrinks_larger_value() {
        let capped = cap_poll_timeout(PollTimeout::from(2000u16), 500);
        assert_eq!(capped.as_millis(), Some(500));
    }

    /// cap より小さい既存 timeout はそのまま (= より短い cap が既に効いている)。
    #[test]
    fn cap_poll_timeout_keeps_smaller_value() {
        let capped = cap_poll_timeout(PollTimeout::from(50u16), 500);
        assert_eq!(capped.as_millis(), Some(50));
    }

    /// cap と同値はそのまま (= 余計に再生成しない)。
    #[test]
    fn cap_poll_timeout_keeps_equal_value() {
        let capped = cap_poll_timeout(PollTimeout::from(500u16), 500);
        assert_eq!(capped.as_millis(), Some(500));
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

    /// DR-0017 §柱2 (層 2): `notify_child_stopped` は leader 不在時に **SIGCONT を
    /// 送らない** (= auto-resume fallback 廃止)。子は stopped のまま残り、
    /// `state.child_stopped()` が立つ (= status/list で可観測)。
    ///
    /// 旧実装は leader 不在で無条件 `killpg(child, SIGCONT)` していたため、
    /// `kill -STOP` した子が即起こされて止まらなかった (= ^Z bug 層 2)。
    ///
    /// PTY child を使うため `#[ignore]` (= ローカルで `--ignored` 実行)。
    #[ignore = "PTY child を使う、ローカルで --ignored 実行 (DR-0017 §柱2 層 2 検証)"]
    #[test]
    fn notify_child_stopped_does_not_auto_resume_without_leader() {
        use crate::sys::Pty;
        use nix::sys::signal::Signal;
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

        let spawned = Pty::spawn(&["cat"], 80, 24, None).expect("spawn cat");
        let child = spawned.child;

        // 子を SIGSTOP で停止させる (= ^Z 相当の停止状態を作る)。
        nix::sys::signal::kill(child, Signal::SIGSTOP).expect("SIGSTOP");
        // WUNTRACED で stop transition を回収 (= notify_child_stopped が呼ばれる前提)。
        let mut stopped = false;
        for _ in 0..100 {
            if let Ok(WaitStatus::Stopped(_, _)) =
                waitpid(child, Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED))
            {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(stopped, "child should be Stopped after SIGSTOP");

        // leader 不在 (= clients 空) で notify_child_stopped を呼ぶ。
        let state = SessionState::default();
        let mut clients: Vec<ClientHandle> = Vec::new();
        // policy は SessionState default (= Notify)。
        let overflow = notify_child_stopped(child, &mut clients, &state, Signal::SIGTSTP as i32);
        assert!(overflow.is_empty(), "no leader = no notify overflow");

        // DR-0017: stopped 観測フラグが立つ (= 可観測性)。
        assert!(
            state.child_stopped(),
            "child_stopped flag must be set for status/list observability"
        );

        // 子が **依然 stopped のまま** であることを確認 (= auto-resume されていない)。
        // SIGCONT を送られていれば WCONTINUED が観測できてしまうが、ここでは
        // WNOHANG | WUNTRACED で「まだ stopped」を複数回確認する。
        let mut still_stopped = true;
        for _ in 0..10 {
            match waitpid(
                child,
                Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED),
            ) {
                // StillAlive = 既に回収済 stop の latch、または何も新規 transition なし。
                Ok(WaitStatus::StillAlive) | Ok(WaitStatus::Stopped(_, _)) => {}
                Ok(WaitStatus::Continued(_)) => {
                    still_stopped = false;
                    break;
                }
                other => panic!("unexpected wait status: {other:?}"),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            still_stopped,
            "DR-0017: daemon must NOT auto-resume a stopped child when no leader is present"
        );

        // cleanup
        let _ = nix::sys::signal::kill(child, Signal::SIGCONT);
        let _ = nix::sys::signal::kill(child, Signal::SIGKILL);
        let _ = waitpid(child, Some(WaitPidFlag::empty()));
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
        let spawned = Pty::spawn(&["cat"], 80, 24, None).expect("spawn cat");
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

        let spawned = Pty::spawn(&["cat"], 80, 24, None).expect("spawn cat");
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
            if let Ok(s) = std::fs::read_to_string(&pid_file)
                && let Some(line) = s.lines().next()
                && let Ok(pid) = line.trim().parse::<i32>()
            {
                break pid;
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
        let resp =
            match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode cbor") {
                ControlMessage::HandshakeResponse(r) => r,
                other => panic!("unexpected: {other:?}"),
            };
        // DR-0013 §4 Phase A: handshake response 直後に daemon が
        // attach 復元用 raw_data frame を 1 つ送る。test code は control を
        // 期待するので、redraw frame を 1 個読み捨てる。
        discard_attach_redraw(stream);
        resp
    }

    /// DR-0013 §4 Phase A test helper:
    /// 手動 handshake する test (= `do_client_handshake` を使わない経路) でも
    /// handshake response の直後に attach 復元用 raw_data frame が 1 つ来るため、
    /// それを読み捨てるための共通ヘルパ。raw frame でなければ panic (= 順序仮定
    /// 違反のサイン)。Phase A の `build_attach_redraw` は primary 空画面でも
    /// `\x1b[?1049l` prepend + state_formatted の最小 sequence を必ず返すため、
    /// 「frame が来ない」case は無い前提。
    fn discard_attach_redraw(stream: &mut UnixStream) {
        let f = Frame::decode_from(stream).expect("attach redraw frame");
        assert_eq!(
            f.ty,
            crate::protocol::TYPE_RAW_DATA,
            "expected attach redraw raw_data frame after handshake response, got ty={}",
            f.ty
        );
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
        let kill_msg = ControlMessage::Kill(Kill {
            signal: None,
            wait: true,
        });
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
            let body = ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
        let body = ControlMessage::Kill(Kill {
            signal: None,
            wait: true,
        })
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
        // DR-0025 Phase 1a: lock state は reducer 経由でのみ mutate する。grant すると
        // holder が Some になり session_mode が Locked に切り替わる。検証意図 (= lock
        // holder の有無で session_mode が導出される) は元 test と同一で、state 構築のみ
        // 直接 field 代入から reduce 経由に追従させた。
        super::super::lock::reduce(
            &mut s.lock,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("abcd".into()),
            },
        );
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
        let body = ControlMessage::Kill(Kill {
            signal: None,
            wait: true,
        })
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
        let body = ControlMessage::Kill(Kill {
            signal: None,
            wait: true,
        })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- DR-0013 Phase A: attach redraw integration ----

    /// DR-0013 §4 Phase A integration test:
    /// 子 PTY が "ATTACH_TEST_OK" を出力した後に **新規** client が attach した時、
    /// daemon は handshake response 直後に `state_formatted()` 由来の redraw を
    /// 1 frame で送る。redraw 内に "ATTACH_TEST_OK" が含まれていれば、画面状態が
    /// 正本化されていることが確認できる (= "attach がほぼ機能しない" の解消)。
    ///
    /// 旧実装 (= screen state 不在) では client は子 PTY の現状画面を取れず、
    /// 子 (= claude TUI 等) が再描画してくれるまで blank だった。本テストが pass
    /// することは Phase A の最重要 acceptance criterion。
    #[test]
    fn serve_attach_redraw_includes_pre_attach_output() {
        // bash で固有 marker を出してから 30 秒 sleep。1st client は marker を
        // 読み取って detach、2nd client は attach 直後の redraw に marker が
        // 含まれることを確認する。
        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'ATTACH_TEST_OK\\r\\n'; sleep 30".into(),
        ];
        let (_, sock_path, _dir, handle) = spawn_serve_thread(cmd);

        // 1st client: marker を読み取って detach
        let mut s1 = client_connect_with_retry(&sock_path);
        let _r1 = do_client_handshake(&mut s1);
        // leader.notify を discard
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");
        // 子の output を marker が来るまで待つ (= screen state に反映される時間も稼ぐ)
        read_until_contains(&mut s1, b"ATTACH_TEST_OK");
        // 1st client は drop (= detach)、daemon は state を保持し続ける
        drop(s1);

        // daemon が detach を観測して state が安定するまで短時間待機。
        std::thread::sleep(Duration::from_millis(100));

        // 2nd client: 新規 attach。do_client_handshake が attach redraw raw_data
        // frame を 1 つ読み捨てているので、その frame body 内に marker が含まれて
        // いるかを確認するため、本 test では handshake response の直後の frame を
        // 自前で取り出す形に分解する。
        let mut s2 = client_connect_with_retry(&sock_path);
        // handshake は手動で送って response と redraw を別々に取る。
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s2)
            .expect("send handshake");
        s2.flush().expect("flush");
        // handshake response
        let resp_frame = Frame::decode_from(&mut s2).expect("response");
        match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode") {
            ControlMessage::HandshakeResponse(_) => {}
            o => panic!("expected HandshakeResponse, got {o:?}"),
        }
        // attach redraw (raw_data): body 内に marker が含まれることを assert
        let redraw_frame = Frame::decode_from(&mut s2).expect("redraw");
        assert_eq!(
            redraw_frame.ty,
            crate::protocol::TYPE_RAW_DATA,
            "expected attach redraw raw_data frame"
        );
        assert!(
            redraw_frame
                .body
                .windows(b"ATTACH_TEST_OK".len())
                .any(|w| w == b"ATTACH_TEST_OK"),
            "redraw bytes should contain pre-attach marker; got {:?}",
            String::from_utf8_lossy(&redraw_frame.body)
        );

        // cleanup: s2 から kill 送信 → daemon 終了
        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// DR-0013 §4 Phase A: alt screen 中の attach 復元。
    /// 子 PTY が alt screen に入って描画した状態で 2nd client が attach した時、
    /// redraw の冒頭が `\x1b[?1049h` で始まり (= PoC §2 で発覚した alt flag 欠落
    /// 補完の検証)、続く state_formatted で画面内容も復元される。
    #[test]
    fn serve_attach_redraw_preserves_alt_screen_flag() {
        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            // alt screen に入って "ALT_MARKER" を描画してから 30 秒 sleep。
            // printf で alt screen on (= \033[?1049h)、その後 marker。
            "printf '\\033[?1049hALT_MARKER\\r\\n'; sleep 30".into(),
        ];
        let (_, sock_path, _dir, handle) = spawn_serve_thread(cmd);

        // 1st client: marker を読み取って detach。
        let mut s1 = client_connect_with_retry(&sock_path);
        let _r1 = do_client_handshake(&mut s1);
        let _ = Frame::decode_from(&mut s1).expect("s1 leader.notify");
        read_until_contains(&mut s1, b"ALT_MARKER");
        drop(s1);

        std::thread::sleep(Duration::from_millis(100));

        // 2nd client: 新規 attach。
        let mut s2 = client_connect_with_retry(&sock_path);
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: MVP_CAPS.iter().map(|s| s.to_string()).collect(),
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s2)
            .expect("send handshake");
        s2.flush().expect("flush");
        let _ = Frame::decode_from(&mut s2).expect("response");
        let redraw_frame = Frame::decode_from(&mut s2).expect("redraw");
        assert_eq!(redraw_frame.ty, crate::protocol::TYPE_RAW_DATA);
        // 冒頭が `\x1b[?1049h` で始まる (= alt flag 補完が wrapper で 1 行追加されている)
        assert!(
            redraw_frame.body.starts_with(b"\x1b[?1049h"),
            "alt screen redraw should start with ?1049h, got: {:?}",
            String::from_utf8_lossy(&redraw_frame.body[..redraw_frame.body.len().min(32)])
        );
        // marker も含まれる (= state_formatted が cell 内容を保持)
        assert!(
            redraw_frame
                .body
                .windows(b"ALT_MARKER".len())
                .any(|w| w == b"ALT_MARKER"),
            "redraw bytes should contain alt screen marker"
        );

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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

    // ---- Phase 11c: wait.request 廃止 (DR-0006 §9 改訂) ----
    // 旧 scrollback regex 経路の wait protocol layer は削除済。state-based
    // wait は CLI 側で screen.snapshot.request を polling する形に再実装され、
    // daemon protocol 層には wait 関連の message / handler / pending_waits は
    // 残らない。

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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Phase 12: byte bound backpressure ----

    /// deadline 付き thread join (= issue 2026-06-11、ignored test 用)。
    ///
    /// daemon serve thread を素の `JoinHandle::join` で待つと、serve が wedge した
    /// 場合に **無限ハング**する (= 2026-05-28 ubuntu CI で 6h timeout を観測した構造)。
    /// watcher thread 経由で join し `recv_timeout` で deadline を被せる。timeout 時は
    /// message 付き panic で fail させる (= thread は leak するが process 終了で回収)。
    fn join_with_deadline<T: Send + 'static>(
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

    /// Phase 12: client_buffer_bytes を超過すると当該 client は backpressure.disconnect
    /// で切断され、socket は close される。他の client は影響を受けず通常動作。
    #[test]
    #[ignore = "yes(1) + PTY + backpressure timing 依存のため ubuntu CI で daemon thread join が hang する (2026-05-28 6h timeout 観測)。ローカルは `cargo test -- --ignored` で実行する"]
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut k)
        .expect("send kill");
        k.flush().expect("flush");
        // 無限ハング防止 (= issue 2026-06-11): まさに本 test が CI で 6h hang した
        // join 箇所。deadline 超過は fail で落とす (= ハングさせない)。
        let _ = join_with_deadline(handle, Duration::from_secs(30), "backpressure daemon serve");
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

    /// DR-0012: signal name string から nix `Signal` を解決する helper の挙動。
    /// 正規 SIG-prefix 大文字を受理、未知 name / 小文字 / 略名 / 数値は reject。
    #[test]
    fn signal_name_to_nix_signal_accepts_canonical_names() {
        // 正規表記 (SIG-prefix 大文字) は受理
        assert!(
            signal_name_to_nix_signal("SIGTERM").is_some(),
            "SIGTERM is canonical"
        );
        assert!(
            signal_name_to_nix_signal("SIGINT").is_some(),
            "SIGINT is canonical"
        );
        assert!(
            signal_name_to_nix_signal("SIGKILL").is_some(),
            "SIGKILL is canonical"
        );
        assert!(
            signal_name_to_nix_signal("SIGUSR1").is_some(),
            "SIGUSR1 portable"
        );
        // 略名・小文字・数値は reject
        assert!(
            signal_name_to_nix_signal("TERM").is_none(),
            "略名 TERM は reject"
        );
        assert!(
            signal_name_to_nix_signal("sigterm").is_none(),
            "小文字 sigterm は reject"
        );
        assert!(
            signal_name_to_nix_signal("15").is_none(),
            "数値 15 は reject"
        );
        assert!(
            signal_name_to_nix_signal("SIGBOGUS").is_none(),
            "未知 name は reject"
        );
    }

    /// DR-0012: BSD-specific signal (SIGINFO 等) は Linux daemon では nix の
    /// `Signal::SIGINFO` variant が未定義のため自動 reject される。
    /// (= name-based wire の cross-OS フォールバック挙動の sanity check)
    #[test]
    #[cfg(target_os = "linux")]
    fn signal_name_to_nix_signal_rejects_bsd_specific_on_linux() {
        // SIGINFO は macOS / *BSD 専用。Linux では nix が variant を出さないので
        // None になる (= signal.invalid で reject される)。
        assert!(
            signal_name_to_nix_signal("SIGINFO").is_none(),
            "SIGINFO on Linux must be rejected"
        );
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
        discard_attach_redraw(&mut s2);
        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s1)
        .expect("send");
        s1.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// DR-0012: 未知 signal name を Kill / Signal で送ると signal.invalid を返す。
    /// (= 旧 Round1 A3 "signum=0 (POSIX probe) reject" を name-based に置換)
    #[test]
    fn serve_signal_unknown_name_rejected() {
        use crate::protocol::messages::Signal as ProtoSignal;
        let (_sid, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());
        let mut s = client_connect_with_retry(&sock_path);
        let _ = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        Frame::cbor_control(
            ControlMessage::Signal(ProtoSignal {
                signal: "SIGBOGUS".into(),
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s2)
        .expect("send");
        s2.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ---- Round2 fixes: regress confirmations ----

    // Round2 #1: 空 Text predicate reject 試験は wait protocol layer 削除に伴い廃止
    // (DR-0006 §9 改訂、state-based wait に移行)。

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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
        discard_attach_redraw(&mut s2);

        // s2 (RwNoLeader) が Kill 試行 → mode.not-allowed
        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
        discard_attach_redraw(&mut s2);

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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
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
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut attached[0])
        .expect("send kill");
        attached[0].flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // ───── DR-0013 Phase B: screen.dump / screen.snapshot ─────

    /// `screen.dump.request` (format=ansi, layer=visible) を送ると
    /// `screen.dump.response` が ANSI bytes で返る + serial が echo される。
    #[test]
    fn serve_screen_dump_ansi_returns_state_formatted() {
        use crate::protocol::messages::{
            ScreenDumpFormat as ProtoDumpFormat, ScreenDumpLayer as ProtoDumpLayer,
            ScreenDumpRequest,
        };
        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'DUMPMARK\\r\\n'; sleep 30".into(),
        ];
        let (_, sock_path, _dir, handle) = spawn_serve_thread(cmd);

        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");
        read_until_contains(&mut s, b"DUMPMARK");

        // screen.dump.request (format=ansi, layer=visible, serial=42)
        let req = ControlMessage::ScreenDumpRequest(ScreenDumpRequest {
            format: ProtoDumpFormat::Ansi,
            layer: ProtoDumpLayer::Visible,
            rect: None,
            serial: Some(42),
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");

        let msg = next_control(&mut s);
        match msg {
            ControlMessage::ScreenDumpResponse(resp) => {
                assert_eq!(resp.serial, Some(42));
                // ANSI dump は `\x1b` で始まる + marker を含む
                assert!(resp.payload.starts_with(b"\x1b"), "expected ANSI prefix");
                assert!(
                    resp.payload.windows(8).any(|w| w == b"DUMPMARK"),
                    "dump should contain DUMPMARK marker"
                );
            }
            o => panic!("expected ScreenDumpResponse, got {o:?}"),
        }

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// `screen.dump.request` (format=cbor) を送ると CBOR encoded `ScreenSnapshot`
    /// が returns され、decode して内容が確認できる。
    #[test]
    fn serve_screen_dump_cbor_returns_encoded_snapshot() {
        use crate::daemon::screen::ScreenSnapshot;
        use crate::protocol::messages::{
            ScreenDumpFormat as ProtoDumpFormat, ScreenDumpLayer as ProtoDumpLayer,
            ScreenDumpRequest,
        };
        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'CBORDUMP'; sleep 30".into(),
        ];
        let (_, sock_path, _dir, handle) = spawn_serve_thread(cmd);

        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");
        read_until_contains(&mut s, b"CBORDUMP");

        let req = ControlMessage::ScreenDumpRequest(ScreenDumpRequest {
            format: ProtoDumpFormat::Cbor,
            layer: ProtoDumpLayer::Visible,
            rect: None,
            serial: None,
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");

        let msg = next_control(&mut s);
        match msg {
            ControlMessage::ScreenDumpResponse(resp) => {
                assert!(resp.serial.is_none());
                // CBOR decode して ScreenSnapshot を取り出す
                let snap: ScreenSnapshot =
                    ciborium::de::from_reader(resp.payload.as_slice()).expect("decode snap");
                // 80x24 default
                assert_eq!(snap.cols, 80);
                assert_eq!(snap.rows, 24);
                // current_seqno は 1 以上 (= byte feed があった)
                assert!(snap.current_seqno >= 1);
                // CBORDUMP の各文字が cells に含まれる
                let texts: String = snap.cells.iter().map(|cp| cp.cell.text.as_str()).collect();
                assert!(texts.contains("CBORDUMP"), "snapshot cells text: {texts}");
            }
            o => panic!("expected ScreenDumpResponse, got {o:?}"),
        }

        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// `screen.dump.request` (layer=scrollback, format=text/plain) を送ると、
    /// 大量出力で visible からスクロールアウトした過去 marker が payload に含まれ、
    /// 同 marker は visible (= 通常の dump) には含まれないことを確認する。
    ///
    /// 設計検証: DR-0013 §8 + §8 Update で「rows-base scrollback ring を Phase C で
    /// 配線」とされた scrollback layer の機能配線テスト。peer issue
    /// (= claude TUI で長文応答が visible からスクロールアウト) の use case が
    /// scrollback layer 経由で解決できることをエンドツーエンドで保証する。
    #[test]
    fn serve_screen_dump_scrollback_text_plain_returns_old_marker() {
        use crate::protocol::messages::{
            ScreenDumpFormat as ProtoDumpFormat, ScreenDumpLayer as ProtoDumpLayer,
            ScreenDumpRequest,
        };
        // viewport 24 行に対し 60 行出力する子コマンド: SCROLLED_OUT_HEAD は最初に出るので
        // 確実に visible からスクロールアウト、VISIBLE_TAIL は最後に出るので visible に残る。
        let cmd = vec![
            "/bin/sh".into(),
            "-c".into(),
            // SCROLLED_OUT_HEAD → 50 行ダミー → VISIBLE_TAIL の構成。
            // viewport 24 行なら SCROLLED_OUT_HEAD は確実に scrollback に押し出される。
            "printf 'SCROLLED_OUT_HEAD\\n'; for i in $(seq 1 50); do printf 'L%d\\n' $i; done; \
             printf 'VISIBLE_TAIL\\n'; sleep 30"
                .into(),
        ];
        let (_, sock_path, _dir, handle) = spawn_serve_thread(cmd);

        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");
        // VISIBLE_TAIL が出るまで待って、子の全出力が screen state に反映されたことを保証。
        read_until_contains(&mut s, b"VISIBLE_TAIL");

        // 1) layer=visible: SCROLLED_OUT_HEAD は含まれない (= スクロールアウト確認)
        let req_visible = ControlMessage::ScreenDumpRequest(ScreenDumpRequest {
            format: ProtoDumpFormat::TextPlain,
            layer: ProtoDumpLayer::Visible,
            rect: None,
            serial: Some(1),
        });
        Frame::cbor_control(req_visible.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");
        let visible_payload = match next_control(&mut s) {
            ControlMessage::ScreenDumpResponse(resp) => {
                assert_eq!(resp.serial, Some(1));
                resp.payload
            }
            o => panic!("expected ScreenDumpResponse, got {o:?}"),
        };
        let visible_text = std::str::from_utf8(&visible_payload).expect("utf8");
        assert!(
            visible_text.contains("VISIBLE_TAIL"),
            "visible should contain VISIBLE_TAIL: {visible_text:?}"
        );
        assert!(
            !visible_text.contains("SCROLLED_OUT_HEAD"),
            "visible should NOT contain SCROLLED_OUT_HEAD (it should have scrolled out): {visible_text:?}"
        );

        // 2) layer=scrollback: SCROLLED_OUT_HEAD が含まれる、VISIBLE_TAIL は含まれない
        let req_sb = ControlMessage::ScreenDumpRequest(ScreenDumpRequest {
            format: ProtoDumpFormat::TextPlain,
            layer: ProtoDumpLayer::Scrollback,
            rect: None,
            serial: Some(2),
        });
        Frame::cbor_control(req_sb.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");
        let sb_payload = match next_control(&mut s) {
            ControlMessage::ScreenDumpResponse(resp) => {
                assert_eq!(resp.serial, Some(2));
                resp.payload
            }
            o => panic!("expected ScreenDumpResponse, got {o:?}"),
        };
        let sb_text = std::str::from_utf8(&sb_payload).expect("utf8");
        assert!(
            sb_text.contains("SCROLLED_OUT_HEAD"),
            "scrollback should contain SCROLLED_OUT_HEAD: {sb_text:?}"
        );
        assert!(
            !sb_text.contains("VISIBLE_TAIL"),
            "scrollback should NOT contain VISIBLE_TAIL (still in visible): {sb_text:?}"
        );

        // 3) layer=both: 両方含まれる
        let req_both = ControlMessage::ScreenDumpRequest(ScreenDumpRequest {
            format: ProtoDumpFormat::TextPlain,
            layer: ProtoDumpLayer::Both,
            rect: None,
            serial: Some(3),
        });
        Frame::cbor_control(req_both.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");
        let both_payload = match next_control(&mut s) {
            ControlMessage::ScreenDumpResponse(resp) => {
                assert_eq!(resp.serial, Some(3));
                resp.payload
            }
            o => panic!("expected ScreenDumpResponse, got {o:?}"),
        };
        let both_text = std::str::from_utf8(&both_payload).expect("utf8");
        assert!(
            both_text.contains("SCROLLED_OUT_HEAD"),
            "both should contain SCROLLED_OUT_HEAD: {both_text:?}"
        );
        assert!(
            both_text.contains("VISIBLE_TAIL"),
            "both should contain VISIBLE_TAIL: {both_text:?}"
        );
        // SCROLLED_OUT_HEAD は VISIBLE_TAIL より前 (= 時系列順、scrollback 先)
        let pos_head = both_text.find("SCROLLED_OUT_HEAD").expect("head present");
        let pos_tail = both_text.find("VISIBLE_TAIL").expect("tail present");
        assert!(
            pos_head < pos_tail,
            "SCROLLED_OUT_HEAD must precede VISIBLE_TAIL in both layer (chronological): {both_text:?}"
        );

        // cleanup
        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// `screen.snapshot.request` を送ると include 指定された component だけが
    /// `Some(...)` で返り、未指定の component は `None` のまま。
    #[test]
    fn serve_state_snapshot_returns_requested_components_only() {
        use crate::protocol::messages::{
            ScreenBufferKind, SnapshotComponent, StateSnapshotRequest,
        };
        let (_, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        let req = ControlMessage::StateSnapshotRequest(StateSnapshotRequest {
            include: vec![
                SnapshotComponent::Cursor,
                SnapshotComponent::Mode,
                SnapshotComponent::WindowSize,
                SnapshotComponent::Buffer,
                SnapshotComponent::SequenceNo,
            ],
            serial: Some(7),
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");

        let msg = next_control(&mut s);
        match msg {
            ControlMessage::StateSnapshotResponse(resp) => {
                assert_eq!(resp.serial, Some(7));
                // 未指定 → None
                assert!(resp.cells.is_none(), "cells must be None");
                assert!(resp.scrollback.is_none(), "scrollback must be None");
                // 指定 → Some
                assert!(resp.cursor.is_some(), "cursor must be Some");
                assert!(resp.mode.is_some(), "mode must be Some");
                assert!(resp.window_size.is_some(), "window_size must be Some");
                assert_eq!(resp.buffer, Some(ScreenBufferKind::Primary));
                assert!(resp.sequence_no.is_some(), "sequence_no must be Some");
                let ws = resp.window_size.unwrap();
                assert_eq!(ws.rows, 24);
                assert_eq!(ws.cols, 80);
            }
            o => panic!("expected StateSnapshotResponse, got {o:?}"),
        }

        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// `screen.snapshot.request` で `include` が空なら ProtocolMalformed を返す。
    #[test]
    fn serve_state_snapshot_empty_include_is_rejected() {
        use crate::protocol::messages::StateSnapshotRequest;
        let (_, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        let mut s = client_connect_with_retry(&sock_path);
        let _r = do_client_handshake(&mut s);
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        let req = ControlMessage::StateSnapshotRequest(StateSnapshotRequest {
            include: vec![],
            serial: None,
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");

        let msg = next_control(&mut s);
        match msg {
            ControlMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::ProtocolMalformed);
            }
            o => panic!("expected Error(ProtocolMalformed), got {o:?}"),
        }

        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    /// `screen.dump.request` を cap negotiation せずに送ると
    /// `unsupported-capability` を返す。client は handshake で screen-dump-v1 を
    /// 含めなければ MVP_CAPS から除外されて intersect される。
    #[test]
    fn serve_screen_dump_without_cap_is_rejected() {
        use crate::protocol::messages::{
            ScreenDumpFormat as ProtoDumpFormat, ScreenDumpLayer as ProtoDumpLayer,
            ScreenDumpRequest,
        };
        let (_, sock_path, _dir, handle) = spawn_serve_thread(long_running_cmd());

        // handshake は data + lock のみ (= screen-dump-v1 を要求しない)
        let mut s = client_connect_with_retry(&sock_path);
        let req = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: vec!["data".into(), "lock".into()],
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: None,
        });
        Frame::cbor_control(req.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");
        // handshake.response を取り出して intersect が data + lock のみであることを確認
        let resp_frame = Frame::decode_from(&mut s).expect("response");
        match ControlMessage::decode_from(resp_frame.body.as_slice()).expect("decode") {
            ControlMessage::HandshakeResponse(r) => {
                assert!(r.caps.iter().any(|c| c == "data"));
                assert!(r.caps.iter().any(|c| c == "lock"));
                assert!(!r.caps.iter().any(|c| c == "screen-dump-v1"));
            }
            o => panic!("expected HandshakeResponse, got {o:?}"),
        }
        discard_attach_redraw(&mut s);

        // leader.notify を drain (= 1st client は leader=true で broadcast を受ける)
        let _ = Frame::decode_from(&mut s).expect("leader.notify");

        // screen.dump.request 送信 → unsupported-capability
        let dreq = ControlMessage::ScreenDumpRequest(ScreenDumpRequest {
            format: ProtoDumpFormat::Ansi,
            layer: ProtoDumpLayer::Visible,
            rect: None,
            serial: None,
        });
        Frame::cbor_control(dreq.encode_to_vec().expect("encode"))
            .encode_to(&mut s)
            .expect("send");
        s.flush().expect("flush");
        let msg = next_control(&mut s);
        match msg {
            ControlMessage::Error(e) => {
                assert_eq!(e.code, ErrorCode::UnsupportedCapability);
            }
            o => panic!("expected unsupported-capability Error, got {o:?}"),
        }

        Frame::cbor_control(
            ControlMessage::Kill(Kill {
                signal: None,
                wait: true,
            })
            .encode_to_vec()
            .expect("encode"),
        )
        .encode_to(&mut s)
        .expect("send");
        s.flush().expect("flush");
        let _ = handle.join().expect("daemon thread");
    }

    // === DR-0016 Phase 4 配線 unit tests ===

    /// DR-0016 §3 §M1: `sig_num_to_name` は nix 既知 signal を canonical 名で返す。
    #[test]
    fn sig_num_to_name_returns_canonical_name_for_known_signal() {
        let tstp = nix::sys::signal::Signal::SIGTSTP as i32;
        assert_eq!(sig_num_to_name(tstp), "SIGTSTP");
        let cont = nix::sys::signal::Signal::SIGCONT as i32;
        assert_eq!(sig_num_to_name(cont), "SIGCONT");
    }

    /// 未知 signal 番号は `SIG<N>` の fallback 名を返す (= panic しない)。
    #[test]
    fn sig_num_to_name_falls_back_to_sig_n_for_unknown() {
        // 0 / 巨大値などは Signal::try_from で Err、fallback に乗る。
        assert_eq!(sig_num_to_name(99999), "SIG99999");
    }

    /// DR-0016 §3 4 段階 — `record_child_continued` は registry に
    /// `ChildContinuedObserved` を 1 件 push する。pid は呼び出し側で渡したものを乗せる。
    #[test]
    fn record_child_continued_pushes_lifecycle_event() {
        use crate::protocol::messages::{InputSecrecy, RecordDirection, RecordFormat};
        use std::path::Path;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cont.jsonl");
        let state = SessionState::default();
        let req = crate::protocol::messages::RecordStartRequest {
            direction: RecordDirection::Both,
            format: RecordFormat::Jsonl,
            output_path: path.to_string_lossy().into_owned(),
            max_bytes: None,
            max_duration_ms: None,
            // record-all (= start が redact-after-prompt を reject するため、lifecycle
            // 記録の意図を保ったまま受理される policy を使う、DR-0016 §6 interim)。
            input_secrecy: InputSecrecy::RecordAll,
            prompt_pattern: None,
        };
        let session = crate::daemon::record::SessionInfo {
            session_id: "t".into(),
            daemon_pid: 1,
            daemon_boot_id: "boot".into(),
            argv: vec!["sh".into()],
            cwd: "/".into(),
        };
        let id = state.record_registry.start(&req, 1, session).unwrap();
        record_child_continued(&state, nix::unistd::Pid::from_raw(4242));
        state.record_registry.stop(id).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // header + 1 lifecycle event
        assert_eq!(lines.len(), 2, "lines = {lines:?}");
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["ev"], "child-continued-observed");
        assert_eq!(v["sig_name"], "SIGCONT");
        assert_eq!(v["pid"], 4242);
        let _ = Path::new(&path); // keep path borrow alive
    }
}
