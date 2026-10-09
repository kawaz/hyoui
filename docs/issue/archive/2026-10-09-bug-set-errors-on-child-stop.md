---
title: hyoui set の応答待ちで子が止まると unexpected response で失敗するはず (未確認)
status: resolved
category: bug
created: 2026-10-09T19:00:00+09:00
last_read: 2026-10-09T20:00:00+09:00
open_entered: 2026-10-09T19:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-09T20:00:00+09:00
discard_reason:
pending_reason:
close_reason: 実機で 60 回中 7 回の失敗を確認。読み飛ばす通知の判定を ControlMessage::is_unsolicited_notification にまとめ、set / status / screen / record / discovery / web の待ちで使う。修正後 60 回中 0 回
blocked_by:
---

# hyoui set の応答待ちで子が止まると unexpected response で失敗するはず (未確認)

`hyoui set` も rw で接続し、応答待ちで読み飛ばすのは ModeChange と LeaderNotify だけ (`crates/hyoui-cli/src/main.rs` の 2908 行付近)。`wait:` の待ち中に子が止まると失敗していた件 (v0.15.2 で修正) と同じ形で、待っている間に SessionChildStoppedNotify 等が届くと失敗するはず。2026-10-09 に worker がコードを読んで気づいた。まず実機で確かめ、失敗するなら wait と同じ読み飛ばしを足す (rw client に届く通知の読み飛ばしを 1 か所にまとめるのが望ましい)。
