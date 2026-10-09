---
title: daemon が core dump 抑止のため RLIMIT_CORE を hard 0 にしていて、子がそれを引き継ぐ (子は core を出せず、上げ直せない)
status: resolved
category: bug
created: 2026-10-09T15:00:00+09:00
last_read: 2026-10-09T19:00:00+09:00
open_entered: 2026-10-09T15:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-09T19:00:00+09:00
discard_reason:
pending_reason:
close_reason: R5-H12 の抑止は soft だけを 0 にし hard は残す。子は exec の前に呼び出し元の soft / hard へ戻す (DR-0043 決定 5)。手元は hard 0 の環境の下なので修正前後の差は CI の環境で確かめる
blocked_by:
---

# daemon が core dump 抑止のため RLIMIT_CORE を hard 0 にしていて、子がそれを引き継ぐ (子は core を出せず、上げ直せない)

## 観測 (2026-10-09、コード)

`crates/hyoui/src/daemon/session.rs` の `Session::start` は、子を spawn する前に `setrlimit_core_zero()` で `RLIMIT_CORE` の soft / hard を両方 0 にする (R5-H12: daemon のメモリにある lock_token 等の secret が core dump から漏れるのを防ぐ。`HYOUI_ALLOW_CORE=1` で無効化)。spawn した子はこの上限を引き継ぎ、hard 0 は子の側で上げ直せない。直接実行した子は core を出せる環境なので、透過になっていない (DR-0005)。

## 直す方向 (統括案)

- daemon は soft だけを 0 にし、hard は呼び出し元のまま残す
- 子は exec の前に、soft を呼び出し元の値 (daemon が下げる前に保存したもの) に戻す。async-signal-safe な `setrlimit` で行う (DR-0043 の signal を既定に戻す処理と同じ場所)
- daemon の secret の保護は soft 0 で保たれる (daemon が自分で soft を上げることは無い)
