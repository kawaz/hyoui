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

> 前提 (2026-07-25 kawaz 指摘、以下 3 Q 共通): hyoui の目的は **バックグラウンドで
> TTY アプリを走らせ続けたまま覗き窓として attach/detach する**こと。attach は一時的な
> 覗き窓に過ぎず、**client 側の操作で子を止めるのは目的の真逆**。「client と子が一緒に
> 止まるのが透過的」という現行の整理は誤り。TSTP2-Q1 は棄却し、下記に差し替え。

## 👺 SUSP-Q1: attach client での Ctrl+Z の割当

現行は「単発 = 300ms 後に子へ SIGTSTP forward (子が止まる) + 子 stopped を見て client も
stop、2 連打 = detach」(DR-0026 §1 + DR-0017 §柱1)。子が止まる時点で目的に反する。

- a) **Ctrl+Z 単発 = detach (子は走り続ける)** (AI の推し) — 反射で押しても目的に沿う
  結果になり、覗き窓を閉じるだけ。子への SIGTSTP は `hyoui kill --signal=TSTP` の明示
  操作でのみ発生させる
- b) Ctrl+Z は完全素通し (子へ forward) に戻し、detach は別キーに任せる — 直接起動と
  同じ挙動だが「反射 Ctrl+Z で子が止まる」問題が残る
- c) Ctrl+Z は何もしない (無視) — 事故は防げるが detach 手段が別途必要

## 👺 SUSP-Q2: 子 stopped に client が follow して自分も stop する仕様 (DR-0017 §柱1) を廃止するか

- a) **廃止** (AI の推し) — client は覗き窓なので、子が止まっても attach は継続し
  「子が停止中」を表示 + resume 手段を案内する。子と client の生死を連動させない
- b) 維持 — 直接起動の体感に寄せる現行仕様

## 👺 SUSP-Q3: `Ctrl-A d` detach prefix を廃止するか

出自は DR-0007 §MVP / DR-0005 (「in-band escape を導入しない原則の唯一の例外」として
明記) で、AI の独断追加ではないが、現在は実端末で発火しない bug 状態
(docs/issue/2026-07-20-detach-key-not-firing-keyboard-protocol.md)。

- a) **廃止して DR-0005 の例外条項ごと削除** (AI の推し、SUSP-Q1=a とセット) — Ctrl+Z が
  detach になるなら prefix は不要。in-band escape ゼロで思想が純化し、bug も消滅する
- b) 修正して残す — prefix 派の操作系を維持したい場合。keyboard protocol 起因の調査続行が必要
- c) 廃止するが `--detach-key` 等で opt-in 復活を残す
