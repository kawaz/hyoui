---
title: serve_admits_new_client_while_peer_floods_tiny_frames が ubuntu CI で 1 回落ちた (flood の frame の処理が進まない、真因未調査)。v0.16.2 のリリースを止めている
status: resolved
category: bug
created: 2026-10-09T21:00:00+09:00
last_read: 2026-10-14T11:00:00+09:00
open_entered: 2026-10-09T21:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-14T11:00:00+09:00
discard_reason:
pending_reason:
close_reason: 真因はテストの測り方。flood への応答を数える reader thread が、1 core では新 client の往復 (約 1ms) の間に走らないことがあり、その間も daemon は flood の frame を処理して書き込んでいた (計装で確認)。daemon 側の順序 (flood が受け取る StatusResponse の client 一覧に新 client が載った数) で数えるようテストを直した。docker の Linux で 1 core 固定: 修正前 5/500 (+ 負荷付き 6/200) 失敗、修正後 0/300 (+ 負荷付き 0/600)
blocked_by:
---

# serve_admits_new_client_while_peer_floods_tiny_frames が ubuntu CI で 1 回落ちた (flood の frame の処理が進まない、真因未調査)。v0.16.2 のリリースを止めている

## 観測

- 落ちたのは CI の「daemon session tests pinned to 1 core (Linux)」step (`taskset -c 0 cargo nextest run ... -E 'test(/daemon::session/)'`) だけで、同じ job の `just ci` (多 core) では毎回通っている
- v0.16.2 からの regression ではない。同じ判定 (`flood frames were being processed meanwhile (N -> N)`) で落ちた run は v0.16.1 の CI (run 37897208260、106 -> 106)、v0.16.2 の Release (run 37904320583、106 -> 106)、v0.16.3 の Release (run 38495809311、136 -> 136)。同じ commit の別 run は通っている (v0.16.2 / v0.16.3 の CI)。v0.16.1 → v0.16.2 の daemon / sys の差分は `unblock_handled_signals` を `sys::unblock_signals` に寄せただけで、serve loop の経路は変わっていない
- docker の Linux (rust:1.98、arm64、10 core) で再現した。該当テストを単体で繰り返し:

| 条件 | 回数 | 失敗 |
|---|---|---|
| core 制限なし | 20 | 0 |
| `taskset -c 0` | 20 + 200 + 300 (計装入り) | 0 + 2 + 3 |
| `taskset -c 0` + 同じ core に busy loop | 200 | 6 |

## 真因

テストは「新 client の handshake + status の間に flood の処理が進んだ」を、flood への応答を読む reader thread の受信数 (`answered`) の前後差で見ていた。これは daemon の処理ではなく reader thread がその間に CPU を得たかを測っている。

daemon・test の各 thread に時刻付きの計装を入れて失敗時の時系列を取ると、3 回の失敗とも、新 client の往復の間 (約 0.8〜1.2ms) に daemon は flood の frame を 3〜7 個処理し、その応答を flood の socket に書き終えていた (WriteProgress::Drained)。reader の受信は 0 回で、最後の受信は窓の直前だった。失敗した 3 回の窓の長さは 0.79 / 1.03 / 1.18ms で、300 回中の窓の中央値は 7.2ms。1 core では短い窓の間に reader thread が起こされないことがある。

## 対処

テストの判定を daemon 側の順序で数えるようにした (`serve_admits_new_client_while_buffered_frames_drain` が leader.notify の位置で数えるのと同じ考え方)。

- 新 client は status.query と detach (自分) を 1 回の write で送る。daemon は 1 周に 1 client 1 frame なので、新 client は登録から「status に応答した次の周回」まで client 一覧に載る
- reader は flood が受け取った StatusResponse ごとに client 一覧の id を数え、接続が閉じるまで読む。新 client の id が載った応答の数 = その間に daemon が処理した flood の frame の数で、これが 0 より大きいことを見る
- kill は別の client から送る (新 client は detach 済み)

status が 1 秒以内に応答すること、status 時点で 2 client が attach していること、flood の writer が続いていることの判定は変えていない。

## 検証

- 判定が効くことの確認: serve loop で「2 client 以上居る時は最後の client 以外の frame を読まない」(= 新 client が居る間 flood を止める) 改変を入れると、修正後のテストは 3 回とも `0 flood responses listed the newcomer` で落ちる。改変なしでは数は 3〜15
- docker の Linux で修正後のテストを単体で繰り返し: `taskset -c 0` で 0/300、制限なしで 0/100、`taskset -c 0` + 同じ core に busy loop で 0/600
