---
title: DR INDEX の archive 運用を docs-layout 規約に揃える
status: open
category: task
created: 2026-09-16T15:51:25+09:00
last_read:
open_entered: 2026-09-16T15:51:25+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: ccmsg
---

# DR INDEX の archive 運用を docs-layout 規約に揃える

## 概要

ccmsg からの依頼 (kawaz 2026-09-16)。docs-layout (claude-rules-personal reference `docs-authoring/docs-layout` の decisions/ 節) を更新した:

1. INDEX.md は現役の DR だけを載せ、後続 DR に置き換えられた DR は `Status: Superseded by DR-NNNN` を本文に書いて `docs/decisions/archive/` へ移す (ファイル名維持)。INDEX に Archived / Superseded の節を持たず、`archive/INDEX.md` に「番号 / タイトル / 一言 / 置き換え先」の表を置き、現役 INDEX からそこへ 1 行リンク。
2. 現役の文書 (DESIGN / src doc / 現役 DR) から archive の DR を番号で名指ししない (置き換え先を指す)。
3. INDEX の状態列は絵文字とラベルだけ、日付や Phase は本文の Status 行に (hyoui の現在の書式から日付を外す)。

hyoui の `docs/decisions/INDEX.md` (`## Archived` 節、`🔁 Superseded` の行、状態列の日付) をこの新規約に揃えてほしい。

## 背景

docs-layout の decisions/ 節が更新されたことに伴い、既存プロジェクトの INDEX 運用を新規約へ追従させる必要がある。hyoui はこの規約差分の対象として名指しされた。

## 受け入れ条件

- [ ] `docs/decisions/INDEX.md` から `## Archived` 節・`🔁 Superseded` 行・状態列の日付を除去
- [ ] 後続 DR に置き換えられた DR を `docs/decisions/archive/` へ移動 (ファイル名維持)、本文に `Status: Superseded by DR-NNNN` を明記
- [ ] `docs/decisions/archive/INDEX.md` を新規作成し、「番号 / タイトル / 一言 / 置き換え先」の表を置く
- [ ] 現役 INDEX から archive/INDEX.md へ 1 行リンク
- [ ] 現役の文書 (DESIGN / src doc / 現役 DR) から archive 済み DR 番号への直接言及を置き換え先の DR 番号に差し替え
