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

## 設計裁定 (統括 2026-09-29、worker の衝突報告への回答)

`crates/hyoui-cli/src/main.rs` の `enrich_entries_with_status` 周辺にある「list で timeout を使わない」判断は「本物の daemon は local socket で必ず即応答する」前提で、2026-09-29 に実機で崩れた (daemon が生きたまま無応答) ので反転する。旧判断が守ろうとした「遅いだけの daemon を壊れた扱いにして誤情報を出さない」は以下で守る:

- timeout は 5 秒 (DR-0006 の RAW_ACK_TIMEOUT と同値) を定数化し根拠を書く
- 超過は silent に stale へ落とさず `hung` として daemon pid と理由 (「5 秒応答なし」) を表示、stderr warning も残す、unlink しない
- 実装は ClientConnection API に handshake 前から効く timeout (`connect_with_timeout` 相当) を追加して `discovery.rs` と list 双方で使う。thread detach 方式は web gateway で leak するので不採用
- daemon pid は peer credential (macOS `LOCAL_PEERPID` / Linux `SO_PEERCRED`) で connect 直後に取る (`sys/` 配下に wrapper)
- main.rs の doc comment は現在形で書き直す

## 受け入れ条件

- [ ] `hyoui list` (namespace 指定 / `--all-namespaces` 双方) が connect 拒否の socket を表示せず unlink する
- [ ] 応答しない生き daemon は `hung` (名称は実装時に決める) として daemon pid 付きで表示される
- [ ] `--prune-stale` を削除し、help / completion / 実装の 3 者を同時に更新する
- [ ] web gateway のセッション一覧 (同じ discovery を使うなら) も同じ区別で表示する
- [ ] DR-0006 §2 の stale socket 記述と DR-0018 の `--prune-stale` 記述を更新する
