# QUESTIONS — 裁定待ちキュー

> 運用規約 (self-contained):
> - 現在ユーザ裁定を待っている質問だけを索引として並べる (経緯は git log と各記録先が担う)
> - ラベルはバッチ毎に一意な prefix (例: `LIST-Q1`, `TSTP-Q1`)。Qn の使い回し禁止
> - 各 Q は「質問 1〜2 行 + 選択肢 + AI の推し + 根拠 1 文 + 参照 (相対パス / 節)」
> - 提示と同一ターンで本 file を path 指定 commit、push はリリース窓に同乗
> - 裁定が下りた Q は本 file から削除、裁定内容は DR / issue / journal / close_reason へ反映
> - チャットは「LIST-Q1 待ち」等のラベル参照だけで済ませ、質問の正本は本 file が持つ
> - 説明要求 (「詳しく」「それ何」) が来たら本 file 内に「### 背景説明」等を追記して再提示、
>   TL に長文を流さない

---

## TSTP-Q — Ctrl+Z 押下時の attach client 挙動

背景: DR-0017 §柱1 で child は session anchor daemon 配下の別 pgrp に配置され、
Ctrl+Z byte (0x1a) が PTY line discipline 経由で SIGTSTP を生成 → child を stopped 化する
「直接起動と同じ挙動」が実現されている。kawaz は反射的に Ctrl+Z を押してしまうため、
(2b) 子を stopped 化せずに detach したい、(2a) stopped で detach したら再 attach 時に
resume してほしい、の 2 本立てで要望。

### 既に確定した項目 (実装計画のアンカー、Q が閉じたら削除)

- **2a**: 案 A (rw attach 時 default で resume、ro 除外)
- **TSTP-Q1**: 案 G (debounce 折衷 state machine)。STATE_ARMED の他 byte 受信時は
  「overlay 消去 → 保留 Ctrl+Z forward → 当該 byte forward」の順で処理
- **TSTP-Q1a**: SHORT_DEBOUNCE = 300ms default、設定ファイル (`~/.config/hyoui/config.toml`
  DR-0024 と同 file、`[attach.tstp]` セクション予定) で調整可能
- **TSTP-Q1c** (overlay): Phase 1 実装対象外。「仮想 screen のみに overlay を出す枠組み」
  自体を先に整備する話として別 issue に切り出す (screen state に一時 overlay 挿入 →
  他 client との協調含む一般機構)

### TSTP-Q0 — Ctrl-A d が raw mode で効かない疑い (bug 調査、Q1 と独立に進む)

kawaz 実機観察: `HYOUI_DETACH_PREFIX` の既定 Ctrl-A d を押しても detach が発火しない
(raw mode 由来で intercept をすり抜けてる可能性 or 単純 bug)。

**進め方**: `crates/hyoui/src/client/attach.rs` の `process_detach_prefix` と attach 主
loop での stdin read 経路を再確認、raw mode 前後で prefix state machine が呼ばれてる
かを実機再現、bug なら fix。Q1 の案 G を実装しても Ctrl-A d 経路は残す (= 予備動線)
ので独立で直す価値がある。

**質問**: 調査 + fix を Q1 実装の前に **先行** させて OK? (推し: 先行 = kawaz が
反射で Ctrl+Z を使う癖対応と、慣用の Ctrl-A d が効くべき状態は独立の要件)

参照: `crates/hyoui/src/client/attach.rs`, `crates/hyoui/src/sys/tty.rs`

### TSTP-Q1b — LONG_GRACE の default 値

STATE_GRACE (直後の連投で子に Ctrl+Z を素通しする窓) の default 値。設定ファイルで
調整可 (Q1a と同流儀) 前提。

**推し: 1500ms**

- 1000ms: 連投したい人には短い
- 1500ms: 連続 Ctrl+Z を素通しつつ、間を空けたら再 detach 検知に戻せる (推し)
- 2000ms: 素通し窓が広すぎて「間空けた気になったのに detach 発動しない」体験になり得る

### TSTP-Q2 — 有効化の default

**推し: default on**

- 案 on: default で案 G 有効
- 案 off: default で無効、設定ファイル or env で opt-in

**根拠**: 「押しちゃう癖」対応が本要件の主目的なので default on。escape hatch は Q3。
Q1a の「取り敢えず 300 デフォルト」から default on 前提で話が進んでいるが、正式に確認。

### TSTP-Q3 — 無効化 escape hatch の配置

DR-0024 (env scrub の CLI flag 最小化 + config.toml 化) 流儀に沿って、config.toml 中心
+ 最小限の CLI/env で escape hatch を用意する:

**推し: config.toml 中心 (`[attach.tstp] intercept = true` を default) + CLI flag なし**

- 案 A: config.toml のみ (`intercept = true/false`)。CLI/env は無し (推し)
- 案 B: config.toml + `--no-tstp-intercept` CLI flag (attach 起動時に 1 発 off)
- 案 C: config.toml + env `HYOUI_TSTP_MODE=intercept|passthrough`

**根拠**: DR-0024 の「config で target 別に細かい制御、CLI は最小」路線と対称。ワンショット
off が必要な運用が出たら B/C を後から非破壊で追加可能。まず A で最小配線から始める。
