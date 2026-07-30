---
title: "BUG: stdout の早期 close (| head 等) で Broken pipe panic する"
status: open
category: bug
created: 2026-07-30T12:40:00+09:00
last_read:
open_entered: 2026-07-30T12:40:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: ^Z suspend 実装 worker が実験中に観測 (2026-07-30)
---

# BUG: stdout の早期 close (`| head` 等) で Broken pipe panic する

## 現象

`hyoui status ... | head -5` のように pipe 先が先に閉じると
`failed printing to stdout: Broken pipe` で panic する (実測、v0.9.29 相当の debug build)。

## あるべき挙動

EPIPE は「読み手がもう要らないと言った」だけなので正常終了 (exit 0 or 141 相当) に倒す。
Rust の `println!` は EPIPE で panic するのが既知の罠で、対策の定石は
(a) main 冒頭で `unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) }` に戻す
(b) stdout への書き込みを `writeln!` + ErrorKind::BrokenPipe の握り分けにする
のどちらか。hyoui は signal を厳密に扱うツールなので (a) の副作用 (全書き込み経路が
SIGPIPE で即死) が許容できるかは検討が要る。

## 対象

list / status / screen dump / tail など stdout に吐く全 subcommand。
