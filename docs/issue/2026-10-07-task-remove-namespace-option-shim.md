---
title: 廃止した namespace の option を受け付けて捨てる処理を 2026-11 に消す
status: open
category: task
created: 2026-10-07T11:00:00+09:00
last_read: 2026-10-07T11:00:00+09:00
open_entered: 2026-10-07T11:00:00+09:00
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

# 廃止した namespace の option を受け付けて捨てる処理を 2026-11 に消す

**2026-11 になったら消す** (kawaz 2026-10-07:「しばらくと言ってるとやらないので 11 月になったら消すと決めておく」)。

DR-0041 で namespace を廃止したが、既存の呼び出し (ccmsg の hyoui 連携、手元の script) が起動できなくならないよう、`--namespace` / `--all-namespaces` 等を parse だけ受け付けて値を捨て、stderr に 1 行の注意を出している。

## 消すもの

- `crates/hyoui/src/cli.rs` の `strip_removed_namespace_options` と `REMOVED_NAMESPACE_NOTICE`
- `crates/hyoui-cli/src/main.rs` の呼び出し (360 行付近)
- テスト `removed_namespace_options_are_accepted_and_ignored`、e2e `removed_namespace_options_are_ignored_with_one_notice` (消した後は「unknown option になる」テストに戻す)
- DR-0041 決定 1 の「2026-11 まで受け付けて捨てる」の文、MANUAL の該当の記述

## 消す前に確かめること

- ccmsg 側の追従 (kawaz/ccmsg `docs/issue/2026-10-06-hyoui-namespace-removal-and-socket-dir.md`) が済んでいるか。済んでいなくても期日で消す方針だが、ccmsg の hyoui 連携が壊れることは先に知らせる
