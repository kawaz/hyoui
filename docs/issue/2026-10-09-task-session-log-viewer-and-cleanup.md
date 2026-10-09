---
title: session ごとの daemon ログ (sessions/logs/<id>.log) を CLI から見る手段と、溜まったログの片付け
status: open
category: task
created: 2026-10-09T15:00:00+09:00
last_read: 2026-10-09T15:00:00+09:00
open_entered: 2026-10-09T15:00:00+09:00
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

# session ごとの daemon ログ (sessions/logs/<id>.log) を CLI から見る手段と、溜まったログの片付け

DR-0037 段 2 で、起動後の daemon の fd 2 と logger は `<状態の root>/sessions/logs/<id>.log` に書くようになった。空のログは終了時に消すが、中身のあるログは自動では消さない (1 MiB で打ち切り、同じ id の再起動は追記)。

## 提案 (実装 worker、未裁定)

- 見る手段: `hyoui log <id>` (`tail -f` 相当、`--follow`)。reference `cli-daemon-subcommands` の `daemon log` の語彙と揃えるか検討
- 片付け: `hyoui list` が stale の socket を片付けるのに合わせて、その session のログも消すか、明示の `--prune` 等で消すか。daemon が SIGKILL 等で終了処理を経ずに終わると空のログが残る点も含めて
