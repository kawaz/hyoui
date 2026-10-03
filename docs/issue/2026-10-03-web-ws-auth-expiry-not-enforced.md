---
title: web WS 接続に access の期限と失効が効いていない (DR-0036 決定 5 の WS 側が未実装)
status: open
category: bug
created: 2026-10-03T16:20:00+09:00
last_read: 2026-10-03T16:20:00+09:00
open_entered: 2026-10-03T16:20:00+09:00
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

# web WS 接続に access の期限と失効が効いていない

出典: `docs/findings/2026-10-03-web-api-protocol-inventory.md` §3 / §5 / §7 (実機 probe とコード読解)。

## 現象

DR-0036 決定 4 / 5 は「長命 WS は `auth.extend` で延ばし、切るのは延長を怠った接続だけ」「失効が確立済み接続に効くのは `auth.extend` 時で、最長の猶予は access TTL (4 時間)」と決めている。実装には次の 3 つが無い。

1. **期限での切断**: `ws_attach.rs` は `hello.auth_expires_at` を送るだけで、接続側に期限を持たず、期限超過で閉じる処理が無い。`auth.extend` を送らない client の接続は無期限に残る
2. **`auth.extend` の family 照合**: `routes.rs` の `extend()` は提示された access を `find_live_family_by_access` で引くだけで、その接続を開いた family と同じかを見ない。別 family の有効な access を出せば `ok:true` になる
3. **再利用検知時の WS 切断**: refresh の再提示を検知すると family は失効するが、その sub の WS は切られない (決定 4 の「その sub の WS を切る」)

1 と 2 があるため、失効 (tombstone) させても `auth.extend` を送らない接続には失効が永久に届かず、DR-0036 が上限として約束する「最長 4 時間」が成り立たない。

## 対処の方向 (裁定は実装時)

- 接続ごとに「開いた family」と「現在の期限」を持ち、期限超過で close する (close code / reason で失効を browser に伝えるかは、findings の論点「close code が常に 1005」と合わせて決める)
- `auth.extend` は接続の family と一致する access だけを受け、期限を更新する
- 再利用検知 / CLI による失効を確立済み接続に伝える経路 (決定 4 は「CLI は gateway に通知しない」なので、期限切断で上限 4 時間を保証するのが最小)

## 関連

- DR-0036 (Status を 🟡 部分実装に更新済み)、DR-0035 (`auth.extend.result` の field 名が契約表 `auth_expires_at` と実装 `expires_at` で食い違う件も同じ findings §7)
