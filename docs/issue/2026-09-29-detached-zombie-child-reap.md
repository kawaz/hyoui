---
title: hyoui run --detached の子 claude が zombie になった時に親が自動回収して終了する
status: wip
category: bug
created: 2026-09-29T14:01:29+09:00
last_read: 2026-09-29T14:04:31+09:00
open_entered: 2026-09-29T14:01:29+09:00
wip_entered: 2026-09-29T14:04:31+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz/claude-ccmsg
---

# hyoui run --detached の子 claude が zombie になった時に親が自動回収して終了する

## 概要

`hyoui run --detached` で起動した子 `claude` プロセスが exit しても、親 hyoui が
`wait` していないため zombie (`<defunct>`, STAT Z+) のまま残り続ける。子の終了を
検知したら親は `wait` して回収し、自身も終了する (再起動方針があればそれに従う)
挙動にする。

## 背景

kawaz 裁定 2026-09-29。観測した実例:

`hyoui run --detached -- claude --model fable[1m] --effort low --name "CSDの修正"`
(親 hyoui pid 46980、9/24 02:32 起動) の子 claude pid 46981 が `<defunct>`
(STAT Z+) のまま 5 日残存。

親 hyoui が wait していないため回収されず、Claude Code の
`~/.claude-personal/sessions/46981.json` が `status: waiting /
waitingFor: "dialog open"` のまま残る → `claude agents --json` が死んだ
セッションを waiting として返し続け、ccmsg webui に残骸が出る。

## 真因の観測 (2026-09-29、統括)

「親が wait していない」ではない。daemon には SIGCHLD self-pipe + `waitpid` の回収経路がある (`sys/signal.rs`、`daemon/session.rs`)。止まっているのは daemon のイベントループ全体:

- daemon 46980 の親 46979 は `hyoui attach run-46979-3e1f4139` で **STAT `T` (停止中)**。kawaz の zsh (68878) のジョブとして suspend されたまま
- `sample 46980`: writer thread が `__sendto` (unix socket `run-46979-3e1f4139.sock` への送信) でブロック、main thread は `_pthread_join` でその thread を待っている
- `hyoui list` は当該セッションを `stale` と表示

構図: attach client が停止して socket を読まない → kernel の socket バッファが埋まる → `broadcast.rs` の `writer_pump` の `write_all` が永久ブロック → backpressure overflow で disconnect しようとして `ClientHandle::drop` に入る → `set_write_timeout(DROP_FLUSH_TIMEOUT)` を設定してから `join` するが、**既に `sendto` でブロック済みの write には後付けの `SO_SNDTIMEO` が効かない** (仮説、要実機検証) → join が返らず daemon が固まる → SIGCHLD 回収も走らず子が zombie。

## 受け入れ条件 (改訂)

- [ ] 再現: attach client を `SIGSTOP` で止めたまま子に大量出力させると daemon が上記の状態 (writer thread が sendto、main が join) になることを実機で確認する
- [ ] 修正後: 同条件で daemon が stopped client を bounded time で切断し、イベントループが継続する (他 client の attach / `hyoui list` が応答する)
- [ ] 修正後: その状態で子が exit したら daemon が回収して終了し、zombie が残らない
- [ ] Claude Code 側の session json が waiting のまま残留しなくなることを実機で確認する
