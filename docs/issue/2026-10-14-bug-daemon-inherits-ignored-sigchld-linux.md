---
title: "daemon 自身が呼び出し元の SIGCHLD 無視を引き継ぐと、Linux で早期に終わった子が自動回収され exit code 0 を報告しうる (未検証仮説)"
status: open
category: bug
created: 2026-10-14T09:40:00+09:00
last_read: 2026-10-14T09:40:00+09:00
open_entered: 2026-10-14T09:40:00+09:00
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

# bug: daemon が呼び出し元の SIGCHLD 無視を引き継ぐ (Linux、未検証)

## 仮説

DR-0043 は子 (PTY の前景 job) の signal を既定に戻すが、daemon 自身の signal の扱いは対象外。呼び出し元が SIGCHLD を SIG_IGN にしていると daemon も無視のまま始まる。Linux では SIGCHLD 無視のプロセスの子は終了時に zombie にならず自動回収されるので、子の fork から serve loop が SIGCHLD handler を張るまでの隙間に子が終わると、daemon の `waitpid` が ECHILD になり `reap_blocking` が 0 を返す = `hyoui run -- false` が 0 を報告しうる。

## 観測済みの材料 (SIG-Q2 実装時、2026-10-14)

- macOS (Darwin 25.5): 引き継いだ SIG_IGN では自動回収は起きず (`CHLD=IGNORE reap=7`)、同じプロセス内で SIG_IGN にした時だけ ECHILD。macOS は影響しない見込み
- Linux (docker python:3-slim): SIGCHLD 無視の呼び出し元から exec した子の `waitpid` は ECHILD

## 確認手順

Linux 版 hyoui で `perl -e '$SIG{CHLD}="IGNORE"; exec @ARGV' -- hyoui run -- false` 相当を実行し、exit code が 1 になるか (0 なら再現)。再現したら、daemon 起動直後 (fork 前) に SIGCHLD を SIG_DFL (または handler) にする最小介入を DR-0043 の決定 6 (daemon 自身の mask) の並びで検討。

## 関連

- [DR-0043](../decisions/DR-0043-child-starts-with-default-signals.md)
- `crates/hyoui/src/daemon/session.rs` の `reap_blocking` (ECHILD を見た時の代わりの値)
