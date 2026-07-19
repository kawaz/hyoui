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
resume してほしい、の 2 本立てで要望。**2a は 案 A (rw attach 時 default で resume、
ro 除外) で確定**。以下は 2b の詳細裁定。

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

### TSTP-Q1 — Ctrl+Z 押下時の実装アプローチ

**推し: 案 G (debounce 折衷、state machine 実装)**

kawaz 提示の折衷案。詳細な state machine 案:

```
STATE_IDLE
  Ctrl+Z 受信 → 保留、SHORT_DEBOUNCE 起動、overlay 表示 → STATE_ARMED
  他 byte    → forward

STATE_ARMED (SHORT_DEBOUNCE 期間中、保留 Ctrl+Z 1 発あり)
  Ctrl+Z 再受信       → detach 発動 (保留破棄)
  他 byte 受信        → 保留 Ctrl+Z を forward、次 byte 続けて forward、
                        overlay 消去 → STATE_GRACE 起動
  SHORT_DEBOUNCE 満了 → 保留 Ctrl+Z を forward、overlay 消去 → STATE_GRACE 起動

STATE_GRACE (LONG_GRACE 期間中、Ctrl+Z を素通し)
  Ctrl+Z 受信       → 即 forward、LONG_GRACE リセット (継続)
  他 byte 受信      → 即 forward、LONG_GRACE 継続
  LONG_GRACE 満了   → STATE_IDLE
```

効果:
- Ctrl+Z 連打 → detach (kawaz 反射押し対応)
- Ctrl+Z 1 発だけなら SHORT_DEBOUNCE 後に子へ forward (通常 suspend として動く)
- 直後の 2 打目以降は素通し (アプリに Ctrl+Z 連打を送りたい正当ユースケース対応)
- 時間経過後の連打は再び detach 検知が発動

比較案:
- 案 E (Ctrl+Z 単発 intercept、常に自 SIGSTOP): 押しちゃう癖には最短だが、子に Ctrl+Z
  を届けたいユースケース (vim `:!bash` / python REPL 等) で escape hatch 必要
- 案 F (`Ctrl-A z` に suspend attach 動詞): prefix 経路の学習を要求、反射押し非対応

**根拠**: kawaz の直感 (連打で detach、単発は素通し + grace で連投許容) を state machine で
そのまま表現。Ctrl+Z の semantics を壊さず detach 経路を上乗せできる。

参照: `crates/hyoui/src/client/attach.rs` の prefix state machine (拡張ベース)

### TSTP-Q1a — SHORT_DEBOUNCE の窓幅

**推し: 300ms**

- 200ms: 反射の 2 連打には十分、通常押しの遅延は体感ほぼゼロ
- 300ms: 慌てて押す 2 連打も拾える (推し)
- 500ms: 確実に拾えるが単発 Ctrl+Z 押し時の遅延が体感できる
- 数値検証で調整 (kawaz 実機 dogfooding で最終決定)

### TSTP-Q1b — LONG_GRACE の窓幅

**推し: 1500ms**

- 1000ms: 連投したい人には短い
- 1500ms: 連続 Ctrl+Z を素通しつつ、間を空けたら再 detach 検知に戻せる (推し)
- 2000ms: 素通し窓が広すぎて「間空けた気になったのに detach 発動しない」体験になり得る

### TSTP-Q1c — overlay 表示

kawaz「あると尚良い」= 必須ではない。段階実装可。

**推し: Phase 1 は overlay 実装せず。Phase 2 で以下から選択**

- 案 α: attach client 側で stdout に ANSI 直接書き込み (画面下部 1 行に色付き) →
  raw output に混ぜるため alt screen TUI と競合、位置制御難しい
- 案 β: daemon の screen state に overlay 挿入 (DR-0013 整合) → 他 client からも見え
  てしまう、screen emulator 側に一時 overlay 機能追加が要る
- 案 γ: terminal title 書き換え (`\e]0;Ctrl+Z again to detach\a`) → 実装最小、画面
  非干渉、tmux 等の title 表示に載る (kawaz は tmux 未使用なら効かない)

Phase 1 (overlay なし) で動作確認 → kawaz dogfooding で「無いと困るか」判定、必要なら
Phase 2 で案 γ (最小介入) を先に試す、が推し。

### TSTP-Q2 — default 挙動

**推し: default on + env で off 可能**

- 案 on: default で案 G 有効、`HYOUI_TSTP_MODE=passthrough` で無効化
- 案 off: default で無効、`HYOUI_TSTP_MODE=intercept` で有効化 (opt-in)

**根拠**: 「押しちゃう癖」対応が default 発想なので on。escape hatch は用意する。

### TSTP-Q3 — 設定手段

**推し: env `HYOUI_TSTP_MODE=intercept|passthrough`**

- 案 X: env `HYOUI_TSTP_MODE=intercept|passthrough` (推し、既存 `HYOUI_DETACH_PREFIX` と対称)
- 案 Y: env `HYOUI_INTERCEPT_TSTP=on|off` (簡潔だが polarity 混乱)
- 案 Z: CLI flag `--tstp-mode=intercept|passthrough` (run/attach 両方に配線コスト)

**根拠**: env で足りるなら env が最小介入 (attach.rs のみ)。将来 CLI flag 化は非破壊で
上乗せ可能。
