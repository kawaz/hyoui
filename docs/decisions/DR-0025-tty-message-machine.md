# DR-0025: TTY Message Machine 化と TTY event の網羅 enum カタログ

- Status: Draft
- Date: 2026-06-30
- Related: DR-0014 (透過原則 + 検証主義、本 DR で実装レベル強化), DR-0008 (外部 protocol、本 DR は daemon 内部設計で別軸), DR-0013 (screen state 正本、本 DR の Layer 3 該当), DR-0022 (input invocation auto-lock、本 DR で構造的に再整理), DR-0016 (TTY IO record、本 DR の event sourcing の自然延長)
- Origin: `parallel_input_serialized_by_auto_lock` 試験 (2026-06-29 セッション) で発見された race 構造の根本原因調査。lock 状態管理と raw_data 配信が「論理軸として分離されてない」点が、kawaz との設計議論で「単一 machine 化 + 全 IO を message に統一」の方向に収束

## Context

`crates/hyoui-cli/tests/input_auto_lock_cli.rs::parallel_input_serialized_by_auto_lock` が
workspace 並列実行で flaky な現象を調査した結果、**lock 実装そのものに bug は無いが、
lock の効果範囲と test の検証範囲がミスマッチ**していることが判明した:

- lock は **client → master fd への write を直列化**する
- だが test は **PTY 上に echo された出力 (= screen dump)** で順序を検証している
- 子プロセス (`/bin/cat`) の `read(2) → write(2)` は kernel scheduler 依存で atomic でない
- 高負荷時に `cat` の line read が分割されると、A の出力途中で B の bytes が割り込みうる
- これは「lock = input 直列化、output 直列化までは保証しない」という仕様の境界

調査の過程で、より深い構造的脆さが見えた:

1. **lock state (`SessionState.lock_holder`) と PTY write (`master_fd.write_all_with_idle_timeout`) が
   論理軸として分離されてない**: 同じ `serve_loop` 内で `&mut SessionState` の借用排他により
   偶然 race してないだけで、「lock 取得と write の atomic 性」が設計として保証されていない
2. **TTY 由来 event は 12+ カテゴリ以上に散在**: 物理 ioctl / ANSI 制御 / OSC / DCS / mouse /
   paste / signal / 子 lifecycle / flow control 等の入り口がそれぞれ別 handler
3. **test が PTY 副作用込みでしか書けない**: machine の意味的振る舞いを検証したいのに、
   PTY と子プロセスの output を経由しないと test が組めない

これらは個別 PR で fix できる類ではなく、**daemon 内部の責務分離設計** の問題。

## 問題の本質

- **single-writer principle の欠如**: TTY fd / child pid / lock state の writer が複数 handler に散在
- **event ordering が偶然依存**: borrow checker の借用排他で「たまたま」serial になってるだけで、
  設計レベルでの atomic 保証ではない
- **testability の低さ**: machine の意味的挙動 (= 「lock 中の write_req は他 client に対し reject」)
  を unit test するために PTY と子プロセスを起動する必要がある
- **「lock の効果境界」の曖昧さ**: 「lock は input 直列化だけで output は別軌道」を仕様レベルで
  明文化していない (= test 設計のミスマッチの根本原因)

## 設計哲学

1. **全 IO を message に統一**: client req / TTY 由来 / signal / child lifecycle / timer の
   すべてを `MachineMsg` enum の variant として扱う
2. **reducer は pure function**: `fn handle(state: &mut MachineState, msg: MachineMsg) ->
   Vec<MachineEvent>` のような純粋関数。reducer 自身は IO を持たない (= 100% unit test 可能)
3. **machine 以外は TTY fd / child pid / signal handler / lock state を直接触らない**:
   例外を作る場合は DR で justify する (= DR-0014 self-check に新規項目として追加)
4. **event sourcing**: 全 message を順番に record すれば state を replay 可能。debug / test /
   bug report で再現性を担保

## message レイヤ構造

TTY 由来の byte stream を 1 個の message にせず、**3 layer に分けて段階的に意味化** する:

```
[Layer 1: raw bytes]
   ↓ (IO boundary、PTY master からの read 結果)
RawBytes { bytes: Vec<u8> }

[Layer 2: parsed event]
   ↓ (vt parser、ECMA-48 / OSC / DCS の syntax 単位に分解)
TtyEvent::PrintableChar { ch, attrs }
TtyEvent::CsiEd(mode)
TtyEvent::OscWindowTitle(title)
TtyEvent::DcsSixel { data }
... (150-200 variant、規格 1:1 対応)

[Layer 3: semantic event]
   ↓ (Layer 2 を意味的に解釈、state machine の入力単位)
SemanticEvent::ModeAltScreenEnter
SemanticEvent::ClipboardSetRequest { selection, payload }
SemanticEvent::CursorMovedTo { row, col }
... (Machine が直接扱う粒度)
```

- Layer 1 → 2 → 3 は **独立した pipeline で各層が単体 test 可**
- Machine reducer は Layer 3 を入力に取る (= byte parsing と意味解釈を持たない)
- alacritty / wezterm の vt parser 設計と整合

## MachineMsg 最上位分類 (= 7 軸)

```rust
enum MachineMsg {
  /// TTY 由来 event (= 子プロセス → master fd → parser → semantic 化済)
  Tty(SemanticEvent),

  /// 物理 TTY 制御 (= ioctl / termios、syscall 直)
  Ioctl(TtyIoctlMsg),

  /// flow control 観測 (= XOFF/XON / POLLOUT pressure / idle timeout)
  FlowCtl(FlowControl),

  /// 子プロセス lifecycle (= fork / exec / stopped / continued / exited / hangup)
  Child(ChildLifecycle),

  /// serve 自身が受信した signal (= SIGTERM / SIGHUP / SIGINT / SIGCHLD 等)
  Signal(SignalToServe),

  /// client → daemon の要求 (= write / lock / signal / resize / status / kill / record 等)
  ClientReq(ClientRequest),

  /// 内部 timer (= deadline / heartbeat / lock acquire timeout 等)
  Timer(InternalTimer),
}
```

各軸の variant は **規模が 150-200 に達する見込み** のため、別 module
`crates/hyoui/src/daemon/machine/event_catalog/{tty,ioctl,flow,child,signal,client,timer}.rs` に
分離する。

## enum variant の必須 doc comment 規約

カタログとして信頼性を持たせるため、**全 variant に「規格名 + 機能名 + 略称」を義務化** する。
URL は補助 (任意)。AI 推測列挙との区別、規格の出典追跡、隣接機能の発見漏れ防止が目的。

### テンプレ

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

**必須要素**:

- **略称 / フル名 / 1 行説明** (= 検索のキー)
- **規格名 + section** (= URL が切れても検索で再到達できる本体情報)
- **実装状態 marker**

**任意要素**:

- URL (= 便利だが義務ではない)
- 引数仕様詳細
- 参考実装 link
- DR 参照 / false-positive 注意点

### 実装状態 marker の意味

| marker | 意味 |
|---|---|
| `[supported]` | 完全実装、test カバー済 |
| `[partial]` | 一部実装、TODO あり |
| `[stub]` | enum に load のみ、未実装で no-op (= 受信は検出するが効果なし) |
| `[planned]` | 設計検討中、enum load 自体未済 (= 将来追加候補として doc にのみ記載) |

### 一次情報源 (= 出典として優先する順)

| 規格・出典 | 対応範囲 | 補足 |
|---|---|---|
| **ECMA-48** | C0/C1 / CSI / SGR / ED / EL / CUU/D/F/B / CUP / SCS 等 ANSI 規格 | 5th ed (1991) が最新公式、無料 PDF |
| **ISO/IEC 6429** | ECMA-48 と同内容 (ISO 版) | 有料、ECMA-48 で代替 |
| **ECMA-35** | character code structure / SS2/SS3 / G0-G3 | 6th ed (1994) |
| **DEC VT5xx manuals** | DEC private mode (DECSET 等)、ESC 拡張、DCS 系 | vt100.net で全 manual archive |
| **xterm ctlseqs** | xterm 独自拡張 + 業界 de facto の集大成 | invisible-island.net (Thomas Dickey 維持) |
| **iTerm2 proprietary** | OSC 1337, image protocol 等 | iterm2.com/documentation-escape-codes.html |
| **kitty protocol** | kitty keyboard / graphics protocol / OSC 拡張 | sw.kovidgoyal.net/kitty/ |
| **VSCode shell integration** | OSC 633 | code.visualstudio.com docs |
| **tmux** | DCS tmux passthrough, mouse 拡張 | tmux man page / wiki |
| **Sixel / ReGIS** | DEC graphics | DEC VT2xx/VT3xx manual |
| **terminfo** | capability name の正規定義 | ncurses 配布 / `man terminfo` |
| **個別提案 gist / 議論** | 業界提案 (sync update / CSI u 等) | URL 変化リスクあり (= 検索で再到達できる名前情報を必ず併記) |

### Link rot 対策

**情報の本体は「規格名 / 機能名 / 略称」、URL は補助**として扱う。

- variant の doc comment に **規格名 (= ECMA-48 §X.Y / xterm ctlseqs / DEC VT510 等) + 機能名
  + 略称** が書いてあれば、URL が切れても読者は即座に検索で再到達できる
- archive.org 併記や weekly link-check CI のような先回り対策は **規格名さえあれば不要** な
  オーバーキル。実害が出てから対処すれば足りる (= 規格そのものが消滅するわけではないので
  情報の確定性は失われない)
- URL は「便利な direct link」程度に留め、義務化はしない

### 例 (= 規約に従った variant の見本)

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
/// Pc = selection (c=clipboard / p=primary / s=select / 0-7=cut buffer)、
/// Pd = base64-encoded payload または `?` (= query)。
///
/// **規格**: xterm 独自拡張 (= 公式 ANSI/ECMA-48 規格には含まれない)
/// - xterm ctlseqs: <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands>
///   (= "OSC Ps ; Pt ST" の Ps=52 section)
///
/// **採用 terminal**:
/// - tmux: <https://github.com/tmux/tmux/wiki/Clipboard>
/// - kitty: <https://sw.kovidgoyal.net/kitty/protocol-extensions/#osc-52>
/// - alacritty: PR #1450
///
/// **DR-0014 false-positive 例**: 巨大 paste で partial state を踏みやすい
///
/// **実装状態**: [planned]
OscClipboard { selection: char, payload: String },

/// CSI ? 2026 h/l — Synchronized Update Mode
///
/// `?2026 h` で begin sync、`?2026 l` で end sync。
/// 描画 atomic 性のため tearing を避ける目的。
///
/// **規格**: Application-private mode、ANSI/ECMA-48 規格外。提案文書のみ:
/// - <https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036>
///   (= contour terminal 作者の提案、2020)
///
/// **採用 terminal**: contour, foot, kitty, mintty, wezterm, iTerm2,
///   alacritty (= 不完全)
///
/// terminfo `Sync` capability で表現される (ncurses 6.5+)
///
/// **DR-0014 false-positive 例**: ネスト sync 中の長時間 → 自動破棄誤判定リスク
///
/// **実装状態**: [planned]
ModeSyncUpdateEnter,
```

## scoreboard 運用

カタログを「実装進捗 scoreboard」として運用:

```bash
# 集計: 各 marker の出現数
rg -o '\[(supported|partial|stub|planned)\]' \
   crates/hyoui/src/daemon/machine/event_catalog/ | \
  awk -F: '{print $2}' | sort | uniq -c

# 例: 特定 marker の variant を一覧
rg -B 5 '\[planned\]' crates/hyoui/src/daemon/machine/event_catalog/
```

新機能検討時のワークフロー:

1. 関連 enum に variant があるか grep
2. なければ規格を一次情報から調査して追加 (= 出典 link 付き doc comment 完備で)
3. 実装段階で marker を `[planned] → [stub] → [partial] → [supported]` に推移

将来は `cargo doc` の出力 / 別 `docs/tty-coverage.md` を自動生成して
「実装率 scoreboard」として公開も検討。

## 段階的 migration plan

本 DR で全体方向を確定、実装は段階分け:

| Phase | 内容 | 性質 |
|---|---|---|
| **Phase 1** | 既存 handler を `TtyMachine::handle(msg)` API に切り直し (中身そのまま、API だけ変える) | API rewrite |
| **Phase 2** | lock state / PTY fd / child pid を machine field に閉じる (= 外から `&mut SessionState` を渡してた経路を遮断) | encapsulation |
| **Phase 3** | machine 単独 unit test を厚く書く (PTY 不要、message 列で検証) | test 強化 |
| **Phase 4** | 既存 integration test を「machine message 列の record assert」に書き換え (= screen dump 検証から脱却) | test 移行 |
| **Phase 5** | parser layering (= Layer 1 → 2 → 3 の独立 pipeline 構築) | 後続 DR-B 起票候補 |
| **Phase 6** | message enum 確定 + 外部 protocol (DR-0008) との変換層詳細 | 後続 DR-C 起票候補 |

Phase 1-2 は本 DR の scope、Phase 3-4 は test 設計改修、Phase 5-6 は別 DR で詳細詰める。

## 既存 DR との関係

- **DR-0014 (透過原則 + 検証主義)**: 本 DR は実装レベルでの強化。「machine 以外は fd /
  signal を直接触らない」原則を self-check に追加すべき
- **DR-0008 (protocol)**: 外部 protocol (= client ↔ daemon の CBOR framing)。本 DR は
  **daemon 内部の reducer 構造**で別軸。`ClientRequest` enum は protocol message と 1:1 対応
  させるか、複数集約するかは Phase 6 で詳細
- **DR-0013 (screen state 正本)**: 本 DR の Layer 3 (= semantic event) の正本化層に該当。
  Layer 2 → 3 への変換が screen state の更新を伴う
- **DR-0022 (input invocation auto-lock)**: 本 DR で構造的に再整理。auto-lock の効果境界
  (= 「input 直列化までで output は別軌道」) を仕様レベルで明文化する根拠を提供
- **DR-0016 (TTY IO record)**: 本 DR の event sourcing の自然延長。record sink は machine の
  全 message を 1 箇所で wrap できる

## Consequences

### 良い影響

- **race の構造的不可能性**: machine が単一の receive loop で 1 件ずつ処理 → atomic が
  設計として保証 (= 「borrow checker 偶然依存」から脱却)
- **testability の劇的向上**: machine 単独 unit test で PTY 不要、message 列で全 race / lock /
  edge case を厚く検証可能
- **観測性**: 全 message が 1 箇所を通る → record / trace / replay が自然
- **責務分離**: client は machine 経由でしか TTY に触れない、「直接 raw_data を PTY に流す」
  現状の脆さが消える
- **TTY 機能の網羅追跡**: enum カタログ + scoreboard で実装漏れ・規格対応状況が一覧化、
  新機能検討時の出発点になる
- **AI 推測列挙との区別**: 一次情報リンク義務化により、規格出典の追跡可能性が担保

### コスト・リスク

- **rewrite scope 大**: 既存 handler 全面書き直し。Phase 1-2 だけで数千行影響想定
- **カタログ enum 150-200 variant の維持コスト**: 出典 link rot のメンテ、新規 terminal 機能の
  追跡コスト (ただし出典義務化で長期信頼性は確保)
- **breaking change**: v1.0 未満なので許容方針 (CLAUDE.md memo 参照)。外部 protocol は
  DR-0008 に従い別軸、本 DR の breaking は **daemon 内部 + test harness** に閉じる
- **Phase 1 で「API 切り直しだけで race 解消はしない」期間が生じる**: 中身を変えないので
  既存 race は残る。Phase 2 完了まで race の構造的解消は実現しない

## Alternatives

### A. 個別 handler 維持 + lock 強化のみ

現状の handler 構造を保ち、lock の効果境界を厳密化 (= 「lock 中は子の echo 完了まで block」
等の仕様追加) する案。

不採用理由: lock 1 軸の強化では他の event 軸 (= signal / child lifecycle / OSC / DCS 等) の
散在問題が残る。設計哲学レベルの統一性が得られない。

### B. tokio actor framework

`tokio::actor` 系の crate (= `actix` / `ractor` 等) で machine を actor 化する案。

不採用理由: hyoui は async runtime に依存しない設計 (= `nix::poll` ベース)。actor framework
導入は依存関係増 + 既存 sync I/O 路と二重化。**手動 reducer + channel で十分** (= rust 標準
`std::sync::mpsc` / `crossbeam-channel` で済む)。

### C. event-driven framework (= 別 crate)

`mio` / `polling` 等のイベント駆動 crate を導入。

不採用理由: 既存 `nix::poll` ベースで動いてる serve_loop と二重化。本 DR の主眼は **message
の論理軸統一** であり、IO 駆動方式の変更は別軸の判断。

### D. 他 terminal multiplexer (tmux / screen / wezterm) の構造踏襲

tmux は `cmd_queue` / `window_pane` / `client` の独立 module 構造。wezterm は internal protocol
で多 process 間通信。

不採用理由: hyoui の責務は terminal multiplexer ではなく「外部自動操作主軸の透過 PTY ラップ」
(DR-0005)。tmux 的な構造は overkill。**machine + 7 軸 message** という最小構成が hyoui の
責務範囲に最適。

## Anti-patterns 防止 self-check

DR-0014 §self-check に追加すべき項目 (= 後続 DR で正式化):

- [ ] machine 外から PTY master fd を read / write していないか?
- [ ] machine 外から child pid に kill / signal していないか?
- [ ] machine 外から `SessionState` の lock_holder / lock_token を mutate していないか?
- [ ] signal handler が self-pipe 以外の経路で state を変更していないか?
- [ ] 新規 client req を追加する際、`ClientRequest` enum への variant 追加経由か?
- [ ] 新規 TTY 由来 event を扱う際、対応する `TtyEvent` variant が enum カタログに存在するか?
- [ ] 新規 variant に一次情報リンク doc comment が完備されているか?

## Open Questions

DR draft 段階で未決、後続議論で詰める:

1. **Layer 3 (semantic) と Layer 2 (parsed) の境界**: どの event を意味化するか、どこから raw
   parsed のまま machine に渡すかの判断基準
2. **`ClientRequest` と `ControlMessage` (protocol) の対応**: 1:1 か、複数集約か。Phase 6 で詳細
3. **`InternalTimer` の実装方式**: poll timeout で代替するか、専用 timer wheel を持つか
4. **machine の concurrency**: 単一 thread reducer で十分か、parser pipeline を別 thread に分けるか
5. **既存 record sink (DR-0016) との統合**: 全 message を record すれば既存 record sink は
   machine internal に吸収されるか、別軸で残すか
6. **error handling**: reducer が `Result` を返すか、`Vec<MachineEvent>` に error variant を含めるか
7. **migration 期間中の coexistence**: Phase 1 で既存 handler を wrap した API を出すとき、
   旧 handler は deprecate するのか並列維持するのか
