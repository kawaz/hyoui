---
title: run --detached --pty-stdin を $(...) 内で起動すると子が生きている間コマンド置換が返らない
status: open
category: bug
created: 2026-10-08T19:00:44+09:00
last_read:
open_entered: 2026-10-08T19:00:44+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: claude-ccmsg (ccmsg v1)
---

# run --detached --pty-stdin を $(...) 内で起動すると子が生きている間コマンド置換が返らない

利用側 (ccmsg v1) からのフラグ。実装判断は hyoui 側に委ねる。以下は 1 回の観測にもとづく推測を含むので、裏取りしてから採否を決めてほしい。

## 概要

hyoui 0.14.0 で `$(hyoui run --detached --pty-stdin <cmd> 2>&1)` のようにコマンド置換の中で起動すると、子が生きている間コマンド置換が返らなかった。`--pty-stdin` なしで起動した場合は子がすぐ終了したので返った (= 比較として成立していない可能性がある)。

daemon (または run の子孫) が呼び出し元の stdout/stderr の pipe を持ち続けているとみられる (推測、未検証)。

## 観測の条件

- ccmsg 側の worker が hyoui 0.14 への追従作業中に観測
- 子は `bash -i`、起動元は端末の無い非対話 shell
- 再現は 1 回の観測のみ。厳密な再現手順は未整理

## 背景

- DR-0042 決定 2 は stdin の扱いについてのみ書いており、stdout/stderr の扱いは読み取れなかった
- 利用側のワークアラウンド: ccmsg v1 の session-launch は pipe の drain を一定時間 (PIPE_DRAIN_GRACE_MS) で打ち切っているので実害は無い。シェルスクリプトから `$(...)` で id を受け取る使い方では詰まる

## 所感 (任意)

`--detached` の時に呼び出し元の stdout/stderr を切り離す (dup2 で /dev/null 等) のが意図通りか。仕様として「pipe を持ち続ける」なら help に注意書きがあると助かる。

## 受け入れ条件

- [ ] 再現手順の確定 (`--pty-stdin` の有無、子が生きている/終了済みを揃えて比較)
- [ ] 意図した仕様の裁定 (切り離す / 持ち続ける) と、必要なら help の追記
