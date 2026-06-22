---
title: serve_backpressure_disconnects_slow_client が CI で 30s deadline hang する (writer_pump drop sequence deadlock)
status: open
category: bug
created: 2026-06-22T23:16:49+09:00
last_read:
open_entered: 2026-06-22T23:16:49+09:00
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

# serve_backpressure_disconnects_slow_client が CI で 30s deadline hang する (writer_pump drop sequence deadlock)

## 概要

`serve_backpressure_disconnects_slow_client` test (`crates/hyoui/src/daemon/session.rs:3520`) が
CI 環境で 30s deadline hang fail する race condition。

short-term fix として deadline を 30s → 60s に延長済み (`session.rs:3613` 周辺) だが、根治できていない。

## 背景

調査で判明した deadlock シーケンス:

1. `yes(1)` の高速 output → daemon が byte 積み → slow client の `client_buffer_bytes` 超過 → overflow 検出
2. overflow 検出と同時に writer_pump が OS socket buffer 満杯で `write_all` がブロック
3. serve_loop iteration で `indices_to_drop` 処理 → `ClientHandle::drop` で writer_thread join 待ち
4. writer_pump は shutdown signal で error 化される設計だが、CI 並列実行の fd/thread リソース競合で drop sequence が stall
5. 結果: daemon serve thread が完了せず deadline hang

CI 並列実行環境ではリソース競合が激しく、writer_pump の shutdown 応答が遅れると
drop sequence が完了せず serve_loop がフリーズする。

## 受け入れ条件

- [ ] CI 並列実行 (フルワークスペース) で `serve_backpressure_disconnects_slow_client` がフラップしない
- [ ] writer_pump の stuck を test deadline 前に検出できる (DR-0011 observability)
- [ ] deadline を short-term 延長 (60s) に頼らず正常タイムスケールで通過する

## 根治案候補

- writer_pump を cancel channel で graceful shutdown (現在は shutdown signal の応答が不確定)
- `ClientHandle::drop` で writer_thread join を timeout 5s 付きに (無限 join をやめる)
- serve_loop polling 改善 (= pending write がある client の revents を即 detect)
- DR-0011 observability: writer_pump の lifecycle を可視化 (stuck 検出が test deadline まで分からない現状の解消)
