---
title: "BUG: auto-resume / 外部 SIGCONT 後も `child-state: stopped` が恒久的に下りない"
status: resolved
category: bug
created: 2026-06-12T00:00:00+09:00
last_read: 2026-07-29T09:29:17+09:00
open_entered: 2026-06-12T00:00:00+09:00
wip_entered:
blocked_entered: 2026-07-03T16:01:05+09:00
pending_entered:
discarded_entered:
resolved_entered: 2026-07-29T00:00:00+09:00
discard_reason:
pending_reason:
close_reason: root cause (macOS が self-stop した子の continued を waitpid で報告しない) を特定して修正、実機マトリクス 6 ケース green
blocked_by:
origin: DR-0019 Update (set/可視化) 実装の push 前 Fable レビュー (2026-06-12)。本 diff 起因ではなく v0.6.3 に既存
---

# BUG: auto-resume / 外部 SIGCONT 後も `child-state: stopped` が恒久的に下りない

- Priority: Middle (実プロセスは正常走行、表示だけが矛盾。SUSPEND 可視化 (DR-0019 Update) と組み合わさるとユーザを混乱させる)

## 症状

子が SIGSTOP → SIGCONT で復帰した後も、`hyoui status` / `hyoui list` の
`child-state` / `child_stopped` が **stopped のまま下りない** ケースがある。

再現 (いずれも attach client 不在の detached 状況):

1. **auto-resume policy**: `--on-child-suspend=auto-resume` (or `hyoui set` で変更) の
   worker に `kill -STOP <child>` → daemon が即 SIGCONT、`ps` は `S+` (走行中) に戻るのに
   `child-state: stopped` のまま (3 秒待っても変わらず)
2. **notify policy + 外部 SIGCONT**: stopped の子を `kill -CONT <child>` で手動復帰
   させても同様に stopped 表示が残る

実プロセス状態 (`ps -o stat`) は正しく `S+` なので、**flag の clear 漏れ** (表示系のみ)。

## 推定原因

`SessionState::child_stopped` は `notify_child_stopped` で `true`、
`record_child_continued` で `false` にする設計 (DR-0017 §柱2)。
`pty.rs` の lifecycle poll は WCONTINUED を持つが、**detached 状況 (= client 不在 /
serve_loop が idle) で Continued transition を拾えていない**模様。

- auto-resume 経路 (`notify_child_stopped` 内 killpg(SIGCONT)) は「子の Continued は
  次回 poll の `record_child_continued` 経路で記録される」前提だが、その poll が
  発火していない
- 外部 SIGCONT も同様 (= daemon は SIGCHLD/WCONTINUED で観測するはずの transition を
  取りこぼしている)

該当コード: `crates/hyoui/src/daemon/session.rs` (`record_child_continued` 呼び出し経路、
`ChildTransition::Continued` の match arm)、`crates/hyoui/src/sys/pty.rs` 系の
WCONTINUED poll。

## 影響

- DR-0019 Update の SUSPEND 可視化と矛盾する表示: 「`on-child-suspend: auto-resume`
  なのに `child-state: stopped`」= ユーザは「auto-resume が効いていない」と誤認する
  (= 可視化機能の動機である誤運用検出を逆方向に汚染)
- DR-0017 柱 2 の可観測性要件 (stopped → resumed は record + list/status で観測可能)
  の実装漏れ側面

## 検証メモ (2026-06-12 実機)

| policy | 操作 | ps STAT | status child-state | 期待 |
|---|---|---|---|---|
| auto-resume | kill -STOP | S+ (復帰済) | stopped ❌ | running |
| notify | kill -STOP | T+ | stopped ✅ | stopped |
| notify | kill -STOP → kill -CONT | S+ | stopped ❌ | running |

## 追加観測 (2026-07-25、DR-0029 実装中) — **attached でも再現、推定原因を狭める必要あり**

上記の再現はいずれも「attach client 不在の detached 状況」だったが、**rw leader が
attach したまま**でも同じ症状が出ることを確認した (macOS / release 0.9.18 / 子 = `/bin/cat`)。

```
[ 2.01s] 起動直後      child_state=running child_stopped=False clients=[0:rw* 1:ro]
[ 3.34s] Ctrl+Z 2 連打 child_state=stopped child_stopped=True  clients=[0:rw* 2:ro]
[ 3.77s] CONT 送信     rc=0 (hyoui kill <s> --signal=CONT --no-terminate)
[ 4.78s] CONT 直後     child_state=stopped child_stopped=True  clients=[0:rw* 4:ro]
[ 6.35s] 子の応答      b'RESUMED-OK\r\nRESUMED-OK\r\n'   ← 子は確実に走っている
[ 6.35s] 最終          child_state=stopped child_stopped=True  clients=[0:rw* 5:ro]
```

子は echo を返しており **実際に resume している**のに `child_stopped` は最後まで下りない。
= 「§推定原因」の「detached 状況 (= client 不在 / serve_loop が idle) で Continued
transition を拾えていない」という仮説は **不十分** (attached で serve_loop が
生きていても拾えていない)。root cause 特定時はこの前提を外して調べること。

| 状況 | policy | 操作 | 子の実挙動 | status child-state |
|---|---|---|---|---|
| **attached (rw leader あり)** | notify | Ctrl+Z 2 連打 → `kill --signal=CONT --no-terminate` | resume 済 (echo 応答あり) | stopped ❌ |

## Root cause (2026-07-29 特定)

原因は 2 つの kernel 挙動の重なりで、いずれも macOS で実測した (`/tmp` の C 再現片で
`sigaction(SIGCHLD)` + `waitpid(WNOHANG|WUNTRACED|WCONTINUED)` を直接叩いて確認):

1. **macOS は子の continue で SIGCHLD を送らない**。stop では handler が 1 回呼ばれるが、
   SIGCONT では 0 回。daemon は SIGCHLD self-pipe でしか wake しないため、
   `poll_with_transition` を回す契機が来ず Continued を drain できない。
   → serve_loop の poll timeout を「子が stopped の間だけ」200ms で cap し、
   Timeout 経路でも lifecycle を poll するようにした。
2. **子が自分で自分を止めた場合、`waitpid(WCONTINUED)` は continued を一切報告しない**。
   CONT の送り主 (daemon 経由 / 外部 `kill -CONT`) に依らず報告されない。
   外部から `kill -TSTP <pid>` で止めた子は報告される。
   → `waitpid` に依存しない観測経路として `crate::sys::procstate::is_stopped`
   (macOS: `proc_pidinfo(PROC_PIDTBSDINFO)` の `pbi_status`、Linux: `/proc/<pid>/stat`) を
   追加し、latch が stopped の間だけ直読みして復帰を確認する。

1 だけでは `kill -STOP $$` する子 (= テストや shell script で最も多い形) が救われず、
2 だけでは polling 契機が無い。両方が要る。

なお 2 の帰結として、self-stop した子については DR-0016 §3 の 4 段階 lifecycle event の
うち `child-continued-observed` が `waitpid` 由来では出ない。現在は procstate 観測を
根拠に同 event を push している。

## 検証 (2026-07-29 実機、debug build)

| # | ケース | ps STAT | status child-state |
|---|---|---|---|
| 1 | rw attach 中に子が self-stop (SIGSTOP) | S+ | running |
| 2 | rw attach 中に子が self-stop (SIGTSTP) | S+ | running |
| 3 | attach 成立前に self-stop | S+ | running |
| 4a | rw client 無しで self-stop (停止維持) | T+ | stopped |
| 4b | 上記に外部 `kill -CONT` | S+ | running |
| 5 | vim + rw attach + 外部 `kill -TSTP` | S+ | running |
| 6 | `--on-child-suspend=auto-resume` | S+ | running |

4a が「本当に停止中なら stopped のまま」を押さえている (= 一律 clear ではない)。

回帰 test: `jobcontrol_auto_resume::status_child_state_returns_to_running_after_resume`。
既存 3 test は子の出力 (RESUMED_MARKER) しか見ておらず、daemon 側 latch を検証して
いなかったため本 bug を素通ししていた。
