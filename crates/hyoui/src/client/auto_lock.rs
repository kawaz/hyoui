//! DR-0022: `hyoui input` invocation 全体で使う auto-lock helper。
//!
//! CLI (`hyoui input`) と web gateway (POST /api/sessions/:id/input) の両方が
//! 同じ意味論で使う (= 送信前に acquire、全 spec 送信後 release、外側 token 継承時は
//! skip)。共通経路として `crates/hyoui` に置き、CLI/web が import する。
//!
//! DR-0022 §5「実装方針 (案 X 採用)」の client-side helper 本体。既存 `LockAcquire` /
//! `LockRelease` control message を発行するだけで、daemon 側の変更は不要。

use std::time::{Duration, Instant};

use crate::client::ClientConnection;
use crate::protocol::ControlMessage;
use crate::protocol::messages::{LockAcquire, LockRelease, LockResponse, LockResult};

/// daemon の応答 1 frame を待つ受信タイムアウト (= half-open 接続での永久 hang 防止)。
///
/// daemon は同期 request/response 設計なので健全時はこの上限に届かない。daemon
/// process が消えたが socket FIN が流れていない half-open ケースで、blocking
/// `recv_control` が永久に固まるのを防ぐ。
pub const LOCK_RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// auto-lock 段階の polling 間隔 (Denied / Queued のときの再試行 sleep)。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// [`acquire_auto_lock`] の失敗種別。
///
/// caller は種別を見て HTTP status code (= web) や CLI exit code をマップする。
#[derive(Debug)]
pub enum AutoLockError {
    /// 指定 timeout 内に取得できなかった (= 他 holder が長時間離さない)。web は 409 相当。
    Timeout {
        /// 経過した timeout (= caller が渡した値、message 用)。
        elapsed: Duration,
    },
    /// daemon 応答が来ない (= 消失 / half-open)。web は 500 相当。
    DaemonUnresponsive(String),
    /// I/O / protocol error。web は 500 相当。
    Io(String),
    /// daemon が semantic error を返した (= binary skew 等)。web は 500 相当。
    Protocol(String),
}

impl std::fmt::Display for AutoLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { elapsed } => write!(
                f,
                "他 client が lock 保持中、{} ms 内に取得できませんでした",
                elapsed.as_millis()
            ),
            Self::DaemonUnresponsive(msg) => write!(f, "daemon 応答なし: {msg}"),
            Self::Io(msg) => write!(f, "{msg}"),
            Self::Protocol(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AutoLockError {}

/// `conn` の reader fd を poll して recv 可能になるまで最大 `timeout` 待つ。
///
/// 戻り値:
/// - `Ok(true)`: POLLIN、caller は `recv_control` で frame を読める。
/// - `Ok(false)`: timeout 超過 / POLLHUP / POLLERR (= daemon 無応答 or 消失)。
///   caller は「daemon 消失」として error 終了すべき。
/// - `Err`: poll(2) 自体の失敗。
///
/// EINTR は signal 割り込みなので通算 deadline で re-poll する。
pub fn poll_recv_ready(
    conn: &ClientConnection,
    timeout: Duration,
) -> Result<bool, crate::sys::Error> {
    use crate::sys::poll::{PollFlags, PollOutcome, poll};
    use nix::poll::{PollFd, PollTimeout};
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let fd = conn.reader_fd();
        let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
        let to = PollTimeout::try_from(remaining.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(PollTimeout::NONE);
        match poll(&mut fds, to) {
            Ok(PollOutcome::Ready(_)) => {
                let re = fds[0].revents().unwrap_or(PollFlags::empty());
                if re.contains(PollFlags::POLLIN) {
                    return Ok(true);
                }
                if re.contains(PollFlags::POLLHUP) || re.contains(PollFlags::POLLERR) {
                    return Ok(false);
                }
                // 想定外 revents は re-poll。
            }
            Ok(PollOutcome::Timeout) => return Ok(false),
            Ok(PollOutcome::Interrupted) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// DR-0022: `hyoui input` invocation の auto-lock を acquire する。
///
/// 内側 helper。CLI/web どちらも同じ意味論で使う。外側 token 継承時 (= 呼び出し側で
/// `HYOUI_LOCK_TOKEN` / `--lock-token` を検出済) はそもそも本関数を呼ばない (= 呼び手責任)。
///
/// 戻り値:
/// - `Ok(token)`: 取得成功。caller は invocation 終了時に [`release_auto_lock`] を呼ぶ
/// - `Err(AutoLockError::Timeout {..})`: 他 holder が離さず timeout
/// - `Err(AutoLockError::DaemonUnresponsive(..))`: daemon 消失 / half-open
/// - `Err(AutoLockError::Io(..))` / `Err(AutoLockError::Protocol(..))`: I/O / protocol 失敗
///
/// MVP daemon は wait queue 未実装なので、他 holder が居る場合は `Denied` が返るのを
/// polling で再試行する (= `lock_acquire_command` と同じ戦略)。
pub fn acquire_auto_lock(
    conn: &mut ClientConnection,
    timeout: Duration,
) -> Result<String, AutoLockError> {
    let deadline = Instant::now() + timeout;

    loop {
        let req = ControlMessage::LockAcquire(LockAcquire {
            // wait=true は MVP daemon では queue 化されないが、将来 queue 実装時に
            // 自然と queue に乗せる前向き signal として送る。
            wait: true,
            timeout_abs_ms: Some(timeout.as_millis().min(u64::MAX as u128) as u64),
            timeout_idle_ms: None,
            process_bound: true,
        });
        if let Err(e) = conn.send_control(&req) {
            return Err(AutoLockError::Io(format!("LockAcquire send 失敗: {e}")));
        }

        // response 1 frame を待つ。期待外 broadcast (mode.change 等) は捨てて再受信。
        let response: LockResponse = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AutoLockError::Timeout { elapsed: timeout });
            }
            let poll_to = remaining.min(LOCK_RECV_TIMEOUT);
            match poll_recv_ready(conn, poll_to) {
                Ok(true) => {}
                Ok(false) => {
                    if Instant::now() >= deadline {
                        return Err(AutoLockError::Timeout { elapsed: timeout });
                    }
                    return Err(AutoLockError::DaemonUnresponsive(
                        "recv timeout (daemon 消失の可能性)".into(),
                    ));
                }
                Err(e) => return Err(AutoLockError::Io(format!("poll 失敗: {e}"))),
            }
            match conn.recv_control(None) {
                Ok(ControlMessage::LockResponse(lr)) => break lr,
                Ok(ControlMessage::Error(em)) => {
                    return Err(AutoLockError::Protocol(format!(
                        "daemon error: {} ({:?})",
                        em.message, em.code
                    )));
                }
                Ok(_) => continue,
                Err(e) => return Err(AutoLockError::Io(format!("recv 失敗: {e}"))),
            }
        };

        match response.result {
            LockResult::Acquired => {
                return response
                    .token
                    .ok_or_else(|| AutoLockError::Protocol("Acquired に token 欠落".into()));
            }
            LockResult::Denied => {
                if Instant::now() >= deadline {
                    return Err(AutoLockError::Timeout { elapsed: timeout });
                }
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            LockResult::Queued => {
                if Instant::now() >= deadline {
                    return Err(AutoLockError::Timeout { elapsed: timeout });
                }
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            LockResult::Timeout => {
                return Err(AutoLockError::Protocol("daemon timeout".into()));
            }
            // `LockResult` is `#[non_exhaustive]`; future variants surface here.
            #[allow(unreachable_patterns)]
            _ => {
                return Err(AutoLockError::Protocol(
                    "unknown LockResult variant (binary/library skew)".into(),
                ));
            }
        }
    }
}

/// DR-0022: auto-lock の明示 release。
///
/// 失敗しても daemon の process-bound GC が接続切断時に回収するので、caller は
/// best-effort で扱う (= warning を出して継続)。
pub fn release_auto_lock(conn: &mut ClientConnection, token: &str) -> Result<(), String> {
    let msg = ControlMessage::LockRelease(LockRelease {
        token: token.to_string(),
    });
    conn.send_control(&msg)
        .map_err(|e| format!("LockRelease send 失敗: {e}"))?;
    // ack は不要 (= control message、daemon は best-effort で broadcast)。
    // 万一 daemon が Error response (= token mismatch 等) を返してきても、それは
    // daemon GC で結局 release されるので caller は気にしない。
    Ok(())
}
