# DR-0025: Daemon Reducer 化と全ドメイン event の形式化

- Status: Active (2026-07-03)
- Date: 2026-06-30
- Review: 1 巡目 (2026-06-30) codex / ultracode 8 観点 (= 設計哲学 / domain 境界 / watch 3 軸 / TTY enum / migration plan / testability / 既存 DR 整合 / Open Questions)、must-address 6 件 + should-address 6 件を反映。2 巡目 (2026-07-03) ultracode 4 観点 (= 全方位 / 内部整合 / 既存 DR 整合 / 実装参照実在性) + finding 別反証検証で confirmed 11 件を反映 (= EffectId routing 規約 / Client→Screen edge 削除 / DR-0021 ack 経路明記 / super-reducer 例統一 / Alternative F,G と Q-NEW1 の Phase 整合 / DR-0008 kind 実名化 / raw_data lock gate の read-only view 帰属 等)
- Related: DR-0014 (透過原則 + 検証主義、本 DR で実装レベル強化), DR-0008 (外部 protocol、本 DR は daemon 内部設計で別軸、kind 写像規約あり), DR-0013 (screen state 正本、本 DR の Screen reducer に継承、byte-base tail/history と rows-base virtual screen の分離も継承), DR-0022 (input invocation auto-lock、本 DR の Lock reducer で構造的に再整理、効果境界明文化), DR-0016 (TTY IO record、本 DR の event sourcing と二重 record しない設計責任を本 DR 側が持つ), DR-0001 (jobcontrol = axis 1 のみ、axis 2 は DR-0015 で廃止済、本 DR の Child reducer に継承), DR-0015 (run = fork + attach、Client/Serve lifecycle と Child reducer に継承), DR-0017 (session anchor 構造、Child reducer の事前条件), DR-0019 (OnChildSuspend policy、Child reducer 内部 state), DR-0021 (PTY drain ack、raw_data ack の発行点を Effect::TtyWrite の EffectResult 受領後として意味論保存)
- Origin: `parallel_input_serialized_by_auto_lock` 試験 (2026-06-29 セッション) で発見された race 構造の根本原因調査。lock 状態管理と raw_data 配信が「論理軸として分離されてない」点が、kawaz との設計議論で「単一 machine 化 + 全 IO を message に統一」に収束。さらに「TTY 操作だけでなく子プロセス / serve / client / screen / lock 含む全ドメインを形式化すべき」「region / matcher / event flow は直交軸」「polling は仮想 screen 自前管理なら不要、event-driven で十分」と発展

## Context

`crates/hyoui-cli/tests/input_auto_lock_cli.rs::parallel_input_serialized_by_auto_lock` が
workspace 並列実行で flaky な現象を調査した結果、**lock 実装そのものに bug は無いが、
lock の効果範囲と test の検証範囲がミスマッチ**していることが判明した:

- lock は **client → master fd への write を直列化**する
- だが test は **PTY 上に echo された出力 (= screen dump)** で順序を検証している
- 子プロセス (`/bin/cat`) の `read(2) → write(2)` は kernel scheduler 依存で atomic でない
- 高負荷時に `cat` の line read が分割されると、A の出力途中で B の bytes が割り込みうる

調査の過程で、より深い構造的脆さが見えた:

1. **lock state と PTY write が論理軸として分離されてない**: 同じ `serve_loop` 内で
   `&mut SessionState` の借用排他により偶然 race してないだけ
2. **TTY 由来 event は 12+ カテゴリに散在**: 物理 ioctl / ANSI 制御 / OSC / DCS / mouse /
   paste / signal / 子 lifecycle / flow control 等が各 handler に散在
3. **子プロセス状態管理が散在**: SIGCHLD 経由の waitpid 結果が複数所で state 更新
4. **serve プロセス自身の状態管理が散在**: shutdown reason / active client count / metric が
   `Session` struct の field に散らばる
5. **client 管理が散在**: handshake / mode change / leader 昇格 / stale 判定 / detach が
   `clients` Vec の操作で行われ、state machine として明文化されていない
6. **仮想 screen は DR-0013 で正本化されているが、screen への write の通知 / 監視機構が
   未整理**: watch register / pattern match は client 側で polling するしかない
7. **test が PTY 副作用込みでしか書けない**: 上記すべての state machine を unit test するため
   に PTY + 子プロセス + socket を起動する必要がある

これらは個別 PR で fix できる類ではなく、**daemon 内部の責務分離設計** の問題。

### 元 flaky の真因と本 DR の効果範囲の注意

`parallel_input_serialized_by_auto_lock` の flaky の **直接の真因は子プロセス `/bin/cat` の
`read(2) → write(2)` が kernel scheduler 依存で atomic でない**こと。本 DR の reducer 化は
**daemon 内部 race** (= 上記 7 項目に列挙した責務散在 / borrow 偶然依存) の構造的解消であり、
**子プロセス側 race は対象外**。元 flaky の根治には:

1. 本 DR の Lock reducer で lock の効果境界 (= input 直列化までで output は別軌道) を仕様
   レベルで明文化
2. 別作業として test 側 expectation を「PTY 上の echo 順序」から「daemon 入力の直列化 fact」
   (= daemon record event の `bytes-in` order assert) に変更

の 2 段で対処する。本 DR が単独で flaky を解消するわけではない。

## 問題の本質

- **single-writer principle の欠如**: TTY fd / child pid / lock state / client registry /
  screen state の writer が複数 handler に散在
- **event ordering が偶然依存**: borrow checker の借用排他で「たまたま」serial になってる
  だけで、設計レベルでの atomic 保証ではない
- **state machine が暗黙**: 子プロセス / serve / client の lifecycle が各所の if-else で
  表現され、状態遷移図が code から読み取れない
- **testability の低さ**: machine の意味的挙動を unit test するために PTY と子プロセスを
  起動する必要がある
- **観測機構の不在**: 「screen への write」「client の lifecycle 変化」「pattern match」を
  外部から監視する仕組みが場当たり的 (= polling / record sink の使い回し)
- **「lock の効果境界」の曖昧さ**: 「lock は input 直列化だけで output は別軌道」を仕様
  レベルで明文化していない (= test 設計のミスマッチの根本原因)

## 設計哲学

1. **全 IO を message に統一**: client req / TTY 由来 / signal / child lifecycle / timer /
   client lifecycle / screen write のすべてを `DaemonMsg` enum の variant として扱う
2. **reducer は pure function**: `fn handle(state: &mut DomainState, msg: DomainMsg) ->
   Vec<DomainEvent>` のような純粋関数。reducer 自身は IO を持たない (= 100% unit test 可能)
3. **single-writer**: machine 以外は TTY fd / child pid / signal handler / lock state /
   client registry / screen state を**直接触らない** (例外は DR で justify)
4. **layered reducer**: 1 個の super-reducer ではなく、domain 別 sub-reducer を並べて
   合成する (= elm / redux pattern)。各 sub-reducer は自 domain の state と event だけ扱う
5. **event sourcing (= reducer 内部 state に限定)**: 全 message を順番に record すれば
   **reducer 内部 state は** replay 可能。debug / test / bug report で reducer の
   deterministic check が容易。**外部 IO 状態 (= PTY fd / client socket / child pid) は
   再現不可** = full session replay はできない、適用範囲を意識する
6. **3 軸直交設計**: 例えば watch なら region (= どこ) / matcher (= 何) / flow (= いつ /
   どう流すか) の 3 軸を独立に組み合わせ可能にする (= ad-hoc な trigger 列挙を排除)

## IO boundary と reducer boundary の関係 (= concurrency Decision)

reducer が pure であるためには、OS resource (PTY fd / child pid / signalfd / SIGCHLD /
unix socket) は reducer の外側 (= IO layer) に追い出す必要がある。本 DR は **単一 thread
での `poll → translate → reduce → effect` loop** を採用する。

```
┌────────────────────────────────────────────────────────────────┐
│ main thread (= serve loop、単一)                              │
│                                                                │
│  1. poll(PTY master fd, signalfd, listener, client sockets)    │
│     ↓                                                          │
│  2. translate (= IO event → DaemonMsg variant)                 │
│     - PTY read bytes → DaemonMsg::Tty(TtyLayer1Bytes { bytes })│
│     - SIGCHLD → DaemonMsg::Child(SigchldReceived { pid })      │
│     - client socket read → DaemonMsg::Client(FrameReceived..) │
│     ↓                                                          │
│  3. reduce (= super-reducer dispatch、pure)                    │
│     fn handle(state, msg) -> (state, Vec<Effect>)              │
│     ↓                                                          │
│  4. execute effects (= 実 IO に変換)                           │
│     - Effect::TtyWrite(bytes) → PTY master write               │
│     - Effect::Kill(pid, sig) → libc::kill                      │
│     - Effect::ClientReply(id, frame) → socket write            │
│     ↓                                                          │
│  5. effect 結果を次の DaemonMsg::EffectResult として feed-back │
│     → loop の頭に戻る                                          │
└────────────────────────────────────────────────────────────────┘
```

### 単一 thread 採用理由

- ordering が決定論的 (= channel ordering 依存を排除、Alternative D 棄却理由と整合)
- borrow 戦略単純化 (= `&mut DaemonState` を 1 thread が独占、`Send + Sync` 制約を回避)
- hyoui の現状 (= `nix::poll` ベース、async runtime なし) と整合
- daemon 1 process = 1 session で並列性需要が高くない (= terminal multiplexer ではない)

### 例外: Parser Layer 1 (= byte 読み出し IO) のみ別 thread 許容

PTY master の `read(2)` を main thread に置くと、heavy byte stream (= 巨大 paste / sixel
graphics) で他 IO event の処理が遅延する懸念がある。Parser Layer 1 (= bytes 読み出し)
のみは別 thread で読み、`mpsc::Sender<DaemonMsg::Tty(TtyLayer1Bytes)>` で main thread に
送出する選択肢を残す (= Phase 6a 着手時に必要性を測定して決定)。Layer 2/3 以降は main
thread の reducer 内で実行 (= ordering 保証維持)。

## Effect layer (= reducer 出力 → 実 IO)

reducer が pure であるため、reducer 出力 `Vec<Effect>` を実 IO に変換する layer が必要
(= elm `Cmd` / redux middleware 相当)。

### Effect 型

effect は **相関 id 付き** で発行する。`EffectResult` はこの id で元 effect と対応付ける。
`EffectId` は **発行元 domain + domain 内連番の複合 id** とし、連番は reducer が自 domain
state 内の counter から採番する (= `SystemTime` / `rand` に依存しない決定論的採番、event
sourcing replay と整合)。domain 部は **EffectResult の routing key を兼ねる**: super-reducer
は `DaemonMsg::EffectResult` を `effect_id` の domain 部が指す **発行元 reducer に dispatch**
する (= EffectResult routing 規約。cross-domain queue は経由しない直接 routing)。

```rust
/// 発行元 domain + domain 内連番。EffectResult routing の key を兼ねる
struct EffectId(Domain, u64);

struct Effect {
  id: EffectId,
  kind: EffectKind,
}

enum EffectKind {
  /// PTY master fd への write
  TtyWrite { bytes: Vec<u8> },

  /// PTY への TIOCSWINSZ ioctl (= resize.request 由来、Tty reducer が発行)
  TtyResize { cols: u16, rows: u16 },

  /// 子プロセスへの signal
  Kill { pid: Pid, signal: Signal },

  /// client socket への frame 送信
  ClientReply { client_id: ClientId, frame: Frame },

  /// 全 (subscribe 中の) client への broadcast
  ClientBroadcast { frame: Frame, filter: BroadcastFilter },

  /// record sink への append
  Record { entry: RecordEntry },

  /// 子プロセス spawn (= run の起動時のみ)
  SpawnChild { argv: Vec<String>, env: Vec<(String, String)>, pty: PtySetup },

  /// 内部 timer 設定
  SetTimer { id: TimerId, delay_ms: u64 },
  CancelTimer { id: TimerId },
}
```

### Phase 2-β の実体化点 (= raw_data hot path)

raw_data 経路の reducer 化で以下を確定する:

- **EffectOutcome は effect 種別の詳細 payload を持てる**: `TtyWrite { written_len,
  requested_len, error: Option<TtyWriteErrorKind> }` variant を追加。発行元 reducer は
  pending state に保持した bytes とこの詳細から record (in / in-write-error) と
  RawAck (ok / err) を組み立てる (= DR-0021 の完了点「master fd write の return」を保存)
- **EffectKind の 2-β 実体化**: `ClientRawAck { client_id, ack }` (= RawAck は
  TYPE_RAW_ACK frame で CBOR control と別型のため ClientReply と分離) /
  `ClientDisconnect { client_id }` (= 旧 ClientFrameOutcome::DropClient 相当の切断予約) /
  `Record { entry }` の entry を record_registry の push 系呼び出しを表す enum に実体化
  (bytes-in / in-write-error / in-rejected。lifecycle 系 record の Effect 化は Phase 2-γ)
- **ExecuteCtx**: execute の実行資源 (clients / overflow_ids / pty / record_registry) を
  struct に集約。pty は `Option` (= TtyWrite が来たのに無ければ設計違反として
  debug_assert。linger 等 pty を持たない呼び出し元が存在するため)
- **認可判定の入力**: client の mode は translate 時に payload へスナップショットする
  (= ClientRegistry pure ミラーの導入は Phase 2-γ、それまでの中間形)。raw_data に
  cap 判定は無い (= cap は CBOR control 系のみ) ので mode + lock view で認可が閉じる
- **DropClient の昇格**: raw arm の reducer 経路は control.rs 内で execute をローカル
  overflow で回し、自 client id が積まれたら `ClientFrameOutcome::DropClient` を返す
  (= serve_loop の frame dispatch / indices_to_drop 構造は不変、既存の切断タイミングを保存)

### Effect 失敗の feedback

effect 実行結果は次の input message として reducer に戻る:

```rust
enum DaemonMsg {
  // ... (各 domain msg)
  EffectResult {
    effect_id: EffectId,
    outcome: EffectOutcome,
  },
}

enum EffectOutcome {
  Ok,
  Failed { kind: EffectErrorKind, retry_advice: RetryAdvice },
}

enum EffectErrorKind {
  WriteEagain,       // 子の slow read で master fd POLLOUT 待ち
  WriteBroken,       // EPIPE / ECONNRESET
  KillEsrch,         // 子が既に死亡
  SpawnFailed { errno },
  // ...
}
```

### state rollback / pending state 戦略

effect 失敗時の state ロールバックは **「pending state パターン」を採用**:

- reducer が effect を出すと同時に、関連 state を `Pending` 状態に遷移 (= 楽観 update せず)
- `EffectResult::Ok` 受信で `Confirmed` に遷移
- `EffectResult::Failed` 受信で `Rolled-back` に遷移し、関連 event を発行 (= client への
  error reply 等)

例 (Lock):

```
Client: LockAcquire request
  → Lock reducer: state = Acquiring (pending、holder 確定せず)
  → Effect: ClientReply(LockResponse::Acquired { token })
  → EffectResult::Ok → Lock reducer: state = Held { holder, token }
  → EffectResult::Failed → Lock reducer: state = Free (rollback)
```

これにより「lock holder を更新したが client への ack が失敗、client は acquire 失敗と
解釈、daemon は holder 確定」のような **state 食い違い** を防ぐ。

**適用範囲の注意**: `EffectResult::Ok` は「daemon 側 socket write の成功」であって client
受信の保証ではない (= write 成功後・client 受信前の切断は残余として起きうる)。この残余は
Lock の process-bound GC (= client disconnect で auto-release) が回収する。pending state
パターンが防ぐのは **daemon 側で観測可能な失敗** による食い違いまで。

## cross-domain event 伝播 protocol

super-reducer が 1 入力 message を受けて複数 domain reducer を巡る場合の規約を確定する。

### 1 transaction = 1 入力 message + 派生 event chain

1 入力 message から派生する全 event は **1 transaction** として処理する。途中で他の入力
message を割り込ませない (= ordering 保証)。

### 連鎖停止条件

- domain reducer は cross-domain dispatch が必要なら `CrossDomainMsg` を返す
- super-reducer は queue (= FIFO) に積み、queue が空になるまで順次 dispatch
- 同 domain への self-dispatch は禁止 (= 無限 loop 防止、self transition は同 reducer 内で
  完結させる)
- queue 深度上限 (= 例: 64) を超えたら、debug build では panic (= 設計違反の即検出)、
  release build では error log + 当該 transaction 破棄 + serve 継続 (= daemon の panic は
  子プロセスの SIGHUP 巻き添えを意味し透過原則と衝突するため、production は fail-safe 側に
  倒す。隔離方針の詳細は Q-NEW3 と同軸で Phase 1b までに確定)

### 許可された cross-domain 方向 (= 有向グラフ)

```
Tty   ──→ Screen     (= TTY parse 結果が screen state を更新)
Child ──→ Serve      (= 子 exit が serve shutdown を trigger)
Child ──→ Client     (= 子 state 変化を leader client に notify)
Client ──→ Child     (= signal.request / kill.request の認可済み転送)
Client ──→ Tty       (= resize.request の認可済み転送、Tty reducer が state 更新 +
                        Effect::TtyResize 発行)
Client ──→ Serve     (= client detach 通知、ShutdownReason::AllClientsDetached の判定材料)
Client ──→ Lock      (= client disconnect で lock auto-release)
Lock  ──→ Client     (= lock state 変化を全 client に broadcast)
Screen ──→ Client    (= watch matched / screen write event を subscribe client に配信)
Serve ──→ Client     (= shutdown 開始を全 client に notify)
```

**禁止**: 循環 (= Tty ↔ Screen 等)、Client raw_data (= bytes) の Tty への dispatch (=
bytes は Client reducer の認可判定後に Effect::TtyWrite 直行。Tty reducer は子からの
出力方向 (= PTY read → parse) 専任で、入力 bytes を経由させない。制御系 request (resize
等) の dispatch は上記グラフ通り許可)、Lock から Screen への直接 dispatch (= Lock は
holder の identity のみ持ち screen を知らない)

client 入力の screen への反映は Client からの dispatch では**ない**: PTY echo を経た
`Tty ──→ Screen` 経路で起きる (= echo off の子では screen に現れないのが正。local echo
を Screen に直接書く設計は screen 正本性 (DR-0013) と透過原則 (DR-0014) の違反になる)。

### borrow 戦略

`&mut DaemonState` の split borrow が必要なので、`DaemonState` を「各 domain state は
独立 field、super-reducer は match で 1 field のみ `&mut` 取得」の形にする:

```rust
struct DaemonState {
  tty: TtyState,
  child: ChildState,
  serve: ServeState,
  clients: ClientRegistry,
  screen: ScreenState,
  lock: LockState,
}

fn handle(state: &mut DaemonState, msg: DaemonMsg) -> Vec<Effect> {
  let mut effects = Vec::new();
  let mut cross_domain_queue = VecDeque::from([msg]);

  while let Some(msg) = cross_domain_queue.pop_front() {
    let (domain_effects, cross) = match msg {
      DaemonMsg::Tty(e)    => tty::reduce(&mut state.tty, e),
      DaemonMsg::Child(e)  => child::reduce(&mut state.child, e),
      DaemonMsg::Serve(e)  => serve::reduce(&mut state.serve, e),
      // Client のみ read-only view (= §read-only view) を追加で受け取る。
      // &mut state.clients と &state.lock は別 field なので split borrow で成立
      DaemonMsg::Client(e) => client::reduce(
        &mut state.clients, DomainViews { lock: &state.lock }, e),
      DaemonMsg::Screen(e) => screen::reduce(&mut state.screen, e),
      DaemonMsg::Lock(e)   => lock::reduce(&mut state.lock, e),
      // ...
    };
    effects.extend(domain_effects);
    cross_domain_queue.extend(cross);
    if cross_domain_queue.len() > MAX_CROSS_DOMAIN_DEPTH {
      // 設計違反の検出 (= §連鎖停止条件): debug は panic、release は
      // error log + 当該 transaction 破棄 + serve 継続
      #[cfg(debug_assertions)]
      panic!("cross-domain queue overflow");
      #[cfg(not(debug_assertions))]
      { report_cross_domain_overflow(&msg); break; }
    }
  }

  effects
}
```

各 domain reducer は **`&mut DaemonState` を受け取らず**、自 domain の state field のみ
受け取る = borrow checker と整合。

### read-only view (= cross-domain 読取、dispatch グラフと対になる依存軸)

reducer が **判定のために他 domain の state を read する** 必要がある場合 (= 例: Client
reducer が raw_data の認可で lock holder を判定する)、対象 domain state の read-only view
を reducer 引数として渡す:

```rust
fn reduce(
  state: &mut ClientRegistry,
  views: DomainViews<'_>,        // 他 domain state の read-only projection
  msg: ClientEvent,
) -> (Vec<Effect>, Vec<CrossDomainMsg>)

struct DomainViews<'a> {
  lock: &'a LockState,   // Client の read 許可対象は現状 Lock のみ
}
```

- view は **reducer 入力の一部** なので pure 性は保たれる (= unit test では view を直接
  組んで渡すだけ、PTY 不要)
- `&mut state.clients` と `&state.lock` は別 field なので split borrow で成立
- **読取許可グラフ** (= dispatch 有向グラフと対で管理する):

```
Client ──read──→ Lock   (= raw_data / input 系 request の holder 認可判定)
```

- 新規 read 依存の追加は本 DR (または後続 DR) への読取許可グラフ追記を必須とする (=
  無秩序な読取結合の増殖を防ぐ。「domain は自 state だけ知る」原則の明示的な例外管理)
- dispatch (= 書き込み側、event 駆動) と view (= 読取側、判定材料) の分離により、
  「Lock reducer が bytes を運ぶ」「super-reducer が認可判定を持つ」のいずれの責務汚染も
  避ける。raw_data の hot path が cross-domain queue を経由しない (= 性能面の利点) 点も
  この分離の根拠

## DR-0008 protocol との接続: ClientRequest 写像規約

外部 protocol (= DR-0008 を正本とする kind 集合、後続 DR による拡張・改訂を含む) と内部
ClientEvent の対応規約。表の kind 名は **DR-0008 §message kind の実名** を用いる:

- **default: 1 protocol kind = 1 ClientEvent variant** (= 1:1 mapping)
- aggregation が必要な場合は **その理由を本 DR (または後続 DR) で明示**

例 (主要 kind 抜粋):

| protocol kind (DR-0008 実名) | ClientEvent variant | 備考 |
|---|---|---|
| `handshake.request` | `Connected { client_id, ... }` | handshake 開始 |
| `handshake.response` | (= Effect::ClientReply) | reducer 出力 |
| `lock.acquire` | `LockAcquireRequested { ... }` | → Lock reducer dispatch |
| `lock.release` | `LockReleaseRequested { ... }` | → Lock reducer dispatch |
| raw PTY data (= frame type `0x00`、CBOR kind ではない) | (= Client reducer が認可判定: mode / cap / lock holder (LockState view read) → 可なら Effect::TtyWrite、否なら Effect::ClientReply(lock.not-held)) | bytes 書き込み、DR-0022 lock gate。成功 ack (= TYPE_RAW_ACK、frame type `0x02`) は Effect::TtyWrite の EffectResult を受けた Client reducer が発行 (= DR-0021 drain ack 意味論を保存) |
| `signal` | `SignalRequested { signum }` | → Client 認可後 Child reducer に dispatch |
| `resize` | `ResizeRequested { cols, rows }` | → Client 認可後 Tty reducer に dispatch (= state 更新 + Effect::TtyResize) |
| `kill` | `KillRequested { signum, scope }` | → Client 認可後 Child reducer に dispatch |
| `status.query` | (= 即 Effect::ClientReply(status.response)) | read-only |
| ... (DR-0008 §kind 表参照) | ... | ... |

新 protocol kind 追加時は **本表に行を追加** (= DR-0008 と本 DR の両方更新する規約)。

## ドメイン分割

daemon 内部を 6 domain に分け、各 domain reducer を持つ:

| reducer | 責務 | 既存対応 |
|---|---|---|
| **TtyParserPipeline** | byte stream → parsed event → semantic event の 3 layer | byte 読み出しは IO layer、parse 以降は本 reducer |
| **ChildStateReducer** | 子プロセス state machine + lifecycle event | DR-0001 axis 1 (axis 2 は DR-0015 で廃止済) + DR-0017 anchor + DR-0019 OnChildSuspend policy |
| **ServeStateReducer** | serve 自身の lifecycle + 内部 metric + shutdown 調停 | (新規、現状散在) |
| **ClientRegistry** | client 集合 / lifecycle / leader-follower / **Transport (= socket / framing) / Auth (= handshake / cap nego) / Backpressure (= write idle / stale 判定)** | DR-0008 cap negotiation / DR-0015 cap-aware broadcast |
| **ScreenReducer** | 仮想 screen state (rows-base) + **byte-base tail/history** (DR-0013 §scrollback) + watch (= region / matcher / flow) | DR-0013 を中核に拡張、rows-base と byte-base の分離も継承 |
| **LockReducer** | lock state / token / process-bound GC (= ClientId は opaque な holder identifier、client lifecycle 監視は本 reducer の責務外) | DR-0022 (input auto-lock) |

### Transport / Auth / Backpressure の扱い (= critical 指摘への対応)

protocol message の framing (= CBOR encode/decode、cap flag negotiation、handshake)、認証
(= 同 UID 信頼境界 + lock token 検証)、backpressure (= writer pump の idle timeout、queue
overflow) は **Client domain reducer 内の sub-state** として扱う (= 独立 reducer にすると
小さすぎ、Client 状態と密結合):

- **Transport sub-state**: `TransportState { socket_fd, framer_state, recv_buffer, send_buffer }`
- **Auth sub-state**: `AuthState { handshake_phase, negotiated_caps, mode, lock_token_inherited }`
- **Backpressure sub-state**: `BackpressureState { write_idle_at, send_queue_depth, stale_kind }`

Client domain reducer は上記 sub-state を保持し、それぞれ独立の reducer function
(`transport::reduce` / `auth::reduce` / `backpressure::reduce`) に dispatch する内部構造。
**「Client が単一 domain」というのは外向きの reducer 入口の話**で、内部実装は sub-reducer
で modular に分割する。

これにより:
- protocol invariant (DR-0008 cap negotiation) が Client domain の Auth sub-state に閉じる
- backpressure 判定が Client domain の Backpressure sub-state に閉じる
- cross-domain 漏出が起きない (= 観点 codex Critical #1 への構造的対応)

super-reducer は **単純なルータ** (= 各 domain reducer への match dispatch + cross-domain
queue + EffectResult routing のみを持ち、業務判断を持たない)。実装形は §cross-domain event
伝播 protocol の borrow 戦略に示した `handle` を正とする (= code の二重掲載はしない)。

各 reducer は **その domain の event だけ受け取り、その domain の state だけ mutate、その
domain の event を出力**。cross-domain の依存は **super-reducer 層が event を別 domain
reducer に再 dispatch** することで表現 (= reducer 自身は他 domain を知らない)。

## TTY domain: 3 layer parser pipeline + enum カタログ

TTY 由来の byte stream を 1 個の message にせず、**3 layer に分けて段階的に意味化** する:

```
[Layer 1: raw bytes]   (IO boundary、PTY master からの read 結果)
   ↓
[Layer 2: parsed event] (vt parser、ECMA-48 / OSC / DCS の syntax 単位に分解)
   ↓
[Layer 3: semantic event] (Layer 2 を意味的に解釈、reducer の入力単位)
```

- Layer 1 → 2 → 3 は **独立した pipeline で各層が単体 test 可**
- TtyParserPipeline reducer は Layer 3 を入力に取る (= byte parsing と意味解釈を持たない)
- alacritty / wezterm の vt parser 設計と整合

### enum variant の必須 doc comment 規約

カタログとして信頼性を持たせるため、**全 variant に「規格名 + 機能名 + 略称」を義務化**
する。URL は補助 (任意)。AI 推測列挙との区別、規格の出典追跡、隣接機能の発見漏れ防止が目的。

#### テンプレ

```rust
/// <略称> — <フル名 / 1 行説明>
///
/// <引数仕様 1-2 行>
///
/// **規格**: <ECMA-48 / xterm 拡張 / DEC private / 独自提案 / 業界 de facto> §<section>
/// - <一次情報 URL> (任意、便利な direct link として置く)
///
/// **参考実装** (任意):
/// - <主要 terminal の実装 doc / source link>
///
/// **DR-N 関連 / false-positive 注意点** (任意):
/// - <該当 DR への参照、注意すべきエッジケース>
///
/// **実装状態**: [supported|partial|stub|planned] — <1 行で何が実装されてるか>
SomeVariant { ... },
```

**必須 (= 緩和規約)**:
- **`supported` / `partial`**: 略称 + フル名 + 規格名 + section + 実装状態 marker
  + `# Verified: YYYY-MM-DD by <reviewer>` marker (= AI 推測の人間レビュー済 marker、未検証
  variant を grep で検出可能化)
- **`stub` / `planned`**: 略称 + 規格名 + 1 行説明 のみ (= ドキュメント負債を最小化、
  振る舞いが入る段階で必須情報に格上げ)

**任意**: URL / 引数仕様詳細 / 参考実装 / DR 参照

#### 実装状態 marker

| marker | 意味 |
|---|---|
| `[supported]` | 完全実装、test カバー済 |
| `[partial]` | 一部実装、TODO あり |
| `[stub]` | enum に load のみ、未実装で no-op |
| `[planned]` | 設計検討中、enum load 自体未済 |

#### 一次情報源 (= 出典として優先する順)

| 規格・出典 | 対応範囲 | 補足 |
|---|---|---|
| **ECMA-48** | C0/C1 / CSI / SGR / ED / EL / CUU/D/F/B / CUP / SCS 等 ANSI 規格 | 5th ed (1991)、無料 PDF |
| **ISO/IEC 6429** | ECMA-48 と同内容 (ISO 版) | 有料、ECMA-48 で代替 |
| **ECMA-35** | character code structure / SS2/SS3 / G0-G3 | 6th ed (1994) |
| **DEC VT5xx manuals** | DEC private mode (DECSET 等)、ESC 拡張、DCS 系 | vt100.net で全 manual archive |
| **xterm ctlseqs** | xterm 独自拡張 + 業界 de facto の集大成 | invisible-island.net (Thomas Dickey) |
| **iTerm2 proprietary** | OSC 1337, image protocol 等 | iterm2.com |
| **kitty protocol** | kitty keyboard / graphics protocol / OSC 拡張 | sw.kovidgoyal.net/kitty/ |
| **VSCode shell integration** | OSC 633 | code.visualstudio.com docs |
| **tmux** | DCS tmux passthrough, mouse 拡張 | tmux man page / wiki |
| **Sixel / ReGIS** | DEC graphics | DEC VT2xx/VT3xx manual |
| **terminfo** | capability name の正規定義 | ncurses 配布 / `man terminfo` |
| **個別提案 gist / 議論** | 業界提案 (sync update / CSI u 等) | URL 変化リスクあり (= 規格名で再到達可能) |

#### Link rot 対策

**情報の本体は「規格名 / 機能名 / 略称」、URL は補助** として扱う。

- variant の doc comment に規格名 + 機能名 + 略称が書いてあれば、URL が切れても読者は
  即座に検索で再到達できる
- archive.org 併記や weekly link-check CI のような先回り対策は overkill
- URL は「便利な direct link」程度に留め、義務化はしない

### enum 例

```rust
/// CSI EL — Erase in Line
///
/// 引数 Ps = 0 (cursor → eol), 1 (sol → cursor), 2 (entire line)。
///
/// **規格**: ECMA-48 §8.3.41 (5th ed., 1991)
/// - <https://ecma-international.org/wp-content/uploads/ECMA-48_5th_edition_june_1991.pdf>
///
/// **参考実装**:
/// - xterm ctlseqs: <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Erase-Lines>
/// - VT510 manual §5.5.1: <https://vt100.net/docs/vt510-rm/EL.html>
///
/// **実装状態**: [planned]
CsiEl(ElMode),

/// OSC 52 — Manipulate Selection Data (clipboard set/get)
///
/// `OSC 52 ; Pc ; Pd ST` で clipboard 操作。
///
/// **規格**: xterm 独自拡張 (= 公式 ANSI/ECMA-48 規格には含まれない)
/// - xterm ctlseqs: <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands>
///
/// **採用 terminal**: tmux, kitty, alacritty 他
///
/// **DR-0014 false-positive 例**: 巨大 paste で partial state を踏みやすい
///
/// **実装状態**: [planned]
OscClipboard { selection: char, payload: String },
```

### scoreboard 運用

```bash
# 集計: 各 marker の出現数
rg -o '\[(supported|partial|stub|planned)\]' \
   crates/hyoui/src/daemon/reducer/tty/event_catalog/ | \
  awk -F: '{print $2}' | sort | uniq -c
```

新機能検討時のワークフロー:
1. 関連 enum に variant があるか grep
2. なければ規格を一次情報から調査して追加
3. 実装段階で marker を `[planned] → [stub] → [partial] → [supported]` に推移

## Screen domain: write event + watch (region / matcher / flow の 3 軸直交)

仮想 screen は hyoui が自前管理しているので、**screen への write は event-driven で発火可能**
(= 外部から polling する必要なし)。watch 機構もこれを土台に組む。

### ScreenWriteEvent (= raw 通知、watch 機構と独立)

screen への write のたびに発火する低レベル event。誰でも subscribe 可。

```rust
/// 仮想 screen への write 通知 (= cell 更新の生 event)
ScreenWriteEvent {
  x: u16,
  y: u16,
  payload: ScreenWritePayload,
  layer: ScreenLayer,
}
```

「screen が変化した事実」を知りたいだけならこれを subscribe で十分。watch register の
責務ではない。配信先は **明示 subscribe した client のみ** (= broadcast せず、購読 client
のみ受け取る、Q12 の default 方針)。

### WatchRegistration (= 意味解釈付きの監視)

watch は **region (= どこ) / matcher (= 何) / flow (= いつ・どう流すか) の 3 軸が直交**
する設計。

```rust
WatchRegistration {
  id: WatchId,
  region: WatchRegion,       // どこを見るか
  matcher: Matcher,          // 何を探すか (= 必須、無意味な register を排除)
  flow: EventFlow,           // どう event を matcher に流すか
}

enum WatchRegion {
  /// 仮想 screen の可視領域全体 (= alt screen 中なら alt の表示範囲、normal なら現在
  /// viewport)
  Visible,

  /// 矩形範囲 (= cells 単位)
  Rect {
    rows: Range<u16>,
    cols: Range<u16>,
    layer: ScreenLayer,
  },

  /// scrollback の末尾 N 行
  ScrollbackTail { rows: u16 },

  /// byte stream 全体 (= DR-0013 §scrollback の byte-base history、巨大 paste / sixel
  /// 検出等の流量判定で使う、cell に reflow されてない生 byte 系列)
  EntireByteStream,
}
```

`x: u16, y: u16` の `Range<u16>` で `ScreenWriteEvent` の `x, y` と型整合。

### Matcher (= 段階的拡張可能)

```rust
enum Matcher {
  /// region 内に write event があった事実だけを通知 (= matcher 必須規約の最小実装)
  /// raw write event のような低レベル通知が欲しい場合は ScreenWriteEvent を subscribe
  /// するのが筋、本 variant は「region を絞った write 通知」が欲しい場合の便宜
  AnyWrite,

  /// Phase 1: literal substring
  Literal { needle: NonEmptyString, case_sensitive: bool },

  /// Phase 1: regex (RE2 互換)
  Regex { pattern: NonEmptyString },
}
```

**`Matcher::Wasm` は本 DR から外し、別 DR で起票** (= 採用是非自体を docs/issue で discuss):

- DR-0014 透過原則 / 最小介入との整合性が未検証
- wasmtime 等の依存追加判断 + sandbox / CPU / メモリ制限が pure reducer 原則 + event
  sourcing replay と衝突
- 実機で Literal/Regex で困った事例がない段階で planned 化は時期尚早
- 必要になった時に追加 (= 拡張は enum variant 追加のみで dispatch trivial)

**`NonEmptyString` で型レベル弾き**: 空 needle や `.*` で骨抜き register を防ぐ。空入力は
parser 段階で reject、誠実な API に。

### EventFlow (= operator chain、reactive stream 同型)

`ScreenWriteEvent` を matcher にどう流すか。**operator chain** として複合:

```rust
struct EventFlow {
  /// 上流の write event → matcher 実行までの operator chain
  /// Vec の順に適用 (= 例: [Throttle(100), Queue(capacity=16)] で「100ms throttle 後に
  /// 16 件 queue に積み別 worker で処理」)
  operators: Vec<FlowOperator>,
}

enum FlowOperator {
  /// 連続する write を一定間隔に間引く (= throttle(N ms): N ms に最大 1 回、超過分 drop)
  Throttle { interval_ms: u64 },

  /// write が一定時間止まるまで遅延 (= debounce(N ms): 最後の write から N ms 後、
  /// その間の write は捨てて 1 回だけ)
  Debounce { idle_ms: u64 },

  /// queue に積み別 worker で処理 (= matcher の重さを screen reducer 側に影響させない)
  Queue { capacity: usize, overflow: OverflowPolicy },
}

enum OverflowPolicy {
  DropOldest,
  DropNewest,
  Disconnect,    // overflow で watch register を解除し client に notify
  // Block は production daemon では非推奨で除外
}
```

`Vec::new()` (= 空 operator chain) は **無加工 = Immediate 相当**。`Immediate` variant は
不要。

operator chain の利点:
- RxJS / Rx.NET の operator chain と同型、開発者の前提知識を活用
- `Throttle` + `Queue` のような複合が natural (= 4 排他 variant では表現不能だった)
- 新規 operator (= Sample / Buffer / Window 等) も Vec への variant 追加で済む

**polling (= interval poll) は不採用**: 仮想 screen を自前管理しているので、write 時に
event 発火で済む。polling は「観測機構を持たない外部 system」を相手にする時の代替であり、
本 domain には不要。

### 設計の rationale

- **region / matcher / flow が直交**: 各軸独立に拡張可能、合成は trivial
- **matcher 必須**: 「register したけど何もしない」状態を排除、API 規約として意味のある
  register だけ受ける
- **`ScreenWriteEvent` の独立**: 「変化があった事実」を知りたいだけなら register せず
  subscribe で十分、watch register は意味解釈付きの監視に限定
- **EventFlow は既知パターン**: throttle / debounce / queue は RxJS / Rx.NET / Reactive
  Streams 等で確立された operator、独自概念を発明しない

### event

```rust
/// matcher が match した時の event
WatchMatched {
  watch_id: WatchId,
  match_data: MatchData,
}

enum MatchData {
  /// Matcher::AnyWrite で発火 (= region 内に write があった事実通知のみ)
  Raw,
  Literal { location: (u16, u16), text: String },
  Regex { location: (u16, u16), full_match: String, captures: Vec<String> },
}
```

watch 配信先 (= Q12 default 方針): **register した client にのみ配信** (= 各 client が
自分の matcher を register、broadcast せず、明示 subscribe 経路で実装 cost 最小)。

## Child domain: state machine + lifecycle event

子プロセスの state を formal state machine として定義し、遷移を全 event 化する。

```rust
enum ChildState {
  Spawning,
  Running,
  Stopped { reason: StopReason },
  Continued,
  Exited(ExitStatus),
  Reaped,
}

enum ChildEvent {
  Spawned { pid, pgid, controlling_tty },
  ExecCompleted,
  StateTransition { from: ChildState, to: ChildState, trigger: TransitionTrigger },
  Reaped { exit_status },
  HungUp,  // SIGHUP 受信
  PgrpChanged { old, new },
}
```

**継承する既存 invariant**:
- DR-0001 axis 1 (= 子 self-stop に対する `OnChildSuspend` policy) を ChildState::Stopped
  の reason + ChildState 自身の policy field として formal 化
- DR-0017 anchor 構造 (= openpty + 手動 fork + TIOCSCTTY) を ChildState::Spawning →
  ExecCompleted の前提条件として記述
- DR-0019 OnChildSuspend policy (= Notify / AutoResume) を ChildState 内部 field
- DR-0001 axis 2 (= parent suspend、`transparent` / `decouple`) は **DR-0015 で廃止済、
  本 DR で再建しない** (= ChildEvent / ChildState 共に該当 variant を持たない)

## Serve domain: lifecycle + metric

serve プロセス自身の lifecycle を formal 化:

```rust
enum ServeState {
  Booting,
  Serving,
  ShuttingDown { reason: ShutdownReason },
  ShutDown,
}

enum ServeEvent {
  Booted,
  AcceptedListener,
  ShutdownInitiated { reason: ShutdownReason },
  ShutdownCompleted,
}

enum ShutdownReason {
  ChildExited,
  SignalReceived(Signal),
  AllClientsDetached,
  ConfigDirective,
}
```

## Client domain: registry + lifecycle

client の lifecycle を formal state machine 化:

```rust
enum ClientState {
  Connecting,
  Handshaking,
  Connected { mode: Mode, caps: Vec<Cap>, role: ClientRole },
  Detaching,
  Disconnected { reason: DisconnectReason },
}

enum ClientRole {
  Leader,
  Follower,
}

enum ClientEvent {
  Connected { client_id, addr },
  HandshakeCompleted { mode, caps },
  HandshakeFailed { reason },
  ModeChanged { from, to },
  RoleChanged { from, to },  // leader 昇格 / 降格
  StaleDetected { reason: StaleReason },
  Detached { reason: DetachReason },
  ForceDisconnected { reason },
}

enum StaleReason {
  HeartbeatTimeout,
  WriteIdleTimeout,
  Backpressure,
}
```

## Lock domain

lock state を独立 reducer 化 (= 現状 `SessionState` に埋め込みで責務混在):

```rust
struct LockState {
  holder: Option<ClientId>,
  token: Option<LockToken>,
  /// process-bound GC: holder の client disconnect で auto-release
  process_bound: bool,
  /// effect 採番 counter (= EffectId(Domain::Lock, n) の n、§Effect layer。
  /// 各 domain state が同名の counter を持つ)
  next_effect_seq: u64,
}

enum LockEvent {
  Acquired { client_id, token },
  Released { client_id, reason: ReleaseReason },
  Denied { client_id, reason: DenyReason },
}

enum ReleaseReason {
  ExplicitRelease,
  ProcessBoundGc,  // client 切断
  Timeout,
}
```

**弱結合維持**:
- Lock reducer は `ClientId` を **opaque な holder identifier** として持つだけ
- **raw_data の lock gate は Client reducer 側**: holder 判定は Client reducer が LockState
  の read-only view (= §read-only view) で行い、Lock reducer は bytes を扱わない (= DR-0022
  の「holder client のみ raw_data 受理」を Client domain の認可判定として実装する)
- client lifecycle 監視 (= disconnect 検出) は **Client reducer の責務**、Client →
  super-reducer → Lock の `ClientDisconnected → LockReleaseAutoGc` dispatch 経路で
  実装 (= Lock は client state を知らない)
- 本 DR の Origin 調査 (= `parallel_input_serialized_by_auto_lock` flaky) で判明した
  「lock の効果範囲と test 検証範囲のミスマッチ」を構造的に解消:
  Lock の効果境界 = client → master fd write の直列化まで (= input 直列化)、Screen
  reducer は Lock state を一切参照しない (= output 直列化は別軌道、本 DR では保証
  対象外)

## 段階的 migration plan

各 Phase に **「その Phase 固有の gate」** を持たせる。**test 整備は各 Phase 内で並走** で
行う (= Phase 8 まで遅延しない、TDD と整合)。

| Phase | 内容 | domain | Phase 固有 gate |
|---|---|---|---|
| **Phase 1a** | **Lock domain 単独 pure reducer 化 (= 最小スパイク)** で race 解消 + reducer pattern の Rust 実装妥当性を 1 domain で実証 | Lock | 1a-1: Lock state mutation が Lock reducer 外に存在しないことを grep で確認 / 1a-2: Lock 単独 unit test (PTY 不要、message 列 record/assert) が integration test と等価カバレッジ達成 / 1a-3: 既存 integration test 全 pass + 実機マトリクス (= TUI/line/REPL 3 category) 再検証 |
| **Phase 1b** | super-reducer 骨格 + 残 5 domain reducer のシグネチャ定義 (= 中身は既存 handler を wrap、passthrough characterization test 付き) | 全 domain | 1b-1: 新 reducer 経路で全 test pass / 1b-2: 旧 handler が dead code として残らない / 1b-3: wrap が単純 passthrough である characterization test 整備 |
| **Phase 2** | Client domain reducer 化 (= Transport/Auth/Backpressure sub-state 込み) + ClientEvent 整理 + protocol kind 1:1 mapping 実装 | Client | 各 protocol kind の reducer test (PTY 不要) + cap negotiation 単独 test + DR-0021 drain ack 経路の regression test pass |
| **Phase 3** | Child state machine 化 + ChildEvent 整理 + DR-0001 axis 1 / DR-0017 anchor / DR-0019 OnChildSuspend を ChildState 内部に formal 化 | Child | 状態遷移網羅 unit test (= 全 ChildState × 全 trigger) + 既存 jobcontrol integration test pass |
| **Phase 4** | Serve lifecycle 整理 + ShutdownReason 集約 + cross-domain dispatch 経路の検証 | Serve | shutdown 全 reason の unit test + Child→Serve dispatch 経路 test |
| **Phase 5** | Screen reducer 骨格 (= 仮想 screen state + ScreenWriteEvent broadcast、watch は除く) + DR-0013 byte-base tail/history と rows-base virtual screen の分離継承。**Q-NEW1 (vt100 cell hook feasibility) の解決が着手前提** | Screen | DR-0013 既存機能の regression なし + ScreenWriteEvent 配信 test |
| **Phase 6a** | TTY parser 3 layer pipeline 骨格 + **Screen reducer (Phase 5) が必要とする最小限の semantic event のみ supported** + Layer 2/3 境界判断基準を本文に追記 | Tty | Layer 1/2/3 各層単独 unit test (= byte 列 → event 列の入出力 test) + Screen 経由の e2e test pass |
| **Phase 6b** | enum カタログの漸増 (= scoreboard 運用、Phase 7 以降と並列実行可能な恒久タスク、新規 terminal 機能の追加都度) | Tty | 追加 variant 単位で unit test + `# Verified:` marker + scoreboard 増分 |
| **Phase 7** | Screen reducer に watch (region/matcher/flow) 実装追加 + matcher cache / operator chain 実装 | Screen | matcher 単独 unit test (= Literal/Regex の境界 / NonEmptyString 型レベル弾き) + operator chain 単独 test (= Throttle/Debounce/Queue + 複合) + watch e2e test |
| **Phase 8** | 残置 integration test の **追加 + 一部置換** (= 全面書き換えではない、reducer 単独 test で代替できる範囲のみ置換、PTY/signal/子プロセス挙動を検証する integration test は残す) | test | 置換対象と残置対象を明示列挙、置換後の test カバレッジが旧 test と等価 or 上回る |

### 既存実装との対応 (= Phase 着手時の具体ターゲット)

2026-07-03 の実装調査で確認した、各 Phase が直接対象とする既存コードの座標:

- **Phase 1a**: lock state の正本は `daemon/lock.rs` の `SessionState` (= 名前に反して実質
  lock 専用 struct + record registry + child_stopped/policy)。mutate は
  `control.rs::handle_lock_acquire` / `handle_lock_release` / `session.rs` serve_loop の
  client drop cascade / `accept.rs::process_pending_handshakes` (= --detach-others 経路) の
  3 ファイル 4 箇所に散在。`SessionState` は `LockState` へ rename・純化する (= record
  registry / child 系 field は各 domain へ移す)。「lock 判定と PTY write の混在」の現物は
  `control.rs::handle_client_frame` (= lock holder read → master fd write が同一関数内の
  連続処理)
- **Phase 2**: detach cascade (= lock auto-release + leader 昇格 + ModeChange broadcast) が
  `session.rs` serve_loop の通常 drop 経路 / `accept.rs` --detach-others 経路 /
  `session.rs::linger_for_late_attach` の 3 箇所にほぼ同一コードで重複している。Client
  reducer 化で 1 本化する
- **Phase 3**: `daemon/pty.rs` の `ChildLifecycle` / `ChildState` / `ChildTransition` が
  既に最も reducer に近い既存物 (= ゼロから書かず、これを formal 化の出発点にする)。
  waitpid 直呼びが `session.rs` (= `Session::drop` / `finalize_child` / `reap_blocking` /
  `child_is_stopped_via_waitpid`) + `sys/raw.rs` (= spawn 直下) に散在しており、Effect /
  EffectResult 経由に集約する
- **Phase 4**: SIGTERM→SIGKILL 昇格が `Session::drop` (= grace 500ms) と `finalize_child`
  (= grace 5s `FINALIZE_TERM_GRACE`) に二重実装され **grace 値も食い違っている**。Serve
  reducer の shutdown 調停に統一する。`linger_for_late_attach` (= 独自 accept loop +
  `SessionState::default()` 再生成を持つ 3 つ目の準同型実装) も Serve lifecycle の一状態
  (= linger phase) として統合する
- **Phase 8**: `session.rs` の serve_* 系 test (= 実 PTY + 実 socket + 実 thread 起動、
  31 件) が置換検討の主対象。lock の reducer 単独 unit test は現状 0 件 (= Phase 1a gate
  1a-2 の出発点)

### Phase 1b 後半の実装形 (= translate 併走方式)

serve_loop への配線は **translate 併走** で行う: poll revents → `DaemonMsg` への
translate 層を導入し、super-reducer `handle()` を実走させた上で (= stub domain は
no-op)、既存 handler を従来通り呼ぶ (= 挙動不変)。reducer 関数の中身に既存 handler を
埋め込む形 (= reducer が IO を持つ) は採らない — 旧 handler は pty / clients / state を
横断 borrow するため pure signature に収まらず、Phase 2+ の pure 化で丸ごと置換する方が
二度手間にならない。

- **Q-NEW2 の解**: 単一 thread poll 直結を維持し、super-reducer 入口に channel は導入
  しない (= capacity / overflow policy 自体が不要)。channel が必要になるのは Phase 6a の
  Parser Layer 1 別 thread 化のみで、mpsc capacity はその時点で決める
- **gate 1b-2 の解釈**: 併走中の旧 handler は実経路として生きているので dead code では
  ない。各 Phase で reducer 実装に置換された handler は**その Phase 内で即削除**する
  (= §coexistence 期間の即削除 default と整合)
- **gate 1b-3 の characterization test**: translate が生成する `DaemonMsg` 列が poll IO
  event と 1:1 対応することを固定する test (= 後続 Phase で reducer 実装を挿しても
  translate 層の意味論が変わらないことの回帰基準)

### coexistence 期間と feature flag

Phase 1b で「旧 handler を wrap する新 reducer 経路」を導入する瞬間、coexistence 期間が
発生する。Phase 2 以降の各 domain 移行時:

- **旧経路は即削除を default** (= dead code を残さない、Q7 の default 方針)
- 大規模 regression 懸念がある場合のみ **feature flag 経由で runtime 切替可能化** を許容
  (= 例: `--internal-use-legacy-handler` 隠し flag、Phase 完了まで 1 release 限定)

### Phase 順序の判断軸

Lock → Client → Child → Serve → Screen → Tty → Screen(watch) の順は **「動機解消順 +
依存順」の混合**:

- Phase 1a (Lock 単独): 元 race の構造的解消を最速で実証
- Phase 2 (Client): Transport/Auth/Backpressure を整備しないと他 reducer の effect 配信
  経路が固まらない
- Phase 3-4 (Child/Serve): 子 lifecycle と serve lifecycle は密結合 (Child→Serve dispatch)
- Phase 5 (Screen 骨格) → Phase 6a (Tty parser) → Phase 7 (Screen watch): Screen の
  byte-base 経路は Tty parser に依存しないが、watch (= matcher 実行 trigger) は
  ScreenWriteEvent と semantic event 両方に依存

各 Phase は前 Phase の reducer signature を前提に書くため、戻り作業を避けるべく順序固定。

## 既存 DR との関係

- **DR-0014 (透過原則 + 検証主義)**: 本 DR は実装レベルでの強化。§Anti-patterns 防止
  self-check を Phase 完了時に段階的に DR-0014 §self-check へ反映する運用
- **DR-0008 (protocol)**: 外部 protocol (= client ↔ daemon の CBOR framing)。本 DR は
  **daemon 内部の reducer 構造**で別軸。**`ClientEvent` と protocol kind の対応は default
  1:1 mapping** で本文 §DR-0008 protocol との接続 に表で記載済
- **DR-0013 (screen state 正本)**: 本 DR の Screen reducer / TTY parser Layer 3 の正本化層。
  **byte-base tail/history (= DR-0013 §scrollback) と rows-base virtual screen の分離を
  Screen reducer に継承**、timestamp semantic は byte-base 側で保持し tail コマンドの
  動作を維持
- **DR-0022 (input invocation auto-lock)**: 本 DR の Lock reducer で構造的に再整理。
  **auto-lock の効果境界 (= input 直列化までで output は別軌道) を仕様レベルで明文化**
  する根拠を提供。Lock + Screen 境界の典型シナリオ (= lock 取得後に screen に何か書く)
  では Lock の効果境界 = client → master fd write の直列化まで、Screen reducer は Lock
  state を一切参照しない (= 弱結合)
- **DR-0021 (PTY drain ack)**: raw_data の ack (= TYPE_RAW_ACK、frame type `0x02`) は
  「master fd write の完了時点で返す」意味論。reducer 化後は **Effect::TtyWrite の
  EffectResult を受けた Client reducer が ack を発行** する形で per-connection FIFO ack
  意味論を保存する (= `WriteEagain` による chunked 進行時も最終 EffectResult 後に ack、
  DR-0021 の完了点定義を変えない)
- **DR-0016 (TTY IO record)**: bytes-level record (= 人間が読む用) と event sourcing
  (= debug/replay 用) は軸が異なるので**並存**。**二重 record しない設計責任を本 DR 側が
  持つ** (= super-reducer 入口で 1 tap、bytes-level record と event sourcing は別 sink、
  両者の構造的役割を明示)
- **DR-0001 (jobcontrol = axis 1 のみ)**: axis 1 (= 子の self-stop に対する `notify` /
  `auto-resume`) のみが Child reducer の internal state machine に該当。**axis 2 (=
  parent suspend、`transparent` / `decouple`) は DR-0015 で廃止済**、本 DR で再建しない
- **DR-0015 (run = fork + attach)**: Client/Serve lifecycle と Child reducer の事前条件
  (= daemon は setsid 済、attach は別 process) を継承
- **DR-0017 (session anchor 構造)**: Child reducer の事前条件 (= openpty + 手動 fork +
  TIOCSCTTY) を継承
- **DR-0019 (OnChildSuspend policy)**: Child reducer 内部 state (=
  ChildState::Stopped { reason } 時の policy) として継承

## Consequences

### 良い影響

- **daemon 内部 race の構造的不可能性**: 各 reducer が単一の receive loop で 1 件ずつ
  処理 → daemon 内部 state (= lock state / client registry / screen state) の writer 散在
  に起因する race を解消 (= 「borrow checker 偶然依存」から脱却)。**子プロセス起因 race**
  (= 子の read/write atomicity、§Context 元 flaky の真因) **は本 DR では解消対象外**、
  別作業で対処
- **testability の劇的向上**: 各 reducer 単独 unit test で PTY 不要、message 列で全 race /
  lock / edge case を厚く検証可能。**ただし PTY/signal/子プロセスの実 IO 検証は引き続き
  integration test として残る** (= reducer 化は test の万能薬ではない、適用範囲を限定)
- **観測性 + 部分的 replay**: 全 message が 1 箇所を通る → record / trace は自然。
  **replay は reducer 内部 state の deterministic check に限定** (= 外部 IO 状態 = PTY fd
  番号 / client socket / child pid は再現不可、full session replay はできない、bug report
  での reducer 状態だけ再現が現実的価値)
- **責務分離**: 6 domain の境界が明確、cross-domain 依存は super-reducer 層で表現 +
  許可方向グラフ + queue 深度上限で明示
- **state machine の可読性**: 各 domain の state 遷移が enum + reducer 関数で直接読める
- **TTY 機能の網羅追跡**: enum カタログ + scoreboard で実装漏れ・規格対応状況が一覧化、
  doc comment 規約は supported/partial のみ厳格化、stub/planned は緩和でドキュメント負債
  最小化
- **AI 推測列挙との区別**: 規格名 + 略称義務化 + `# Verified:` marker により規格出典の
  追跡可能性が担保
- **watch 機構の API 単純化**: region / matcher / flow の 3 軸 (= 概念的直交、合成時の
  semantic 衝突は明示)、ad-hoc な trigger 列挙を排除、reactive operator として確立された
  パターンを採用

### コスト・リスク

- **rewrite scope 大**: 既存 handler 全面書き直し。Phase 1-8 で数千行影響想定
- **カタログ enum 150-200 variant の維持コスト**: 出典 link rot のメンテ、新規 terminal 機能
  の追跡コスト (ただし規格名義務化で長期信頼性は確保)
- **breaking change**: v1.0 未満なので許容方針 (CLAUDE.md memo 参照)。外部 protocol は
  DR-0008 に従い別軸、本 DR の breaking は daemon 内部 + test harness に閉じる
- **Phase 1 では race の構造的解消は実現しない**: API 切り直しが先、Phase 2 以降の domain
  単位 reducer 化で初めて race 解消が成立する

## Alternatives

### A. 個別 handler 維持 + lock 強化のみ

不採用理由: lock 1 軸の強化では他 domain (= child / serve / client / screen / tty) の散在
問題が残る。設計哲学レベルの統一性が得られない。

### B1. tokio actor framework

不採用理由: hyoui は async runtime に依存しない設計 (= `nix::poll` ベース)。actor framework
導入は依存関係増 + 既存 sync I/O 路と二重化。

### B2. crossbeam-channel ベースの軽量 actor (= minimalist actor)

検討対象として B1 と分離。`crossbeam-channel` + 1 thread 1 receive loop の minimalist
actor は依存追加なしで Rust idiom と整合。layered reducer との比較:

| 軸 | layered reducer | minimalist actor |
|---|---|---|
| ordering 制御 | super-reducer 内 cross-domain queue で明示 | actor 間 channel ordering 依存 |
| borrow 戦略 | `&mut DaemonState` の split borrow (= 各 domain field 独立) | actor 間 owned state、channel で値 move |
| alloc cost | `Vec<Effect>` 返却頻度高、SmallVec / arena / output param `&mut Vec<_>` で抑制可 | message 毎 alloc + send overhead |
| test 容易性 | reducer 関数を直接呼び unit test | actor を thread spawn せず channel mock で test 可 |
| 実装複雑度 | super-reducer routing + queue 1 個 | actor 数だけ thread + channel + lifecycle 管理 |

不採用理由: hyoui は 1 daemon = 1 session で並列性需要が高くない、ordering 決定論性の
方が価値が高い。`Vec<Effect>` alloc は SmallVec/arena/output param で抑制 (= 実装段階で
benchmark 計測)。

### C. 単一 super-reducer (= domain 分割なし)

不採用理由: state が巨大化、visibility 悪い。modular 化のため domain 分割は必須。

### D. 複数 sub-machine が独立 thread で message channel 連携 (= 純 actor)

不採用理由: sub-machine 間の event ordering が channel に依存、決定論性が低下。reducer
合成 (= layered reducer) の方が ordering 制御が容易。

### E. 他 terminal multiplexer (tmux / screen / wezterm) の構造踏襲

不採用理由: hyoui の責務は terminal multiplexer ではなく「外部自動操作主軸の透過 PTY ラップ」
(DR-0005)。tmux 的な構造は overkill。ただし tmux の `cmd_queue` (= cross-domain queue
相当) と wezterm の internal protocol (= effect feedback 相当) から構造的教訓を取り込み
(= 本 DR の cross-domain dispatch protocol + Effect layer に反映済)。

### F. vt100 crate 維持 (= TTY parser は自作せず)

DR-0013 で採用済の vt100 crate をそのまま使い続け、本 DR の TTY parser pipeline は薄い
adapter のみ書く案。

不採用理由 (= 暫定): vt100 は **cell hook を提供しない** (= write event 発火が困難)、
Layer 2/3 の semantic 化が adapter 側で 2 度手間。ただし **完全自作は scope 大** なので
**Phase 5 着手前に prototype で fork/diff 抽出の feasibility 検証** が必要 (= Q-NEW1。
ScreenWriteEvent の粒度が cell hook 可否に依存するため Phase 6a でなく Phase 5 の blocker)。

### G. vt100 crate を Layer 2 として wrap + 独自 Layer 3

最有力候補。vt100 を Layer 2 (= parsed event) として使い、本 DR の Layer 3 (= semantic
event) と watch / cell hook 機構は独自実装で被せる案。

採用方向 (= 暫定、Phase 5 着手前の Q-NEW1 prototype で確定。Layer 2/3 境界判断 (Q1) は
別軸で Phase 6a のまま): 既存 DR-0013 の vt100 採用と整合、独自実装範囲を最小化
しつつ拡張性を確保。enum カタログ 150-200 variant の内訳:

- vt100 が提供: ~80 variant (= ECMA-48 主要 + 主要 OSC)
- 自前で増やす: ~70-120 variant (= xterm 拡張 / DEC private / iTerm2 / kitty / 業界提案)
- 規格 ref 付け直しのみ: ~80 variant (= vt100 提供分に doc comment を付与)

### H. Serve domain を super-reducer のトップレベル lifecycle 制御に吸収

Serve は 4 状態 + ShutdownReason 4 種のみで他 domain event の集約 dispatcher 的責務しか
持たない。独立 reducer に切り出すと cross-domain 翻訳が冗長。

不採用理由 (= 暫定): Serve も「shutdown 調停」「全 client への shutdown notify」等の状態
遷移ロジックを持つので独立 domain として残すのが筋。ただし将来 Serve が ChildEvent や
ClientEvent の単純な再 dispatch しかしないと判明したら統合検討。

## Anti-patterns 防止 self-check

DR-0014 §self-check に **段階的に取り込み** すべき項目。各項目は **対応 Phase 完了時に
DR-0014 §self-check に反映** する (= design-impl-bidirectional-check 規律、CLAUDE.md →
DR-0014 だけ読む後続セッションが本 DR の項目を踏むため)。

### Phase 非依存 (= 本 DR Active 化と同時に DR-0014 反映)

- [ ] polling / interval check を導入する際、event-driven で代替できないか確認したか?
- [ ] 新 protocol kind を追加する際、本 DR の写像規約表に行を追加したか?
- [ ] 新規 TTY 由来 event を扱う際、対応する enum variant が catalog に存在するか? (= 段階
      整備の scoreboard 漸進、Phase 6b の恒久タスク)
- [ ] supported / partial 変更時に doc comment の規格名 + 略称 + `# Verified:` marker が
      完備されているか?
- [ ] reducer 内で非決定要素 (`SystemTime::now` / `rand` / `HashMap` iteration 順 /
      fd readiness 順) を使っていないか?

### Phase 2 (Client) 完了後

- [ ] machine 外から client registry を直接操作していないか?
- [ ] 新規 client req を追加する際、対応する `ClientEvent` variant 追加経由か?
- [ ] Transport / Auth / Backpressure の sub-state が Client domain 外に漏出していないか?

### Phase 1a / 2 (Lock) 完了後

- [ ] machine 外から `LockState` の holder / token を mutate していないか?
- [ ] Lock reducer が ClientId 以上の client state を参照していないか? (= 弱結合維持)

### Phase 3 (Child) 完了後

- [ ] machine 外から child pid に kill / signal していないか?
- [ ] signal handler が self-pipe 以外の経路で state を変更していないか?
- [ ] DR-0001 axis 1 / DR-0017 anchor / DR-0019 OnChildSuspend invariant が ChildState
      内部に formal 化されているか?

### Phase 5-7 (Screen / Tty / Watch) 完了後

- [ ] machine 外から PTY master fd を read / write していないか?
- [ ] machine 外から screen state を mutate していないか?
- [ ] watch 関連の機能追加で region / matcher / flow の 3 軸のどれかに属するか明確か?
- [ ] DR-0013 byte-base tail と rows-base screen の分離が保たれているか?
- [ ] DR-0016 record と event sourcing の二重 record が発生していないか?

## Open Questions

各 question に **Phase 依存タグ** を付与。`[本文に解決済]` は本 revise で本文 Decision に
昇格したもの (= 残置 question から外す)。

### `[本文に解決済]` (= 本 revise で Decision 昇格)

- ~~Q2 (ClientRequest と protocol の対応)~~: §DR-0008 protocol との接続規約 を新設、default
  1:1 mapping + 例表
- ~~Q4 (reducer concurrency)~~: §IO boundary と reducer boundary の関係 を新設、単一 thread
  採用 + Parser Layer 1 例外
- ~~Q5 (record sink との関係)~~: super-reducer 入口で tap する方針を本文に明記
  (Consequences §観測性)、二重 record しない設計責任を本 DR 側が持つ
- ~~Q6 (error handling)~~: §Effect layer の pending state パターン、effect 失敗が
  `DaemonMsg::EffectResult` で reducer に feed-back される設計に集約
- ~~Q7 (coexistence 期間)~~: §migration plan の coexistence 期間と feature flag、即削除 default
- ~~Q8 (cross-domain event 伝播)~~: §cross-domain event 伝播 protocol を新設、queue +
  有向グラフ + borrow 戦略
- ~~Q10 (WASM matcher sandbox)~~: `Matcher::Wasm` 自体を別 DR に外したため本 DR では不要
- ~~Q11 (watch dedup)~~: default 方針「別 WatchId = 別 register = 別 event、dedup は client
  責務」を本文 §Screen domain に明記
- ~~Q12 (watch 配信先)~~: default「register した client にのみ配信」を本文 §Screen domain
  event に明記
- ~~Q-NEW2 (super-reducer 入口の channel capacity / overflow policy)~~: §Phase 1b 後半の
  実装形 に解決を記載 — 単一 thread poll 直結を維持し channel 自体を導入しない。必要に
  なるのは Phase 6a の Parser Layer 1 別 thread 化のみ

### `[Phase 5 着手前 blocker]`

- **Q-NEW1 (vt100 crate Alternative G の feasibility)**: vt100 fork / diff 抽出 /
  per-call coarse-grained のどれで cell hook を実現するか prototype 検証。Phase 5 の gate
  が ScreenWriteEvent 配信 test を含み、vt100 は cell hook を提供しない (= Alternative F)
  ため、**Phase 6a でなく Phase 5 の着手前 blocker** (= 未解決のまま入ると ScreenWriteEvent
  の粒度設計が手戻りする)

### `[Phase 6a 着手前 blocker]`

- **Q1 (Layer 3 semantic と Layer 2 parsed の境界)**: どの event を意味化するか、どこから
  raw parsed のまま reducer に渡すかの判断基準。Phase 6a の prototype で測定して確定

### `[Phase 7 着手前 blocker]`

- **Q9 (watch matcher の重い処理対策)**: `Matcher::Regex` の compile を register 時に 1 回
  だけ cache する (= default)、event 発火毎は再 compile しない (= 既定)

### `[Phase 内詳細、Phase 着手時に詰める]`

- **Q3 (InternalTimer の実装方式) [Phase 2-3]**: poll timeout 経由で代替するか、専用 timer
  wheel を持つか。時間も message として reducer に注入する方針 (= TimerTick / Clock 値を
  caller が message で渡す) を採用、test の決定論性を担保
- **Q-NEW3 (reducer panic 時の隔離方針) [Phase 1a/1b]**: catch_unwind 有無 / 影響範囲 /
  state rebuild 方法、CLAUDE.md の partial state 規律と直接ぶつかる可能性
- **Q-NEW4 (DR-0013 Phase C との依存関係) [Phase 5-7]**: DR-0013 Phase C (observe mode /
  multi-client resize / reflow / zstd) と本 DR Phase 7 の依存関係を明確化

### `[後続 DR で再起票可]`

- **`Matcher::Wasm` 採用是非**: 本 DR から外し別 DR で起票 (= 実機で Literal/Regex で
  困った事例の集積が条件)
- **EventFlow operator 拡張** (= Sample / Buffer / Window 等): 必要性が出てから enum 追加
- **WebUI / 非 unix socket transport 接続**: 本 DR の Client domain Transport sub-state が
  「transport が何か knowing しない reducer」を実現することで、WebSocket / SSE / HTTP
  long-poll 等の追加 transport は **新規 Transport variant 追加 + encoder layer (= CBOR /
  JSON / MessagePack) のみで実現可能** (= reducer 本体は無変更)。考慮点:
  - HTTP REST API として薄く被せる場合は「register → poll」モデルが必要、WebSocket / SSE
    なら不要
  - binary frame vs JSON frame の encoder を Transport sub-state に持たせる
  - cell 単位差分配信は `ScreenWriteEvent + ScreenLayer` の組み合わせで素直
  - auth (= Origin 認証 / Bearer token / OAuth) は Client domain の Auth sub-state を
    拡張するだけ、他 domain は無影響
  - multi-client (= WebUI + CLI 同時接続) は ClientRegistry が leader/follower / cap
    negotiation を持つので reducer 責務範囲内
  - 別 DR で起票時、本 DR の 6 domain reducer 構造の上に WebUI 専用 transport adapter +
    auth 拡張を被せる形になる予定 (= 本 DR を前提整備として位置付け)
