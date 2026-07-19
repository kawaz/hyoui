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
「直接起動と同じ挙動」が実現されている。kawaz は「Ctrl+Z 押しちゃう癖」があり、
子を stopped 化せずに detach したい (2b) + stopped 状態で detach したら再 attach 時に
resume してほしい (2a) の 2 本立てで要望。2a は **案 A (rw attach 時 default で
resume、ro 除外)** で確定。以下は 2b (= Ctrl+Z を子に流さない経路) の詳細裁定。

### TSTP-Q1 — 実装アプローチ

**推し: 案 E (Ctrl+Z を attach client 側で intercept、子には流さず client 自身が
`raise(SIGSTOP)`。子は走ったまま、外側 shell に戻る。`fg` で attach 復帰)**

- 案 E: Ctrl+Z 単独 → client 食う → `raise(SIGSTOP)`。子は継続。fg で復帰
- 案 F: `Ctrl-A z` 等 prefix + キーに suspend attach 動詞を割り当て。Ctrl+Z byte は素通し
- 案 D: 既存の detach key (`Ctrl-A d`) 案内のみで済ませる (= kawaz が「聞いてない」と却下済)

**根拠**: kawaz 要望「Ctrl+Z を流さずに detach」の直訳は E。F は prefix 経路の学習を
再度要求するため 2b の主旨 (= 反射で押す Ctrl+Z を活用) から外れる。

参照: `crates/hyoui/src/client/attach.rs` (detach prefix 実装), DR-0017 §柱1

### TSTP-Q2 — default 挙動

**推し: E-3 (default on + env で off 可能)**

- E-1: default on (env なしで有効)
- E-2: default off + `HYOUI_TSTP_MODE=intercept` で有効化 (opt-in)
- E-3: default on + `HYOUI_TSTP_MODE=passthrough` で無効化可能 (opt-out)

**根拠**: E-1 は kawaz 用途に刺さるが、vim `:!bash` の子 shell を SIGTSTP したい /
python REPL に SIGTSTP を届けたい正当ユースケースで escape hatch なしは詰む。
E-2 は発見不能で「押しちゃう癖」に効かない。E-3 が両立解。

### TSTP-Q3 — env 変数名

**推し: `HYOUI_TSTP_MODE=intercept|passthrough`**

- 案 X: `HYOUI_TSTP_MODE=intercept|passthrough` (推し、semantics 明示的)
- 案 Y: `HYOUI_INTERCEPT_TSTP=on|off` (簡潔だが真偽値の polarity が読みにくい)
- 案 Z: `--tstp-mode=intercept|passthrough` (env でなく CLI flag として run/attach に配線)

**根拠**: env は既存の `HYOUI_DETACH_PREFIX` と対称配置。CLI flag は attach 経由でも
run 経由でも指定必要になり配線コスト上、まず env で足りるか確認したい。
