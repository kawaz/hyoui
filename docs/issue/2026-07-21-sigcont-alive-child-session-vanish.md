---
title: SIGCONT を alive 中の子セッションに送るとセッションが消滅する疑い
status: open
category: bug
created: 2026-07-21T01:34:48+09:00
last_read:
open_entered: 2026-07-21T01:34:48+09:00
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

# SIGCONT を alive 中の子セッションに送るとセッションが消滅する疑い

## 概要

2026-07-20 15:07 頃、kawaz の実機操作で「SIGCONT を alive 中の子セッションに送るとセッションが消滅する」疑いが観測された。詳細な再現条件は未確定。当時 run-* セッションが消えた。

`screen-dump-empty-while-tail-has-output` の調査中に別事象として保留にしていた件。

## 背景

まず観察を行う: 子が Alive 状態で `hyoui kill --signal=CONT <session>` (または外部 `kill -CONT <child_pid>`) を送ると何が起きるか、child が Stopped / Alive のどちらの状態にあるかで挙動が違うかを実機マトリクスで押さえる必要がある。

DR-0001 の軸 1/2 (SIGCHLD self-pipe + Continued transition 処理) との関連、および `daemon_child_stopped_flag_not_cleared` issue との relation も要確認 (未実装漏れ・既存 DR の justify 範囲を確認する — 撤退判断より先に実装漏れを疑う原則に従う)。

## 受け入れ条件

- [ ] child が Alive 状態で SIGCONT を送った場合の挙動を実機で観測・記録
- [ ] child が Stopped 状態で SIGCONT を送った場合の挙動を実機で観測・記録 (最低 3 category のマトリクス: TUI alt screen 系 / line-oriented 系 / interactive REPL 系)
- [ ] セッション消滅の再現条件を特定 (or 再現しないことを確認)
- [ ] DR-0001 軸 1/2 実装箇所 (SIGCHLD self-pipe / Continued transition 処理) を grep で特定し、本事象との関係を判定
- [ ] `daemon_child_stopped_flag_not_cleared` issue との relation を確認
