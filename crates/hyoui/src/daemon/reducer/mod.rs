//! daemon super-reducer と全 domain の型骨格 (DR-0025 Phase 1b 前半)。
//!
//! DR-0025 §設計哲学 が定める「全 IO を message に統一 / reducer は pure function /
//! single-writer / layered reducer / cross-domain event 伝播」を Rust の型として
//! 具現化する層。本 module が持つのは:
//!
//! - [`Domain`] / [`EffectId`] / [`Effect`] / [`EffectKind`] — DR-0025 §Effect layer
//!   の effect 型と、EffectResult routing の key を兼ねる複合 id
//! - [`EffectOutcome`] / [`EffectErrorKind`] / [`RetryAdvice`] — DR-0025 §Effect 失敗の
//!   feedback
//! - [`DaemonMsg`] — 6 domain event + [`DaemonMsg::EffectResult`] を束ねる super-reducer
//!   入力 (DR-0025 §cross-domain event 伝播 protocol)
//! - [`DaemonState`] / [`DomainViews`] — split borrow 前提の domain state 集約と
//!   read-only view (DR-0025 §borrow 戦略 / §read-only view)
//! - [`handle`] — cross-domain FIFO queue + 深度上限 + EffectResult 直接 routing を持つ
//!   super-reducer (DR-0025 §連鎖停止条件)
//!
//! 各 domain の中身 (parser / state machine / registry) は per-domain sub-module
//! ([`tty`] / [`child`] / [`serve`] / [`client`] / [`screen`]) に最小 stub として置く。
//! Lock domain は Phase 1a で pure reducer 化済のため [`super::lock`] の
//! [`LockState`] / [`LockMsg`] を参照する。

// reducer 骨格の大半は各 domain reducer に振る舞い (= Effect 発行) が入るまで未使用の
// stub。translate 併走 (DR-0025 Phase 1b 後半) で実経路に配線されるのは handle /
// DaemonState / translate 経路と一部 domain event variant (Tty::Layer1Bytes /
// Child::{SigchldReceived,Reaped} / Client::{FrameReceived,Detached}) のみ。Effect 発行系
// (EffectKind / EffectId / 失敗 feedback) と placeholder 型、未 translate の domain event /
// EffectResult 経路は reducer が Effect を出し始める後続 Phase まで未使用のため、module
// 全体で allow する (= 個別 item 化は該当 25 箇所超で可読性を損なうため不採用)。
#![allow(dead_code)]

use std::collections::VecDeque;

use nix::sys::signal::Signal;
use nix::unistd::Pid;

use crate::protocol::messages::ControlMessage;

use super::lock::{LockMsg, LockState};

mod child;
mod client;
mod screen;
mod serve;
mod tty;
// serve_loop (= `daemon::session`) から呼ぶため crate::daemon 全体に公開する。
// 他 domain module は reducer 内 private のまま (= translate 経由でのみ配線)。
pub(in crate::daemon) mod execute;
pub(in crate::daemon) mod translate;

/// cross-domain queue の深度上限 (DR-0025 §連鎖停止条件)。
///
/// 1 transaction の処理中、cross-domain queue の残長がこれを超えたら「設計違反
/// (= 循環 dispatch / 想定外の連鎖爆発)」とみなす。超過時の挙動は build 種別で分岐する
/// ([`drive`] 参照)。
const MAX_CROSS_DOMAIN_DEPTH: usize = 64;

/// effect 発行元 domain。EffectResult routing の key を兼ねる (DR-0025 §Effect layer)。
///
/// [`EffectId`] の第 1 要素として effect の出自を表し、[`DaemonMsg::EffectResult`] を
/// **発行元 reducer へ直接 routing** する際の宛先判定に使う ([`route`])。super-reducer の
/// domain match dispatch の分岐キーでもある。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::daemon) enum Domain {
    Tty,
    Child,
    Serve,
    Client,
    Screen,
    Lock,
}

/// 発行元 domain + domain 内連番の複合 id (DR-0025 §Effect layer)。
///
/// - 第 1 要素 [`Domain`] は effect の発行元。[`DaemonMsg::EffectResult`] の routing key を
///   兼ねる (= super-reducer は `EffectResult` を第 1 要素が指す発行元 reducer へ直接 dispatch。
///   cross-domain queue は経由しない)。
/// - 第 2 要素 `u64` は domain 内連番。各 domain state が保持する counter から**決定論的に
///   採番**する (= `SystemTime` / `rand` に依存しない、event sourcing replay と整合)。
///
/// 採番 counter の実体 (= 各 domain state の `next_effect_seq`) は、各 reducer が
/// Effect を実際に発行し始める Phase で当該 domain state に追加する (置き場所の規定は
/// DR-0025 §Lock domain の `LockState` 例)。Phase 1b 前半時点ではどの state も未保持。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::daemon) struct EffectId(pub(in crate::daemon) Domain, pub(in crate::daemon) u64);

impl EffectId {
    /// 発行元 domain (= EffectResult routing の宛先)。
    pub(in crate::daemon) fn domain(&self) -> Domain {
        self.0
    }

    /// domain 内連番。
    pub(in crate::daemon) fn seq(&self) -> u64 {
        self.1
    }
}

/// reducer 出力の実 IO 命令 (DR-0025 §Effect layer)。
///
/// reducer は pure なので実 IO を持たず、代わりに `Effect` を返す。実 IO への変換は
/// Phase 1b 後半で導入する execute layer が担う。`id` で発行 effect と
/// [`DaemonMsg::EffectResult`] を相関付ける。
#[derive(Debug)]
pub(in crate::daemon) struct Effect {
    pub(in crate::daemon) id: EffectId,
    pub(in crate::daemon) kind: EffectKind,
}

/// effect の種別 (DR-0025 §Effect 型)。
///
/// client 送信系 (`ClientReply` / `ClientBroadcast`) の payload は pure な
/// [`ControlMessage`] を直接持つ (= reducer が state から組み立て、CBOR encode + socket
/// write は execute layer が担う)。残る重い型 (record entry / pty spawn setup) は
/// [`RecordEntry`] / [`PtySetup`] の placeholder で表現し、既存コードへの結合を避ける
/// (= 各型の doc に置換予定 Phase を記す)。OS プリミティブ ([`Pid`] / [`Signal`]) と
/// std 型はそのまま用いる。
#[derive(Debug)]
pub(in crate::daemon) enum EffectKind {
    /// PTY master fd への write。
    TtyWrite { bytes: Vec<u8> },

    /// PTY への `TIOCSWINSZ` ioctl (= resize.request 由来、Tty reducer が発行)。
    TtyResize { cols: u16, rows: u16 },

    /// 子プロセスへの signal 送出。
    Kill { pid: Pid, signal: Signal },

    /// 特定 client socket への reply 送信。reducer が組んだ pure な [`ControlMessage`] を
    /// 持ち、encode + socket write は execute layer ([`execute::execute`]) が行う。
    ClientReply {
        client_id: ClientId,
        message: ControlMessage,
    },

    /// subscribe 中の client への broadcast。payload は pure な [`ControlMessage`]、
    /// 配信は execute layer ([`execute::execute`])。
    ClientBroadcast {
        message: ControlMessage,
        filter: BroadcastFilter,
    },

    /// record sink への append。
    Record { entry: RecordEntry },

    /// 子プロセス spawn (= run 起動時のみ)。
    SpawnChild {
        argv: Vec<String>,
        env: Vec<(String, String)>,
        pty: PtySetup,
    },

    /// 内部 timer 設定。
    SetTimer { id: TimerId, delay_ms: u64 },

    /// 内部 timer 取消。
    CancelTimer { id: TimerId },
}

/// effect 実行結果 ([`DaemonMsg::EffectResult`] の payload、DR-0025 §Effect 失敗の feedback)。
#[derive(Debug)]
pub(in crate::daemon) enum EffectOutcome {
    /// daemon 側での effect 実行成功 (= socket write 成功等。client 受信保証ではない、
    /// DR-0025 §state rollback / pending state 戦略の適用範囲注意)。
    Ok,
    /// effect 実行失敗。`kind` で失敗種別、`retry_advice` で再試行方針を返す。
    Failed {
        kind: EffectErrorKind,
        retry_advice: RetryAdvice,
    },
}

/// effect 失敗の種別 (DR-0025 §Effect 失敗の feedback)。
///
/// DR-0025 の列挙は代表例 (`// ...` で拡張余地明示)。実 execute layer が観測する errno /
/// error を Phase 1b 後半以降で足す。
#[derive(Debug)]
pub(in crate::daemon) enum EffectErrorKind {
    /// 子の slow read で master fd が `POLLOUT` 待ち (= `EAGAIN`)。
    WriteEagain,
    /// `EPIPE` / `ECONNRESET` (= 書込先が閉じた)。
    WriteBroken,
    /// signal 対象の子が既に死亡 (`ESRCH`)。
    KillEsrch,
    /// 子 spawn 失敗。
    SpawnFailed { errno: i32 },
}

/// effect 失敗時の再試行助言 (DR-0025 §Effect 失敗の feedback)。
///
/// DR-0025 では型名のみで中身未定義。pending state パターンの rollback / backoff 判断に
/// 使う最小の 3 分類を Phase 1b 前半で置く (= 精緻化した設計判断、後続 Phase で細分可)。
#[derive(Debug)]
pub(in crate::daemon) enum RetryAdvice {
    /// 再試行せず rollback へ (= 恒久的失敗)。
    DoNotRetry,
    /// poll readiness を待って再試行 (= `WriteEagain` 等)。
    RetryWhenReady,
    /// 一定 delay 後に再試行。
    RetryAfter { delay_ms: u64 },
}

/// client の opaque な識別子 (placeholder)。
///
/// Phase 1a の [`LockState`] は client を素の `u64` で扱う。effect routing / reply の型安全性
/// のため newtype 化する。Phase 2 (Client reducer) で `ClientRegistry` が採番する実 id へ
/// 統合予定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::daemon) struct ClientId(pub(in crate::daemon) u64);

/// 内部 timer の識別子 (placeholder)。Phase 2-3 の timer 実装 (DR-0025 Q3) で実体化予定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::daemon) struct TimerId(pub(in crate::daemon) u64);

/// broadcast の配信対象フィルタ placeholder。
///
/// Phase 2 (Client reducer の cap-aware broadcast、DR-0015 継承) で subscription / cap の
/// 実体に置換予定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum BroadcastFilter {
    /// 全 client。
    All,
    /// subscribe 中の client のみ。
    SubscribersOnly,
}

/// record sink へ append する entry の placeholder。
///
/// Phase 4 (Serve / record domain) で `daemon::record` の event 型に置換予定。現状は
/// serialize 済 bytes を opaque に保持するだけ。
#[derive(Debug)]
pub(in crate::daemon) struct RecordEntry(pub(in crate::daemon) Vec<u8>);

/// 子 PTY spawn 設定の placeholder。
///
/// Phase 3 (Child reducer) で `daemon::pty` の spawn 経路 (openpty + fork + TIOCSCTTY、
/// DR-0017 anchor) に置換予定。
#[derive(Debug)]
pub(in crate::daemon) struct PtySetup {
    pub(in crate::daemon) cols: u16,
    pub(in crate::daemon) rows: u16,
}

/// super-reducer の入力 message (DR-0025 §cross-domain event 伝播 protocol)。
///
/// 6 domain の event variant + [`DaemonMsg::EffectResult`]。cross-domain dispatch も
/// この型で表現する (= domain reducer が返す `cross` は「別 domain 宛の `DaemonMsg`」)。
/// Lock は Phase 1a の [`LockMsg`] をそのまま参照する。
#[derive(Debug)]
pub(in crate::daemon) enum DaemonMsg {
    Tty(tty::TtyMsg),
    Child(child::ChildMsg),
    Serve(serve::ServeMsg),
    Client(client::ClientMsg),
    Screen(screen::ScreenMsg),
    Lock(LockMsg),
    /// effect 実行結果の feedback。`effect_id` の domain 部が指す発行元 reducer へ直接
    /// routing する (DR-0025 §Effect layer の EffectResult routing 規約)。
    EffectResult {
        effect_id: EffectId,
        outcome: EffectOutcome,
    },
}

/// domain reducer の出力 (DR-0025 §borrow 戦略の `(Vec<Effect>, Vec<CrossDomainMsg>)`)。
///
/// DR-0025 は tuple で示すが、`effects` / `cross` の役割を明示するため named struct に
/// 精緻化した (= call site の可読性 + test 構築の明快さ)。`cross` は別 domain 宛の
/// [`DaemonMsg`] (= super-reducer が cross-domain queue に積む)。
#[derive(Debug, Default)]
pub(in crate::daemon) struct DomainOutput {
    pub(in crate::daemon) effects: Vec<Effect>,
    pub(in crate::daemon) cross: Vec<DaemonMsg>,
}

impl DomainOutput {
    /// 空出力 (= effect も cross-domain dispatch も無い)。未配線 stub reducer が返す。
    pub(in crate::daemon) fn empty() -> Self {
        Self::default()
    }
}

/// daemon 全体の state (DR-0025 §borrow 戦略)。
///
/// 各 domain state を独立 field に持つことで、super-reducer は match で 1 field のみ
/// `&mut` 取得できる (= split borrow)。Client reducer が Lock を read する場合も、
/// `&mut state.clients` と `&state.lock` は別 field なので同時借用が成立する
/// ([`DomainViews`])。Lock は Phase 1a の [`LockState`] を使う。
#[derive(Debug, Default)]
pub(in crate::daemon) struct DaemonState {
    pub(in crate::daemon) tty: tty::TtyState,
    pub(in crate::daemon) child: child::ChildDomainState,
    pub(in crate::daemon) serve: serve::ServeState,
    pub(in crate::daemon) clients: client::ClientRegistry,
    pub(in crate::daemon) screen: screen::ScreenDomainState,
    pub(in crate::daemon) lock: LockState,
}

/// 他 domain state の read-only projection (DR-0025 §read-only view)。
///
/// reducer が **判定のために他 domain の state を read** する必要がある場合に、reducer 引数
/// として渡す。view は reducer 入力の一部なので pure 性を保つ (= unit test では view を直接
/// 組んで渡すだけ)。読取許可グラフは現状 `Client ──read──→ Lock` の 1 本のみ (= raw_data /
/// input 系 request の holder 認可判定)。新規 read 依存の追加は DR への読取許可グラフ追記を
/// 必須とする。
pub(in crate::daemon) struct DomainViews<'a> {
    /// Client reducer が raw_data 認可で lock holder を判定するための read-only view。
    pub(in crate::daemon) lock: &'a LockState,
}

/// message を担当 domain に対応付ける routing 契約 (DR-0025 §Effect layer / §cross-domain)。
///
/// domain event はその variant の domain へ、[`DaemonMsg::EffectResult`] は
/// `effect_id.domain()` が指す**発行元 domain**へ写す (= EffectResult routing 規約)。
/// super-reducer の dispatch はこの契約に従って各 reducer へ振り分ける。
fn route(msg: &DaemonMsg) -> Domain {
    match msg {
        DaemonMsg::Tty(_) => Domain::Tty,
        DaemonMsg::Child(_) => Domain::Child,
        DaemonMsg::Serve(_) => Domain::Serve,
        DaemonMsg::Client(_) => Domain::Client,
        DaemonMsg::Screen(_) => Domain::Screen,
        DaemonMsg::Lock(_) => Domain::Lock,
        DaemonMsg::EffectResult { effect_id, .. } => effect_id.domain(),
    }
}

/// 1 message を担当 reducer に dispatch し、その出力を返す (DR-0025 §borrow 戦略)。
///
/// domain event はその domain の [`DomainOutput`] を返す reducer へ、`EffectResult` は
/// [`dispatch_effect_result`] 経由で発行元 domain へ routing する。Client のみ read-only
/// view ([`DomainViews`]) を追加で受け取る。
fn dispatch(state: &mut DaemonState, msg: DaemonMsg) -> DomainOutput {
    match msg {
        DaemonMsg::Tty(e) => tty::reduce(&mut state.tty, e),
        DaemonMsg::Child(e) => child::reduce(&mut state.child, e),
        DaemonMsg::Serve(e) => serve::reduce(&mut state.serve, e),
        DaemonMsg::Client(e) => {
            client::reduce(&mut state.clients, DomainViews { lock: &state.lock }, e)
        }
        DaemonMsg::Screen(e) => screen::reduce(&mut state.screen, e),
        // Lock domain: `super::lock::reduce_msg` が Phase 1a の pure `reduce` を内部で
        // 呼び、`LockEvent::Released` を `ClientBroadcast(ModeChange)` Effect に翻訳する
        // (= DR-0025 §Lock domain の `Lock ──→ Client` broadcast)。acquire/release の
        // reply (LockResponse) 経路は control.rs 配線と不可分で未翻訳 (= 次片以降)。
        DaemonMsg::Lock(e) => super::lock::reduce_msg(&mut state.lock, e),
        DaemonMsg::EffectResult { effect_id, outcome } => {
            dispatch_effect_result(state, effect_id, outcome)
        }
    }
}

/// `EffectResult` を発行元 domain へ直接 routing する (DR-0025 §Effect layer)。
///
/// `effect_id.domain()` が指す reducer の EffectResult 受口へ渡す。各 domain の受口は
/// Phase 1b 前半では空 stub。
fn dispatch_effect_result(
    state: &mut DaemonState,
    effect_id: EffectId,
    outcome: EffectOutcome,
) -> DomainOutput {
    match effect_id.domain() {
        Domain::Tty => tty::reduce_effect_result(&mut state.tty, effect_id.seq(), &outcome),
        Domain::Child => child::reduce_effect_result(&mut state.child, effect_id.seq(), &outcome),
        Domain::Serve => serve::reduce_effect_result(&mut state.serve, effect_id.seq(), &outcome),
        Domain::Client => client::reduce_effect_result(
            &mut state.clients,
            DomainViews { lock: &state.lock },
            effect_id.seq(),
            &outcome,
        ),
        Domain::Screen => {
            screen::reduce_effect_result(&mut state.screen, effect_id.seq(), &outcome)
        }
        // [stub] Lock domain の EffectResult 受口。Phase 1b 後半で LockState の pending
        // state 遷移 (Acquiring → Held / Free、DR-0025 §Effect 例 (Lock)) に配線予定。
        Domain::Lock => DomainOutput::empty(),
    }
}

/// cross-domain transaction を回すエンジン (DR-0025 §連鎖停止条件)。
///
/// `initial` を起点に、`dispatch_fn` が返す `cross` を FIFO queue に積みながら queue が
/// 空になるまで順次処理する。1 入力 message から派生する全 event を 1 transaction として
/// 処理し (= 途中で他入力を割り込ませない)、発生した [`Effect`] を発行順に集約して返す。
///
/// queue 長が [`MAX_CROSS_DOMAIN_DEPTH`] を超えたら設計違反とみなす:
/// - debug build: 即 panic (= 循環 dispatch / 連鎖爆発の即検出)
/// - release build: error log + 当該 transaction 破棄 + 継続 (= daemon panic は子への
///   SIGHUP 巻き添えを意味し透過原則と衝突するため production は fail-safe 側に倒す)
///
/// `dispatch_fn` を注入可能にすることで、routing / state 変更から切り離して queue の
/// FIFO 順序と深度上限を単体 test できる。
fn drive<F>(initial: DaemonMsg, mut dispatch_fn: F) -> Vec<Effect>
where
    F: FnMut(DaemonMsg) -> DomainOutput,
{
    let mut effects = Vec::new();
    let mut queue: VecDeque<DaemonMsg> = VecDeque::new();
    queue.push_back(initial);

    while let Some(msg) = queue.pop_front() {
        let out = dispatch_fn(msg);
        effects.extend(out.effects);
        queue.extend(out.cross);

        if queue.len() > MAX_CROSS_DOMAIN_DEPTH {
            #[cfg(debug_assertions)]
            {
                panic!(
                    "cross-domain queue overflow: len={} > {}",
                    queue.len(),
                    MAX_CROSS_DOMAIN_DEPTH
                );
            }
            #[cfg(not(debug_assertions))]
            {
                report_cross_domain_overflow(queue.len());
                break;
            }
        }
    }

    effects
}

/// super-reducer (DR-0025 §ドメイン分割 / §borrow 戦略)。
///
/// 1 入力 [`DaemonMsg`] を起点に cross-domain transaction を回し、発行された [`Effect`] 列を
/// 返す。実 IO への変換 (execute layer) は Phase 1b 後半で被せる。
pub(in crate::daemon) fn handle(state: &mut DaemonState, msg: DaemonMsg) -> Vec<Effect> {
    drive(msg, |m| dispatch(state, m))
}

/// release build で cross-domain queue が深度上限を超えた時の fail-safe 通知
/// (DR-0025 §連鎖停止条件)。
///
/// [stub] Phase 1b 後半で daemon の logging 経路へ差し替える。現状は stderr への 1 行出力に
/// 留める (= debug build では [`drive`] が panic するためこの経路は通らない)。
fn report_cross_domain_overflow(queue_len: usize) {
    eprintln!(
        "[hyoui] cross-domain queue overflow (len={queue_len} > {MAX_CROSS_DOMAIN_DEPTH}); \
         dropping transaction and continuing"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 各 domain event が自 domain へ routing されることを保証する
    /// (DR-0025 §Effect layer / §cross-domain の routing 契約)。
    ///
    /// なぜこの test か: super-reducer の dispatch は [`route`] の契約に従って各 reducer へ
    /// 振り分ける。domain event の owner 対応が崩れると、別 domain の state を誤って触る
    /// (= single-writer 違反)。6 domain 全てを列挙して owner を固定する。
    #[test]
    fn route_maps_each_domain_msg_to_owner_domain() {
        assert_eq!(
            route(&DaemonMsg::Tty(tty::TtyMsg::ResizeRequested {
                cols: 80,
                rows: 24
            })),
            Domain::Tty
        );
        assert_eq!(
            route(&DaemonMsg::Child(child::ChildMsg::HungUp)),
            Domain::Child
        );
        assert_eq!(
            route(&DaemonMsg::Serve(serve::ServeMsg::Booted)),
            Domain::Serve
        );
        assert_eq!(
            route(&DaemonMsg::Client(client::ClientMsg::Connected {
                client_id: ClientId(1)
            })),
            Domain::Client
        );
        assert_eq!(
            route(&DaemonMsg::Screen(screen::ScreenMsg::WriteEvent {
                x: 0,
                y: 0
            })),
            Domain::Screen
        );
        assert_eq!(
            route(&DaemonMsg::Lock(LockMsg::ClientDisconnected {
                client_id: 1
            })),
            Domain::Lock
        );
    }

    /// `EffectResult` が `effect_id.domain()` の発行元 domain へ routing されることを保証する
    /// (DR-0025 §Effect layer の EffectResult routing 規約 = 「effect_id の domain 部が指す
    /// 発行元 reducer に dispatch」)。
    ///
    /// なぜこの test か: pending state パターンは「effect を出した reducer が結果を受けて
    /// Confirmed / Rolled-back に遷移する」ことに依存する。routing が発行元以外へ届くと
    /// state 食い違いが起きる。全 6 domain 分の EffectResult が発行元へ戻ることを固定する。
    #[test]
    fn route_effect_result_targets_origin_domain() {
        for d in [
            Domain::Tty,
            Domain::Child,
            Domain::Serve,
            Domain::Client,
            Domain::Screen,
            Domain::Lock,
        ] {
            let msg = DaemonMsg::EffectResult {
                effect_id: EffectId(d, 7),
                outcome: EffectOutcome::Ok,
            };
            assert_eq!(
                route(&msg),
                d,
                "EffectResult は effect_id.domain() の発行元へ routing される"
            );
        }
    }

    /// [`EffectId`] が domain + 連番を保持し accessor で取り出せることを保証する
    /// (DR-0025 §Effect layer = routing key と決定論採番の基礎)。
    #[test]
    fn effect_id_exposes_domain_and_seq() {
        let id = EffectId(Domain::Lock, 42);
        assert_eq!(id.domain(), Domain::Lock, "第 1 要素が発行元 domain");
        assert_eq!(id.seq(), 42, "第 2 要素が domain 内連番");
    }

    /// cross-domain queue が FIFO 順で処理されることを保証する
    /// (DR-0025 §連鎖停止条件 = queue は FIFO)。
    ///
    /// なぜこの test か: 1 transaction 内の派生 event の処理順序が LIFO だと event ordering
    /// が壊れ、DR-0025 §設計哲学の ordering 決定論性が崩れる。initial (Screen) が Client と
    /// Child を派生させ、各 domain が識別可能な連番 effect を出す構成で、出力 effect の順が
    /// [initial=1, Client=2, Child=3] (= FIFO) になることを確認する。LIFO なら [1, 3, 2] に
    /// なるため、この assert が FIFO を弁別する。
    #[test]
    fn cross_domain_queue_is_fifo() {
        // 各 domain を識別する連番付き effect を作るヘルパ。
        fn mark(seq: u64) -> Effect {
            Effect {
                id: EffectId(Domain::Serve, seq),
                kind: EffectKind::CancelTimer { id: TimerId(seq) },
            }
        }
        let effects = drive(
            DaemonMsg::Screen(screen::ScreenMsg::WriteEvent { x: 0, y: 0 }),
            |msg| match route(&msg) {
                // initial: effect 1 を出し、Client → Child の順に cross-domain 派生。
                Domain::Screen => DomainOutput {
                    effects: vec![mark(1)],
                    cross: vec![
                        DaemonMsg::Client(client::ClientMsg::Connected {
                            client_id: ClientId(0),
                        }),
                        DaemonMsg::Child(child::ChildMsg::HungUp),
                    ],
                },
                Domain::Client => DomainOutput {
                    effects: vec![mark(2)],
                    cross: Vec::new(),
                },
                Domain::Child => DomainOutput {
                    effects: vec![mark(3)],
                    cross: Vec::new(),
                },
                _ => DomainOutput::empty(),
            },
        );
        let seqs: Vec<u64> = effects.iter().map(|e| e.id.seq()).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3],
            "initial → Client → Child の FIFO 順で effect が発行される (LIFO なら [1,3,2])"
        );
    }

    /// cross-domain queue が深度上限を超えたら debug build で panic することを保証する
    /// (DR-0025 §連鎖停止条件の cfg 分岐、debug 側 = 設計違反の即検出)。
    ///
    /// なぜこの test か: 循環 dispatch / 連鎖爆発を fail-fast で検出できないと、無限 loop で
    /// daemon が固まる。1 message が常に 2 message を派生させ続ける (= 純増) dispatch で
    /// [`MAX_CROSS_DOMAIN_DEPTH`] を突破させ、panic を確認する。`cargo test` は既定で
    /// `debug_assertions` on のためこの経路 (panic) を通る。release 側 (log + break + 継続)
    /// は cfg で分岐するため本 test では検証しない (= 別 build profile を要する、設計上の
    /// fail-safe として DR に記録)。
    #[test]
    #[should_panic(expected = "cross-domain queue overflow")]
    fn cross_domain_depth_overflow_panics_in_debug() {
        let _ = drive(DaemonMsg::Child(child::ChildMsg::HungUp), |_msg| {
            DomainOutput {
                effects: Vec::new(),
                // 1 消費するたびに 2 積む = queue が純増し続け、必ず上限超過する。
                cross: vec![
                    DaemonMsg::Child(child::ChildMsg::HungUp),
                    DaemonMsg::Child(child::ChildMsg::HungUp),
                ],
            }
        });
    }

    /// Client reducer が `&mut state.clients` と `&state.lock` を同時に借りられることを
    /// コンパイル事実として保証する (DR-0025 §borrow 戦略 / §read-only view)。
    ///
    /// なぜこの test か: DomainViews による split borrow が成立しないと、Client reducer が
    /// lock holder を read する認可判定 (DR-0022) を pure に書けず、super-reducer 側に認可
    /// 判定が漏れる。別 field の同時借用が通ること自体が検証対象 (= 本 test が compile して
    /// 動く事実が split borrow の成立証明)。stub なので出力は空。
    #[test]
    fn domain_views_split_borrow_compiles() {
        let mut state = DaemonState::default();
        let out = client::reduce(
            &mut state.clients,
            DomainViews { lock: &state.lock },
            client::ClientMsg::FrameReceived {
                client_id: ClientId(1),
            },
        );
        assert!(
            out.effects.is_empty() && out.cross.is_empty(),
            "配線前の stub reducer は空 DomainOutput を返す"
        );
    }

    /// 未配線 domain への [`handle`] が effect を出さないことを保証する (= stub 契約)。
    ///
    /// なぜこの test か: Phase 1b 前半では全 domain reduce が空 stub なので、super-reducer
    /// を通しても副作用 (Effect) が出ないのが正しい状態。domain event と EffectResult の
    /// 両経路が panic せず空を返すことを固定し、Phase 1b 後半で reducer に振る舞いを入れた
    /// 時の回帰基準にする。
    #[test]
    fn handle_stub_domains_emit_no_effects() {
        let mut state = DaemonState::default();
        assert!(
            handle(
                &mut state,
                DaemonMsg::Tty(tty::TtyMsg::ResizeRequested { cols: 80, rows: 24 })
            )
            .is_empty(),
            "domain event 経路の stub は effect を出さない"
        );
        assert!(
            handle(
                &mut state,
                DaemonMsg::EffectResult {
                    effect_id: EffectId(Domain::Tty, 1),
                    outcome: EffectOutcome::Ok,
                }
            )
            .is_empty(),
            "EffectResult 経路の stub は effect を出さない"
        );
    }
}
