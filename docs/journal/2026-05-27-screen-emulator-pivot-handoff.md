# hyoui handoff — screen emulator 中心への方針大転換 (2026-05-27)

- Date: 2026-05-27
- Session: c7988b6b (Round 4-5 fix loop + DR-0009 分割 + target/ history rewrite + 方針議論)
- Status: 次セッションへ引継ぎ

## 1 行サマリ

実機検証 (cmux-msg 連携 / claude TUI 観戦) で **attach がほぼ機能しない + wait pattern 誤マッチ多発** が判明。
根本原因は daemon に **screen emulator が無い** こと。

→ ROADMAP を **screen emulator + attach/detach 安定化を最優先** に組み替える方針大転換。
input / wait / tail / lock / tx 等は **全部この基盤完了後の延長** に降格。version 区切り (v0.1.x / v0.2.0 / ...) も廃止し、列挙型の roadmap に変更。

## 本セッションで完了した作業

- Round 4 fix loop: CRITICAL 9 / HIGH 14 完全消化 (実装 + deferred)
- Round 5 review (= 8 別ペルソナ並列): dedup 後 95 件集約
- Round 5 fix loop: CRITICAL 3/4 / HIGH 22/22 (= 実装 + deferred)
- Round 5 Field findings (= cmux-msg 検証由来): FB1/FB2/FB4/FB5/FB6 解消、FB2b は test ignore で TODO 化
- DR-0009 = session.rs 責務分割: session.rs 4364 → 2594 (-41%)、7 新 module
- R4-H4 unsafe 撤去 (= Option<SessionInner>)、daemon/ 全体 unsafe 0
- DR-0010 起票 (= 旧 v0.2.0 scope 11→7、serve 別 repo)
- DR-0011 起票 (= observability 戦略、実装は別 task)
- DR-0012 (= signal wire name 化、protocol breaking): 3 commits 完成 (push 未)
- backlog を `/tmp` から `docs/REVIEW-BACKLOG.md` 移管 (= /tmp は symlink で互換維持)
- target/ を過去全 commit から削除 (= memory `jj-tips` パターン + multi-branch conflict 無視で全 split)
- main release: v0.1.0 〜 v0.1.15

## 方針大転換 (= 本セッション末で確定)

### 背景: 実機で発覚した重大欠陥

1. **attach がほぼ機能しない**
   - attach socket は通るが、client terminal に画面が再現されない
   - 子 (= claude TUI) は新 attach client を知らず redraw しない
   - resize 通知のみでは部分 redraw しか起きない
   - client が入力すると `press once more to exit` 等の部分メッセージだけが流入、画面崩壊
   - ユーザ期待: detach 時の画面 + 入力連動が綺麗に再現される

2. **wait pattern の誤マッチ多発**
   - claude 等 TUI は alternate screen を持ち、bg/fg 切替や redraw で全画面 ANSI を再送
   - scrollback に過去描画分が混じる
   - `wait --pattern "Continue?"` で過去履歴に誤発火が頻発
   - 「現在 visible state に対する match」が無いと使い物にならない

### 根本原因 + 解決方針

両方とも **daemon が screen state を持っていない** ことが原因。

**daemon = screen state の唯一の正本**となる基盤 (= screen emulator) を入れれば、
attach 復元 / wait L1 / tail L1 / snapshot / debug inspection が全て自然に開ける。

「生 TTY bytes を触る部分を最小化、統一してデータを正確に扱える基盤」がコア哲学。

### ROADMAP 構造の見直し

version 区切り (= v0.1.0 / v0.2.0 / v0.3.0) は **指標として弱い**、固定化すると実態と乖離。
**4 層列挙型** に組み替える:

```
必須 = screen emulator + attach/detach 安定化 (DR-0013)
最優先 = (= 撤廃、基盤完成までは何も着手しない)
優先 = wait/tail/snapshot/input/lock/tx (= 基盤完成後、順次)
追加予定 = serve gateway / record-replay / Python・Node bindings / packaging / L2 wait / multi-modifier / observability / signal wire name 化 / itumono skill 改修 / ...
```

ユーザの最終整理: **wait / tail もまだ不要**。attach/detach の安定動作が**唯一の必須要件**。
その基盤が整えば、wait / tail / input 等は **そこまで大変ではない**。

## DR-0013 sketch (= 次セッションで起票する、内容は本セッションで詰め済)

題: `DR-0013: screen emulator + attach/detach 安定化 + データモデル統一`

### 1. データモデル

- daemon = screen state の **唯一の正本**
- 生 TTY bytes は internal、client API は state 経由
- protocol = state 操作 / 観測の API + raw bytes layer (= 既存 TYPE_RAW_DATA は維持)

### 2. screen emulator crate 選定 (← 次セッション調査タスク 重点)

候補 (= 比較対象):
- `vte` (alacritty 系、parser のみ、state は別途自前): parser-only、超軽量、依存 nano
- `wezterm-core` (wezterm 由来): screen state + parser、機能豊富、依存重い可能性
- `alacritty_terminal` (alacritty 由来): full terminal emulator、production-tested
- `termwiz` (wezterm 系): 関連、cell grid 操作 lib

**比較観点** (= 次セッションで Agent が深掘り):
- API surface (= cell 直接アクセス / cursor / mode / alternate buffer 切替)
- 依存 footprint (= 現 hyoui の lean 方針 `nix + serde + ciborium + regex + thiserror` との整合)
- maintenance 状況、stability
- serde 連携 (= state snapshot を CBOR で送る前提)
- no_std 可否、license

### 3. 老舗 terminal multiplexer 実装研究 (← 次セッション調査タスク 重点)

**表面 API ではなく実装まで踏み込んだ徹底研究**。コスト惜しまず:

- **tmux**: 30 年級の老舗。alternate screen / scrollback / multi-client / detach/attach。C ソース直読
- **GNU screen**: 更に古い、UTF-8 / window 概念 / screen state hold
- **abduco**: detach-only minimal、tmux の subset 思想 (= 実は本 project に最も近い)
- **dvtm**: vt100 emulator + window manager 分離
- **zellij**: Rust 製の現代版、wezterm-core 等使ってる可能性、最近の実装パターン
- **wezterm**: terminal emulator + multiplexer 両刀、Rust 製、複数 pane / 複数 tab / GPU 描画
- **alacritty**: emulator のみだが state 管理は参考になる

**観点**:
- attach 復元のシーケンス (= handshake → state redraw)
- alternate screen / scrollback の分離管理
- detach 時の state flush / in-flight 処理
- multi-client での state 一貫性
- resize 時の reflow
- IPC protocol (= unix socket vs shared memory 等)
- 認証 / 排他制御
- 失敗 path / panic safety

「**既存の研究は必ずこちらの品質につながる**」(= kawaz 指示)、コスト惜しまず徹底。

### 4. state 構造

- cell grid: row × col × Cell { char (String, grapheme cluster), fg, bg, attrs, width }
- cursor: { x, y, visible, shape, blink }
- mode: { alternate_screen, app_keypad, app_cursor, mouse_*, bracketed_paste, ... }
- buffer: Primary | Alternate
- scrollback: ring of past rows (= 既存 ring buffer 統合)
- window_size: { rows, cols, px_w, px_h }
- style: 現 cursor style / 線種等

### 5. attach 復元 protocol

- attach handshake 完了直後
- daemon が screen state を **redraw sequence** として client に送る:
  - clear screen + mode reset
  - 各 cell を順次描画 (= バックグラウンド色 + 文字 + style)
  - cursor 位置を最終的に restore
  - alternate screen mode なら `?1049h` も含める
- client terminal は **detach 時と同じ画面** を再現

### 6. detach 時の state flush

- detach 通知受信 → in-flight bytes を全部 screen emulator に feed
- state を確定 (= 中途半端な ANSI sequence を捨てる or 保存する判断)
- 後続 attach で state 再現可能に

### 7. alternate screen hook

- `?1049h` / `?1049l` / `?1047h` / `?1047l` を daemon の emulator が hook
- buffer 切替 (= primary → alternate、画面退避)
- attach 時にどちらの buffer が active か復元

### 8. resize 対応

- attach client の terminal size が変わったら WINCH を子に通知
- screen state を新 size で **reflow** (= 行 折返し再計算、cursor 位置調整)
- 複数 client が異なるサイズなら leader のサイズで子が描画、他は crop/padding (= §10A 参照)

### 9. scrollback 管理

- emulator 側で管理 (= 現 `crates/hyoui/src/scrollback.rs` を統合 or 置換)
- 過去 row へのアクセス API
- size 上限、`last_evicted_age` 等は既存仕様を継承

### 10. debug/inspection protocol (= 新規)

新 control message:

```
ScreenDumpRequest { format: Binary|Ansi|Json|Cbor, layer: Visible|Scrollback|Both, rect: Option<{x,y,w,h}> }
ScreenDumpResponse { payload: Vec<u8> }

StateSnapshotRequest { include: Set<{Cells, Cursor, Mode, Style, Scrollback, WindowSize, Buffer}> }
StateSnapshotResponse { cells, cursor, mode, style, scrollback, window_size, buffer (Option フィールド) }
```

cap flag: `screen-dump-v1` / `state-snapshot-v1`

用途:
- デバッグ (= daemon state が正しいか目視 / 機械的に確認)
- 自動 test (= 「特定操作後の cell[3][5] が `>` か」)
- 自動操作の信頼性 (= 現 visible に prompt がある等の predicate primitive)
- post-mortem (= 後から画面再現)

CLI 露出 (= DR-0013 後の別タスクで):
```
hyoui screen dump <session> [--format=...] [--layer=...] [--rect=...]
hyoui screen snapshot <session> [--include=...] [--format=...]
```

### 11. DR-0008 連動

- 既存 raw bytes layer (= TYPE_RAW_DATA) は維持
- structured state アクセス message を追加 (= TYPE_CBOR_CONTROL 経由、上記 §10)
- cap flag で negotiation (= 既存 cap 機構を活用)

### 12. 追加機能 (= 優先度低め、メイン scope 完了後の延長)

#### A. resize 無し ro 復元 ("observe mode")

attach 時に **resize 通知を子に出さず**、daemon screen state を **native size のまま** 表示する mode。

- 用途: 動いてる claude セッションを別 terminal から触らず観戦、複数 client 異サイズの reflow 戦争回避
- 仕様: client terminal < daemon size = crop / > daemon size = padding (黒 fill or placeholder)
- flag: attach handshake に `--no-resize-propagate` (= 仮称) を追加
- 既存 leader の resize は引き続き効く、observe mode client は leader 計算から除外
- 優先度: 必須 1-11 完了後、いずれ実装。記録のみ

## 次セッションでやる順 (推奨)

### Pre: 調査 (= 着手前に必要、徹底)

1. **screen emulator crate 比較調査** (= §2、Agent 1 で 1-2h 程度)
2. **老舗 multiplexer 実装研究** (= §3、Agent 1-2 で実装読み込み、tmux/abduco/zellij/wezterm 優先)
3. 調査結果を `docs/research/` 配下に保存 (= 後の判断材料に)

### Step 1: DR-0013 起票

調査結果を反映して詳細詰めた DR を起票。crate 採用 + 全体設計確定。

### Step 2: ROADMAP 書き直し + 旧 DR annotate

- `docs/ROADMAP.md` を本ファイル「方針大転換」セクションの 4 層列挙型に
- DR-0007 / DR-0010 / DR-0011 / DR-0012 に「version 区切りは廃止、ROADMAP が正本」と annotate
- DR-0010 の旧 v0.2.0 scope (= input family + serve 別 repo) は **input family の整理だけ正本**、scope の version 区切りは廃止

### Step 3: DR-0013 実装 Phase A (= screen emulator 採用 + attach 復元)

- crate 取り込み、state 管理 module 新設
- 子 ANSI bytes feed 経路
- attach handshake 後の redraw sequence 生成
- detach 時の state flush
- alternate screen hook
- 既存 broadcast / scrollback の置換 or 統合

### Step 4: DR-0013 Phase B (= resize + scrollback API + debug protocol)

- resize 対応、reflow
- scrollback の screen emulator 統合
- debug/inspection protocol (= screen.dump / state.snapshot)
- CLI 露出 (= `hyoui screen ...`)

### Step 5: 必須完了後、優先項目に進む

wait / tail / snapshot / input / lock / tx を screen state API ベースで実装。
順序と命名は次セッション以降で議論 (= 本セッション末の議論を参考: input family 統一、spec syntax with text/key/hex/paste/file/wait/wait-idle、bracketed paste の direct/paste 切替等)。

## 残作業 (= 本セッション未完了、次セッションで判断)

| Task | 状況 | 判断 |
|---|---|---|
| push reject 解消 | 中間 empty `nlyktzxv` abandon + 再 push | 本セッション末でやる |
| DR-0012 (signal wire name) 4th commit (= ROADMAP + DR-0008 annotate) | 3 commits 完成、4th 未 | 次セッション、ROADMAP 書き直しと併せて |
| Round 5 MEDIUM 残 27 件 | batch 1 で 9 件 done、残 27 (= R5-M ID で grep) | 基盤完成後、随時 |
| Round 5 LOW 14 件 | 未着手 | 同上、quick wins だけ拾う |
| itumono-skills 改修 (= /tmp → docs/REVIEW-BACKLOG.md 規約) | 未着手 | 別 PR で実施、本リポでは backlog 移管済 |
| Round 5-FB2b (= headless_stdin_eof test を event-based に) | ignore 化済、TODO | 基盤完成後 |

## 議論の経緯詳細 (= 本セッション末の thread、次セッションで参照)

本セッションで詰めた input / lock / wait / screen の議論 (= 約 1 時間の対話):

1. **subcommand フラット多すぎ問題**: 11 個 → 7 個 (DR-0010) → 更に整理判断
2. **lock / wait / tx は input 系の primitives** (= ユーザ指摘): 独立 family でなく input family 内に統合
3. **keys spec を text:/file:/hex: で統合**: subcommand → spec prefix に圧縮可
4. **leaf 名 keys vs send**: 慎重、`input <spec>...` で leaf 自体廃止案
5. **multi-modifier (Ctrl-Shift-A)**: terminal capability negotiation が要 (= xterm modifyOtherKeys / kitty keyboard protocol)、MVP 後回し正当
6. **input text vs text: prefix**: UTF-8 limited なら text: で代替可、binary は別経路 (= hex:)
7. **bracketed paste の意義**: multi-line script を 1 paste block で送る、direct と paste 系で prefix 分け (= `text:` direct / `paste:` bracketed)
8. **wait pattern の誤マッチ**: claude TUI が全画面再送 → scrollback regex は誤発火
9. **attach の崩壊**: redraw 不在で部分メッセージのみ流入、画面復元 ≠
10. **screen emulator が根本解**: daemon = state 正本、生 TTY は internal、client は state 経由
11. **version 区切り廃止**: 列挙型 ROADMAP、必須/優先/追加予定の 3 層
12. **debug protocol も含める**: screen.dump / state.snapshot で機械観察可能化
13. **resize 無し ro 復元**: observe mode、優先度低めだが記録

## 参考

- `docs/REVIEW-BACKLOG.md` — Round 4-5 + Field findings、全体未対応一覧
- `docs/decisions/DR-0005` — 思想 (= 外側自動操作主軸)
- `docs/decisions/DR-0006` — CLI ground rules (= 古い記述あり、特に §8/§9 は新方針で改訂要)
- `docs/decisions/DR-0007` / `DR-0010` — version 区切り廃止対象、新 ROADMAP に従う
- `docs/decisions/DR-0008` — protocol design、structured state アクセス message 追加要
- `docs/decisions/DR-0009` — session.rs 分割 (= 実装済、screen emulator 統合先は主に daemon/ 配下)
- `docs/decisions/DR-0011` — observability 戦略 (= screen 基盤完了後に Phase A)
- `docs/findings/2026-05-27-headless-claude-remote-control-leak.md` — 実機検証 finding
- `docs/issue/2026-05-27-cmux-msg-experiment-feedback-v020-refresh.md` — 実機検証 feedback
- `docs/journal/2026-05-27-keys-spec-orphan-rescue.md` — 旧 DR-0009 (keys spec) orphan の救出 (= 内容は次 DR で参照可)
- `~/.claude-personal/rules/jj-tips.md` — multi-branch + conflict 環境での history rewrite パターン (= 本セッションで追記済)
