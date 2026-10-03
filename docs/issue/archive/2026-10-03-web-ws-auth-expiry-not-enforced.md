---
title: web WS 接続に access の期限と失効が効いていない (DR-0036 決定 5 の WS 側が未実装)
status: resolved
category: bug
created: 2026-10-03T16:20:00+09:00
last_read: 2026-10-04T00:04:42+09:00
open_entered: 2026-10-03T16:20:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-04T00:04:42+09:00
discard_reason:
pending_reason:
close_reason: WS 接続ごとに開いた family と access 期限を持ち、期限切れ / family 失効 / auth.extend 拒否 / 同一プロセスの再利用検知で close code 4401 を付けて閉じる。auth.extend はその接続の family の現行 access だけ受ける。front は 4401 で access を捨てて取り直す
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

## 決着 (2026-10-04)

3 つとも実装した。正本は DR-0036 決定 4 / 決定 5 と DR-0035 決定 1 の WS close 表。

- **期限での切断**: WS は接続ごとに「開いた family」と「現在の access の期限」を持つ (`crates/hyoui-web/src/auth/ws.rs` の `WsAuth`)。期限までに延ばさなかった接続を gateway が閉じる。これで `auth.extend` を送らない接続にも「最長 access TTL」の上限が効く
- **`auth.extend` の family 照合**: 延ばせるのはその接続を開いた family の現行 access だけ。別 family / 期限切れ / 不明 / tombstone は `ok:false` の後に閉じる
- **再利用検知時の WS 切断**: `/auth/refresh` が再利用を検知して family を tombstone にしたら、同じプロセス内の確立済み接続に知らせ、その family の接続だけを切る。DR の「その sub の WS を切る」は「その family の WS を切る」に直した (失効するのは family 1 本で、同じ sub の他 family の接続を切っても有効な token で繋ぎ直せるため)。CLI の失効は従来どおり通知せず、延長か期限で切れる
- **close code**: 認証が終わって閉じる時は `4401` (reason は `access expired` / `access revoked` / `access rejected`)。front (`session.js`) はこれを受けたら閉じた接続の access を捨てて取り直し、通らなければログイン overlay に進む。`WEB_PROTOCOL_VERSION` は上げない (close code と golden の追加、DR 表の誤記訂正だけで、kind / field の削除・意味変更が無い)
- DR-0035 表の `auth.extend.result` の field 名を実装 (`expires_at`) に合わせ、golden test を足した

検証: `auth::ws` の test が期限 / 延長 / family 照合 / 再利用検知 / 購読前の失効を tokio の時計を止めて固定、e2e (`web_e2e_api.rs`) が実 gateway で close code `4401`・別 family の拒否・再利用検知での切断を固定。実ブラウザ (Chrome) で、別 family の access が tab-share で届いた時に `4401 access rejected` の後 1 秒で新しい access に繋ぎ直すこと、再利用検知で `4401 access revoked` の後にログイン overlay が出ることを観測した。

ログイン overlay を出している間、session ページの fallback polling が `/auth/refresh` を約 0.7 回/秒で叩き続けることも観測したが、変更前の assets でも同じ頻度で起きる (本件の変更とは独立)。
