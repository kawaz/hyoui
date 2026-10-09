---
title: hyoui wait が全角文字の連続に一致しない (全角セルの継続部分が空白として連結されている)
status: resolved
category: bug
created: 2026-10-09T11:00:00+09:00
last_read: 2026-10-09T13:00:00+09:00
open_entered: 2026-10-09T11:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-09T13:00:00+09:00
discard_reason:
pending_reason:
close_reason: daemon は継続 cell を空の cell として送り、wait はそれを空白で埋めていた。1 行の text 化を screen_text::row_text にまとめ、wait と screen dump の text の両方がそれを使う。input の wait: spec も同じく直った
blocked_by:
---

# hyoui wait が全角文字の連続に一致しない (全角セルの継続部分が空白として連結されている)

## 観測 (2026-10-09、0.14.2)

child action menu の行 `子プロセスが停止中` が画面に出ている時、`hyoui wait $S '停止中'` は rc=1、`'停 *止 *中'` は rc=0。`hyoui screen dump --format=text` では連続して見える。

## 見立て (推測)

wait が照合する文字列を作る時、全角文字の 2 セル目 (継続セル) を空白として連結している。screen dump の text 形式は継続セルを飛ばしているので、両者の作り方が食い違っている。

## 直す方向

wait の照合用の文字列と screen dump の text を同じ関数で作り、継続セルを出さない。全角を含むパターンのテストを足す。
