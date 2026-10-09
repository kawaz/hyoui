---
title: fd が毎周 Ready になる client が居る間、serve loop の Timeout 時の子の状態確認が走らない (macOS で止まった子の復帰に気づくのが遅れる)
status: open
category: design
created: 2026-10-09T16:00:00+09:00
last_read: 2026-10-09T16:00:00+09:00
open_entered: 2026-10-09T16:00:00+09:00
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

# fd が毎周 Ready になる client が居る間、serve loop の Timeout 時の子の状態確認が走らない (macOS で止まった子の復帰に気づくのが遅れる)

## 観測 (2026-10-09、DR-0037 段 1 の実装 worker のコード読解)

serve loop の子の状態確認 (stopped 中の復帰確認、self-pipe が無い時の確認) は、poll が Timeout を返した周回でだけ走る。frame を頻繁に送る client が居て poll が毎周 Ready を返す間は、この確認が走らない。DR-0037 段 1 で buffered frame の周回の確認漏れは直したが、Ready の経路のこの性質は段 1 より前からある。

## 実害の範囲

production の daemon は 1 process に serve が 1 つで self-pipe を持つので、SIGCHLD で子の状態の変化は通知される。macOS は子が continue した時に SIGCHLD を送らない (WCONTINUED が報告されない) ため、止まっていた子の復帰は Timeout 周回の procstate 直読みでしか気づけない。chatty な client が居る間、復帰の検知が遅れる (status の child_state が stopped のまま残る)。

## 直す方向 (未検討)

子の状態確認を「Timeout の周回」でなく「前回の確認から一定時間が経った周回」で行う (deadline 化)。DR-0037 段 6 の「sleep の deadline 化」と同じ考え方。
