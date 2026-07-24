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

## 👺 TSTP2-Q1: Ctrl+Z 単発時に attach client も一緒に stop する現仕様を維持するか

実測で DR-0026 intercept は仕様通り動作 (単発 = 300ms 保留 → 子 SIGTSTP)。ただし
DR-0017 §柱1 の follow 仕様により **client も一緒に stop して外側 shell に戻る** ため、
体感が「Ctrl+Z 押したら即ターミナルに戻る」になる (2026-07-24 kawaz 報告の再現 A は
これで、bug ではなく仕様)。

- a) **現仕様維持** (AI の推し) — 直接起動した TUI の Ctrl+Z と同じ体感 (子が止まれば
  手前も shell に戻る = 透過原則)。detach したい時は 2 連打が既に効く
- b) 単発時は子だけ stop し client は attach 継続 (画面は凍結表示 + resume 手段を案内) —
  「眺めたまま止めたい」用途向けだが、直接起動との体感差が生まれ、DR-0017 改訂が必要
- 参照: 調査記録は次 commit の docs/issue/2026-07-24-bug-tstp-intercept-followups.md
