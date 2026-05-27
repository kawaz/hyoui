# 2026-05-27 nonstop session — DR-0013 state-based 大転換の総まとめ

本日 1 セッションで「実機検証で発覚した attach 不能 + wait 誤マッチ」から、
DR-0013 起票 → vt100 採用 → Phase A/B 実装 → CLI 露出 → 旧 wait protocol 削除 →
ドキュメント全面書き直しまで完走した。後から状況復元するための時系列まとめ。

## 発端: 実機検証で発覚した 2 つの致命的欠陥

cmux-msg 連携と claude TUI 観戦を試みたところ:

1. **attach がほぼ機能しない**: detach 時の画面が client terminal に再現されず、
   子 (= claude TUI 等 alt screen 常駐アプリ) は新 attach client を知らず redraw
   しなかった。resize 通知だけでは部分 redraw しか起きず、client が入力すると
   `press once more to exit` のような部分メッセージが流入して画面崩壊
2. **wait pattern の誤マッチ多発**: claude TUI が alt screen を持ち、bg/fg 切替や
   redraw で全画面 ANSI を再送するため、scrollback に過去描画分が混じり
   `wait --pattern "Continue?"` が過去履歴に誤発火した

両方とも「**daemon が screen state を持っていない**」ことが根本原因と判明。
生 TTY bytes を単純 broadcast する abduco 流の現行設計のままでは、attach / wait
の表面 API をいくら整えても解消しない構造的欠陥だった。

## Phase 0: 先行調査 5 件

DR を書く前に research を 5 件積んで、採用 crate と pattern を確定:

1. **screen-emulator-crate-comparison.md** — vt100 / vte / alacritty_terminal /
   wezterm-term / termwiz を比較。`Screen::state_formatted() -> Vec<u8>` が
   attach 復元に直結する vt100 を採用、依存 3 個で hyoui の lean 方針と整合
2. **multiplexer-implementation-study-classic.md** — tmux / abduco / screen /
   dvtm の DEC Williams parser / SGR キャッシュ / ring buffer scrollback /
   attach 復元 Pattern A/B/C
3. **multiplexer-implementation-study-rust.md** — zellij / wezterm / alacritty の
   Render push 型 / SequenceNo pull 型 / Arc<CellExtra> COW / mem::swap alt
   screen / DEC sync update
4. **ghostty-libghostty-study.md** — ghostty 本家が `Format.vt` /
   `stream_readonly` で hyoui と同じ思想に独立到達していることを確認、業界
   standard pattern として補強。libghostty-vt 自体は C API 未完成のため将来再評価枠
5. **vt100-poc-report.md** — vt100 PoC 結果、条件付き GO (reflow truncate 制約は
   wrapper で吸収可能)

## DR-0013 起票と ROADMAP 4 層化

調査結果を DR-0013 に集約し、ROADMAP を version 区切り型 (v0.1.x / v0.2.0 / ...)
から 4 層列挙型 (必須 / 優先 / 追加予定 / 過去 milestone) に書き換えた。

DR-0013 の主要決定:

- **データモデル**: daemon = screen state の唯一の正本、vt100 crate (0.16.2) で表現
- **attach 復元 protocol Phase A**: push 型 `ScreenStateInit` で
  `state_formatted()` + alt mode prepend + cursor 再描画末尾を 1 frame 送信
- **attach 復元 protocol Phase B**: 将来の SequenceNo + pull 型 incremental sync
- **detach 時 flush 不要**: vt100 内蔵 DEC Williams state machine で partial
  sequence は自然に貯まる、5s timeout で health check のみ
- **resize 戦略**: vt100 `set_size` は truncate-only、primary buffer 用の
  input bytes log を bounded ring (1 MiB default) で持って resize 時に新 Parser を
  作って replay
- **multi-client 異サイズ**: tmux pattern の `smallest` を MVP 採用
- **scrollback 管理**: vt100 内蔵 ring を主体、ただし byte-base 層 (= 既存
  `scrollback.rs`) は tail 用に維持 (= Phase B 実装着手時の発見、§8 Update で追記)
- **debug protocol**: `ScreenDumpRequest` / `StateSnapshotRequest` + cap flag
  `screen-dump-v1` / `state-snapshot-v1`
- **CBOR serialization hybrid**: redraw は raw bytes、structured snapshot は
  圧縮 wrapper (空 cell skip + 属性 bit pack + Color variant 整数化)

## Phase A 実装

`vt100 = "0.16"` を `Cargo.toml` に追加、`crates/hyoui/src/daemon/screen/`
module 新設:

- `state.rs` (= `VirtualScreen` wrapper、vt100::Parser 包む)
- 子 PTY read loop で `vt100::Parser::process` を呼ぶように差し替え (= 生 byte の
  直接 broadcast はしない、必ず state 経由)
- attach handshake 直後に `state_formatted()` + alt mode prepend を 1 frame で送出

これで **claude TUI の観戦が綺麗に再現される** ようになった。

## Phase B 実装

- `input_log.rs` (= primary buffer 用 bounded ring buffer、resize replay)
- `snapshot.rs` (= 構造化 state を CBOR で送る圧縮 wrapper)
- `ScreenDumpRequest` / `ScreenDumpResponse` / `StateSnapshotRequest` /
  `StateSnapshotResponse` の handler 実装
- DEC sync update (`?2026h`) hook + 5s stalled sequence reset (= health check)

### Phase B 実装中の発見: scrollback の byte/rows 責務分離

DR-0013 §8 の素朴な「既存 `scrollback.rs` を vt100 内蔵 ring に置換」を実施しようと
したら、`hyoui tail` の `since_ms` / `since_strict` / `last_bytes` の byte-base
timestamp 意味論が壊れることが判明 (= vt100 内蔵 ring は rows-base で timestamp を
持たない)。

**修正方針**: byte-base 層 (`scrollback.rs`) は tail 専用に維持、rows-base 層
(= vt100 内蔵 ring) は cell 単位アクセス用として責務分離。二重管理ではなく
異なる用途の層と再定義。DR-0013 §8 に Update annotate を追記。

bytes ↔ rows の換算 (= 旧記述 `scrollback_rows = scrollback_bytes / (cols * 4)`)
も廃止 (= cell byte 数は UTF-8 と style overhead で大きく揺れる)。代わりに
`screen_input_log_bytes` (default 1 MiB) を独立 config として導入。

## DR-0006 §8-§11 の state-based 書き直し

DR-0013 で daemon = screen state 正本になったのを受けて、DR-0006 を全面改訂:

- **§8 input family**: `hyoui input <session> <spec>...` の 1 leaf に集約
  (= DR-0010 §1 の 3 leaf 案を廃止)、spec prefix で type 判別
  (`text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:`)
- **§9 wait**: マッチ対象を「daemon の現在 visible state」に再定義。scrollback
  regex への match は L2 (`追加予定`) に降格、旧 §11 は Archive section に保全
- **§10 snapshot**: `hyoui screen dump` (= ANSI bytes、terminal 再生可) と
  `hyoui screen snapshot` (= JSON / CBOR、機械処理) の 2 subcommand に分割
- **§11 tail**: byte-base 維持、state-based wait / snapshot との棲み分けを明示
  (= 「画面の現在 visible で X が出るまで待ちたい」は wait、「daemon が受け取った
  生 bytes stream を grep したい」は tail)

旧仕様は Archive section に保全して historical reference として残した。

## CLI 露出フェーズ

DR-0006 改訂後の仕様を CLI に乗せる作業群:

- `hyoui screen dump` / `hyoui screen snapshot` subcommand 追加
- `hyoui input <session> <spec>...` の spec parser 実装
- `ClientConnection::send_raw_bytes` で raw_data frame 送信 API 追加
- input family handler (text/hex/file/paste/key) と integration test
- `cli/wait_core.rs` 新設 (= state-based polling + visible cells → text 構築)
- `hyoui wait` subcommand を state-based polling に rewrite
- input spec の `wait:` / `wait-idle:` を wait_core に wire
- `hyoui tail` を DR-0006 §11 (= state 棲み分け明示) に整合
- `--lock-token` を input family に wire、env `HYOUI_LOCK_TOKEN` fallback
- `tx` / `lock` / `unlock` subcommand を reserve + issue 起票
- input file: spec に size limit + type validation (= security)
- spec prefix / subcommand に edit-distance typo suggest
- key name Unicode alias + typo suggest (= DR-0006 §8.4)
- shell completion を screen + input subcommand + spec prefix まで wire

並行して **lock / unlock** 本実装:

- `hyoui lock acquire` / `hyoui lock release` の subcommand parser
- dispatcher 配線 + integration test (= DR-0006 §7)
- `docs/issue/` で lock / unlock 完了 + tx 残懸念 narrow に更新

## 旧 wait protocol の wire / 実装からの削除

state-based 移行が CLI 側で完成したのを確認した上で、daemon 側の旧 wait protocol
を 3 段階で削除:

1. `refactor(protocol): remove legacy wait message types` — `wait.request` /
   `wait.result` の message type 削除
2. `refactor(daemon): remove legacy wait handler` — daemon 側
   `handle_wait_request_with_cap` 等を削除、`wait.rs` は state polling 補助
   (= snapshot 発火 trigger / poll interval 算出) に縮退
3. `refactor(protocol): drop wait-l0 cap + wait.* error codes + DESIGN sync` —
   cap flag `wait-l0` + error code `wait.too-many` / `wait.invalid-text` /
   `wait.invalid-pattern` を削除、DESIGN.md 同期

その後 DR-0007 / DR-0008 / DR-0009 にも annotate を追加 (= 旧 wait 言及は
historical reference 扱い、現行 wire / cap 集合の正本へ誘導)。

## ドキュメント全面書き直し (本セッション末尾)

- `docs/DESIGN-ja.md` を DR-0013 Phase A/B 反映後の正本として全面書き直し
  (= cli/ + daemon/screen/ + 9 module 分割後の現実、v0.1.x cap 集合、
  byte/rows 層の責務分離、attach handshake redraw 復元 section 新設)
- `docs/DESIGN.md` を DESIGN-ja.md と整合する英訳に書き直し
- `README-ja.md` を input family / wait / screen dump / snapshot / lock / tx の
  新 CLI 集合に更新、比較表に「daemon が screen state 正本」「現在 visible state に
  対する待ち合わせ」「構造化 snapshot / dump」の行を追加
- `README.md` を README-ja.md と整合する英訳に書き直し
- `docs/ROADMAP.md` の `優先` セクションに完了マーク [x] を打って、本セッションの
  deliverable を可視化

## ハマり所と解決策のペア

| ハマり所 | 解決策 |
|---|---|
| 実機 claude TUI で attach がほぼ機能しない | daemon = screen state 正本化、`state_formatted()` + alt mode prepend で 1 frame redraw (DR-0013 §4 Phase A) |
| wait pattern が過去 redraw に誤マッチ | マッチ対象を scrollback bytes regex → 現在 visible state cells から構築した text に変更 (DR-0006 §9.1) |
| vt100 `state_formatted()` が alt screen flag を復元しない | wrapper で alt mode 復元 sequence (`?1049h` 等) を redraw bytes の冒頭に prepend (DR-0013 §4) |
| vt100 `set_size` が truncate-only で reflow 品質低い | primary buffer 用 input bytes log (= bounded ring) を持ち、resize 時に新 Parser を作って replay (DR-0013 §7) |
| scrollback.rs を vt100 内蔵 ring に置換すると tail の byte-base 意味論が壊れる | byte-base 層 (tail 専用) と rows-base 層 (cell access 用) を **責務分離**、二重管理ではなく異なる用途の層と再定義 (DR-0013 §8 Update) |
| naive cell-level CBOR snapshot が 283 倍に膨張 | hybrid 戦略 (= attach 復元は raw bytes、structured snapshot は空 cell skip + 属性 bit pack + Color 整数化の圧縮 wrapper) |
| 旧 wait protocol の wire / cap / error code が daemon に残っていて意味的に死んでいる | 3 段階 (message type → handler → cap/error code) で削除、DR-0007/0008/0009 に historical reference annotate を追加 |

## 完了済機能 (= 本セッション deliverable)

- vt100 ScreenState wrapper (= daemon screen state の正本)
- attach handshake redraw frame (= alt screen 常駐 TUI の観戦が綺麗に再現される)
- input bytes log + resize replay
- structured snapshot CBOR 圧縮 wrapper
- DEC sync update hook + 5s stalled reset
- `hyoui screen dump` / `hyoui screen snapshot` CLI
- `hyoui input <session> <spec>...` (= text/hex/file/paste/key/wait/wait-idle prefix)
- `hyoui wait` state-based polling
- `hyoui lock acquire` / `hyoui lock release` (= tx は別 task に narrow)
- `--lock-token` + env `HYOUI_LOCK_TOKEN` 自動継承
- shell completion (= screen + input subcommand + spec prefix)
- key alias / typo suggest / file: validation
- 旧 wait protocol の wire / 実装からの削除
- DR-0006 §8-§11 の state-based 書き直し
- DR-0007 / DR-0008 / DR-0009 への state-based migration annotate
- DESIGN{,-ja}.md + README{,-ja}.md の全面書き直し
- ROADMAP の完了マーク

## 残懸念 (= 次セッション向け)

- `hyoui lock tx <session> -- cmd args...` の本実装 (= subcommand 予約済 + issue 起票済)
- lock の wait queue 実装 (= 旧 v0.1.x で「即 Denied 返却」だった部分の proper 化)
- `last_evicted_age` 補完 counter の配線 (= vt100 内蔵 ring を Phase C で配線する時)
- per-line SequenceNo + pull 型 incremental sync (= Phase B 残項目)
- PDU serial 番号導入
- observability ([[DR-0011]] Phase A 以降)
- L2 wait (= named area / cursor 位置 / mode flag)
- multi-modifier (= xterm modifyOtherKeys / kitty keyboard protocol)
- `serve` gateway (= 別 repo `kawaz/hyoui-serve`)

## 関連

- [[DR-0013]] — screen emulator + attach/detach 安定化 (本セッションの中核)
- [[DR-0006]] — CLI ground rules (§8-§11 を state-based に書き直し済)
- [[DR-0007]] / [[DR-0008]] / [[DR-0009]] — wait state-based migration annotate
- `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md` — DR-0013 起票前の handoff sketch
- `docs/research/2026-05-27-*.md` — Phase 0 調査 5 件
- `docs/ROADMAP.md` — 4 層列挙型 (本セッションで完了マーク反映)
