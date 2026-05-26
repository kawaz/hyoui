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

use crate::protocol::Mode;
use crate::protocol::messages::SessionMode;

use super::session::ClientHandle;

/// session 全体の状態 (Phase 10)。lock 周りの state machine を保持する。
///
/// 現状の field:
/// - `lock_holder`: lock 保持中の client id (= `None` なら未 lock)
/// - `lock_token`: 発行済 token (= `LockRelease` 検証用)
///
/// Wait queue は MVP では未実装 (`LockAcquire { wait: true, .. }` でも `Denied`
/// を返す)。queue 実装は v0.2.0+ の Phase 12 で検討。
#[derive(Debug, Default)]
pub(super) struct SessionState {
    pub(super) lock_holder: Option<u64>,
    pub(super) lock_token: Option<String>,
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
/// 同 UID の悪意あるプロセスが推測で lock を奪取しうるため、最低限の対策として:
///
/// 1. `/dev/urandom` から **16 byte** を `read_exact` で取り切る (= 全 128 bit
///    分の entropy を確実に得る)
/// 2. もし urandom open / read が失敗した場合は `panic` して daemon を止める
///    (= 弱い token で運用継続するより落ちる方が安全)
pub(super) fn generate_lock_token() -> String {
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
