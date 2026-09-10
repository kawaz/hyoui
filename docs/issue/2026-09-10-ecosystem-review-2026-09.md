---
title: エコシステム外部レビュー(2026-09)の指摘への対応検討
status: open
category: task
created: 2026-09-10T14:47:48+09:00
last_read:
open_entered: 2026-09-10T14:47:48+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz依頼(2026-09-10、claude-rules-personalセッション経由)
---

# エコシステム外部レビュー(2026-09)の指摘への対応検討

## 概要

外部レビュー(2026-09-09〜10)で本リポ(hyoui)向けの指摘が出た。個別ファイルと横断
(共通)ファイルの両方を読み、各指摘の採否を検討する。

- 個別ファイル: `claude-rules-personal` の
  `docs/research/2026-09-10-ecosystem-review/hyoui.md`
- 共通ファイル: 同 `docs/research/2026-09-10-ecosystem-review/common.md`
  (hyoui 発の横展開候補 P-5 / P-6 / P-7 / P-8、hyoui にも効く横断パターン
  P-1 〜 P-49 全般)

## 背景

kawaz からの依頼(2026-09-10、claude-rules-personal セッション経由)。

温度感: レビューは初版の指摘から個別プロジェクトの精読を進めるたびに認識が
改まり、指摘が覆されたケースが多い。**全面的に鵜呑みにせず実物と照合してから
採否を決めること**。「裁定待ち」項目は kawaz の判断が要る。対応タイミングは
担当セッションまたは kawaz に任せる。

個別ファイルの指摘一覧 (優先度: ★3 次の作業で / ★2 近いうちに / ★1 気づいた
時に):

- H-1 ★3: CI の恒常 red (`ignored-tests` job `continue-on-error`) を止め、
  flaky 系 issue を `test-integrity` の形で片付ける
- H-2 ★1: open issue 42 本の棚卸し (ccmsg v2 が必要とするもの / それ以外の
  仕分け)
- H-8 ★1: README の位置付けを「ccmsg の実行基盤」に更新する
- H-3 ★1 [ccmsg 安定後]: daemon/service 体系のバックポート (`web service` →
  `service` 改名。裁定待ち箇所あり)
- H-4 ★2: CLAUDE.md を rules-personal の再編 (jj-workflow / push / 言語 /
  検証主義の重複解消、colocate 化) に追随させる
- H-5 ★2: DR INDEX の Status 列から経緯 (history narrative) を抜く
- H-6 ★1: `docs/REVIEW-BACKLOG.md` の来歴節を journal へ移し、`/tmp` symlink
  互換を廃止
- H-7 ★2 [ccmsg 安定後]: DR-0028 (daemon graceful upgrade) の優先順位を
  ccmsg 依存の観点で決め直す

共通ファイル側で hyoui が出所のパターン (バックポート候補): P-5 (DR INDEX
Status 列)、P-6 (3 category 検証)、P-7 (観測道具の bug を最優先で直す)、
P-8 (Anti-patterns は踏んだ実例で書く)。これらは反映先が rules-personal 側
なので、hyoui 側での対応は「認識のみ」でよい。

## 受け入れ条件

- [ ] hyoui.md の H-1 〜 H-8 それぞれについて、実物 (issue / CLAUDE.md /
      INDEX / README 等) と照合した上で 採用 / 却下 / 裁定待ち を判定
- [ ] 採用したものは対応 (実施 or 後続 issue 起票) し、却下したものは理由を
      本 issue に追記
- [ ] 裁定待ち (H-3, H-5 の横展開部分) は kawaz の判断を仰ぐ
- [ ] 結果を本 issue に追記して close する

## TODO

<!-- wip 時のみ -->
