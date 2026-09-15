---
title: エコシステム外部レビュー (2026-09) の指摘への対応検討
status: resolved
category: task
created: 2026-09-10T14:47:48+09:00
last_read: 2026-09-15T13:40:00+09:00
open_entered: 2026-09-10T14:47:48+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-09-15T13:40:00+09:00
discard_reason:
pending_reason:
close_reason: 全指摘を実物照合し採否を判定 (本文「採否」節)。採用分は docs 修正として同日 land、裁定要は QUESTIONS.md ECO-Q1/Q2 へ
blocked_by:
origin: kawaz 依頼 (2026-09-10、claude-rules-personal セッション経由)
---

# エコシステム外部レビュー (2026-09) の指摘への対応検討

## 概要

外部レビューで本リポ (hyoui) 向けの指摘が出た。以下 2 ファイルを読んで対応を検討する。

- 個別ファイル: `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-rules-personal/main/docs/research/2026-09-10-ecosystem-review/hyoui.md`
- 共通ファイル: `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-rules-personal/main/docs/research/2026-09-10-ecosystem-review/common.md`

## 背景

kawaz からの依頼 (2026-09-10、claude-rules-personal セッション経由)。レビューは初版の指摘から個別プロジェクトの精読を進めるたびに認識が改まり、指摘が覆されたケースが多い。**全面的に鵜呑みにせず実物と照合してから採否を決めること**。「裁定待ち」項目は kawaz の判断が要る。対応タイミングは担当セッションまたは kawaz に任せる。

## 受け入れ条件

- [x] 個別ファイル・共通ファイルの指摘を実物 (本リポのコード・DR・issue) と照合する
- [x] 各指摘について採用 / 却下と理由を判定する (裁定が要るものは「裁定待ち」として明示)
- [x] 採否の結果を本 issue に追記して close する

## 採否 (2026-09-15、実物照合の上で判定)

| 指摘 | 照合結果 | 採否 | 対応 |
|---|---|---|---|
| H-1 CI 恒常 red | 一部誤り。`continue-on-error: true` は事実だが、直近 15 run で macOS は 08-21 以降 8 run 連続 green、ubuntu は毎回違う 1〜2 本 (daemon shutdown 系 / attach 系) が落ちる。「固定 2 本」は 0 回、`notify_default_does_not_resume_self_stopped_child` はリポに存在しないテスト名 | 修正案 (2) 却下、(1) は裁定待ち | `docs/QUESTIONS.md` ECO-Q1 (統括推し: 外さず ubuntu の真因調査を継続) |
| H-2 issue 棚卸し | 主張どおり (open 42 件、最古 2026-05-26) | 採用 | 「ccmsg が必要とするもの / それ以外」の仕分けを別途実施 |
| H-8 README の位置付け | 主張どおり (ccmsg 言及 0 件) | 採用 | README 日英の Status 冒頭に 1 段落追記 |
| H-3 `web service` → `service` | 主張どおり (`service` はトップレベルに無し) | 裁定待ち | `docs/QUESTIONS.md` ECO-Q2。reference 側の追記は rules-personal の作業 |
| H-4 CLAUDE.md の dead reference | 一部誤り。dead reference と非 colocate は事実、挙げられた colocate issue は存在しない | 採用 (colocate 化は対象外) | jj-workflow / push / 言語 節を削除、hyoui 固有事実は 1 行で残す (6,628 → 6,103 B) |
| H-5 DR INDEX の Status 列 | 主張どおり (最長 1,347 字) | 採用。テンプレ横展開は rules-personal 側 | Status 列をラベル + 判定日に整理 (最長 189 字)、現役の状態記述は DR 本文へ |
| H-6 REVIEW-BACKLOG の来歴 | 一部誤り。来歴節は事実、`/tmp` symlink は既に不在 | 採用 | 来歴を `docs/journal/2026-05-27-review-backlog-migration.md` へ、symlink 前提を削除 |
| H-7 DR-0028 の優先順位 | 誤り。DR-0028 は Phase 1〜3 実装済 (2026-07-21 land、`hyoui upgrade` CLI あり) で、DR / INDEX / issue の 3 箇所が未実装扱いのまま放置されていた | 論立ては却下、真の所見 (B 方向整合の抜け) に対応 | DR Status / INDEX を実態に追随、旧 issue close、未整備の検証は `2026-09-15-upgrade-e2e-test` に切り出し |
