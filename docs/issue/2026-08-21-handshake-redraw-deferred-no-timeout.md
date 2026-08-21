---
title: handshake 直後の attach redraw が sync update 中は無期限に deferred される (timeout 機構なし)
status: open
category: bug
created: 2026-08-21T12:45:02+09:00
last_read:
open_entered: 2026-08-21T12:45:02+09:00
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

# handshake 直後の attach redraw が sync update 中は無期限に deferred される (timeout 機構なし)

## 概要

daemon は handshake 直後に attach 復元用 redraw を必ず 1 つ送る契約 (DR-0013 §4 Phase A) だが、
`accept.rs:483` で `screen_state.sync_in_progress()` (= DEC 2026 sync update 中) の場合は送らず
`pending_redraws` に積む。flush 条件は `flush_pending_redraws_if_sync_over` (session.rs:955) =
子の出力で sync が終わることのみで、**timeout 機構が無い** (`update_sync_flag_with_carry` は
子出力 bytes でしか進まない)。

## 背景

子が `?2026h` 送出後〜`?2026l` 前に SIGSTOP/SIGTSTP で停止すると (signal は非同期なので
mid-frame stop は普通に起きる。claude TUI は sync update を多用するため主要ドッグフーディング
対象で踏む)、redraw が永久に出ない。この状態で client 側が「redraw 到着後に menu を出す」実装を
していると「redraw を出すには子の resume が要る / resume する手段の menu は redraw 待ち」の
循環デッドロックになる。

2026-08-21 の menu focus 修正で client 側は redraw 到着に従属しない形 (冒頭で menu を描き、
初回 RAW_DATA を resume 証拠として扱わない) に組み替えて回避したが、**daemon 側の
「redraw が無期限に出ない」根本は残っている**。

### 検討事項 (未裁定)

- 案 A: 子が stopped の間は sync 中でも deferred redraw を flush する (stopped 中は新規 bytes
  が来ないので mid-sync snapshot の送出が最善という考え方)
- 案 B: deferral に timeout を設ける
- 案 C: 現状維持 (client 側が redraw 到着に依存しない設計を保てば実害が無い、という立場)

案 A/B は「壊れかけの partial state を送る」判断を含むため、DR-0014 の partial state 規律
(default は warn のみ + 自動破棄しない、判定基準を DR に明示 + false-positive のマトリクス検証)
に従い、採用するなら判定基準を DR に書くこと。

### 出典

2026-08-21、DR-0032 menu focus bug の設計レビュー (fable5-high) が client 側修正案の検討中に発見。

### 関連

DR-0013 §4 Phase A (attach redraw の契約、なお accept.rs のコメントにある「(§6)」参照は
DR-0013 §6 = alternate screen hook を指しており誤読を誘うので直すとよい) / DR-0014
(partial state を扱う実装の規律) / DR-0032 §2 (child action menu)

## 受け入れ条件

- [ ] 案 A/B/C のいずれかを裁定し、採用するなら DR に判定基準を明記
- [ ] accept.rs のコメント誤参照 (§6 → §4) を修正

## TODO

