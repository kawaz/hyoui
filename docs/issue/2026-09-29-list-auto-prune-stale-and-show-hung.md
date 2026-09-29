---
title: hyoui list は残骸 socket を常に自動掃除し、応答しない生き daemon は hung として pid 付きで表示する
status: open
category: request
created: 2026-09-29T14:25:41+09:00
last_read:
open_entered: 2026-09-29T14:25:41+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# hyoui list は残骸 socket を常に自動掃除し、応答しない生き daemon は hung として pid 付きで表示する

## 概要

kawaz 裁定 2026-09-29: 「stale が残る状況が意味不明、list 時点で掃除すればよい」→ 条件付きで採用。

現在 `discovery.rs` の stale は 2 種類を同じ status に畳んでいる:

1. **connect 拒否 (ECONNREFUSED 等)** = daemon が SIGKILL / panic / OS 再起動で死んで socket ファイルだけ残った残骸。誰も使えないので `hyoui list` が常に unlink してよい (`--prune-stale` フラグは廃止)
2. **connect 成功だが status 応答なし** = daemon は生きているが固まっている (例: 2026-09-29 の stopped client への sendto ブロック)。socket を消すと生き daemon と子が孤児化して attach も kill もできなくなるので unlink しない。`hung` のような別 status で daemon pid を表示し、kill の手掛かりを残す

## 受け入れ条件

- [ ] `hyoui list` (namespace 指定 / `--all-namespaces` 双方) が connect 拒否の socket を表示せず unlink する
- [ ] 応答しない生き daemon は `hung` (名称は実装時に決める) として daemon pid 付きで表示される
- [ ] `--prune-stale` を削除し、help / completion / 実装の 3 者を同時に更新する
- [ ] web gateway のセッション一覧 (同じ discovery を使うなら) も同じ区別で表示する
- [ ] DR-0006 §2 の stale socket 記述と DR-0018 の `--prune-stale` 記述を更新する
