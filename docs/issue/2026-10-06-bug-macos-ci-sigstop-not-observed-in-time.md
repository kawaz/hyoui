---
title: macOS CI で SIGSTOP した子の停止が約 1 秒以内に観測できず notify_child_stopped_does_not_auto_resume_without_leader が落ちた (真因未特定)
status: open
category: bug
created: 2026-10-06T19:30:00+09:00
last_read: 2026-10-06T19:30:00+09:00
open_entered: 2026-10-06T19:30:00+09:00
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

# macOS CI で SIGSTOP した子の停止が約 1 秒以内に観測できず notify_child_stopped_does_not_auto_resume_without_leader が落ちた (真因未特定)

## 観測

- v0.13.1 の Release workflow (run 37436178715) の「ci / Ignored tests (macos-latest / PTY+daemon)」で 1 回だけ失敗した
- 失敗箇所は `crates/hyoui/src/daemon/session.rs` の `notify_child_stopped_does_not_auto_resume_without_leader`、`child should be Stopped after SIGSTOP`
- 同じ commit の CI workflow (run 37436178484) と v0.13.0 では通っている
- 当時のテストは、`cat` に SIGSTOP を送ったあと `waitpid(WNOHANG|WUNTRACED)` を 10ms 間隔で 100 回呼んで待っていた
- 失敗した回はこのテストに 3.07 秒かかり、成功した回は 0.34〜0.46 秒だった。同じ回の他のテストも 4 倍前後遅く、ビルド直後の最初のテストバイナリだった

## 不安定さの軸と再現条件

- 軸: SIGSTOP を送ってから、Stopped を観測できるまでの時間
- 再現条件: 手元の macOS (10 コア) では再現しなかった
  - `yes` 20 本の負荷で ignored 4 本を 30 回 → 失敗 0
  - 計測入りの一時ビルドを 10 並列 × 10 ラウンド → 停止の観測は最大 0.9ms
- 当初の仮説「同じプロセスで並走する回帰テストが停止の報告を奪った」は外れた。lib 内の waitpid / waitid はすべて自分の子の pid を指定している。追加した回帰テストは ignored job に入らない

## 真因の仮説 (区別できていない)

- (a) 負荷で子が CPU を得られない、または exec が終わらず、停止が約 1 秒に間に合わなかった
- (b) `cat` が止まる前に終わり、waitpid が Exited を返していた

旧テストは Stopped 以外を記録しなかったので、ログから (a) と (b) を区別できない。

## 取った対処

テストの待ち方を、時間と回数で待つ形から観測して待つ形に変えた (`procstate::wait_stopped_nowait`、`waitid(WSTOPPED|WEXITED|WNOWAIT)`)。子が止まらずに終わった場合は `si_code` を出して落ちる。

- (b) なら、次に落ちた時に原因が分かる
- (a) なら、待ち時間の上限が無くなったので落ちない

同じ形の待ち方をしていたテスト 4 本も、同じ形に揃えた。

## 次に落ちたら

失敗メッセージの `si_code` で (b) かどうかを判定する。止まらずにハングした場合は、停止の報告を誰かが奪っている (WNOWAIT を使わない waitpid) ので、同じプロセスで並走していたテストを調べる。
