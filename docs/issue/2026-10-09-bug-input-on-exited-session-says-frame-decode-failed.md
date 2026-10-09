---
title: 子がすぐ exit した session に input を送ると「auto-lock acquire 失敗: recv 失敗: frame decode failed」と出る (EOF の伝え方が分かりにくい)
status: open
category: bug
created: 2026-10-09T19:00:00+09:00
last_read: 2026-10-09T19:00:00+09:00
open_entered: 2026-10-09T19:00:00+09:00
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

# 子がすぐ exit した session に input を送ると「auto-lock acquire 失敗: recv 失敗: frame decode failed」と出る (EOF の伝え方が分かりにくい)

2026-10-09、実機確認の途中で worker が観測。子が exit して daemon が終わりかけている session に `hyoui input` を送ると、auto-lock の取得で接続の EOF を受け、「frame decode failed」という文言で終わる。利用者に必要なのは「session は既に終わっている」こと。EOF を受けた時の文言を、原因と次の行動 (`hyoui list` で確かめる等) にする (rule `interface-wording`)。
