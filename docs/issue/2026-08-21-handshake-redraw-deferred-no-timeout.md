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

## 追記 (2026-08-21、fable5-high の独立 probe)

前提を実測で補正。

### 実挙動の補正

起票時の前提「redraw が無期限保留される」は、**menu 発動条件を満たす attach (rw leader + tty) では成立しない**。
attach client は `conn.run()` の前に `send_initial_resize()` を送り (`crates/hyoui-cli/src/main.rs:1088`)、
`handle_resize` (`daemon/control.rs:711`) が同サイズでも無条件に `screen_state.resize()` を呼び、
`resize()` (`daemon/screen/state.rs:243`) が Parser 再構築後に `sync_in_progress = false` を無条件代入する。
次の serve_loop 周回で `flush_pending_redraws_if_sync_over` が deferred redraw を flush するため、
**deferral は attach 自身の resize 副作用で数 ms 後に解除される**。

実測: 子が `?2026h` → SIGSTOP した session に何も入力せず attach し、outer 画面を 200ms 刻みで観測すると
t=200ms で既に SYNC-OPEN (= 保留されたはずの redraw) が可視。

### ただし緩和は accidental かつ fragile

この解除は設計された挙動ではなく副作用。しかも `resize()` の doc comment は「replay 中に `?2026h` を踏めば
再度 detect される」と書いているが、**input_log の replay は vt100 の `process()` 直呼びで
`update_sync_flag_with_carry` を通らないため未閉 `?2026h` は再検出されない** = doc と実装が矛盾している。
doc どおりに実装を直した瞬間、deferral は本物の無期限保留に戻る。また resize が mid-sync state を黙って
捨てること自体、DR-0014 の partial state 規律 (自動破棄するなら判定基準を DR に明示) と要整合。

### 追加論点

案 A/B/C に加えて「案 D: `resize()` の sync 扱いを doc と一致させる (= replay で再検出する)」を検討する場合、
それ単独では無期限保留を復活させるだけなので案 A or B とセットで裁定する必要がある。

### client 側への影響

2026-08-21 の menu focus 修正 (attach.rs) は redraw ゼロでも menu が成立する構造で、この accidental 緩和に
依存していないことをレビューで確認済み。**本 issue の裁定がどちらに転んでも client 側は成立する**。

## 受け入れ条件

- [ ] 案 A/B/C のいずれかを裁定し、採用するなら DR に判定基準を明記
- [ ] accept.rs のコメント誤参照 (§6 → §4) を修正

## TODO

