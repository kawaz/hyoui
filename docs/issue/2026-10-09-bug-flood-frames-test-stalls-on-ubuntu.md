---
title: serve_admits_new_client_while_peer_floods_tiny_frames が ubuntu CI で 1 回落ちた (flood の frame の処理が進まない、真因未調査)。v0.16.2 のリリースを止めている
status: open
category: bug
created: 2026-10-09T21:00:00+09:00
last_read: 2026-10-09T21:00:00+09:00
open_entered: 2026-10-09T21:00:00+09:00
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

# serve_admits_new_client_while_peer_floods_tiny_frames が ubuntu CI で 1 回落ちた (flood の frame の処理が進まない、真因未調査)。v0.16.2 のリリースを止めている

## 観測

- v0.16.2 の Release workflow (run 37904320583) の「ci / Test (ubuntu-latest / stable)」で落ちた。macOS は通っている。v0.16.1 の ubuntu では同じテストが通っていた
- 失敗: `crates/hyoui/src/daemon/session.rs:6358` の `flood frames were being processed meanwhile (106 -> 106)`
- このテストは DR-0037 段 1 で足したもの。1 つの client が小さな frame を送り続けている最中に、新しい client の handshake と status が進むことを確かめる。失敗した判定は「その間に flood 側の frame の処理も進んでいる」こと
- v0.16.2 で入った変更 (set などの通知の読み飛ばし、web daemon の signal mask、ConnectionClosed の文言) はこの経路を触っていない

## 次にやること

- 「flaky」として CI の再実行で通さない (test-integrity)。まずテストと serve loop を読み、処理数が止まる経路を探す。候補:
  - 段 4 の 1 周の書き込み量の上限 (64 KiB) と flood への ack の送信
  - Linux の socket buffer の大きさで flood 側が送信で詰まる
  - 判定のタイミング
- 負荷をかけて ubuntu 相当の条件 (docker の Linux か CI の繰り返し) で再現を試みる
- 直したら v0.16.3 として出す。v0.16.2 の中身 (set などの通知の読み飛ばし、web daemon の signal mask、ConnectionClosed の文言) はまだ利用者に届いていない
