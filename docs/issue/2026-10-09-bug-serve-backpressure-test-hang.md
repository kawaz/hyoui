---
title: serve_backpressure_disconnects_slow_client が負荷下で 150 回超に 1 回、kill 後の serve の終了を 30 秒待ってハングする (真因未特定)
status: open
category: bug
created: 2026-10-09T09:00:00+09:00
last_read: 2026-10-09T09:00:00+09:00
open_entered: 2026-10-09T09:00:00+09:00
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

# serve_backpressure_disconnects_slow_client が負荷下で 150 回超に 1 回、kill 後の serve の終了を 30 秒待ってハングする (真因未特定)

## 観測 (2026-10-06〜09)

`crates/hyoui/src/daemon/session.rs` の `serve_backpressure_disconnects_slow_client` が、`yes` 10 本の負荷で `--ignored` (同じプロセスで 4 本並走) を回した時に 1 回だけハングした。slow client の切断 (EOF の検知) までは通り、kill を送った後に serve スレッドの終了を待つ `join_with_deadline` が 30 秒の期限に達した。

| ビルド | 回数 | ハング |
|---|---|---|
| v0.13.0 `b1a1805b` | 40 (v0.13.1 と交互) | 0 |
| v0.13.1 `63a05e2c` | 40 (同上) | 0 |
| v0.14.0 + テスト修正 | 40 | 0 |
| `585993ec` (v0.13.1 + テストの待ち方の修正) | 30 | 1 |

v0.13.1 の起動時 waitpid の修正による退行かどうかは、どちらも 0 回なので判定できていない。

## 不安定さの軸と再現条件

- 軸: kill を送ってから、daemon の serve スレッドが終わるまでの時間
- 再現条件: 実質的に再現できていない (150 回超で 1 回)

## 真因の仮説 (未検証)

kill を受けてから、子の後始末 (SIGCONT → SIGTERM → 猶予 → SIGKILL) と waitpid、writer スレッドの join までのどこかで止まっている。候補は、`yes` の出力を client へ書く write が詰まって join が返らない、または子の回収待ちが止まる。

## すぐ直せない理由と、次に要る観測

起きた 1 回で残っているのは panic の文言だけで、どこで止まったかの手がかりが無い。次の手: 期限に達した時点でテストプロセスの全スレッドのスタック (macOS なら `sample <pid>`) と子の状態 (`ps -o pid,stat`) を自動で取る仕組みを入れて、繰り返し回す。
