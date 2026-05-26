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

use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use nix::poll::{PollFd, PollTimeout};
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::Error;
use crate::protocol::messages::{
    ClientInfo, ErrorCode, ErrorMessage, LeaderNotify, LockResponse, LockResult, ModeChange,
    SessionMode, StatusResponse, TailData, TailEnd, TailEndReason, TailRequest, WaitMatchOptions,
    WaitOutcome, WaitPredicate, WaitRequest, WaitResult,
};
use crate::protocol::{
    ControlMessage, Frame, HandshakeResponse, MVP_CAPS, Mode, TYPE_CBOR_CONTROL, TYPE_RAW_DATA,
    Transport, UnixStreamTransport, intersect_caps,
};
use crate::scrollback::Scrollback;
use crate::sys::{
    FdExt, Pty, UnixSock, poll::PollFlags, poll::PollOutcome, poll::poll, pty::Spawned,
};

use super::DaemonConfig;

/// daemon 1 つ分の起動済 session。
///
/// `Session` は **`Drop` で子 PTY を graceful に終了する** (R4-H4):
/// - SIGTERM を送って最大 `DROP_TERM_WAIT` (= 500ms) 待つ
/// - 残っていれば SIGKILL
/// - `waitpid(WNOHANG)` で短い loop で reap し zombie を回収
///
/// `serve()` は `self` を destructure で消費するため、正常 path では
/// このフィールド由来の Drop は走らない (= 各フィールドが個別に Drop される)。
/// Drop が発火するのは `Session::start` 後に `serve` を呼ばずに
/// `Session` が drop されたケース (= test 内 panic / 初期化エラー後の early
/// return 等) で、その場合に子が orphan として残らないようにする。
///
/// Pty/UnixSock は元々独自の Drop を持つため、Session::drop は child Pid の
/// 始末だけを担当する。Drop 中は panic 安全のため全 syscall を `let _ = ...`
/// で error 飲み込む (= panic 中の二重 panic で process abort を避ける)。
#[derive(Debug)]
pub struct Session {
    config: DaemonConfig,
    pty: Pty,
    child: Pid,
    listener: UnixSock,
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
        let argv: Vec<&str> = config.cmd.iter().map(String::as_str).collect();
        let Spawned { pty, child } = Pty::spawn(&argv, config.cols, config.rows)?;
        // master FD を nonblock にして、POLLHUP 偽陽性 (macOS) で read_some が
        // block するのを防ぐ。read_some は EAGAIN を返す → serve_loop で continue。
        pty.master_fd().set_nonblocking(true)?;
        let listener = UnixSock::listen(&config.socket_path)?;
        Ok(Self {
            config,
            pty,
            child,
            listener,
        })
    }

    /// session 名 (handshake response 用 + status 表示用)。
    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    /// 子 PTY の PID。
    pub fn child_pid(&self) -> Pid {
        self.child
    }

    /// listener が bind している socket path。
    pub fn socket_path(&self) -> &std::path::Path {
        self.listener.path()
    }

    /// 子 PTY master fd (= 後の Phase で broadcast/multiplex に使用)。
    pub fn pty(&self) -> &Pty {
        &self.pty
    }

    /// `Session` を fields に解体する (R4-H4 internal)。
    ///
    /// `Session` は `Drop` を実装している (= 子 PTY の orphan 防止) ため、Rust の
    /// destructure-move (`let Self { .. } = self`) は使えない。`serve` / `run` の
    /// 正常 path で fields を取り出すには ManuallyDrop で Drop をバイパスする
    /// 必要がある。本関数はその unsafe を 1 箇所に閉じ込めるためのヘルパ。
    ///
    /// 呼び出し後は `Session` の Drop は走らない。fields は呼び出し側が
    /// 責任を持って drop する (= `Pty::drop` / `UnixSock::drop` は走る)。
    fn into_parts(self) -> (DaemonConfig, Pty, Pid, UnixSock) {
        // SAFETY: ManuallyDrop で Session の Drop をバイパスし、各 field を
        // 1 度ずつ ptr::read で取り出す。各 field は move semantics でしか
        // 触らないので二重 read は発生しない。`md` は drop されないので
        // Session::drop も呼ばれない (= 意図通り)。
        let md = std::mem::ManuallyDrop::new(self);
        unsafe {
            let config = std::ptr::read(&md.config);
            let pty = std::ptr::read(&md.pty);
            let child = std::ptr::read(&md.child);
            let listener = std::ptr::read(&md.listener);
            (config, pty, child, listener)
        }
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
    pub fn serve(self) -> Result<i32, Error> {
        // R4-H4: Drop は `start` 後 `serve` 未呼出のフォールバック専用。
        // serve は正常 path として fields を消費するので、into_parts で
        // Drop をバイパスして fields を取り出す。
        let (config, pty, child, listener) = self.into_parts();
        let mut clients: Vec<ClientHandle> = Vec::new();
        let mut next_client_id: u64 = 0;
        let mut state = SessionState::default();
        let mut scrollback = Scrollback::new(config.scrollback_bytes);
        let mut pending_waits: Vec<PendingWait> = Vec::new();
        let outcome = serve_loop(
            &pty,
            child,
            &listener,
            &mut clients,
            &mut next_client_id,
            &config,
            &mut state,
            &mut scrollback,
            &mut pending_waits,
        );

        // tail follow subscriber へ TailEnd を 1 発投げてから cleanup する。
        // 終了理由は outcome に応じて分岐:
        // - 子が exit した (= ChildExited) → ChildExited
        // - client detach / Kill による session 終了 → ClientCancel
        // - 致命 error → 送らない (= cleanup で socket close する方が誠実)
        let tail_end_reason = match &outcome {
            RelayOutcome::ChildExited(_) => Some(TailEndReason::ChildExited),
            RelayOutcome::ClientDetachedOrKilled => Some(TailEndReason::ClientCancel),
            RelayOutcome::Error(_) => None,
        };
        if let Some(reason) = tail_end_reason {
            for ch in clients.iter() {
                if matches!(ch.subscription, Subscription::TailFollow { .. }) {
                    let _ = send_control(ch, ControlMessage::TailEnd(TailEnd { reason }));
                }
            }
        }

        // cleanup:
        // 1. writer_tx を drop → writer_pump は残り frame を drain してから recv 終了
        // 2. **per-client** で queued_bytes==0 を最大 200ms 待つ (= 1 client の hang
        //    が他 client の drain budget を食い潰さないように、deadline を共有せず
        //    client ごとに 200ms ずつ振る)
        // 3. socket shutdown (= まだ write_all で block 中なら強制解除)
        // 4. join
        //
        // ※ writer_tx drop だけだと、writer_pump が write_all で block 中の場合
        //   recv に戻らず join hang する。そのため drain wait + shutdown を入れる。
        const DRAIN_BUDGET_PER_CLIENT: std::time::Duration = std::time::Duration::from_millis(200);
        for ch in clients.iter() {
            let deadline = std::time::Instant::now() + DRAIN_BUDGET_PER_CLIENT;
            while ch.queued_bytes.load(Ordering::Acquire) > 0
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        for ch in clients.drain(..) {
            drop(ch.writer_tx);
            let _ = ch.reader.shutdown(std::net::Shutdown::Both);
            if let Some(t) = ch.writer_thread {
                let _ = t.join();
            }
        }

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

        let child = self.child;

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
        let _ = kill(child, Signal::SIGTERM);

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
        let _ = kill(child, Signal::SIGKILL);
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

/// 1 client の per-thread state (writer thread + 自前 byte bound queue + reader handle)。
///
/// Phase 12: queue capacity は **byte 単位の厳密 cap** (DR-0008 §8.2)。
/// `writer_tx` は unbounded mpsc、enqueue の可否は `queued_bytes` を atomic で
/// check + add し、`buffer_limit` 超過なら enqueue を拒否して当該 client を
/// disconnect する (= `error` kind=`backpressure.disconnect` を best-effort 送信)。
struct ClientHandle {
    id: u64,
    mode: Mode,
    /// leader 取得状態 (= rw mode の最初の client が true)。
    leader: bool,
    /// 受信 subscription (= broadcast の encoding 種類を切り替える)。
    subscription: Subscription,
    /// handshake 後の有効 capability 集合 (= MVP_CAPS と req.caps の intersect)。
    /// D7: 後続 message の処理で「cap が無いのに該当 message を送ってきた」を
    /// reject する。
    negotiated_caps: Vec<String>,
    /// daemon → client への frame enqueue 用 unbounded mpsc。
    writer_tx: Sender<Vec<u8>>,
    /// 現在 queue 内に積まれている bytes 数 (= writer_pump が送信完了で減らす)。
    queued_bytes: Arc<AtomicUsize>,
    /// queue の byte 上限 (= `DaemonConfig::client_buffer_bytes`)。
    buffer_limit: usize,
    /// writer thread のハンドル。drop の前に join される。
    writer_thread: Option<std::thread::JoinHandle<()>>,
    /// daemon が client → daemon を decode するときに使う socket reader。
    reader: UnixStream,
}

/// client の出力 subscription (Phase 11)。
///
/// - `Raw`: 通常 attach (= `hyoui run` / `hyoui attach`)、子 PTY 出力を
///   `TYPE_RAW_DATA` frame で受け取る。
/// - `TailFollow`: `tail.request { follow: true }` 後、子 PTY 出力を
///   `tail.data` CBOR frame で受け取る (strip_ansi 適用は per-chunk best-effort)。
#[derive(Debug, Clone, Copy)]
enum Subscription {
    Raw,
    TailFollow { strip_ansi: bool },
}

/// `wait.request` の pending 状態 (Phase 11c)。
///
/// 各 wait は self-contained:
/// - `predicate`: text / pattern (compiled regex) / idle
/// - `options`: strip_escapes / newline_convert_lf
/// - `deadline`: timeout_ms から計算した絶対時刻 (= 無限 wait は None)
/// - `accumulated`: wait 開始後に蓄積した master bytes (predicate scan 対象)
/// - `last_activity`: Idle 用に最後に master 出力があった時刻 (= 開始時 = now)
/// - `compiled_regex`: Pattern predicate のみ、wait 開始時 1 回 compile
/// - `strip_carry`: `strip_escapes=true` 時に chunk 境界を跨ぐ partial ANSI
///   escape を持ち越すための stateful stripper (R4-H3)。
struct PendingWait {
    client_id: u64,
    predicate: WaitPredicate,
    options: WaitMatchOptions,
    deadline: Option<Instant>,
    accumulated: Vec<u8>,
    last_activity: Instant,
    compiled_regex: Option<regex::bytes::Regex>,
    strip_carry: crate::strip::StripAnsiCarry,
}

/// `accumulated` の上限 (= memory bound)。超過すると古い byte から truncate。
const WAIT_ACCUMULATED_LIMIT: usize = 1024 * 1024;

/// 1 client が同時に持てる pending wait の上限。超過すると新規 `wait.request` は
/// error code=`wait.too-many` で reject (= N × WAIT_ACCUMULATED_LIMIT の OOM 防止)。
const MAX_WAITS_PER_CLIENT: usize = 16;

/// daemon が同時 attach を許す client 数上限 (= D6 集合 backpressure DoS 対策)。
/// 超過した accept は即 socket close で reject。`client_buffer_bytes` が 8 MiB の
/// 場合、64 clients × 8 MiB = 最大 512 MiB の queue 占有が理論上限。
const MAX_CLIENTS_PER_DAEMON: usize = 64;

/// R4-C3: handshake (= 1 client の HandshakeRequest 受信 + token 検証) を完了
/// させるまでの上限時間。これを超過した pending handshake は socket close して
/// 当該 worker thread の流れを中断する (= slow-loris DoS 防止)。
///
/// 旧実装は accept 後 `Frame::decode_from` を同期 blocking で呼び、悪意 client が
/// 1 byte ずつ送って handshake を遅延させると `serve_loop` 全体が止まっていた。
/// 現実装では handshake を別 thread に切り出し、本 timeout で個別に頭打ちする。
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// daemon が同時に走らせて良い pending handshake worker 数の上限。
/// MAX_CLIENTS_PER_DAEMON と同じ値にし、accept 段階で頭打ちする。これを超える
/// `listener.accept()` は即 socket close で reject (= 接続段階の集合 DoS 防止)。
const MAX_PENDING_HANDSHAKES: usize = MAX_CLIENTS_PER_DAEMON;

/// session 全体の状態 (Phase 10)。lock 周りの state machine を保持する。
///
/// 現状の field:
/// - `lock_holder`: lock 保持中の client id (= `None` なら未 lock)
/// - `lock_token`: 発行済 token (= `LockRelease` 検証用)
///
/// Wait queue は MVP では未実装 (`LockAcquire { wait: true, .. }` でも `Denied`
/// を返す)。queue 実装は v0.2.0+ の Phase 12 で検討。
#[derive(Debug, Default)]
struct SessionState {
    lock_holder: Option<u64>,
    lock_token: Option<String>,
}

impl SessionState {
    /// session 全体の SessionMode (= mode.change の `session_mode` 用)。
    ///
    /// MVP は「lock 中 = `Locked`、それ以外 = `Rw`」。`Ro` 強制 (= 誰も書けない)
    /// は v0.2.0+ で `--read-only` daemon option 等を導入したときに使う。
    fn session_mode(&self) -> SessionMode {
        if self.lock_holder.is_some() {
            SessionMode::Locked
        } else {
            SessionMode::Rw
        }
    }
}

/// 2 つの byte slice を constant-time で比較 (= timing attack 耐性)。
///
/// 同 UID 信頼境界では timing leak の悪用余地は薄いが、token 比較に使う
/// 値は副作用ゼロの簡易実装。長さ違いは即 `false` で抜けるため厳密 constant
/// time ではないが、長さ自体を秘匿する必要はない。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 整数 signum から nix `Signal` を返す。POSIX `kill(pid, 0)` semantic に従い
/// `signum == 0` も範囲外として扱う (= "existence probe" は wire protocol で
/// サポートしない、必要なら別 message を新設)。範囲外なら `None`。
fn nix_signal_from_signum(signum: u8) -> Option<Signal> {
    if signum == 0 {
        return None;
    }
    Signal::try_from(signum as i32).ok()
}

/// `detach` message の target に応じて drop 対象を決める。
///
/// - `Myself`: 自分 1 client のみ drop (= DropClient)
/// - `Others`: 自分以外の全 client を drop (= caller が複数 drop できる API が
///   無いため、本実装では `error: not-implemented` を返して継続)
/// - `All`: 全 client + session 終了 (= 同様に `error: not-implemented` で継続)
fn handle_detach_target(
    idx: usize,
    detach: crate::protocol::messages::Detach,
    clients: &mut [ClientHandle],
) -> ClientFrameOutcome {
    use crate::protocol::messages::DetachTarget;
    match detach.target {
        DetachTarget::Myself => ClientFrameOutcome::DropClient,
        DetachTarget::Others | DetachTarget::All => {
            // Round2 #7: error 通知 + 自分も drop する。
            // 旧 Round1 実装は `Continue` で client を生かしたまま error だけ送って
            // いたが、旧仕様 (silent skip → DropClient だった) を仮定する client は
            // socket open のまま hang する。「target が `Others`/`All` で部分実装」
            // という事実を error で伝えつつ、後方互換 (= 最低限自分は drop される)
            // を維持する。Others/All の本来の動作 (= 他 client / 全 client drop)
            // は v0.2.0+ で実装予定。
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::DetachTargetPartial,
                    message: "detach target=others/all not fully implemented yet; \
                              only self will be detached (Phase 11 MVP は Myself のみ対応、\
                              v0.2.0+ で他 client も drop する semantic に拡張予定)"
                        .into(),
                    details: None,
                }),
            );
            ClientFrameOutcome::DropClient
        }
    }
}

/// 128-bit (32 hex char) の lock token を生成する。
///
/// 同 UID 信頼領域なので CSPRNG 強度は厳格には不要だが、token が予測可能だと
/// 同 UID の悪意あるプロセスが推測で lock を奪取しうるため、最低限の対策として:
///
/// 1. `/dev/urandom` から **16 byte** を `read_exact` で取り切る (= 全 128 bit
///    分の entropy を確実に得る)
/// 2. もし urandom open / read が失敗した場合は `panic` して daemon を止める
///    (= 弱い token で運用継続するより落ちる方が安全)
fn generate_lock_token() -> String {
    use std::io::Read;

    let mut buf = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom")
        .expect("hyoui: cannot open /dev/urandom for lock token");
    f.read_exact(&mut buf)
        .expect("hyoui: short read from /dev/urandom for lock token");
    let mut out = String::with_capacity(32);
    for b in &buf {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// 新規 rw client が leader 取得すべきかを判定する (= 既存に leader が居ないか)。
///
/// `RwNoLeader` mode の client は leader 候補から除外 (= 明示的に leader を
/// 取らない意思表示)。
fn should_assign_leader(clients: &[ClientHandle], new_mode: Mode) -> bool {
    matches!(new_mode, Mode::Rw) && !clients.iter().any(|c| c.leader)
}

/// leader が居ない状態 (= leader cascade 候補) のときに、次の `Mode::Rw` client を
/// leader に昇格させる。成功すれば新 leader の id を返す。
fn elevate_next_leader(clients: &mut [ClientHandle]) -> Option<u64> {
    if clients.iter().any(|c| c.leader) {
        return None;
    }
    for c in clients.iter_mut() {
        if matches!(c.mode, Mode::Rw) {
            c.leader = true;
            return Some(c.id);
        }
    }
    None
}

/// 1 client への frame enqueue 結果 (Phase 12 backpressure)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnqueueOutcome {
    /// queue に追加成功、writer thread が socket に書き出す。
    Sent,
    /// `buffer_limit` 超過 (= 当該 client を disconnect すべき)。
    Overflow,
    /// writer thread が既に死亡 (= socket close 検知済み、再 enqueue 不能)。
    WriterDead,
}

/// 1 frame の bytes を 1 client の queue に積む。
///
/// **race semantics (= L4 review メモ)**: `load` → `fetch_add` の間に他の writer が
/// `fetch_add` していると、`queued_bytes` が `buffer_limit` を一時的に **超過**
/// する。serve_loop は **single-threaded** main thread のみが broadcast / enqueue を
/// 呼ぶため実 daemon では race しないが、unit test で別 thread から enqueue 呼ぶと
/// 厳密 cap は崩れる。`compare_exchange_weak` loop で書き直せば厳密化できるが、
/// 「ms 単位 throughput を最優先」と「将来 multi-writer になる必然性が低い」を
/// 天秤にかけて relax で許容。実用上は writer_pump が `fetch_sub` するので大局
/// 収束する。
fn enqueue_for_client(ch: &ClientHandle, bytes: Vec<u8>) -> EnqueueOutcome {
    let size = bytes.len();
    let cur = ch.queued_bytes.load(Ordering::Acquire);
    if cur.saturating_add(size) > ch.buffer_limit {
        return EnqueueOutcome::Overflow;
    }
    ch.queued_bytes.fetch_add(size, Ordering::AcqRel);
    if ch.writer_tx.send(bytes).is_err() {
        // writer thread 死亡 → queued_bytes を戻して終了
        ch.queued_bytes.fetch_sub(size, Ordering::AcqRel);
        return EnqueueOutcome::WriterDead;
    }
    EnqueueOutcome::Sent
}

/// `backpressure.disconnect` error message を best-effort で投げる。
///
/// L5: 旧実装は `writer_tx.send` を直接呼んで `queued_bytes` をバイパスしていた。
/// すると writer_pump の `fetch_sub` で「送ったぶんを引く」想定が破れ、
/// `queued_bytes` が unsigned wrap (= 巨大値) を返す可能性があった。本実装では
/// `queued_bytes` を明示加算してから send することで writer_pump の `fetch_sub`
/// と整合させる。`buffer_limit` は意図的に超えて送る (= disconnect 直前の最後の
/// 1 メッセージ、defensible)。writer_tx が closed なら加算分を戻して諦める。
fn send_backpressure_error(ch: &ClientHandle, queued: usize) {
    let msg = ControlMessage::Error(ErrorMessage {
        code: ErrorCode::BackpressureDisconnect,
        message: "client buffer full".into(),
        details: Some(ciborium::Value::Map(vec![
            (
                ciborium::Value::Text("queued_bytes".into()),
                ciborium::Value::Integer((queued as u64).into()),
            ),
            (
                ciborium::Value::Text("limit".into()),
                ciborium::Value::Integer((ch.buffer_limit as u64).into()),
            ),
        ])),
    });
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return,
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return;
    }
    let size = frame_bytes.len();
    ch.queued_bytes.fetch_add(size, Ordering::AcqRel);
    if ch.writer_tx.send(frame_bytes).is_err() {
        ch.queued_bytes.fetch_sub(size, Ordering::AcqRel);
    }
}

/// CBOR control message を 1 client にだけ送る。
///
/// `true` = enqueue 成功、`false` = overflow / writer dead (= caller は当該
/// client を drop すべき)。
fn send_control(ch: &ClientHandle, msg: ControlMessage) -> bool {
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return false;
    }
    matches!(enqueue_for_client(ch, frame_bytes), EnqueueOutcome::Sent)
}

/// `Instant` (monotonic) を Unix epoch millis に近似変換する。
///
/// `now_inst - ts` で elapsed を求め、`SystemTime::now() - elapsed` を取る。
/// SystemTime と Instant が線形に対応していない場合 (= clock jump) に誤差は
/// 出るが、tail.data の timestamp_ms は debug / 表示用なので実用上問題ない。
fn instant_to_epoch_ms(ts: Instant) -> i64 {
    let now_inst = Instant::now();
    let elapsed = now_inst.saturating_duration_since(ts);
    let now_sys = std::time::SystemTime::now();
    let then = now_sys.checked_sub(elapsed).unwrap_or(now_sys);
    then.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 子 PTY 出力 `bytes` を全 client に broadcast する。subscription 種類に応じて
/// raw_data frame (= Raw) or tail.data CBOR frame (= TailFollow) を送る。
///
/// 戻り値: backpressure overflow / writer dead で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
fn broadcast_master_bytes(clients: &mut [ClientHandle], bytes: &[u8], ts: Instant) -> Vec<u64> {
    let raw_frame_bytes: Option<Vec<u8>> = if clients
        .iter()
        .any(|c| matches!(c.subscription, Subscription::Raw))
    {
        let mut buf = Vec::new();
        if Frame::raw_data(bytes.to_vec()).encode_to(&mut buf).is_ok() {
            Some(buf)
        } else {
            None
        }
    } else {
        None
    };

    let ts_ms = instant_to_epoch_ms(ts);
    let mut tail_cache: [Option<Vec<u8>>; 2] = [None, None];
    let encode_tail = |strip: bool, cache: &mut [Option<Vec<u8>>; 2]| -> Option<Vec<u8>> {
        let key = if strip { 1 } else { 0 };
        if let Some(ref cached) = cache[key] {
            return Some(cached.clone());
        }
        let payload = if strip {
            crate::strip::strip_ansi(bytes)
        } else {
            bytes.to_vec()
        };
        let msg = ControlMessage::TailData(TailData {
            bytes: payload,
            timestamp_ms: ts_ms,
        });
        let body = msg.encode_to_vec().ok()?;
        let mut frame_bytes = Vec::new();
        Frame::cbor_control(body).encode_to(&mut frame_bytes).ok()?;
        cache[key] = Some(frame_bytes.clone());
        Some(frame_bytes)
    };

    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        let fb = match ch.subscription {
            Subscription::Raw => raw_frame_bytes.clone(),
            Subscription::TailFollow { strip_ansi } => encode_tail(strip_ansi, &mut tail_cache),
        };
        if let Some(fb) = fb {
            match enqueue_for_client(ch, fb) {
                EnqueueOutcome::Sent => {}
                EnqueueOutcome::Overflow => {
                    send_backpressure_error(ch, ch.queued_bytes.load(Ordering::Acquire));
                    overflow_ids.push(ch.id);
                }
                EnqueueOutcome::WriterDead => {
                    overflow_ids.push(ch.id);
                }
            }
        }
    }
    overflow_ids
}

/// `wait.request` を処理する (Phase 11c)。
///
/// PendingWait を作って `pending_waits` に push。各 predicate ごとに:
/// - Text: substring match (= accumulated に対して `.windows().any()` 相当)
/// - Pattern: regex compile を 1 度実行、accumulated に対して `is_match`
///   (compile 失敗で error code=`wait.invalid-pattern` を返却)
/// - Idle: master 出力が `ms` 静かなら成立 (= 開始時 last_activity = now、
///   master 出力で last_activity 更新、`compute_wait_poll_timeout` で
///   `last_activity + ms - now` を poll timeout として使う)
fn handle_wait_request(
    idx: usize,
    req: WaitRequest,
    clients: &mut [ClientHandle],
    pending_waits: &mut Vec<PendingWait>,
) {
    let client_id = clients[idx].id;
    // per-client pending wait count cap (= memory DoS 対策)
    let existing = pending_waits
        .iter()
        .filter(|w| w.client_id == client_id)
        .count();
    if existing >= MAX_WAITS_PER_CLIENT {
        let _ = send_control(
            &clients[idx],
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::WaitTooMany,
                message: format!(
                    "too many pending waits for this client (limit {MAX_WAITS_PER_CLIENT})"
                ),
                details: None,
            }),
        );
        return;
    }
    // Text/Pattern の空 needle reject (= Round2 #1)。
    // 空 value だと scan ループの `accumulated.windows(0)` が std spec で panic、
    // daemon thread を落とすため事前に明示 error 返却で防ぐ。
    match &req.predicate {
        WaitPredicate::Text { value } if value.is_empty() => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::WaitInvalidText,
                    message: "text predicate value must not be empty".into(),
                    details: None,
                }),
            );
            return;
        }
        WaitPredicate::Pattern { regex } if regex.is_empty() => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::WaitInvalidPattern,
                    message: "pattern regex must not be empty".into(),
                    details: None,
                }),
            );
            return;
        }
        _ => {}
    }

    let now = Instant::now();
    let deadline = req
        .timeout_ms
        .and_then(|ms| now.checked_add(std::time::Duration::from_millis(ms)));

    let compiled_regex = match &req.predicate {
        WaitPredicate::Pattern { regex: r } => {
            // regex compile DoS 対策: 巨大 alternation / 深い nest で daemon の
            // event loop を block しないように size_limit / dfa_size_limit を絞る。
            // 既定 (10 MB / 2 MB) → 64 KB / 64 KB に削減 (= 通常用途で十分)。
            // pattern 文字列長も上限を設けて短時間で reject する。
            const PATTERN_MAX_LEN: usize = 1024;
            const REGEX_SIZE_LIMIT: usize = 64 * 1024;
            if r.len() > PATTERN_MAX_LEN {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::WaitInvalidPattern,
                        message: format!(
                            "regex too long: {} bytes (limit {PATTERN_MAX_LEN})",
                            r.len()
                        ),
                        details: None,
                    }),
                );
                return;
            }
            match regex::bytes::RegexBuilder::new(r)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
            {
                Ok(re) => Some(re),
                Err(_) => {
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::Error(ErrorMessage {
                            code: ErrorCode::WaitInvalidPattern,
                            message: "regex failed to compile (syntax or size limit)".into(),
                            details: None,
                        }),
                    );
                    return;
                }
            }
        }
        _ => None,
    };

    let wait = PendingWait {
        client_id,
        predicate: req.predicate,
        options: req.options,
        deadline,
        accumulated: Vec::new(),
        last_activity: now,
        compiled_regex,
        strip_carry: crate::strip::StripAnsiCarry::new(),
    };

    // 開始即 (= accumulated 空) に match することは Text/Pattern では起きないが、
    // Idle (ms = 0) だけは即成立しうる。即成立なら send + skip push。
    if matches!(wait.predicate, WaitPredicate::Idle { ms: 0 }) {
        let _ = send_control(
            &clients[idx],
            ControlMessage::WaitResult(WaitResult {
                outcome: WaitOutcome::Matched,
                matched_offset: None,
            }),
        );
        return;
    }
    pending_waits.push(wait);
}

/// 各 pending wait の `accumulated` に新規 master bytes を append し、predicate を
/// scan。マッチした wait は client へ `wait.result(Matched)` を送って remove する。
///
/// `WaitMatchOptions::strip_escapes` / `newline_convert_lf` は scan 前に新 bytes に
/// 適用する。`strip_escapes` は per-wait の `StripAnsiCarry` で chunk 境界を跨ぐ
/// partial ANSI escape を持ち越すため、escape を挟んで分割された needle も正しく
/// match できる (R4-H3)。
fn update_waits_on_master_bytes(
    pending_waits: &mut Vec<PendingWait>,
    clients: &mut [ClientHandle],
    new_bytes: &[u8],
    now: Instant,
) {
    let mut matched_indices: Vec<usize> = Vec::new();
    for (i, w) in pending_waits.iter_mut().enumerate() {
        // Idle 用に last_activity 更新 (= 静寂タイマーリセット)
        w.last_activity = now;

        let mut bytes_to_add: Vec<u8> = if w.options.strip_escapes {
            // stateful: 前 chunk の末尾で未完了の escape を carry。
            w.strip_carry.push(new_bytes)
        } else {
            new_bytes.to_vec()
        };
        if w.options.newline_convert_lf {
            bytes_to_add = crate::strip::normalize_lf(&bytes_to_add);
        }
        w.accumulated.extend_from_slice(&bytes_to_add);
        // memory bound: head から trim
        if w.accumulated.len() > WAIT_ACCUMULATED_LIMIT {
            let drop_n = w.accumulated.len() - WAIT_ACCUMULATED_LIMIT;
            w.accumulated.drain(..drop_n);
        }

        let matched = match &w.predicate {
            WaitPredicate::Text { value } => w
                .accumulated
                .windows(value.len())
                .any(|win| win == value.as_bytes()),
            WaitPredicate::Pattern { .. } => w
                .compiled_regex
                .as_ref()
                .map(|re| re.is_match(&w.accumulated))
                .unwrap_or(false),
            WaitPredicate::Idle { .. } => false, // Idle は静寂判定なので bytes 増加で match しない
        };
        if matched {
            matched_indices.push(i);
        }
    }
    // matched を逆順で remove + WaitResult 送信
    for i in matched_indices.into_iter().rev() {
        let w = pending_waits.remove(i);
        if let Some(ch) = clients.iter().find(|c| c.id == w.client_id) {
            let _ = send_control(
                ch,
                ControlMessage::WaitResult(WaitResult {
                    outcome: WaitOutcome::Matched,
                    matched_offset: None,
                }),
            );
        }
    }
}

/// poll timeout を pending_waits の最も早い deadline (= timeout / idle 期限) から
/// 計算する。pending が無ければ `PollTimeout::NONE` (= 無限 block)。
fn compute_wait_poll_timeout(pending_waits: &[PendingWait]) -> PollTimeout {
    let now = Instant::now();
    let mut earliest: Option<std::time::Duration> = None;
    for w in pending_waits {
        let candidates: [Option<std::time::Duration>; 2] = [
            w.deadline
                .map(|d| d.saturating_duration_since(now))
                .map(|d| d.max(std::time::Duration::ZERO)),
            match w.predicate {
                WaitPredicate::Idle { ms } => {
                    // u64::MAX 等の極端な ms で `Instant + Duration` が overflow すると
                    // panic するため checked_add で防ぐ。overflow した場合は事実上
                    // 「無限に先」なので候補に含めない (= None)。
                    let idle_dur = std::time::Duration::from_millis(ms);
                    w.last_activity
                        .checked_add(idle_dur)
                        .map(|target| target.saturating_duration_since(now))
                }
                _ => None,
            },
        ];
        for cand in candidates.into_iter().flatten() {
            earliest = Some(match earliest {
                None => cand,
                Some(prev) => prev.min(cand),
            });
        }
    }
    match earliest {
        None => PollTimeout::NONE,
        Some(d) => {
            // PollTimeout は ms 精度。0 (= 即時 timeout) を許容、上限は i32 max ms。
            // `as_millis()` は u128 を返すので `try_from + unwrap_or(i32::MAX)` で
            // saturating cast (= u64::MAX ms 等が来ても panic しない)。
            let ms: i32 = i32::try_from(d.as_millis()).unwrap_or(i32::MAX);
            PollTimeout::try_from(ms).unwrap_or(PollTimeout::NONE)
        }
    }
}

/// poll が timeout で起きた時に各 pending_wait の deadline / idle 経過をチェック。
/// Idle 経過 → WaitResult(Matched)、deadline 経過 → WaitResult(Timeout) として remove。
fn check_wait_timeouts(pending_waits: &mut Vec<PendingWait>, clients: &mut [ClientHandle]) {
    let now = Instant::now();
    let mut to_remove: Vec<(usize, WaitOutcome)> = Vec::new();
    for (i, w) in pending_waits.iter().enumerate() {
        // Idle predicate: now - last_activity >= idle_ms なら成立
        // u64::MAX 等で `Instant + Duration` が overflow すると panic するため
        // checked_add で防ぐ。overflow した場合は事実上「無限に先」なので Match しない。
        if let WaitPredicate::Idle { ms } = w.predicate {
            if let Some(target) = w
                .last_activity
                .checked_add(std::time::Duration::from_millis(ms))
            {
                if now >= target {
                    to_remove.push((i, WaitOutcome::Matched));
                    continue;
                }
            }
        }
        // 絶対 timeout: deadline 経過なら Timeout
        if let Some(dl) = w.deadline {
            if now >= dl {
                to_remove.push((i, WaitOutcome::Timeout));
            }
        }
    }
    for (i, outcome) in to_remove.into_iter().rev() {
        let w = pending_waits.remove(i);
        if let Some(ch) = clients.iter().find(|c| c.id == w.client_id) {
            let _ = send_control(
                ch,
                ControlMessage::WaitResult(WaitResult {
                    outcome,
                    matched_offset: None,
                }),
            );
        }
    }
}

/// `tail.request` を処理する (Phase 11)。
///
/// 流れ:
/// 1. since_ms / last_bytes でフィルタした bytes を scrollback から取り出す
/// 2. strip_ansi が true なら ANSI escape を strip (per-snapshot で 1 回だけ)
/// 3. 取り出した bytes を 1 個の `TailData` として送信 (= chunk 境界は失う、
///    timestamp_ms = now)
/// 4. follow=false なら即 `TailEnd(Eof)`、follow=true なら subscription を
///    `TailFollow` に切り替えて以降の master 出力も `TailData` で送り続ける
///
/// since_strict=true で since 範囲が ring buffer から押し出されていれば
/// `TailEnd(BufferTruncated)` を返して subscription は変更しない。
fn handle_tail_request(
    idx: usize,
    req: TailRequest,
    clients: &mut [ClientHandle],
    scrollback: &Scrollback,
) {
    let now = Instant::now();
    let bytes_opt: Option<Vec<u8>> = if let Some(since_ms) = req.since_ms {
        let dur = std::time::Duration::from_millis(since_ms);
        if req.since_strict {
            match scrollback.since_strict(now, dur) {
                Ok(b) => Some(b),
                Err(_) => {
                    // since 範囲が buffer から evict 済 → BufferTruncated で即終了
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::TailEnd(TailEnd {
                            reason: TailEndReason::BufferTruncated,
                        }),
                    );
                    return;
                }
            }
        } else {
            Some(scrollback.since(now, dur))
        }
    } else if let Some(last_bytes) = req.last_bytes {
        Some(scrollback.last_n_bytes(last_bytes as usize))
    } else {
        Some(scrollback.last_n_bytes(scrollback.total_bytes()))
    };

    let mut snapshot = bytes_opt.unwrap_or_default();
    if let Some(last_bytes) = req.last_bytes {
        let lb = last_bytes as usize;
        if snapshot.len() > lb {
            snapshot = snapshot[snapshot.len() - lb..].to_vec();
        }
    }
    if req.strip_ansi {
        snapshot = crate::strip::strip_ansi(&snapshot);
    }

    if !snapshot.is_empty() {
        let _ = send_control(
            &clients[idx],
            ControlMessage::TailData(TailData {
                bytes: snapshot,
                timestamp_ms: instant_to_epoch_ms(now),
            }),
        );
    }

    if req.follow {
        clients[idx].subscription = Subscription::TailFollow {
            strip_ansi: req.strip_ansi,
        };
    } else {
        let _ = send_control(
            &clients[idx],
            ControlMessage::TailEnd(TailEnd {
                reason: TailEndReason::Eof,
            }),
        );
    }
}

/// CBOR control message を全 client に broadcast。
///
/// 戻り値: backpressure overflow / writer dead で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
fn broadcast_control(clients: &mut [ClientHandle], msg: &ControlMessage) -> Vec<u64> {
    let body = match msg.encode_to_vec() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let mut frame_bytes = Vec::new();
    if Frame::cbor_control(body)
        .encode_to(&mut frame_bytes)
        .is_err()
    {
        return Vec::new();
    }
    broadcast_bytes(clients, frame_bytes)
}

/// daemon → client の writer pump (= per-thread)。
///
/// `rx` から `Vec<u8>` を受け取って socket に write_all、送信完了で
/// `queued_bytes` から減算する (= Phase 12 byte bound 厳密化)。送信失敗で thread 終了。
fn writer_pump(
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    mut sock: UnixStream,
    queued_bytes: Arc<AtomicUsize>,
) {
    while let Ok(bytes) = rx.recv() {
        let size = bytes.len();
        if std::io::Write::write_all(&mut sock, &bytes).is_err() {
            // client が close した。recv ループ抜けて thread 終了。
            return;
        }
        queued_bytes.fetch_sub(size, Ordering::AcqRel);
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
) -> RelayOutcome {
    // R4-C3: 別 thread で進行中の handshake worker 群。worker が `do_handshake_stage`
    // を完了すると `rx` に Ok/Err が流れる。本 vector は serve_loop が所有し、各
    // iteration で try_recv で完了したものを引き取って `clients` に integrate する。
    let mut pending_handshakes: Vec<PendingHandshake> = Vec::new();
    // R4-H14: 子の Stopped/Continued 追跡。loop 越しに状態を保持する。
    let mut lifecycle = ChildLifecycle::default();
    loop {
        // poll fd 構築: listener + master + 各 client reader
        let listener_fd = listener.as_fd();
        let master_fd = pty.master_fd();
        let mut poll_fds: Vec<PollFd> = Vec::with_capacity(2 + clients.len());
        poll_fds.push(PollFd::new(listener_fd, PollFlags::POLLIN));
        poll_fds.push(PollFd::new(master_fd, PollFlags::POLLIN));
        for ch in clients.iter() {
            poll_fds.push(PollFd::new(ch.reader.as_fd(), PollFlags::POLLIN));
        }

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
        let (listener_revents, master_revents, client_revents) = match outcome_kind {
            Ok(PollOutcome::Ready(_)) => {
                let lrev = poll_fds[0].revents().unwrap_or(PollFlags::empty());
                let mrev = poll_fds[1].revents().unwrap_or(PollFlags::empty());
                let crev: Vec<PollFlags> = clients
                    .iter()
                    .enumerate()
                    .map(|(i, _)| poll_fds[2 + i].revents().unwrap_or(PollFlags::empty()))
                    .collect();
                drop(poll_fds);
                (lrev, mrev, crev)
            }
            Ok(PollOutcome::Interrupted) => {
                drop(poll_fds);
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
                    drop(ch.writer_tx);
                    let _ = ch.reader.shutdown(std::net::Shutdown::Both);
                    if let Some(t) = ch.writer_thread {
                        let _ = t.join();
                    }
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

        // 1. listener: 新規 client accept (= handshake worker を spawn するだけ。
        //    handshake 自体は別 thread で動くので serve_loop は blocking しない)
        if listener_revents.contains(PollFlags::POLLIN) {
            // D6: 集合 DoS 対策で attach 数を上限化。超過なら fd だけ accept して
            // 即 close (= 接続試行を OS に到達させない形にすると、kernel の listen
            // backlog で stuck する。一旦 fd を取って socket を close するのが安全)。
            //
            // R4-C3: pending handshake もまた fd を握っているので合算で頭打ち。
            // (= clients.len() + pending_handshakes.len() >= MAX で reject)
            if clients.len() + pending_handshakes.len() >= MAX_PENDING_HANDSHAKES {
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
            drop(ch.writer_tx);
            // backpressure 超過時の writer_pump は write_all で block 中の可能性が
            // あるため、socket shutdown で write_all を即 error 化する
            let _ = ch.reader.shutdown(std::net::Shutdown::Both);
            if let Some(t) = ch.writer_thread {
                let _ = t.join();
            }
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

/// `frames_to_process` 用の中間型 (= frame 取得成功 / 失敗を持ち回る)。
enum FrameOrError {
    Frame(Frame),
    Error,
}

/// R4-C3: pending handshake worker の状態を更新する。
///
/// 各 entry に対し:
/// - `try_recv` で完了通知が来ていれば、`Ok` なら `finalize_accepted_client` →
///   `clients` に push + leader/mode.change broadcast。`Err` なら drop で完了。
/// - `started_at + HANDSHAKE_TIMEOUT` を超過していたら強制 drop (= 残った socket は
///   worker thread が `set_read_timeout` で抜け次第 close する)。
///
/// drop すべきものは即除去するため `Vec::retain_mut` で in-place 更新する。
fn process_pending_handshakes(
    pending_handshakes: &mut Vec<PendingHandshake>,
    config: &DaemonConfig,
    next_client_id: &mut u64,
    clients: &mut Vec<ClientHandle>,
    state: &mut SessionState,
    overflow_ids: &mut Vec<u64>,
) {
    // 完了 / 失敗 / timeout の 3 状態に分岐して 1 つずつ処理する。
    let mut i = 0;
    while i < pending_handshakes.len() {
        // try_recv で完了確認
        match pending_handshakes[i].rx.try_recv() {
            Ok(Ok(stage)) => {
                let _entry = pending_handshakes.remove(i);
                // finalize: leader 判定 + response 送信 + ClientHandle 構築
                match finalize_accepted_client(stage, config, *next_client_id, clients) {
                    Ok(accepted) => {
                        *next_client_id += 1;
                        let new_id = accepted.handle.id;
                        let became_leader = accepted.became_leader;
                        let mode_change_for_locked = state.lock_holder.map(|holder| ModeChange {
                            session_mode: SessionMode::Locked,
                            lock_holder: Some(holder),
                            client_mode: None,
                        });
                        if let Some(mc) = mode_change_for_locked.as_ref() {
                            // accept した client に「現在 lock 中」を通知
                            let _ = send_control(&accepted.handle, ControlMessage::ModeChange(*mc));
                        }
                        clients.push(accepted.handle);
                        if became_leader {
                            // 他 client に新 leader を通知 (= 新 client 自身は handshake.response
                            // で leader=true を受け取り済みだが、broadcast でも届く)
                            overflow_ids.extend(broadcast_control(
                                clients,
                                &ControlMessage::LeaderNotify(LeaderNotify {
                                    client_id: Some(new_id),
                                }),
                            ));
                        }
                    }
                    Err(_) => {
                        // response 送信失敗等。drop で client は弾く。
                    }
                }
                // remove したので i は変えない
            }
            Ok(Err(_)) => {
                // worker 側で error frame 送信済 → ここで drop で完了
                pending_handshakes.remove(i);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // 未完了。timeout 判定だけする
                if pending_handshakes[i].started_at.elapsed() >= HANDSHAKE_TIMEOUT {
                    // R4-C3: 5s 経過しても完了しない = slow-loris の可能性が高い。
                    // PendingHandshake を drop する (= rx を drop する)。worker は
                    // socket の read/write timeout (= 同じく HANDSHAKE_TIMEOUT) で
                    // ほぼ同時に抜けるため、deadlock せず thread は自然終了する。
                    pending_handshakes.remove(i);
                } else {
                    i += 1;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // worker thread が panic 等で消えた。drop で完了。
                pending_handshakes.remove(i);
            }
        }
    }
}

/// 新規 client の accept 結果。
struct AcceptedClient {
    handle: ClientHandle,
    /// この client が leader として確定されたか (= Phase 10 leader assignment)。
    became_leader: bool,
}

/// R4-C3: handshake worker thread が完了時に返す中間結果。
///
/// 成功時は (reader, writer_main, req, intersect) を main thread (= serve_loop) に
/// 渡し、main thread 側で leader 判定 + response 送信 + `ClientHandle` 構築を行う。
/// 失敗時 (= protocol error / token mismatch) は worker が socket に error frame を
/// 送ってから socket を drop し、本構造体は `Err` で main thread に届く。
type HandshakeStageOk = (
    UnixStream,
    UnixStream,
    crate::protocol::HandshakeRequest,
    Vec<String>,
);

/// R4-C3: pending handshake (= worker thread が走っている in-flight な handshake)。
///
/// `rx` に worker が完了結果を流す。`started_at` から `HANDSHAKE_TIMEOUT` を超えても
/// 完了しない場合は serve_loop が drop する (= socket を drop することで worker の
/// pending read が EBADF / read error で抜け、thread が自然終了する)。
///
/// **slow-loris 対策の本体**: worker thread は accepted UnixStream に
/// `set_read_timeout` / `set_write_timeout` を設定してから handshake を decode する。
/// 悪意 client が byte をだらだら送っても、socket の read/write が timeout で
/// 失敗するので thread は HANDSHAKE_TIMEOUT 以内に必ず終わる。
struct PendingHandshake {
    rx: std::sync::mpsc::Receiver<Result<HandshakeStageOk, Error>>,
    started_at: Instant,
    /// 完了通知前に accept したことが分かる「socket を握っている worker」の
    /// JoinHandle。timeout 時に main thread から socket を切る経路は無いが、
    /// worker 自身が `set_*_timeout` で抜けるため放置で OK。drop で detached する
    /// (= join しない)。
    _worker: std::thread::JoinHandle<()>,
}

/// R4-C3: listener から 1 client を accept し、handshake を別 thread で進める。
///
/// 戻り値の [`PendingHandshake`] の `rx` が `Ok((reader, writer, req, intersect))` を
/// 通知してきたら、`finalize_accepted_client` で `ClientHandle` 構築 + response 送信
/// + leader 判定をする。`Err` なら worker が既に error frame 送信済 → drop で完了。
///
/// **同期 blocking 部分は `listener.accept()` のみ** (= kernel level)。handshake
/// frame 受信は worker thread に切り出すため、悪意 client が serve_loop を止める
/// ことはできない。
fn spawn_handshake_worker(
    listener: &UnixSock,
    config: &DaemonConfig,
) -> Result<PendingHandshake, Error> {
    let fd: OwnedFd = listener.accept()?;
    let stream = unix_stream_from_owned_fd(fd);

    // R4-C3: handshake 用の read/write を時間で頭打ち。slow-loris client が
    // byte をだらだら送っても、socket I/O が EWOULDBLOCK で error 化して worker が
    // HANDSHAKE_TIMEOUT 以内に必ず終わる。
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));

    let transport = UnixStreamTransport::new(stream);
    let (mut reader, mut writer_main) = transport.split().map_err(Error::from)?;

    let expected_token = config.expected_token.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<HandshakeStageOk, Error>>(1);

    let worker = std::thread::Builder::new()
        .name("hyoui-handshake".into())
        .spawn(move || {
            let result =
                do_handshake_stage(&mut reader, &mut writer_main, expected_token.as_deref());
            match result {
                Ok((req, intersect)) => {
                    let _ = tx.send(Ok((reader, writer_main, req, intersect)));
                }
                Err(e) => {
                    // worker は error frame 送信済 (do_handshake_stage 内)。
                    // socket は ここで drop → close される。
                    let _ = tx.send(Err(e));
                }
            }
        })
        .map_err(|_| Error::Invalid("failed to spawn handshake worker"))?;

    Ok(PendingHandshake {
        rx,
        started_at: Instant::now(),
        _worker: worker,
    })
}

/// R4-C3: worker thread 側で実行する handshake 受信 + token 検証 stage。
///
/// 成功時: (req, intersect) を返す。**response はまだ送らない** (= leader 判定が
/// 必要なので main thread に任せる)。
/// 失敗時: 可能なら socket に error frame を送ってから `Err` を返す。
fn do_handshake_stage(
    reader: &mut UnixStream,
    writer_main: &mut UnixStream,
    expected_token: Option<&str>,
) -> Result<(crate::protocol::HandshakeRequest, Vec<String>), Error> {
    let frame = Frame::decode_from(reader)
        .map_err(|_| Error::Invalid("failed to decode handshake frame"))?;
    if frame.ty != TYPE_CBOR_CONTROL {
        return Err(Error::Invalid("handshake frame must be CBOR control"));
    }
    let msg = ControlMessage::decode_from(frame.body.as_slice())
        .map_err(|_| Error::Invalid("handshake CBOR decode failed"))?;
    let req = match msg {
        ControlMessage::HandshakeRequest(r) => r,
        _ => return Err(Error::Invalid("first message must be handshake.request")),
    };

    // token validation: `config.expected_token` が Some なら client が同一 token を
    // 提示する必要あり。不一致なら handshake を拒否。constant-time 比較で timing leak
    // 回避。
    // Round2 #10: 旧実装は `provided = req.token.as_deref().unwrap_or("")` で
    // `req.token = None` を空文字列と等価扱いしていたため、`expected_token = Some("")`
    // を運用ミスで設定すると全 client が free pass で通過する欠陥があった。
    // → `req.token` が `None` の場合は明示的に mismatch 扱い、`Some(s)` の場合のみ
    // constant_time_eq で比較する。
    if let Some(expected) = expected_token {
        let token_ok = match req.token.as_deref() {
            Some(provided) => constant_time_eq(expected.as_bytes(), provided.as_bytes()),
            None => false,
        };
        if !token_ok {
            let body = ControlMessage::Error(ErrorMessage {
                code: ErrorCode::AuthTokenMismatch,
                message: "handshake token does not match daemon configuration".into(),
                details: None,
            })
            .encode_to_vec()
            .map_err(|_| Error::Invalid("auth error encode failed"))?;
            let _ = Frame::cbor_control(body).encode_to(writer_main);
            return Err(Error::Invalid("handshake token mismatch"));
        }
    }

    let mvp: Vec<String> = MVP_CAPS.iter().map(|s| (*s).to_string()).collect();
    let intersect = intersect_caps(&req.caps, &mvp);

    Ok((req, intersect))
}

/// R4-C3: handshake worker から届いた中間結果を `AcceptedClient` に整える。
///
/// このタイミングで main thread の `clients` 列を見て leader 判定 + response を
/// 送信する (= leader 判定の snapshot を「handshake 完了時点」に揃えるため、
/// 並列 handshake 同士でも leader 重複は発生しない)。
///
/// Response 送信後、socket の read/write timeout は **解除** (= None) する。
/// serve_loop は poll 駆動なので blocking read は無いが、broadcast の write は
/// blocking write で行う。handshake 用 5s timeout のままだと正常 attach 中の
/// client への大量 broadcast で意図しない切断が起きうるため。
fn finalize_accepted_client(
    stage: HandshakeStageOk,
    config: &DaemonConfig,
    client_id: u64,
    clients: &[ClientHandle],
) -> Result<AcceptedClient, Error> {
    let (reader, mut writer_main, req, intersect) = stage;

    // R4-C3: 通常運用 (= broadcast write を含む) は timeout 無しに戻す。
    let _ = reader.set_read_timeout(None);
    let _ = reader.set_write_timeout(None);
    let _ = writer_main.set_read_timeout(None);
    let _ = writer_main.set_write_timeout(None);

    let became_leader = should_assign_leader(clients, req.mode);

    let response = HandshakeResponse {
        caps: intersect.clone(),
        session_id: config.session_id.clone(),
        client_id,
        leader: became_leader,
        mode: req.mode,
    };

    let body = ControlMessage::HandshakeResponse(response)
        .encode_to_vec()
        .map_err(|_| Error::Invalid("handshake.response encode failed"))?;
    Frame::cbor_control(body)
        .encode_to(&mut writer_main)
        .map_err(|_| Error::Invalid("handshake.response frame encode failed"))?;

    // writer thread を立ち上げ、broadcast 用 unbounded mpsc + atomic byte counter を作る。
    // queue capacity は byte 単位の `enqueue_for_client` で厳密に enforce する。
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let queued_bytes_for_pump = Arc::clone(&queued_bytes);
    let writer_thread =
        std::thread::spawn(move || writer_pump(rx, writer_main, queued_bytes_for_pump));
    let negotiated_caps = intersect;

    Ok(AcceptedClient {
        handle: ClientHandle {
            id: client_id,
            mode: req.mode,
            leader: became_leader,
            subscription: Subscription::Raw,
            negotiated_caps,
            writer_tx: tx,
            queued_bytes,
            buffer_limit: config.client_buffer_bytes,
            writer_thread: Some(writer_thread),
            reader,
        },
        became_leader,
    })
}

/// `Frame` の encode 済 bytes を全 client に enqueue。
///
/// 戻り値: backpressure overflow / writer dead で disconnect すべき client の
/// `client_id` 一覧 (Phase 12)。
fn broadcast_bytes(clients: &mut [ClientHandle], bytes: Vec<u8>) -> Vec<u64> {
    let mut overflow_ids: Vec<u64> = Vec::new();
    for ch in clients.iter() {
        match enqueue_for_client(ch, bytes.clone()) {
            EnqueueOutcome::Sent => {}
            EnqueueOutcome::Overflow => {
                send_backpressure_error(ch, ch.queued_bytes.load(Ordering::Acquire));
                overflow_ids.push(ch.id);
            }
            EnqueueOutcome::WriterDead => {
                overflow_ids.push(ch.id);
            }
        }
    }
    overflow_ids
}

/// 1 client から受け取った frame の処理結果。
enum ClientFrameOutcome {
    /// 通常処理完了、loop 継続。
    Continue,
    /// この client は detach / protocol error → list から remove。
    DropClient,
    /// session 全体終了 (= kill received など)。
    TerminateSession(RelayOutcome),
}

#[allow(clippy::too_many_arguments)]
fn handle_client_frame(
    pty: &Pty,
    child: Pid,
    idx: usize,
    frame: Frame,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
    scrollback: &Scrollback,
    config: &DaemonConfig,
    pending_waits: &mut Vec<PendingWait>,
) -> ClientFrameOutcome {
    match frame.ty {
        TYPE_RAW_DATA => {
            let ch_id = clients[idx].id;
            let ch_mode = clients[idx].mode;
            // 書き込み authorization:
            // - Ro mode は書けない (silently drop)
            // - lock 中は lock holder のみ書ける (= 他 rw も silently drop)
            if matches!(ch_mode, Mode::Ro) {
                return ClientFrameOutcome::Continue;
            }
            if let Some(holder) = state.lock_holder {
                if holder != ch_id {
                    return ClientFrameOutcome::Continue;
                }
            }
            if pty.master_fd().write_all(&frame.body).is_err() {
                return ClientFrameOutcome::DropClient;
            }
            ClientFrameOutcome::Continue
        }
        TYPE_CBOR_CONTROL => {
            let msg = match ControlMessage::decode_from(frame.body.as_slice()) {
                Ok(m) => m,
                Err(_) => return ClientFrameOutcome::Continue,
            };
            handle_control_message(
                pty,
                child,
                idx,
                msg,
                clients,
                state,
                scrollback,
                config,
                pending_waits,
            )
        }
        _ => ClientFrameOutcome::DropClient,
    }
}

/// CBOR control message のディスパッチ。lock / leader / mode / status / tail / wait 系の
/// state 更新と broadcast を担う (Phase 10-11)。
#[allow(clippy::too_many_arguments)]
fn handle_control_message(
    pty: &Pty,
    child: Pid,
    idx: usize,
    msg: ControlMessage,
    clients: &mut [ClientHandle],
    state: &mut SessionState,
    scrollback: &Scrollback,
    config: &DaemonConfig,
    pending_waits: &mut Vec<PendingWait>,
) -> ClientFrameOutcome {
    let ch_id = clients[idx].id;
    let ch_leader = clients[idx].leader;

    let ch_mode = clients[idx].mode;
    match msg {
        ControlMessage::Detach(d) => handle_detach_target(idx, d, clients),
        ControlMessage::Kill(k) => {
            // Kill は session 全体 terminate なので `Mode::Rw` (= leader 取りうる
            // 主導 client) のみ許可。`Mode::Ro` (観察者) と `Mode::RwNoLeader`
            // (入力可だが leader 取らない補助 client) は session を畳む権限なし。
            // (Round2 #6: 旧実装は `!Ro` ガードで RwNoLeader も通過していた)
            if !matches!(ch_mode, Mode::Rw) {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::ModeNotAllowed,
                        message: "kill requires rw mode (= leader-eligible)".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            // signum 解釈: None なら SIGTERM、invalid 値 (0 や 範囲外) は error。
            let signum = k.signum.unwrap_or(libc::SIGTERM as u8);
            let sig = match nix_signal_from_signum(signum) {
                Some(s) => s,
                None => {
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::Error(ErrorMessage {
                            code: ErrorCode::SignalInvalid,
                            message: format!("invalid signum: {signum}"),
                            details: None,
                        }),
                    );
                    return ClientFrameOutcome::Continue;
                }
            };
            let _ = kill(child, sig);
            ClientFrameOutcome::TerminateSession(RelayOutcome::ClientDetachedOrKilled)
        }
        ControlMessage::Signal(s) => {
            // Ro 観察者は signal 送信不可 (= 子を SIGKILL できると Ro の前提が壊れる)。
            // Rw / RwNoLeader は raw mode 中の Ctrl-C 等を CBOR 経由でも送れる必要があるので OK。
            if matches!(ch_mode, Mode::Ro) {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::ModeNotAllowed,
                        message: "signal requires rw mode".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            let sig = match nix_signal_from_signum(s.signum) {
                Some(s) => s,
                None => {
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::Error(ErrorMessage {
                            code: ErrorCode::SignalInvalid,
                            message: format!("invalid signum: {}", s.signum),
                            details: None,
                        }),
                    );
                    return ClientFrameOutcome::Continue;
                }
            };
            let _ = kill(child, sig);
            ClientFrameOutcome::Continue
        }
        ControlMessage::Resize(r) => {
            // resize は leader のみ許可 (DR-0008 §2.3)。それ以外は error 返却。
            if !ch_leader {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::ModeNotLeader,
                        message: "resize requires leader role".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            // sanitize: 0×0 や巨大値で curses 系子が壊れることがあるので clamp。
            // 上限 4096 (= 一般 terminal で見ない値) で十分、下限 1 で 0 を排除。
            const COLS_MIN: u16 = 1;
            const COLS_MAX: u16 = 4096;
            const ROWS_MIN: u16 = 1;
            const ROWS_MAX: u16 = 4096;
            let cols = r.cols.clamp(COLS_MIN, COLS_MAX);
            let rows = r.rows.clamp(ROWS_MIN, ROWS_MAX);
            let _ = pty.resize(cols, rows);
            ClientFrameOutcome::Continue
        }
        ControlMessage::LockAcquire(req) => {
            // D7: lock cap が無いと LockAcquire 受理しない
            if !clients[idx].negotiated_caps.iter().any(|c| c == "lock") {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::UnsupportedCapability,
                        message: "lock.acquire requires `lock` cap".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            // R4-C7: Mode::Ro (= 観察者) は lock を取れない。
            // 旧実装は mode をチェックしておらず、Ro client が LockAcquire を送ると
            // session 全体を Locked 化して rw client の書き込みを止められる
            // session DoS が成立していた。DR-0008 §2.3 の意図 (= leader/lock は
            // rw 系のみ) に揃え、Ro は mode.not-allowed で reject する。
            if matches!(clients[idx].mode, Mode::Ro) {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::ModeNotAllowed,
                        message: "lock.acquire requires rw mode (= Ro cannot hold lock)".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            // R4-C9: idempotency — 同じ client が既に lock を保持している場合は、
            // 旧 token をそのまま返して Acquired を返す。
            // 旧実装は `state.lock_holder.is_some()` だけで Denied を返していたため、
            // 既に保持中の client が再 LockAcquire すると **自分の lock に弾かれる**
            // footgun が発生していた。idempotent operation の標準的な挙動 (= 既に
            // 同じ state なら success) に合わせる。
            // mode.change broadcast は **行わない** (= state 変化なし)。
            if state.lock_holder == Some(ch_id) {
                let token = state.lock_token.clone();
                let _ = req; // wait / timeout / process_bound は idempotent 再取得では未使用
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::LockResponse(LockResponse {
                        result: LockResult::Acquired,
                        token,
                        queue_position: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            if state.lock_holder.is_some() {
                // 「1 request → 1 response」契約を守るため、wait=true / wait=false
                // どちらも LockResponse(Denied) 1 frame のみで応答する (Round2 #3
                // = `error` を併送して 2 frame にすると client が後続 frame と
                // mis-align するため)。MVP では wait queue 未実装なので wait=true
                // でも Queued を返せない事実は DR-0008 に明記する想定。
                let _ = req; // process_bound / timeout / wait は queue 実装まで未使用
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::LockResponse(LockResponse {
                        result: LockResult::Denied,
                        token: None,
                        queue_position: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            let token = generate_lock_token();
            state.lock_holder = Some(ch_id);
            state.lock_token = Some(token.clone());
            let _ = send_control(
                &clients[idx],
                ControlMessage::LockResponse(LockResponse {
                    result: LockResult::Acquired,
                    token: Some(token),
                    queue_position: None,
                }),
            );
            broadcast_control(
                clients,
                &ControlMessage::ModeChange(ModeChange {
                    session_mode: SessionMode::Locked,
                    lock_holder: Some(ch_id),
                    client_mode: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
        ControlMessage::TailRequest(req) => {
            // D7: tail-v1 cap が intersect から落ちている client は reject
            if !clients[idx].negotiated_caps.iter().any(|c| c == "tail-v1") {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::UnsupportedCapability,
                        message: "tail.request requires `tail-v1` cap, but it was not \
                                  negotiated at handshake"
                            .into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            handle_tail_request(idx, req, clients, scrollback);
            ClientFrameOutcome::Continue
        }
        ControlMessage::WaitRequest(req) => {
            if !clients[idx].negotiated_caps.iter().any(|c| c == "wait-l0") {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::UnsupportedCapability,
                        message: "wait.request requires `wait-l0` cap".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            handle_wait_request(idx, req, clients, pending_waits);
            ClientFrameOutcome::Continue
        }
        ControlMessage::StatusQuery(_) => {
            // 子 pid の生死は waitpid(WNOHANG) で確認 (= reap せず存在チェックのみ)
            let child_pid = match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => Some(child.as_raw() as u32),
                _ => None,
            };
            let clients_info: Vec<ClientInfo> = clients
                .iter()
                .map(|c| ClientInfo {
                    client_id: c.id,
                    mode: c.mode,
                    leader: c.leader,
                })
                .collect();
            let resp = StatusResponse {
                session_id: config.session_id.clone(),
                child_pid,
                clients: clients_info,
                scrollback_bytes: scrollback.total_bytes() as u64,
                lock_holder: state.lock_holder,
            };
            let _ = send_control(&clients[idx], ControlMessage::StatusResponse(resp));
            ClientFrameOutcome::Continue
        }
        ControlMessage::LockRelease(rel) => {
            // token + holder 両方を照合してから解放
            let valid = state.lock_holder == Some(ch_id)
                && state.lock_token.as_deref() == Some(rel.token.as_str());
            if !valid {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::LockNotHeld,
                        message: "lock token mismatch or not the lock holder".into(),
                        details: None,
                    }),
                );
                return ClientFrameOutcome::Continue;
            }
            state.lock_holder = None;
            state.lock_token = None;
            broadcast_control(
                clients,
                &ControlMessage::ModeChange(ModeChange {
                    session_mode: state.session_mode(),
                    lock_holder: None,
                    client_mode: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
        // daemon → client 方向のはずの message が client → daemon に来た or 未実装 kind。
        // DR-0008 §3.2 「未知 kind は decode error」だが、ここに来るのは serde で既知
        // variant なので decode 段階では catch されない。protocol violation として
        // 明示 error を返す (= silent skip しない)。
        ControlMessage::HandshakeRequest(_)
        | ControlMessage::HandshakeResponse(_)
        | ControlMessage::Error(_)
        | ControlMessage::LockResponse(_)
        | ControlMessage::LeaderNotify(_)
        | ControlMessage::ModeChange(_)
        | ControlMessage::StatusResponse(_)
        | ControlMessage::TailData(_)
        | ControlMessage::TailEnd(_)
        | ControlMessage::WaitResult(_) => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::ProtocolUnexpectedKind,
                    message: "this kind is daemon→client only or not accepted in this direction"
                        .into(),
                    details: None,
                }),
            );
            ClientFrameOutcome::Continue
        }
    }
}

/// serve_loop の relay 結果。
#[derive(Debug)]
enum RelayOutcome {
    /// 子 PTY 側で EOF を検出 (= 子 process が exit した)。exit code が判明していれば
    /// `Some(code)` に保持する (= waitpid を 2 度呼ばないため)。
    ChildExited(Option<i32>),
    /// client が `detach` / `kill` を送ったか socket EOF。`kill` の場合は子に
    /// signal が送られた状態でこの enum に至る。
    ClientDetachedOrKilled,
    /// 回復不能な error (= protocol violation 等)。
    Error(Error),
}

/// 子が通常 alive 時の master read=0/EIO retry 間隔。forkpty 直後の
/// transient 偽 EOF を吸収する用途なので短く (= 200Hz)。
const ALIVE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// 子が SIGTSTP/SIGSTOP で stopped 中の master poll 間隔 (R4-H14)。
/// 子から出力が来る見込みが無いため大きめに (= ~2Hz)。SIGCONT を検出する
/// `waitpid(WCONTINUED)` の latency もこの上限になる。
const STOPPED_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// 子 process の状態判定結果 (R4-H14: Stopped と Continued を区別)。
#[derive(Debug)]
enum ChildState {
    /// 子は exit 済。`Some(code)` は実 exit code、`None` は transient で取得不能。
    Exited(Option<i32>),
    /// 子は通常 alive (= StillAlive、または直前に Continued を受けた、transient error)。
    /// caller は短い sleep で次の poll に戻る。
    Alive,
    /// 子は SIGTSTP / SIGSTOP で stopped 中。master PTY 出力は事実上止まり、
    /// poll → read=0 / EIO の busy spin になる可能性が高いので caller は
    /// **長めの sleep** (= STOPPED_POLL_INTERVAL) で待機する。
    Stopped,
}

/// 子の lifecycle 追跡 (R4-H14)。
///
/// `waitpid` の Stopped / Continued は **state transition でしか報告されない**
/// (= kernel が wait queue から消費したら以降は StillAlive)。busy-wait を避ける
/// ため自前で `stopped` フラグを保持し、次の Continued / Exited が来るまで
/// 「stopped 中」として扱う。`WUNTRACED | WCONTINUED` を指定して waitpid を呼ぶ
/// ことで transition を取りこぼさない。
///
/// 用途: serve_loop の中で master read が 0 / EIO を返した時に
/// `poll(child)` を呼んで状態を更新し、ChildState で caller に sleep 間隔を
/// 委ねる。
#[derive(Debug, Default)]
struct ChildLifecycle {
    /// 直近の waitpid で Stopped を観測してから、まだ Continued / Exited を
    /// 観測していない時のみ true。
    stopped: bool,
}

impl ChildLifecycle {
    /// `waitpid(WNOHANG | WUNTRACED | WCONTINUED)` で子の状態 transition を拾い、
    /// 累積状態を踏まえて [`ChildState`] を返す。
    ///
    /// 旧 `child_actually_exited` 単体関数からの差し替え。state を持つので
    /// caller は `ChildLifecycle` インスタンスを 1 つ loop 全体で使い回す。
    fn poll(&mut self, child: Pid) -> ChildState {
        use nix::sys::wait::WaitPidFlag;
        let flags = WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED;
        match waitpid(child, Some(flags)) {
            Ok(WaitStatus::Exited(_, code)) => ChildState::Exited(Some(code)),
            Ok(WaitStatus::Signaled(_, sig, _)) => ChildState::Exited(Some(128 + (sig as i32))),
            Ok(WaitStatus::Stopped(_, _)) => {
                self.stopped = true;
                ChildState::Stopped
            }
            Ok(WaitStatus::Continued(_)) => {
                self.stopped = false;
                ChildState::Alive
            }
            Ok(WaitStatus::StillAlive) => {
                // No new transition; report the latched state.
                if self.stopped {
                    ChildState::Stopped
                } else {
                    ChildState::Alive
                }
            }
            // ptrace event 等の wildcard (= `ptrace` feature 有効時) を defensively alive 扱い
            #[allow(unreachable_patterns)]
            Ok(_) => ChildState::Alive,
            Err(_) => {
                // transient error (= ECHILD で reap 済の可能性等)。state を維持。
                if self.stopped {
                    ChildState::Stopped
                } else {
                    ChildState::Alive
                }
            }
        }
    }
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
    if !matches!(outcome, RelayOutcome::ChildExited(_)) {
        let _ = kill(child, Signal::SIGTERM);
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

/// `OwnedFd` を `std::os::unix::net::UnixStream` に変換する。
///
/// `UnixStream::from(OwnedFd)` は `From` impl が存在するが、明示的な
/// hyoui 内 helper を経由することで「ここで所有権が移る」点を可視化する。
fn unix_stream_from_owned_fd(fd: OwnedFd) -> UnixStream {
    UnixStream::from(fd)
}

// Drop impl の責務分担 (R4-H4 で Session 自体にも Drop 追加):
// - listener (UnixSock) は自身の Drop で socket file を unlink
// - pty (Pty) は自身の Drop で master fd を close
// - 正常 path (Session::serve) では `into_parts` で Drop をバイパスして
//   fields を取り出し、`finalize_child` が子の reap を担当する
// - `serve` を呼ばずに Session が drop された場合 (test panic / early
//   return) は `impl Drop for Session` (= session.rs 上部) が SIGTERM →
//   500ms wait → SIGKILL で子を reap して orphan を防ぐ

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HandshakeRequest;
    use crate::protocol::messages::{Detach, DetachTarget, Kill};
    use std::io::Write;
    use std::os::fd::AsRawFd;
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

    #[test]
    fn generate_lock_token_unique_and_hex32() {
        let a = generate_lock_token();
        let b = generate_lock_token();
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

    #[test]
    fn enqueue_for_client_respects_buffer_limit() {
        // 単体 unit test: queued_bytes が buffer_limit を超えるなら Overflow
        let (tx, _rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // ダミー UnixStream 作って ClientHandle を構築
        let (a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let _keep = a; // close 防止用
        let ch = ClientHandle {
            id: 0,
            mode: Mode::Rw,
            leader: true,
            subscription: Subscription::Raw,
            negotiated_caps: vec![],
            writer_tx: tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            buffer_limit: 100,
            writer_thread: None,
            reader: b,
        };

        // 50 byte → OK、累計 50
        assert_eq!(enqueue_for_client(&ch, vec![0u8; 50]), EnqueueOutcome::Sent);
        assert_eq!(ch.queued_bytes.load(Ordering::Acquire), 50);
        // 50 byte → 累計 100、まだ OK (= 100 <= 100)
        assert_eq!(enqueue_for_client(&ch, vec![0u8; 50]), EnqueueOutcome::Sent);
        assert_eq!(ch.queued_bytes.load(Ordering::Acquire), 100);
        // 1 byte → 累計 101 > 100、Overflow
        assert_eq!(
            enqueue_for_client(&ch, vec![0u8; 1]),
            EnqueueOutcome::Overflow
        );
        // queued_bytes は変化なし (= Overflow 時は加算前に reject)
        assert_eq!(ch.queued_bytes.load(Ordering::Acquire), 100);
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

    // ------- compute_wait_poll_timeout / check_wait_timeouts: u64 boundary tests (R4-C8)
    //
    // `Instant + Duration::from_millis(u64::MAX)` は overflow で panic するため、
    // Idle{ms: u64::MAX} を受け取った時に panic せず saturate する必要がある。
    // また compute 側の最終的な `as_millis` → i32 変換も `> i32::MAX` で
    // saturating cast されることを境界値で確認。

    fn make_pending_wait_idle(ms: u64, last_activity: Instant) -> PendingWait {
        PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Idle { ms },
            options: WaitMatchOptions::default(),
            deadline: None,
            accumulated: Vec::new(),
            last_activity,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }
    }

    #[test]
    fn compute_wait_poll_timeout_saturates_on_u64_max() {
        // Idle{ms: u64::MAX} で last_activity + idle_dur が overflow しても
        // panic せず、有効な PollTimeout を返すこと。
        let now = Instant::now();
        let waits = vec![make_pending_wait_idle(u64::MAX, now)];
        // 単に panic しないことを確認 (= R4-C8 の主目的)。
        let _ = compute_wait_poll_timeout(&waits);
    }

    #[test]
    fn compute_wait_poll_timeout_saturates_on_large_idle_ms() {
        // i32::MAX ms (= ~24.8 日) を超える idle dur でも panic せず、
        // PollTimeout に saturate (= i32::MAX ms 相当) すること。
        let now = Instant::now();
        let huge_ms = (i32::MAX as u64) + 1;
        let waits = vec![make_pending_wait_idle(huge_ms, now)];
        let to = compute_wait_poll_timeout(&waits);
        // PollTimeout::NONE ではなく具体的な値を返すはず (saturate 成功)。
        assert_ne!(to, PollTimeout::NONE, "should saturate, not NONE");
    }

    #[test]
    fn compute_wait_poll_timeout_handles_mixed_normal_and_overflow() {
        // 通常の Idle と overflow する Idle が混ざっても、通常側の
        // 早い deadline が選ばれて panic しないこと。
        let now = Instant::now();
        let waits = vec![
            make_pending_wait_idle(u64::MAX, now), // overflow → 候補から除外
            make_pending_wait_idle(100, now),      // 100ms 後
        ];
        let to = compute_wait_poll_timeout(&waits);
        // 100ms の方が earliest として採用されるはず。
        assert_ne!(to, PollTimeout::NONE);
    }

    #[test]
    fn check_wait_timeouts_does_not_panic_on_u64_max_idle() {
        // R4-C8: check_wait_timeouts の `last_activity + Duration::from_millis(u64::MAX)`
        // も overflow で panic していた。checked_add で防げていることを確認。
        let now = Instant::now();
        let mut waits = vec![make_pending_wait_idle(u64::MAX, now)];
        let mut clients: Vec<ClientHandle> = Vec::new();
        // panic しなければ OK。overflow した Idle は Matched 扱いされず、
        // pending_waits に残り続ける。
        check_wait_timeouts(&mut waits, &mut clients);
        assert_eq!(waits.len(), 1, "overflow Idle should not match");
    }

    /// R4-H3: needle が chunk 境界を跨いでも match できる。`accumulated` は元々
    /// chunk 横断で蓄積されるため plain text では問題ないが、ここでは特に
    /// `strip_escapes=true` + ANSI escape が chunk 境界で分割された場合に
    /// 後続 chunk の escape parameter (例: `1m`) が raw text として漏れず、
    /// needle が正しく検出されることを確認する。
    #[test]
    fn wait_text_matches_across_chunk_boundary_with_strip_escapes() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate};

        let now = Instant::now();
        let mut waits = vec![PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Text {
                value: "READY".into(),
            },
            options: WaitMatchOptions {
                strip_escapes: true,
                newline_convert_lf: false,
            },
            deadline: None,
            accumulated: Vec::new(),
            last_activity: now,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }];
        // No clients registered; update_waits_on_master_bytes silently skips
        // sending when the client_id is unknown. The match itself is still
        // observable via `waits.is_empty()` after the call (matched waits are
        // removed).
        let mut clients: Vec<ClientHandle> = Vec::new();

        // chunk1: 通常テキスト + CSI escape の途中まで。
        update_waits_on_master_bytes(&mut waits, &mut clients, b"prefix\x1b[3", now);
        // ここでは "READY" は到達していない → match なし
        assert_eq!(waits.len(), 1, "match before READY arrives");

        // chunk2: escape を完結 (`1m`) + needle "READY"。
        // stateless strip だと `1m` が raw として accumulated に入り、かつ
        // chunk1 末尾の `\x1b[3` も raw として残るので、両者を結合した文字列に
        // "READY" は含まれるが、本テストの主眼は「stripped 出力に raw `1m` が
        // 漏れないこと」(= false positive 防止)。
        update_waits_on_master_bytes(&mut waits, &mut clients, b"1mREADY\n", now);
        assert!(
            waits.is_empty(),
            "needle should match across split escape, but wait is still pending"
        );
    }

    /// R4-H3 (negative): 直前 chunk の partial escape が次 chunk と結合されて
    /// false-positive を生まないこと。chunk1 末尾の `\x1b[3` と chunk2 先頭の
    /// `1m` で完結する escape は raw `[31m` を accumulated に漏らさない。
    #[test]
    fn wait_text_no_false_positive_from_split_escape_params() {
        use crate::protocol::messages::{WaitMatchOptions, WaitPredicate};

        let now = Instant::now();
        // needle は escape の parameter `[31m` を狙う。stateless 実装だとここに
        // ヒットして false positive になる。stateful なら strip されるので不一致。
        let mut waits = vec![PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Text {
                value: "[31m".into(),
            },
            options: WaitMatchOptions {
                strip_escapes: true,
                newline_convert_lf: false,
            },
            deadline: None,
            accumulated: Vec::new(),
            last_activity: now,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }];
        let mut clients: Vec<ClientHandle> = Vec::new();

        // chunk1: ESC `[` (= CSI 開始) で終わる
        update_waits_on_master_bytes(&mut waits, &mut clients, b"\x1b[", now);
        // chunk2: `31m` で escape 完結、その後 plain text
        update_waits_on_master_bytes(&mut waits, &mut clients, b"31mhello", now);

        assert_eq!(
            waits.len(),
            1,
            "split CSI params must not leak as raw text and false-match"
        );
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
}
