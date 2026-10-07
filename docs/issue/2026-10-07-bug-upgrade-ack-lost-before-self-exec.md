---
title: hyoui upgrade で daemon が upgrade.ack を送り終える前に self-exec し、client が「recv error before ack」で exit 1 になることがある
status: open
category: bug
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

# hyoui upgrade で daemon が upgrade.ack を送り終える前に self-exec し、client が「recv error before ack」で exit 1 になることがある

## 観測 (2026-10-07、DR-0041 の実装 worker)

- tag が upgrade をまたいで残ることのテスト (`tags_survive_a_daemon_upgrade`) を書いている時に見つかった。1 回目の `just ci` で 1 件落ち、単独で 6 回回して 1 回再現した
- daemon は upgrade.ack を client の writer thread に積むだけで、送り終わるのを待たずに self-exec する (`crates/hyoui/src/daemon/control.rs` の upgrade の処理、DR-0028 §4)。self-exec で socket の fd 越しの書き込みが途切れ、client は ack を受け取れずに exit 1
- upgrade 自体は成功している (daemon は新しいバイナリで動き続け、tag も残る)。client の終了コードと表示だけが誤る

## テストでの扱い

`tags_survive_a_daemon_upgrade` は、このテストが確かめたいのは upgrade 後の tag であって ack の配送ではないため、「recv error before ack」の 1 種類の失敗だけを許している (Design rationale をコメントに書いてある)。本 issue を直したら、その許容を外す。

## 直す方向 (未検討)

- ack を送り終えた (writer の queue が空になった、または write が返った) ことを確かめてから self-exec する
- DR-0037 (daemon の nonblocking 化、NB-Q2 の writer thread の扱い) と関わる
