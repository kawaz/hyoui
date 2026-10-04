---
title: web の監督者が制御 socket の要求を main loop で読み、改行を送らない client 1 つで 5 秒止まる
status: open
category: bug
created: 2026-10-04T16:30:00+09:00
last_read: 2026-10-04T16:30:00+09:00
open_entered: 2026-10-04T16:30:00+09:00
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

# web の監督者が制御 socket の要求を main loop で読み、改行を送らない client 1 つで 5 秒止まる

## 現象

`hyoui web daemon supervise` (監督者) は制御 socket の 1 要求 (JSON 1 行) を main loop の中で読み、read timeout は 5 秒。改行を送らない client が 1 つ繋ぐだけで、その間監督者の event loop が止まる (子の監視・再起動、他の CLI 要求への応答が 5 秒遅れる)。

DR-0038 の実装中に、監督者 socket を `hyoui/web/` 直下に置いた build で観測 (2026-10-04): session の discovery が監督者 socket を session と誤認して handshake を送り、監督者は 1 行読みで待ち、互いを待って `hyoui list --all-namespaces` が 5.05 秒、その最中の `hyoui web daemon list` が 4.95 秒かかった。DR-0038 では監督者 socket を `run/` に下げて discovery から外したので、この経路では起きなくなったが、監督者の読み方そのものは変わっていない。

## 方向

監督者の event loop は外部の応答を待たない (DR-0037 が session daemon について定める不変条件と同じ考え方。監督者は DR-0037 の対象外だが、同じ理由で止まってはならない)。要求の読み取りを nonblocking にするか、接続ごとに読み取りを main loop の外に出す。

## 関連

- `docs/issue/2026-09-29-daemon-must-never-hang.md` (session daemon 側の同種の問題)
- DR-0037 (daemon のイベントループは外部の応答を待たない)
