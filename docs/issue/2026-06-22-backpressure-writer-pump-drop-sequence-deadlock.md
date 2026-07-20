---
title: serve_backpressure_disconnects_slow_client が CI で 30s deadline hang する (真因未観測・調査継続)
status: blocked
category: bug
created: 2026-06-22T23:16:49+09:00
last_read: 2026-07-20T10:23:08+09:00
open_entered: 2026-06-22T23:16:49+09:00
wip_entered:
blocked_entered: 2026-07-03T16:01:05+09:00
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by: DR-0025 Phase 2
origin: 自リポ TODO
---

# serve_backpressure_disconnects_slow_client が CI で 30s deadline hang する (真因未観測・調査継続)

## 概要

`serve_backpressure_disconnects_slow_client` test (`crates/hyoui/src/daemon/session.rs:3520`) が
CI 環境で 30s deadline hang fail する race condition。

short-term fix として deadline を 30s → 60s に延長済み (`session.rs:3613` 周辺) だが、根治できていない。

## 観測事実

- 2026-06-22: CI 環境 (ubuntu / macOS runner) で 30s deadline に達して hang fail することがある
- v0.9.3 で deadline を 30s → 60s に延長したところ CI が一発 green になった
  - ただし「真に 30s では足りない」と「flaky で偶然通った」を区別できない (= 1 サンプル)
- v0.9.4 で test-failure-no-tampering rule 違反のため 30s に revert 済み
- 30s に戻した状態で CI failure は継続して観測可能

## 未観測の不明事項

以下は **観測根拠のない仮説**。thread stack / open fd / poll state の実測はしていない:

- writer_pump が OS socket buffer 満杯で `write_all` block しているかどうか
- drop sequence がどのステップで stall しているか
- CI 並列実行の fd/thread リソース競合が原因かどうか

## 仮説 (要検証)

観測事実と実装コードから推定した deadlock シーケンス候補:

1. `yes(1)` の高速 output → daemon が byte 積み → slow client の `client_buffer_bytes` 超過 → overflow 検出
2. overflow 検出と同時に writer_pump が OS socket buffer 満杯で `write_all` がブロック (**未観測**)
3. serve_loop iteration で `indices_to_drop` 処理 → `ClientHandle::drop` で writer_thread join 待ち
4. writer_pump は shutdown signal で error 化される設計だが、CI 並列実行の fd/thread リソース競合で drop sequence が stall (**未観測**)
5. 結果: daemon serve thread が完了せず deadline hang

真因候補: 「runner load 不足」「writer_pump deadlock」「fd / thread リソース競合」のいずれかだが判定不能。

## 次の調査ステップ

1. 30s 状態で CI failure を複数 SHA / run で観測 → failure 率を確認 (= flaky か再現性ありかの区別)
2. failure 時の writer_pump thread dump 取得
3. DR-0011 observability: writer_pump lifecycle の可視化があれば真因特定が早い

## 受け入れ条件

- [ ] CI 並列実行 (フルワークスペース) で `serve_backpressure_disconnects_slow_client` がフラップしない
- [ ] writer_pump の stuck を test deadline 前に検出できる (DR-0011 observability)
- [ ] deadline を short-term 延長 (60s) に頼らず正常タイムスケールで通過する

## 根治案候補

- writer_pump を cancel channel で graceful shutdown (現在は shutdown signal の応答が不確定)
- `ClientHandle::drop` で writer_thread join を timeout 5s 付きに (無限 join をやめる)
- serve_loop polling 改善 (= pending write がある client の revents を即 detect)
- DR-0011 observability: writer_pump の lifecycle を可視化 (stuck 検出が test deadline まで分からない現状の解消)

## 2026-06-30 観測

main commit `ab46b529` (= DR-0025 Draft land) で CI が再度 failure。CI run id 28413666305 (= 本 commit と無関係、docs 変更のみ)。

- ubuntu-latest `--ignored` で再現 (30s deadline hang)
- macos-latest `--ignored` では本 test は pass、別 test (`pipe_send_eof_default_terminates_bc`) が bc 互換性問題で failure
- failure 率: 連続観測。本 session で land した 4 commit (= docs/CI 修正のみ) のうち 3 commit 分の CI で 100% 再現

→ 仮説強化: writer_pump deadlock は **特定環境 (ubuntu-latest GitHub Actions runner) で deterministic に発生**、flaky でない可能性が高い。ローカル macOS では再現せず。

→ **DR-0025 (Daemon Reducer 化、Lock domain Phase 1a で構造的解消見込み)** との関係: DR-0025 reducer 化で writer_pump lifecycle が Effect layer の rollback 経路に formal 化されるため、本 issue は DR-0025 進捗で吸収可能性が高い。並行で個別 fix を進めるか、DR-0025 Phase 1a 完了待ちか、判断待ち。

## Triage (2026-07-03)

DR-0025 Phase 2 (Client domain reducer 化、Transport/Auth/Backpressure sub-state を含む) で
writer pump / drop sequence の状態管理が reducer 化される見込み。上記 6/30 追記の吸収可能性を
Phase 2 として確定し、Phase 2 完了待ちとして blocked に遷移する。
