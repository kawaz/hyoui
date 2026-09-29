---
title: hyoui run --detached の子 claude が zombie になった時に親が自動回収して終了する
status: open
category: bug
created: 2026-09-29T14:01:29+09:00
last_read:
open_entered: 2026-09-29T14:01:29+09:00
wip_entered:
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

実装の採否・方法は hyoui 側で裏取りして判断する (このリポの責務)。

## 受け入れ条件

- [ ] `hyoui run --detached` の子プロセスが終了した時、親 hyoui が検知して
      `wait(2)` 相当の回収を行い zombie を残さない
- [ ] 親 hyoui 自身も (再起動方針がなければ) 子の終了とともに終了する
- [ ] 上記により Claude Code 側の session json が waiting のまま残留しなくなる
      ことを実機で確認する
