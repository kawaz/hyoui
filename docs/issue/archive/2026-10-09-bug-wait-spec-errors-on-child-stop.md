---
title: hyoui input の wait: で待っている間に子が止まると「予期しない message」で失敗するはず (未確認)
status: resolved
category: bug
created: 2026-10-09T13:00:00+09:00
last_read: 2026-10-09T19:00:00+09:00
open_entered: 2026-10-09T13:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-09T19:00:00+09:00
discard_reason:
pending_reason:
close_reason: 実機で 4 種類とも約 1.1 秒で失敗することを確認。wait の snapshot 応答待ちで child.stopped.notify と upgrade.ack を読み飛ばし、exit.notify は専用の文言で終える。set の同じ形は set-errors-on-child-stop
blocked_by:
---

# hyoui input の wait: で待っている間に子が止まると「予期しない message」で失敗するはず (未確認)

## 見立て (2026-10-09、wait の全角の修正をした worker のコード読解、実機未確認)

`hyoui input` は rw で接続するので、daemon から SessionChildStoppedNotify も届く。一方、`crates/hyoui-cli/src/wait_core.rs` の `fetch_snapshot` などが読み飛ばすのは ModeChange / LeaderNotify だけで、`wait:` spec で待っている間に子が止まると、届いた StoppedNotify を予期しない message として扱い、エラーで終わるはず。

## 確かめること

`hyoui input S wait:<pattern> ...` で待っている間に、外から子に `kill -STOP` を送り、input がどう終わるかを見る。エラーになるなら、wait の受信で読み飛ばす message の一覧に StoppedNotify (と、rw client に届く他の通知) を足すか、wait は ro で接続するかを決める。
