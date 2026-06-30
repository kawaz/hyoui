# DR-0025: Daemon Reducer 化と全ドメイン event の形式化

- Status: Draft
- Date: 2026-06-30
- Related: DR-0014 (透過原則 + 検証主義、本 DR で実装レベル強化), DR-0008 (外部 protocol、本 DR は daemon 内部設計で別軸), DR-0013 (screen state 正本、本 DR の Screen reducer に該当), DR-0022 (input invocation auto-lock、本 DR の Lock reducer で構造的に再整理), DR-0016 (TTY IO record、本 DR の全 event を統一的に record する自然延長), DR-0001 (jobcontrol 2 軸、本 DR の Child state reducer に該当)
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
5. **event sourcing**: 全 message を順番に record すれば state を replay 可能。debug / test /
   bug report で再現性を担保
6. **3 軸直交設計**: 例えば watch なら region (= どこ) / matcher (= 何) / flow (= いつ /
   どう流すか) の 3 軸を独立に組み合わせ可能にする (= ad-hoc な trigger 列挙を排除)

## ドメイン分割

daemon 内部を 6 domain に分け、各 domain reducer を持つ:

| reducer | 責務 | 既存対応 |
|---|---|---|
| **TtyParserPipeline** | byte stream → parsed event → semantic event の 3 layer | DR-0013 screen state を内蔵 |
| **ChildStateReducer** | 子プロセス state machine + lifecycle event | DR-0001 jobcontrol 2 軸 |
| **ServeStateReducer** | serve 自身の lifecycle + 内部 metric | (新規、現状散在) |
| **ClientRegistry** | client 集合 / lifecycle / leader-follower | (新規、現状 `clients: Vec<_>` 直接操作) |
| **ScreenReducer** | 仮想 screen state + watch (= region / matcher / flow) | DR-0013 を中核に拡張 |
| **LockReducer** | lock state / token / process-bound GC | DR-0022 (input auto-lock) |

super-reducer は **単純なルータ**:

```rust
fn handle(state: &mut DaemonState, msg: DaemonMsg) -> Vec<DaemonEvent> {
  match msg {
    DaemonMsg::Tty(e)    => tty::reduce(&mut state.tty, e),
    DaemonMsg::Child(e)  => child::reduce(&mut state.child, e),
    DaemonMsg::Serve(e)  => serve::reduce(&mut state.serve, e),
    DaemonMsg::Client(e) => client::reduce(&mut state.clients, e),
    DaemonMsg::Screen(e) => screen::reduce(&mut state.screen, e),
    DaemonMsg::Lock(e)   => lock::reduce(&mut state.lock, e),
  }
}
```

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

**必須**: 略称 + フル名 + 規格名 + section + 実装状態 marker
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
責務ではない。

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

WatchRegion {
  /// 監視範囲。指定なし (= None) なら「全体」
  rows: Option<Range<u32>>,
  cols: Option<Range<u32>>,
  layer: ScreenLayer,
}
```

### Matcher (= 段階的拡張可能)

```rust
enum Matcher {
  /// Phase 1: literal substring
  Literal { needle: String, case_sensitive: bool },

  /// Phase 1: regex (RE2 互換)
  Regex { pattern: String },

  /// Phase 2 (planned): user-defined WASM closure
  /// matcher 内部で region 内の更に部分領域に対する判定 (= 「region 内の [x1,y1,x2,y2]
  /// に赤色 cell があるか」等) を user が TS で書いて自前 build して WASM 化して使う
  Wasm { module: WasmModule, entry_point: String, args: WasmArgs },
}
```

### EventFlow (= 既知の reactive operator)

`ScreenWriteEvent` を matcher にどう流すか。汎用 operator として:

```rust
enum EventFlow {
  /// write event のたび matcher を実行 (= 無加工)
  Immediate,

  /// 連続する write を一定間隔に間引く (= 高頻度 write 時の matcher 実行を抑制)
  /// throttle(N ms): N ms に最大 1 回 matcher 実行、超過分は drop
  Throttle { interval_ms: u64 },

  /// write が一定時間止まるまで matcher 実行を遅延 (= 描画安定後に判定)
  /// debounce(N ms): 最後の write から N ms 後に matcher 実行、その間の write は捨てて
  /// 1 回だけ
  Debounce { idle_ms: u64 },

  /// write event を queue に積み、matcher 実行は別 worker (= matcher の重さを screen
  /// 側に影響させない)
  Queue { capacity: usize, overflow: OverflowPolicy },
}
```

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
  Literal { location: (u16, u16), text: String },
  Regex { location: (u16, u16), full_match: String, captures: Vec<String> },
  Wasm { payload: Vec<u8> },  // WASM 側が返した opaque payload
}
```

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

DR-0001 jobcontrol 2 軸の現行実装をこの state machine に整理し直す。

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

## 段階的 migration plan

domain ごとに段階分け:

| Phase | 内容 | domain |
|---|---|---|
| **Phase 1** | super-reducer 骨格 + 6 domain reducer のシグネチャ定義 (= 中身は既存 handler を wrap) | 全 domain |
| **Phase 2** | Lock domain reducer 化 + Lock event を pure 化 | Lock |
| **Phase 3** | Client registry reducer 化 + ClientEvent 整理 | Client |
| **Phase 4** | Child state machine 化 + ChildEvent 整理 | Child |
| **Phase 5** | Serve lifecycle 整理 | Serve |
| **Phase 6** | TTY parser pipeline + enum カタログ (= Layer 1-3 独立 pipeline 構築) | Tty |
| **Phase 7** | Screen reducer + watch (region/matcher/flow) 実装 | Screen |
| **Phase 8** | 既存 integration test を「reducer message 列の record assert」に書き換え | test |

Phase 2-5 は比較的 scope 小、Phase 6-7 は規模大 (= enum カタログ 150-200 variant)。
各 Phase 完了後に既存 integration test が全 pass することを gate に進める。

## 既存 DR との関係

- **DR-0014 (透過原則 + 検証主義)**: 本 DR は実装レベルでの強化。「machine 以外は fd /
  signal を直接触らない」原則を self-check に追加
- **DR-0008 (protocol)**: 外部 protocol (= client ↔ daemon の CBOR framing)。本 DR は
  **daemon 内部の reducer 構造**で別軸。`ClientRequest` enum は protocol message と 1:1
  対応させるか複数集約するかは Phase 1 で詳細詰める
- **DR-0013 (screen state 正本)**: 本 DR の Screen reducer / TTY parser Layer 3 の正本化層
- **DR-0022 (input invocation auto-lock)**: 本 DR の Lock reducer で構造的に再整理。
  auto-lock の効果境界 (= input 直列化までで output は別軌道) を仕様レベルで明文化する
  根拠を提供
- **DR-0016 (TTY IO record)**: 本 DR の event sourcing の自然延長。全 reducer の event を
  1 箇所で record できる
- **DR-0001 (jobcontrol 2 軸)**: Child reducer の state machine で formal 化、invariant が
  state 遷移図として直接読める形に

## Consequences

### 良い影響

- **race の構造的不可能性**: 各 reducer が単一の receive loop で 1 件ずつ処理 → atomic が
  設計として保証 (= 「borrow checker 偶然依存」から脱却)
- **testability の劇的向上**: 各 reducer 単独 unit test で PTY 不要、message 列で全 race /
  lock / edge case を厚く検証可能
- **観測性**: 全 message が 1 箇所を通る → record / trace / replay が自然
- **責務分離**: 6 domain の境界が明確、cross-domain 依存は super-reducer 層で表現
- **state machine の可読性**: 各 domain の state 遷移が enum + reducer 関数で直接読める
- **TTY 機能の網羅追跡**: enum カタログ + scoreboard で実装漏れ・規格対応状況が一覧化
- **AI 推測列挙との区別**: 一次情報リンク義務化により規格出典の追跡可能性が担保
- **watch 機構の API 単純化**: region / matcher / flow の 3 軸直交、ad-hoc な trigger 列挙
  を排除、reactive operator として確立されたパターンを採用

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

### B. tokio actor framework

不採用理由: hyoui は async runtime に依存しない設計 (= `nix::poll` ベース)。actor framework
導入は依存関係増 + 既存 sync I/O 路と二重化。**手動 reducer + channel で十分**。

### C. 単一 super-reducer (= domain 分割なし)

不採用理由: state が巨大化、visibility 悪い。modular 化のため domain 分割は必須。

### D. 複数 sub-machine が独立 thread で message channel 連携 (= 純 actor)

不採用理由: sub-machine 間の event ordering が channel に依存、決定論性が低下。reducer
合成 (= layered reducer) の方が ordering 制御が容易。

### E. 他 terminal multiplexer (tmux / screen / wezterm) の構造踏襲

不採用理由: hyoui の責務は terminal multiplexer ではなく「外部自動操作主軸の透過 PTY ラップ」
(DR-0005)。tmux 的な構造は overkill。**reducer + 6 domain** という最小構成が hyoui の責務
範囲に最適。

## Anti-patterns 防止 self-check

DR-0014 §self-check に追加すべき項目 (= 後続 DR で正式化):

- [ ] machine 外から PTY master fd を read / write していないか?
- [ ] machine 外から child pid に kill / signal していないか?
- [ ] machine 外から `LockState` の holder / token を mutate していないか?
- [ ] machine 外から client registry を直接操作していないか?
- [ ] machine 外から screen state を mutate していないか?
- [ ] signal handler が self-pipe 以外の経路で state を変更していないか?
- [ ] 新規 client req を追加する際、対応する `ClientRequest` enum variant 追加経由か?
- [ ] 新規 TTY 由来 event を扱う際、対応する `TtyEvent` variant が enum カタログに存在するか?
- [ ] 新規 variant に「規格名 + 機能名 + 略称」doc comment が完備されているか?
- [ ] watch 関連の機能追加で region / matcher / flow の 3 軸のどれかに属するか明確か?
- [ ] polling / interval check を導入する際、event-driven で代替できないか確認したか?

## Open Questions

DR draft 段階で未決、後続議論で詰める:

1. **Layer 3 (semantic) と Layer 2 (parsed) の境界**: どの event を意味化するか、どこから
   raw parsed のまま reducer に渡すかの判断基準
2. **`ClientRequest` と `ControlMessage` (protocol) の対応**: 1:1 か、複数集約か
3. **InternalTimer の実装方式**: poll timeout で代替するか、専用 timer wheel を持つか
4. **reducer の concurrency**: 単一 thread reducer で十分か、parser pipeline を別 thread に
   分けるか
5. **既存 record sink (DR-0016) との統合**: 全 message を record すれば既存 record sink は
   reducer internal に吸収されるか、別軸で残すか
6. **error handling**: reducer が `Result` を返すか、`Vec<DomainEvent>` に error variant を
   含めるか
7. **migration 期間中の coexistence**: Phase 1 で既存 handler を wrap した API を出すとき、
   旧 handler は deprecate するのか並列維持するのか
8. **cross-domain event の伝播**: child reducer が出した `ChildEvent::Exited` を screen
   reducer / client reducer が受け取る経路の詳細
9. **watch matcher の重い処理対策**: `Matcher::Regex` の compile / `Matcher::Wasm` の
   instantiate を register 時に 1 回だけやって cache するか、event 発火毎にやるか
10. **WASM matcher の sandbox**: CPU / メモリ / 実行時間制限の規定
11. **複数 watch register の重複処理**: 同一 region + 同一 matcher を 2 回 register したら
    2 event 発火か、daemon 側で dedup か
12. **watch event の配信先**: register した client にのみ配信か、全 client broadcast か、
    explicit subscribe か
