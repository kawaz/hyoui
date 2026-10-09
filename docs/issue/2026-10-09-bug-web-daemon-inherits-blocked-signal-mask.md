---
title: web daemon も呼び出し元で block された SIGTERM / SIGINT を mask に残すはず (未確認)
status: open
category: bug
created: 2026-10-09T19:00:00+09:00
last_read: 2026-10-09T19:00:00+09:00
open_entered: 2026-10-09T19:00:00+09:00
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

# web daemon も呼び出し元で block された SIGTERM / SIGINT を mask に残すはず (未確認)

session の daemon は v0.15.2 で、起動直後に自分が handler を張る signal の block を外すようにした (DR-0043 決定 6)。web daemon (`crates/hyoui-cli/src/web_daemon/supervisor.rs` ほか) も SIGTERM / SIGINT に handler を張るが、block を外していない。block した呼び出し元から起動すると、`kill -TERM` が効かないはず。2026-10-09 に worker がコードを読んで気づいた。launchd から起動する場合は mask が空なので、起きるのは手で起動した時。
