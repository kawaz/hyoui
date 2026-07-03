//! lock domain reducer と leader cascade ヘルパ (DR-0009 Phase A 後半で
//! `session.rs` から分離、DR-0025 Phase 1a で pure reducer 化)。
//!
//! - [`LockState`]: DR-0025 Phase 1a — lock holder / token / process-bound flag を
//!   保持する pure reducer domain。mutation は [`reduce`] にのみ集約し、field は
//!   private にして module 外からの直接代入を型で禁止する (= single-writer 原則、
//!   DR-0025 §設計哲学)。観測は [`LockState::holder`] / [`LockState::session_mode`]
//!   の read-only accessor 経由でのみ行う。
//! - [`reduce`]: [`LockMsg`] を受けて [`LockState`] を遷移させ [`LockEvent`] を
//!   返す pure function (= socket / PTY / broadcast / record を一切持たない、
//!   100% unit test 可能)。Effect 型 / super-reducer は DR-0025 Phase 1b で導入
//!   予定で、Phase 1a では caller が [`LockEvent`] を見て既存の reply / broadcast /
//!   lifecycle record を実行する。
//! - [`SessionState`]: lock domain (`lock`) + record registry + 子 stopped 観測 /
//!   suspend policy を束ねる session-scope state。record / child 系の domain 分離は
//!   DR-0025 後続 Phase (Child / Serve reducer 化)。
//! - [`generate_lock_token`]: 128-bit hex token を /dev/urandom から生成 (= IO
//!   境界、reducer の外で呼び grant 確定時のみ token を [`LockMsg::Acquire`] に渡す)。
//! - [`should_assign_leader`] / [`elevate_next_leader`]: 新規 rw client / 既存
//!   leader が抜けた時の leader cascade 判定
//!
//! `LockAcquire` / `LockRelease` の handler 本体や `constant_time_eq` (= handshake
//! token 比較) は本 module には含めない (= `control.rs` の `handle_lock_acquire` /
//! `handle_lock_release` が reduce の caller として残る)。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::protocol::Mode;
use crate::protocol::messages::SessionMode;

use super::broadcast::ClientHandle;
use super::config::ChildSuspendPolicy;
use super::record::RecordRegistry;

/// DR-0025 Phase 1a: lock domain の pure reducer state。
///
/// lock は「holder client のみ raw_data を master fd に書ける」session 全体の
/// 排他 (DR-0022)。state の mutation は [`reduce`] にのみ集約する (= single-writer
/// 原則、DR-0025 §設計哲学)。field を private にして module 外からの直接代入を型で
/// 禁止し、観測は [`LockState::holder`] / [`LockState::session_mode`] accessor 経由に
/// 限定する。
///
/// Wait queue は MVP では未実装 (= `LockAcquire { wait: true, .. }` でも caller は
/// `Denied` を返す)。queue 実装は DR-0025 後続 Phase / v0.2.0+ で検討。
#[derive(Debug, Default)]
pub(super) struct LockState {
    /// lock 保持中の client id (= `None` なら未 lock)。
    holder: Option<u64>,
    /// 発行済 token (= `LockRelease` 検証用、holder が `Some` の間だけ `Some`)。
    token: Option<String>,
    /// process-bound GC: holder の client disconnect で auto-release するか。
    ///
    /// DR-0022 の lock 意味論は「holder の接続が切れたら自動解放」で固定のため、
    /// grant 時に常に `true` をセットし [`LockMsg::ClientDisconnected`] で参照する。
    /// 現状 `false` にする経路は release / 未 lock だけ (= lock 保持中は常に
    /// process-bound) だが、将来の非 process-bound lock (= 明示 release のみで
    /// disconnect では解放しない) の余地を型で保持する (DR-0025 §Lock domain の
    /// `process_bound` field)。
    process_bound: bool,
}

impl LockState {
    /// lock 保持中の client id (= `None` なら未 lock)。
    ///
    /// raw_data の holder 認可判定 (DR-0022) や status query の read に使う。
    /// Phase 1a では read は accessor 直読み、DomainViews 化 (= Client reducer が
    /// 認可判定で lock を read) は DR-0025 Phase 1b/2。
    pub(super) fn holder(&self) -> Option<u64> {
        self.holder
    }

    /// session 全体の SessionMode (= mode.change の `session_mode` 用)。
    ///
    /// MVP は「lock 中 = `Locked`、それ以外 = `Rw`」。`Ro` 強制 (= 誰も書けない)
    /// は v0.2.0+ で `--read-only` daemon option 等を導入したときに使う。
    pub(super) fn session_mode(&self) -> SessionMode {
        if self.holder.is_some() {
            SessionMode::Locked
        } else {
            SessionMode::Rw
        }
    }
}

/// [`reduce`] への入力 message。現状の lock mutation 4 箇所 (acquire / release /
/// client drop cascade / `--detach-others` 奪取) の意図を網羅する (DR-0025 §Lock
/// domain)。
#[derive(Debug)]
pub(super) enum LockMsg {
    /// lock 取得要求。
    ///
    /// `token` は grant 成立時に採用する候補。token 生成は `/dev/urandom` read
    /// (= IO) なので reducer の外で行い、**grant 確定時のみ**呼ぶために 2 段で渡す:
    /// caller は最初 `None` で送り、reducer が [`LockEvent::TokenRequired`] を返したら
    /// [`generate_lock_token`] で生成して `Some` で再送する。これにより idempotent /
    /// denied 経路では urandom を触らない (= 既存 `handle_lock_acquire` の
    /// 「grant 経路だけ token 生成」挙動を保存し、urandom 失敗の影響範囲を変えない)。
    Acquire {
        client_id: u64,
        token: Option<String>,
    },
    /// lock 解放要求 (= explicit release)。holder + token 両方一致でのみ解放する。
    Release { client_id: u64, token: String },
    /// holder client の切断による process-bound GC (DR-0022)。holder なら auto-release、
    /// 非 holder / 未 lock なら no-op。client lifecycle 監視自体は Client domain の
    /// 責務で、Lock reducer は「切断された client id」を受け取るだけ (= 弱結合、
    /// DR-0025 §Lock domain の弱結合維持)。
    ClientDisconnected { client_id: u64 },
}

/// [`reduce`] の出力 event。caller はこれを解釈して既存の reply / broadcast /
/// lifecycle record を実行する (= Effect 化は DR-0025 Phase 1b、Phase 1a では
/// caller が直接 IO する)。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LockEvent {
    /// lock 取得成功。`token` を `LockResponse` で client に返す。
    ///
    /// `newly_acquired` は state 遷移の有無を区別する: `true` = free → held の新規
    /// grant (= caller は mode.change broadcast + lock-acquired lifecycle を発火)、
    /// `false` = 同一 holder の idempotent 再取得 (= state 不変なので broadcast /
    /// lifecycle を発火しない、既存挙動 DR-0008 R4-C9)。
    Acquired {
        client_id: u64,
        token: Option<String>,
        newly_acquired: bool,
    },
    /// grant すべき (= free) だが token 未生成。caller は [`generate_lock_token`] で
    /// 生成して [`LockMsg::Acquire`] を `token: Some(..)` で再送する。
    TokenRequired { client_id: u64 },
    /// lock 解放成功 (= explicit release or process-bound GC)。caller は mode.change を
    /// broadcast する。
    Released {
        client_id: u64,
        reason: ReleaseReason,
    },
    /// 要求拒否。acquire は他 client 保持中 (`HeldByOther`)、release は holder / token
    /// 不一致 (`NotHolderOrTokenMismatch`)。
    Denied { client_id: u64, reason: DenyReason },
}

/// [`LockEvent::Released`] の解放理由。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReleaseReason {
    /// `lock.release` による明示解放。
    ExplicitRelease,
    /// holder client 切断による自動解放 (DR-0022 process-bound GC)。
    ProcessBoundGc,
}

/// [`LockEvent::Denied`] の拒否理由。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DenyReason {
    /// acquire: 他 client が lock 保持中 (wait queue は MVP 未実装なので即 Denied)。
    HeldByOther,
    /// release: holder でない or token 不一致。
    NotHolderOrTokenMismatch,
}

/// lock domain の pure reducer (DR-0025 §設計哲学「reducer は pure function」)。
///
/// IO を一切持たず、`&mut LockState` を [`LockMsg`] に従って遷移させ、発生した
/// [`LockEvent`] を返す。socket / PTY / broadcast / record は caller が event を
/// 見て実行する。返す `Vec` は現状高々 1 event (= ClientDisconnected の no-op は空)、
/// `Vec` シグネチャは DR-0025 の super-reducer 合成 (= 複数 event chain) に将来
/// 揃えるため。
pub(super) fn reduce(state: &mut LockState, msg: LockMsg) -> Vec<LockEvent> {
    match msg {
        LockMsg::Acquire { client_id, token } => {
            // idempotency (DR-0008 R4-C9): 既に同一 client が holder なら state を
            // 変えず、発行済 token をそのまま返す (= newly_acquired: false、broadcast /
            // lifecycle を伴わない)。同 state への要求は success を返す標準 idempotent
            // 挙動 (= 再取得が自分の lock に弾かれる footgun を作らない)。渡された
            // token 候補は使わない。
            if state.holder == Some(client_id) {
                return vec![LockEvent::Acquired {
                    client_id,
                    token: state.token.clone(),
                    newly_acquired: false,
                }];
            }
            // 他 client が保持中: wait queue は MVP 未実装なので即 Denied
            // (= wait=true でも Queued を返せない、DR-0008)。state 不変。
            if state.holder.is_some() {
                return vec![LockEvent::Denied {
                    client_id,
                    reason: DenyReason::HeldByOther,
                }];
            }
            // free: grant 可能。token 生成 (= urandom IO) は reducer の外なので、
            // caller が未生成 (None) なら TokenRequired を返して生成を促す。grant
            // 経路でだけ token を要求することで、既存の「grant 時のみ
            // generate_lock_token を呼ぶ」挙動 (= urandom 失敗の影響範囲) を保存する。
            match token {
                Some(t) => {
                    state.holder = Some(client_id);
                    state.token = Some(t.clone());
                    state.process_bound = true;
                    vec![LockEvent::Acquired {
                        client_id,
                        token: Some(t),
                        newly_acquired: true,
                    }]
                }
                None => vec![LockEvent::TokenRequired { client_id }],
            }
        }
        LockMsg::Release { client_id, token } => {
            // token + holder 両方一致でのみ解放する (= 他 client / 古い token による
            // 解放を防ぐ、DR-0006 §7)。
            let valid =
                state.holder == Some(client_id) && state.token.as_deref() == Some(token.as_str());
            if !valid {
                return vec![LockEvent::Denied {
                    client_id,
                    reason: DenyReason::NotHolderOrTokenMismatch,
                }];
            }
            state.holder = None;
            state.token = None;
            state.process_bound = false;
            vec![LockEvent::Released {
                client_id,
                reason: ReleaseReason::ExplicitRelease,
            }]
        }
        LockMsg::ClientDisconnected { client_id } => {
            // process-bound GC (DR-0022): holder の接続が切れたら自動解放する。
            // 非 holder の切断 / 未 lock は lock に影響しない (= no-op、event なし)。
            if state.process_bound && state.holder == Some(client_id) {
                state.holder = None;
                state.token = None;
                state.process_bound = false;
                vec![LockEvent::Released {
                    client_id,
                    reason: ReleaseReason::ProcessBoundGc,
                }]
            } else {
                Vec::new()
            }
        }
    }
}

/// session 全体の状態 (Phase 10)。lock domain + record registry + 子観測 state。
///
/// DR-0025 Phase 1a で lock holder / token を [`LockState`] に切り出し、mutation を
/// [`reduce`] に集約した。record_registry / child_stopped / child_suspend_policy の
/// domain 分離は DR-0025 後続 Phase (Child / Serve reducer 化) で行う。
///
/// 現状の field:
/// - `lock`: lock domain state (= holder / token / process-bound、mutation は
///   [`reduce`] 経由のみ)
/// - `record_registry`: DR-0016 record sink 集合 (= Phase 4 で hot path 配線)
#[derive(Debug, Default)]
pub(super) struct SessionState {
    /// DR-0025 Phase 1a: lock domain state。mutation は [`reduce`] 経由のみ。
    pub(super) lock: LockState,
    /// DR-0016 §8 — session-scope の record sink 集合。`Arc` で複数 hook 点から
    /// clone して持つ (= push 時に lock 不要、registry 内部で同期)。
    pub(super) record_registry: Arc<RecordRegistry>,
    /// DR-0017 §柱2: 子が現在 stopped (= SIGTSTP/SIGSTOP で停止中) と観測されて
    /// いるか。`notify_child_stopped` で `true`、`record_child_continued` で
    /// `false` にする。`status` query (= `&SessionState` 経由) から読むため
    /// 内部可変 (`AtomicBool`)。auto-resume 廃止後は stopped のまま残り得るため、
    /// `status` / `list` での可観測性に使う。
    child_stopped: AtomicBool,
    /// DR-0019 Update: 子 STOPPED 時の daemon 挙動 (= `on-child-suspend` policy)。
    ///
    /// 旧設計では `DaemonConfig::on_child_suspend` 固定値だったが、`hyoui set` で
    /// **runtime 変更可能**にするため SessionState の内部可変 state に移した。
    /// `notify_child_stopped` はここから読む。`u8` で `ChildSuspendPolicy` を
    /// encode する (= 0=Notify / 1=AutoResume、`policy_to_u8` / `policy_from_u8`)。
    ///
    /// 初期値は `Default` で `Notify` (= u8=0)。daemon 起動時に config の値で
    /// `init_child_suspend_policy` で上書きする。
    child_suspend_policy: AtomicU8,
}

/// `ChildSuspendPolicy` を `AtomicU8` 格納用の u8 に変換する。
fn policy_to_u8(p: ChildSuspendPolicy) -> u8 {
    match p {
        ChildSuspendPolicy::Notify => 0,
        ChildSuspendPolicy::AutoResume => 1,
    }
}

/// `AtomicU8` の値から `ChildSuspendPolicy` を復元する (= 未知値は安全側 Notify)。
fn policy_from_u8(v: u8) -> ChildSuspendPolicy {
    match v {
        1 => ChildSuspendPolicy::AutoResume,
        _ => ChildSuspendPolicy::Notify,
    }
}

impl SessionState {
    /// session 全体の SessionMode (= mode.change の `session_mode` 用)。
    ///
    /// lock domain に委譲する (DR-0025 Phase 1a)。
    pub(super) fn session_mode(&self) -> SessionMode {
        self.lock.session_mode()
    }

    /// DR-0017 §柱2: 子の stopped 観測状態を更新する。
    pub(super) fn set_child_stopped(&self, stopped: bool) {
        self.child_stopped.store(stopped, Ordering::Relaxed);
    }

    /// DR-0017 §柱2: 子が現在 stopped と観測されているか (= `status`/`list` 用)。
    pub(super) fn child_stopped(&self) -> bool {
        self.child_stopped.load(Ordering::Relaxed)
    }

    /// DR-0019 Update: 起動時に config の `on_child_suspend` で初期化する。
    pub(super) fn init_child_suspend_policy(&self, policy: ChildSuspendPolicy) {
        self.child_suspend_policy
            .store(policy_to_u8(policy), Ordering::Relaxed);
    }

    /// DR-0019 Update: `set.request` で runtime 変更する。
    pub(super) fn set_child_suspend_policy(&self, policy: ChildSuspendPolicy) {
        self.child_suspend_policy
            .store(policy_to_u8(policy), Ordering::Relaxed);
    }

    /// DR-0019 Update: 現在の `on-child-suspend` policy を読む
    /// (= `notify_child_stopped` / `status` / `list` 用)。
    pub(super) fn child_suspend_policy(&self) -> ChildSuspendPolicy {
        policy_from_u8(self.child_suspend_policy.load(Ordering::Relaxed))
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

    // ---- DR-0025 Phase 1a: lock reducer 単独 test (PTY / socket / thread 不要) ----
    //
    // これらは `session.rs` の serve_lock_* integration test (= 実 PTY + 実 socket +
    // 実 thread 経由) が検証する lock 意味論を、message 列 → LockEvent 列の pure な
    // 入出力として直接検証する。対応関係は各 test の doc に明記する (gate 1a-2)。

    /// grant フロー: free 状態への acquire は「token 生成が必要 (TokenRequired)」を
    /// 返し、token を与えて再 acquire すると holder / token が確定して
    /// `Acquired { newly_acquired: true }` を返す。
    ///
    /// なぜ 2-phase か: token 生成は urandom IO で reducer の外に置くため、reducer は
    /// 「grant すべきだが token が無い」を TokenRequired で表明し、caller が生成後に
    /// 再投入する ([`LockMsg::Acquire`] の doc)。free → held の新規取得は
    /// `newly_acquired: true` で、caller が mode.change broadcast を発火する契機になる。
    /// integration 対応: serve_lock_acquire_grants_and_broadcasts_mode_change
    /// (= token を返し mode.change(Locked, holder) を broadcast)。
    #[test]
    fn reduce_acquire_on_free_requires_token_then_grants() {
        let mut s = LockState::default();
        // Phase 1 (peek): token 未生成なので生成を促す。state は不変 (= まだ free)。
        let peek = reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: None,
            },
        );
        assert_eq!(peek, vec![LockEvent::TokenRequired { client_id: 7 }]);
        assert_eq!(s.holder(), None, "peek では holder を確定しない");
        assert_eq!(s.session_mode(), SessionMode::Rw);
        // Phase 2 (grant): 生成済 token を与えると holder / token / process_bound が確定。
        let grant = reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("cafebabe".into()),
            },
        );
        assert_eq!(
            grant,
            vec![LockEvent::Acquired {
                client_id: 7,
                token: Some("cafebabe".into()),
                newly_acquired: true,
            }]
        );
        assert_eq!(s.holder(), Some(7));
        assert_eq!(
            s.session_mode(),
            SessionMode::Locked,
            "grant 後は session_mode が Locked"
        );
    }

    /// idempotency (DR-0008 R4-C9): 既に holder の client が再 acquire しても Denied に
    /// せず、発行済 token をそのまま返す。`newly_acquired: false` で state 変化なしを
    /// 示す (= caller は mode.change / lifecycle を発火しない)。
    ///
    /// なぜこの期待値か: 旧実装は `holder.is_some()` だけで Denied を返し、自分の lock に
    /// 自分が弾かれる footgun があった。standard な idempotent 挙動 (= 同 state なら
    /// success) に揃える。peek の token: None を渡しても、holder 一致なら token 生成は
    /// 不要 (= 既存 token を返す)。
    /// integration 対応: serve_lock_acquire_is_idempotent_for_self_holder。
    #[test]
    fn reduce_acquire_idempotent_for_same_holder() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("tok-1".into()),
            },
        );
        // 同一 holder が token:None で再 acquire → 既存 token を newly_acquired:false で返す
        let again = reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: None,
            },
        );
        assert_eq!(
            again,
            vec![LockEvent::Acquired {
                client_id: 7,
                token: Some("tok-1".into()),
                newly_acquired: false,
            }]
        );
        assert_eq!(
            s.holder(),
            Some(7),
            "idempotent 再取得で holder は変わらない"
        );
    }

    /// 他 client が保持中の acquire は `Denied { HeldByOther }` を返し state を変えない
    /// (= wait queue は MVP 未実装なので wait 相当でも即拒否、DR-0008)。
    ///
    /// なぜこの期待値か: lock は session 全体の排他なので、holder が居る間は他 client の
    /// 取得を拒否する。token: None で peek しても、holder が別人なら token 生成に進まず
    /// (= urandom を触らず) 即 Denied になるのが正 (= 既存の「grant 経路だけ生成」保存)。
    /// integration 対応: serve_lock_acquire_while_locked_returns_denied。
    #[test]
    fn reduce_acquire_denied_when_held_by_other() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 1,
                token: Some("held".into()),
            },
        );
        let denied = reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 2,
                token: None,
            },
        );
        assert_eq!(
            denied,
            vec![LockEvent::Denied {
                client_id: 2,
                reason: DenyReason::HeldByOther,
            }]
        );
        assert_eq!(s.holder(), Some(1), "denied で holder は 1 のまま");
    }

    /// 正当な release (= holder かつ token 一致) は state をクリアし
    /// `Released { ExplicitRelease }` を返す。session_mode は Rw に戻る。
    ///
    /// なぜこの期待値か: release は holder + token 両方一致を要求する (DR-0006 §7)。
    /// 成功すると holder / token / process_bound が全て初期状態に戻り、caller は
    /// mode.change(Rw) を broadcast する。
    /// integration 対応: serve_lock_release_clears_state。
    #[test]
    fn reduce_release_clears_state_on_valid_token() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("secret".into()),
            },
        );
        let released = reduce(
            &mut s,
            LockMsg::Release {
                client_id: 7,
                token: "secret".into(),
            },
        );
        assert_eq!(
            released,
            vec![LockEvent::Released {
                client_id: 7,
                reason: ReleaseReason::ExplicitRelease,
            }]
        );
        assert_eq!(s.holder(), None, "release で holder がクリアされる");
        assert_eq!(s.session_mode(), SessionMode::Rw);
    }

    /// token 不一致の release は `Denied { NotHolderOrTokenMismatch }` を返し state を
    /// 変えない (= 古い token / 推測 token による解放を防ぐ、DR-0006 §7)。
    ///
    /// なぜこの期待値か: holder 本人でも token がずれていれば解放しない (= token は
    /// 解放認可の証憑)。caller はこの Denied を `Error(LockNotHeld)` に写像する。
    /// integration 対応: serve_lock_release_clears_state の対極 (= token 一致成功のみ
    /// を検証しており、不一致 reject は reducer test で補完)。
    #[test]
    fn reduce_release_denied_on_token_mismatch() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("right".into()),
            },
        );
        let denied = reduce(
            &mut s,
            LockMsg::Release {
                client_id: 7,
                token: "wrong".into(),
            },
        );
        assert_eq!(
            denied,
            vec![LockEvent::Denied {
                client_id: 7,
                reason: DenyReason::NotHolderOrTokenMismatch,
            }]
        );
        assert_eq!(s.holder(), Some(7), "token 不一致では解放されない");
    }

    /// 非 holder の client が release を試みても `Denied` で拒否し state を変えない。
    ///
    /// なぜこの期待値か: holder でない client の release は holder 照合で落ちる
    /// (= token が偶然一致しても holder が違えば拒否)。
    #[test]
    fn reduce_release_denied_when_not_holder() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 1,
                token: Some("held".into()),
            },
        );
        let denied = reduce(
            &mut s,
            LockMsg::Release {
                client_id: 2,
                token: "held".into(),
            },
        );
        assert_eq!(
            denied,
            vec![LockEvent::Denied {
                client_id: 2,
                reason: DenyReason::NotHolderOrTokenMismatch,
            }]
        );
        assert_eq!(s.holder(), Some(1));
    }

    /// holder client の切断は process-bound GC で auto-release し
    /// `Released { ProcessBoundGc }` を返す (DR-0022)。
    ///
    /// なぜこの期待値か: lock は holder の接続に束縛される (= 接続が切れたら残った
    /// lock を回収して他 client が取れる状態に戻す)。caller (= session.rs client drop
    /// cascade / accept.rs `--detach-others`) はこの Released を見て mode.change を
    /// broadcast する。integration 対応: session.rs serve_loop drop cascade /
    /// accept.rs detach-others 経路 (= 明示 integration test は無く、reducer test で
    /// GC 意味論を固定する)。
    #[test]
    fn reduce_client_disconnected_releases_holder() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 7,
                token: Some("tok".into()),
            },
        );
        let gc = reduce(&mut s, LockMsg::ClientDisconnected { client_id: 7 });
        assert_eq!(
            gc,
            vec![LockEvent::Released {
                client_id: 7,
                reason: ReleaseReason::ProcessBoundGc,
            }]
        );
        assert_eq!(s.holder(), None, "holder 切断で lock が回収される");
    }

    /// 非 holder の client 切断は lock に影響せず event を出さない (= no-op)。
    ///
    /// なぜこの期待値か: lock を持っていない client が抜けても、holder の lock は
    /// そのまま維持されるべき。caller は空 event を見て mode.change を発火しない。
    #[test]
    fn reduce_client_disconnected_noop_for_non_holder() {
        let mut s = LockState::default();
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 1,
                token: Some("held".into()),
            },
        );
        let noop = reduce(&mut s, LockMsg::ClientDisconnected { client_id: 2 });
        assert!(noop.is_empty(), "非 holder の切断は event を出さない");
        assert_eq!(s.holder(), Some(1), "holder は維持される");
    }

    /// 未 lock 状態の client 切断も no-op (= holder が None なら回収対象が無い)。
    #[test]
    fn reduce_client_disconnected_noop_when_free() {
        let mut s = LockState::default();
        let noop = reduce(&mut s, LockMsg::ClientDisconnected { client_id: 9 });
        assert!(noop.is_empty());
        assert_eq!(s.holder(), None);
    }

    /// `LockState::session_mode` は holder の有無で `Rw` / `Locked` を導出する。
    ///
    /// なぜこの期待値か: session_mode は「lock 中か否か」を client に伝える派生値
    /// (MVP は Ro 強制を持たない)。holder=None → Rw、holder=Some → Locked。
    /// integration 対応: session.rs session_mode_reflects_lock_holder (= 同じ導出を
    /// SessionState 委譲経由で検証)。
    #[test]
    fn session_mode_reflects_holder() {
        let mut s = LockState::default();
        assert_eq!(s.session_mode(), SessionMode::Rw, "未 lock は Rw");
        reduce(
            &mut s,
            LockMsg::Acquire {
                client_id: 3,
                token: Some("t".into()),
            },
        );
        assert_eq!(s.session_mode(), SessionMode::Locked, "lock 中は Locked");
    }
}
