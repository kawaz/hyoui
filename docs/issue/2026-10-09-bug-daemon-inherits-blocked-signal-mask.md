---
title: 呼び出し元で block された signal (SIGTERM 等) が daemon の mask に残り、kill -TERM <daemon> が効かない
status: open
category: bug
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

# 呼び出し元で block された signal (SIGTERM 等) が daemon の mask に残り、kill -TERM <daemon> が効かない

## 観測 (2026-10-09、SIG-Q1 の実装 worker が 0.14.2 で実測)

perl で SIGTERM / SIGUSR2 を block した呼び出し元から `hyoui run --detached` で起動すると、daemon の signal mask にも残り、`kill -TERM <daemon pid>` が効かなかった。SIG-Q1 (DR-0043) で直したのは子の側で、daemon 自身の mask は対象外として DR-0043 の境界にだけ書いた。

## 直す方向

daemon は起動時 (daemonize の子の側、self-pipe の handler を張る前) に、自分が handler を張る signal の block を外す (または mask を空にしてから必要なものだけ block する)。子への介入ではなく daemon 自身の手入れなので、DR-0043 に決定として足すか、daemonize の実装メモで足りる。
