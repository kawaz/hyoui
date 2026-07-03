---
title: hyoui-cli main.rs unit tests が ubuntu CI で hang する (send_raw_bytes_partial_byte_race_regression / list_marks_stale_socket_when_no_ping_response)
status: open
category: bug
created: 2026-07-03T18:10:16+09:00
last_read:
open_entered: 2026-07-03T18:10:16+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: CI run 28644829018 の hang 観測 (session a7761122)
---

# hyoui-cli main.rs unit tests が ubuntu CI で hang する

## 概要

`Test (ubuntu-latest / stable)` job (= `just ci` 内の `cargo test`) が
hyoui-cli `src/main.rs` unit test binary の実行中に無出力のまま hang した。
136 test 中 134 が完走し、以下 2 test が終了しないまま 1h50m 経過、手動 cancel。

- `tests::send_raw_bytes_partial_byte_race_regression`
- `tests::list_marks_stale_socket_when_no_ping_response`

## 観測事実 (2026-07-03)

- CI run 28644829018 (commit 834a5742 = docs/issue のみの変更、コード変更なし)
- job `Test (ubuntu-latest / stable)` 84948879625:
  - 07:15:24Z `Running unittests src/main.rs (hyoui-57925514f65fcc97)` で 136 tests 開始
  - 07:17:32Z の `wait_retries_until_socket_appears ... ok` を最後に出力停止
  - 09:07 cancel 時の orphan process: `just` / `bash` / `cargo` /
    `hyoui-57925514f65fcc97` (= test binary) / **`cat`** (= test が spawn した子)
  - ok 出力 134 件と test 一覧の差分から未完了は上記 2 件に確定
- 同 run の `Test (macos-latest / stable)` は 5m18s で green
- 同一 code の直近 3 run (28411603361 / 28413666305 / 28415355152、6/30) では
  ubuntu test job は green → **flaky** (deterministic ではない)
- 既知の backpressure deadlock issue
  ([[2026-06-22-backpressure-writer-pump-drop-sequence-deadlock]]) とは別事象:
  あちらは lib 側 `--ignored` の `serve_backpressure_disconnects_slow_client`、
  こちらは hyoui-cli main.rs の通常 unit test

## 仮説 (未検証)

- orphan `cat` から、hang した test は daemon + PTY + `cat` 子プロセス構成
  (= `send_raw_bytes_partial_byte_race_regression` が有力)。RawAck 待ち
  (DR-0021、`RAW_ACK_TIMEOUT=5s`) が効かない経路か、in-process daemon 側 thread の
  join 待ちで stuck の可能性
- `list_marks_stale_socket_when_no_ping_response` は「応答しない socket」を扱う
  test で、timeout 不全なら単独 hang しうる。2 test が同時に未完了なのは
  cargo test の並列 thread で相互リソース (socket dir / PTY) 競合の可能性もあり

## 影響と暫定対応

- 暫定対応済: ci.yml の test job に `timeout-minutes: 30` を追加
  (hang 時に 6h runner 空焼きせず fail として顕在化させる)
- release.yml の CI gate 導入後は、本 hang が発火すると release が block される
  (= gate としては正しい挙動、rerun で回復)

## 次の調査ステップ

1. 該当 2 test をローカルで `--test-threads=1` / 高負荷並列の両方で反復実行し再現条件を探す
2. 再現時に test binary の thread dump (`rust-lldb` / `SIGQUIT` 相当) を取得
3. DR-0021 RawAck timeout 経路と `ClientHandle::drop` の join 経路を机上確認
4. DR-0025 Phase 2 (Client domain / Backpressure sub-state) での吸収可能性を評価

## 受け入れ条件

- [ ] hang の真因が観測データで特定されている (推測での close 不可)
- [ ] 該当 2 test が CI 並列実行で安定して完走する
