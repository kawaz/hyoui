---
title: client ごとの受信途中 buffer に合計の上限が無い (1 client 最大 16MiB、client 数に比例)
status: open
category: design
created: 2026-10-09T14:00:00+09:00
last_read: 2026-10-09T14:00:00+09:00
open_entered: 2026-10-09T14:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# client ごとの受信途中 buffer に合計の上限が無い (1 client 最大 16MiB、client 数に比例)

## 背景

DR-0037 段 1 (client 受信の増分 decoder 化) で、frame の途中で止まった client が居ても serve loop は止まらなくなった。その代わり、各 client が frame の上限 (16MiB) までの途中の bytes を同時に抱えられる。以前は途中の client が居ると loop ごと止まったので、同時に溜まる途中の bytes は 1 client 分だった。

- 実際に送られた bytes の分しか確保しない (宣言 size の先行確保はしない)
- 相手は同じユーザーで handshake を通った client に限られる
- 64 client が全員 16MiB の途中 frame を送れば、理論上 1GiB になる

## 論点 (決めない)

- 受信途中の bytes に、client ごと・合計のどちらかで上限を設けるか。超えたら、その client を切るか
- frame の上限 (16MiB) 自体が今の用途 (input の file: spec 等) に対して適切か
- DR-0037 の不変条件 (loop は外部の応答を待たない) の対として、「1 client が daemon の資源を際限なく使えない」を DR に書くか
