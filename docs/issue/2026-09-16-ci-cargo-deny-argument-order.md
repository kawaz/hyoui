---
title: CI の cargo-deny-action は cargo-deny 0.20 系で引数順エラーになる
status: open
category: bug
created: 2026-09-16T22:00:00+09:00
last_read:
open_entered: 2026-09-16T22:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: DR-0036 実装中の観測 (2026-09-16)
---

# CI の cargo-deny-action は cargo-deny 0.20 系で引数順エラーになる

## 現象

ローカルの cargo-deny 0.20.2 では `cargo deny check --all-features` が `unexpected argument '--all-features'` で落ちる (正しい順は `cargo deny --all-features check`)。CI の `cargo-deny-action@v2` は `command: check` + `arguments: --all-features` を組み立てるので、action が 0.20 系の cargo-deny を引いた時点で deny job が引数エラーで落ちる。今は action 側が古い版を pin しているので通っている。

## 対処案

`.github/workflows/ci.yml` の cargo-deny step で `arguments` を `command-arguments` 相当 (action の入力仕様を確認) に移すか、action を使わず `cargo deny --all-features check` を直接実行する。

## 受け入れ条件

- [ ] ローカル cargo-deny 0.20 系と同じ引数順で CI が動く
