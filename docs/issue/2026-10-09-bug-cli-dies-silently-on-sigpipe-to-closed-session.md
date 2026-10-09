---
title: 終わりかけの session に status / set / input を送ると、何も表示せず SIGPIPE で exit 141 になる
status: open
category: bug
created: 2026-10-09T20:00:00+09:00
last_read: 2026-10-09T20:00:00+09:00
open_entered: 2026-10-09T20:00:00+09:00
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

# 終わりかけの session に status / set / input を送ると、何も表示せず SIGPIPE で exit 141 になる

## 観測 (2026-10-09、v0.16.1 + 文言の修正、`true` の子の session に起動直後から 60 回ずつ)

- status: 58/60 が無表示で rc=141
- set: 52/60 が無表示で rc=141
- input: 59/60 が EIO の行を出した後に rc=141

## 原因

`crates/hyoui-cli/src/main.rs` の `is_print_and_exit_command` が、一発で終わる CLI の SIGPIPE を既定 (SIG_DFL) に戻している (stdout が閉じた pipe の時に静かに終わるため)。そのため daemon が閉じた socket に書き込んだ瞬間に SIGPIPE で死に、何も表示されない。接続の EOF を「session は既に終わっています」と伝える修正 (Error::ConnectionClosed) は、SIGPIPE を無視している経路 (web / attach / run) でしか効かない。

## 直す方向 (未検討)

stdout の SIGPIPE の扱いは保ったまま、client の socket への書き込みだけ SIGPIPE を出さない書き方にする (macOS は socket の SO_NOSIGPIPE、Linux は send の MSG_NOSIGNAL)。transport の writer の型に関わるので、設計を決めてから入れる。input の `master PTY write failed with I/O error EIO` も「子は既に終わっている」と伝えていない (同じ流れで直す)。set 以外の一発 CLI は ack の前に session.exit.notify を受けると「unexpected response: SessionExitNotify」になる (同じ流れで直す)。
