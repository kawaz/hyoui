---
title: socket dir が /tmp 固定 fallback のため macOS 定期掃除で daemon 生存中に socket file が消える
status: open
category: bug
created: 2026-07-20T17:52:49+09:00
last_read:
open_entered: 2026-07-20T17:52:49+09:00
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

# socket dir が /tmp 固定 fallback のため macOS 定期掃除で daemon 生存中に socket file が消える

## 概要

socket dir が `/tmp` 固定 fallback のため、macOS の定期掃除で daemon 生存中に socket file が消える。
結果としてセッションが孤立し、外側から観測・操作不能になる。

## 背景

観測: 2026-07-20、hyoui 0.9.9。

- daemon PID は生存
- `lsof` で `/tmp/hyoui-501/run-77043-16c61eb9.sock` の fd 保持を確認
- しかし `/tmp/hyoui-501/` の実体は空 (mtime 17:42 = 掃除直後)
- `hyoui list` は no sessions
- input を socket 直指定しても ENOENT

= daemon は生きているが socket file だけが消え、外側から観測・操作不能なセッションが孤立する。

kawaz 裁定: `/tmp` をやめる。

### 修正方針

- macOS では掃除対象外の per-user 常設 dir (例: `~/Library/Application Support/hyoui/run` や `~/.local/state/hyoui` 系) を fallback にする
- `XDG_RUNTIME_DIR` 優先は維持
- `cli.rs` のヘルプ文言 2 箇所 (3669 行目付近、4002 行目付近) の同期更新が必要
- `socket.rs` のエラーヒント文言も同期更新が必要

## 受け入れ条件

- [ ] macOS fallback dir が `/tmp` 以外の掃除対象外 per-user 常設 dir になっている
- [ ] `XDG_RUNTIME_DIR` が設定されている環境ではそちらが優先されることを確認
- [ ] `cli.rs` のヘルプ文言 2 箇所が新 fallback パスに追従している
- [ ] `socket.rs` のエラーヒント文言が新 fallback パスに追従している
- [ ] macOS 定期掃除下で daemon 生存中に socket file が消えないことを実機確認

## TODO

<!-- wip 時のみ -->
