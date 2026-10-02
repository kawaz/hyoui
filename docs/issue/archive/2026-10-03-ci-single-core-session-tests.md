---
title: CI に 1 core 制約の daemon session テスト job を足す
status: resolved
category: task
created: 2026-10-03T02:32:04+09:00
last_read:
open_entered: 2026-10-03T02:32:04+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-03T03:06:09+09:00
discard_reason:
pending_reason:
close_reason: ci.yml の ubuntu Test job に taskset -c 0 で daemon::session テストを回す step を追加。ローカル 1 core コンテナで現行 3/3 pass (65 件 約 13 秒)、v0.9.64 の serve_loop 修正を revert すると 3/3 fail を確認
blocked_by:
origin: 自リポ TODO
---

# CI に 1 core 制約の daemon session テスト job を足す

## 概要

CI に 1 core 制約 (`taskset -c 0` 等) で daemon session テストを回す job を足し、子 exit 周りの race を CI でも決定的に捕まえる。

## 背景

`docs/findings/2026-10-03-linux-container-test-divergence.md` の実測で、serve_loop の SIGCHLD 経路が drain 窓を飛ばす race (v0.9.64 で修正) は `--cpuset-cpus=0` / `0-1` で 12/12 発生、多 core では 2〜3/12、GitHub ubuntu-latest runner では観測されなかった。test helper の attach redraw 捨て race も /bin/sh=dash の起動速度依存で同様に CI では出にくい。CI が多 core + 速い runner のため、この種の race を CI が検出できない。

案: ubuntu job に `taskset -c 0 cargo nextest run -p hyoui --lib -E 'test(/daemon::session/)'` 相当の step を足す (所要時間とコストを見積もってから)。macOS は taskset 相当が無いので Linux のみ。

## 受け入れ条件

- [x] v0.9.64 の修正を revert すると CI の 1 core job が fail することを確認してから入れる
- [x] 追加 job の所要時間とコストを見積もり、許容範囲であることを確認
- [x] Linux (ubuntu) のみで動作する
