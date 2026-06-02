//! lock 状態と leader cascade ヘルパ (DR-0009 Phase A 後半で `session.rs` から分離)。
//!
//! - [`SessionState`]: lock holder / token を保持する state machine。`session_mode`
//!   は `mode.change` broadcast 用
//! - [`generate_lock_token`]: 128-bit hex token を /dev/urandom から生成
//! - [`should_assign_leader`] / [`elevate_next_leader`]: 新規 rw client / 既存
//!   leader が抜けた時の leader cascade 判定
//!
//! `LockAcquire` / `LockRelease` の handler 本体や `constant_time_eq` (= handshake
//! token 比較) は本 module には含めない (= `session.rs` の `handle_control_message`
//! に残る、Phase B で `control.rs` へ移動予定)。

use std::sync::Arc;

use crate::protocol::Mode;
use crate::protocol::messages::SessionMode;

use super::broadcast::ClientHandle;
use super::record::RecordRegistry;

/// session 全体の状態 (Phase 10)。lock 周りの state machine を保持する。
///
/// 現状の field:
/// - `lock_holder`: lock 保持中の client id (= `None` なら未 lock)
/// - `lock_token`: 発行済 token (= `LockRelease` 検証用)
/// - `record_registry`: DR-0016 record sink 集合 (= Phase 4 で hot path 配線)
///
/// Wait queue は MVP では未実装 (`LockAcquire { wait: true, .. }` でも `Denied`
/// を返す)。queue 実装は v0.2.0+ の Phase 12 で検討。
#[derive(Debug, Default)]
pub(super) struct SessionState {
    pub(super) lock_holder: Option<u64>,
    pub(super) lock_token: Option<String>,
    /// DR-0016 §8 — session-scope の record sink 集合。`Arc` で複数 hook 点から
    /// clone して持つ (= push 時に lock 不要、registry 内部で同期)。
    pub(super) record_registry: Arc<RecordRegistry>,
}

impl SessionState {
    /// session 全体の SessionMode (= mode.change の `session_mode` 用)。
    ///
    /// MVP は「lock 中 = `Locked`、それ以外 = `Rw`」。`Ro` 強制 (= 誰も書けない)
    /// は v0.2.0+ で `--read-only` daemon option 等を導入したときに使う。
    pub(super) fn session_mode(&self) -> SessionMode {
        if self.lock_holder.is_some() {
            SessionMode::Locked
        } else {
            SessionMode::Rw
        }
    }
}

/// 128-bit (32 hex char) の lock token を生成する。
///
/// 同 UID 信頼領域なので CSPRNG 強度は厳格には不要だが、token が予測可能だと
/// 同 UID の悪意あるプロセスが推測で lock を奪取しうるため、最低限の対策として
/// `/dev/urandom` から **16 byte** を `read_exact` で取り切る (= 全 128 bit
/// 分の entropy を確実に得る)。
///
/// # Errors (R5-H11)
///
/// urandom の open / read が失敗した場合は [`std::io::Error`] を返す。
/// 旧実装は `.expect()` で panic していたが、`panic = "abort"` 設定下では
/// daemon process 全体が abort して **全 client が巻き添えに切断される**。
/// 攻撃者が EMFILE / ENFILE 等を作り出せれば session DoS が成立しうるため、
/// caller (= [`super::control::handle_lock_acquire`]) は本 error を
/// `LockResponse(Denied)` + `ErrorCode::InternalError` notify で client に
/// 返し、session 自体は継続する。
pub(super) fn generate_lock_token() -> std::io::Result<String> {
    let f = std::fs::File::open("/dev/urandom")?;
    generate_lock_token_from(f)
}

/// `generate_lock_token` の本体 (= reader 抽象化版)。
///
/// 任意の [`std::io::Read`] から 16 byte を取って 32-char hex string を返す。
/// 本 module の `generate_lock_token` は `/dev/urandom` を渡す薄い wrapper で、
/// テストは failing reader を渡して `read_exact` 失敗 path を検証する。
pub(super) fn generate_lock_token_from<R: std::io::Read>(mut r: R) -> std::io::Result<String> {
    let mut buf = [0u8; 16];
    r.read_exact(&mut buf)?;
    let mut out = String::with_capacity(32);
    for b in &buf {
        use std::fmt::Write;
        // String への write! は infallible だが文法上 Result を返す。
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// 新規 rw client が leader 取得すべきかを判定する (= 既存に leader が居ないか)。
///
/// `RwNoLeader` mode の client は leader 候補から除外 (= 明示的に leader を
/// 取らない意思表示)。
pub(super) fn should_assign_leader(clients: &[ClientHandle], new_mode: Mode) -> bool {
    matches!(new_mode, Mode::Rw) && !clients.iter().any(|c| c.leader)
}

/// leader が居ない状態 (= leader cascade 候補) のときに、次の `Mode::Rw` client を
/// leader に昇格させる。成功すれば新 leader の id を返す。
pub(super) fn elevate_next_leader(clients: &mut [ClientHandle]) -> Option<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// R5-H11: `generate_lock_token_from` の正常 path (= 16 byte 供給) では
    /// 32-char hex string を返す。
    #[test]
    fn generate_lock_token_from_returns_hex32_on_success() {
        let bytes: &[u8] = &[
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ];
        let tok = generate_lock_token_from(bytes).expect("must succeed with 16 bytes");
        assert_eq!(tok, "0123456789abcdeffedcba9876543210");
    }

    /// R5-H11: reader が 16 byte を満たさず短く返してきたら `read_exact` が
    /// `UnexpectedEof` を返し、`generate_lock_token_from` は Err を返す
    /// (= panic しない)。旧実装は `.expect()` で daemon を abort させていた。
    #[test]
    fn generate_lock_token_returns_error_on_io_failure() {
        // 8 byte (= 16 未満) しか供給しない reader → `read_exact` が UnexpectedEof
        let short: &[u8] = &[0u8; 8];
        let err = generate_lock_token_from(short).expect_err("must error on short read");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::UnexpectedEof,
            "expected UnexpectedEof, got {:?}",
            err.kind()
        );
    }

    /// R5-H11: reader が即 I/O error を返してきたら、その error が
    /// そのまま伝播する (= panic しない)。
    #[test]
    fn generate_lock_token_propagates_read_error() {
        struct FailingReader;
        impl std::io::Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "simulated EACCES",
                ))
            }
        }
        let err = generate_lock_token_from(FailingReader).expect_err("must propagate error");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
