---
title: status_liveness_check_must_not_reap_exited_child が 50ms の sleep で子の exit を待っており負荷時に落ちうる
status: resolved
category: bug
created: 2026-10-05T11:50:00+09:00
last_read: 2026-10-06T12:30:00+09:00
open_entered: 2026-10-05T11:50:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-06T12:30:00+09:00
discard_reason:
pending_reason:
close_reason: 50ms の sleep を waitid(WEXITED|WNOWAIT) での exit 待ちに置き換え (unsafe は sys/procstate.rs)。負荷下 (yes 10 並走) 30 回は修正前も 0 失敗で再現はしておらず、時間依存を取り除いた修正
blocked_by:
---

# status_liveness_check_must_not_reap_exited_child が 50ms の sleep で子の exit を待っており負荷時に落ちうる

## 観測

2026-10-05、stdin 転送の作業中に `cargo test --workspace` の全体実行で 1 回だけ失敗した (`crates/hyoui/src/daemon/control.rs` の `status_liveness_check_must_not_reap_exited_child`)。単独で 5 回流すと 5 回とも pass、その後の全体実行でも pass。stdin の変更とは無関係の test。

## 真因の仮説 (コード読解、未再現)

test は `/usr/bin/true` を spawn し、「子が exit して zombie になるのを少し待つ」ために `std::thread::sleep(50ms)` してから、`kill(pid, 0)` が成功すること (zombie も alive 扱い) と、その後の `waitpid(pid, WNOHANG)` が exit を観測できることを確かめる。`kill(pid, 0)` は子がまだ走っていても成功するので前半は時間に依らないが、後半の `WNOHANG` は子がまだ exit していなければ `StillAlive` を返す。全体実行の高負荷で fork / exec が 50ms を超えて遅れると、後半の期待が崩れて落ちる。

## 不安定さの軸と再現条件

- 軸: 子が exit するまでの時間 vs 固定 50ms の待ち
- 再現条件 (仮説): CPU 負荷の高い全体実行中
- 即直せない理由: stdin 転送の作業範囲外。真因は仮説の段階

## 直し方の方向

時間で待たず、子の exit を観測して待つ。`waitid(P_PID, pid, WEXITED | WNOWAIT)` は子が exit するまで block し、reap しない (zombie のまま残す) ので、「zombie を作ってから生死チェックが reap しないことを確かめる」という test の意図をそのまま保てる。負荷下 (例 `yes` を並走) で繰り返して再現と修正を確かめる。
