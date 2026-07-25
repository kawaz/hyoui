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

## 実測 (2026-07-25、DR-0029 実装中の副産物) — 根本原因候補を特定

`hyoui kill <session> --signal=CONT --no-terminate` を **stopped 中の子**に対して実行し、
attach client を繋いだまま時系列観測した (macOS、release build 0.9.18、子 = `/bin/cat`):

| 経路 | 子の resume | attach client | daemon |
|---|---|---|---|
| `hyoui kill <s> --signal=CONT --no-terminate` | (確認不能) | **切断される** (= client は「daemon との接続が失われました」で exit) | 生存 |
| kernel 直 `kill -CONT <child_pid>` | **する** (= 送った文字列が echo される) | 繋がったまま | 生存 |

**根本原因候補**: `crates/hyoui-cli/src/main.rs` の `kill_command` が
`AttachOptions { detach_others: true }` で接続しており、これが `--no-terminate` 経路
(`signal_no_terminate`) にも効いている。つまり **signal を送るだけのつもりの
`hyoui kill --no-terminate` が、その session の attach client を全員蹴る**。
「セッションが消滅した」という観測は、ユーザ端末の attach が切れた現象と一致する。

`detach_others: true` のコメントは「kill は破壊操作なので既存 leader を蹴ってでも実行する」
と justify しているが、この理由が成り立つのは terminate 経路だけ。`--no-terminate` は
定義上「session を畳まない signal 送信」なので、client を蹴る必然性がない。

- 追試 script: `probe_timeline.py` 相当 (pty で `hyoui run -- /bin/cat` → Ctrl+Z 2 連打で
  stop → CONT 経路を 2 通りで比較。scratchpad に置いたので必要なら再作成)
- 関連して `child_stopped` フラグは **kernel 直 SIGCONT で子が実際に動き出した後も
  true のまま**だった (= `status`/`list` が stopped と嘘をつく)。
  [2026-06-12-bug-child-stopped-flag-not-cleared](./2026-06-12-bug-child-stopped-flag-not-cleared.md) と同一事象

**影響**: DR-0029 §1 で attach client の follow を廃止したため、停止中の子を起こす正規手段が
`hyoui kill --signal=CONT --no-terminate` になった (= 画面通知でもこれを案内している)。
その正規手段が全 client を蹴る状態なので優先度が上がった。

## 受け入れ条件

- [ ] `kill_command` の `detach_others: true` を terminate 経路限定にする (= `--no-terminate`
      では `detach_others: false` / mode も再検討)。修正後、上表の 1 行目が 2 行目と同じ結果に
      なることを実機で確認
- [ ] child が Alive 状態で SIGCONT を送った場合の挙動を実機で観測・記録
- [ ] child が Stopped 状態で SIGCONT を送った場合の挙動を実機で観測・記録 (最低 3 category のマトリクス: TUI alt screen 系 / line-oriented 系 / interactive REPL 系)
- [ ] セッション消滅の再現条件を特定 (or 再現しないことを確認)
- [ ] DR-0001 軸 1/2 実装箇所 (SIGCHLD self-pipe / Continued transition 処理) を grep で特定し、本事象との関係を判定
- [ ] `daemon_child_stopped_flag_not_cleared` issue との relation を確認
