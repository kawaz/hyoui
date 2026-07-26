---
title: "CI の ignored-tests job が continue-on-error で恒常 red を隠している (ubuntu/macOS それぞれ固定 test が 100% fail)"
status: open
category: bug
created: 2026-07-26T09:40:00+09:00
last_read: 2026-07-26T09:40:00+09:00
open_entered: 2026-07-26T09:40:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: CI flaky 根治タスク中に GitHub API で直近 12 run × 全 attempt の job 結果を集計して発見 (2026-07-26)
---

# CI の ignored-tests job が恒常 red を隠している

## 概要

`.github/workflows/ci.yml` の `ignored-tests` job (= `cargo test --workspace -- --ignored`)
は `continue-on-error: true` が付いており、**失敗しても workflow は緑になる**。

直近 12 run × 全 attempt の job 結果を GitHub API で集計したところ、この job は
**サンプルした全 run で ubuntu / macOS 両方とも失敗していた**。flaky ではなく恒常 red。
`continue-on-error` によって誰も気づかない状態が継続している。

## 集計結果 (2026-07-26、直近 12 run)

失敗テストは OS ごとにほぼ固定:

| OS | テスト | 件数 | 様式 |
|---|---|---|---|
| ubuntu | `daemon::session::tests::serve_backpressure_disconnects_slow_client` | 12 | 毎回 **31.0s** = テスト内 `join_with_deadline(30s)` の deadline hang |
| macOS | `notify_default_does_not_resume_self_stopped_child` | 7 | **0.14〜0.28s で即死** (= タイミング依存でも資源枯渇でもない決定的失敗) |
| macOS | `smoke_hyoui_run_echo` | 2 | |
| macOS | `sys::raw::anchor_tests::session_anchor_makes_child_stoppable` | 1 | |
| macOS | `pipe_send_eof_default_terminates_bc` | 1 | |

## 個別の状況

### ubuntu: `serve_backpressure_disconnects_slow_client`

既に [[2026-06-22-backpressure-writer-pump-drop-sequence-deadlock]] (status: blocked) が
扱っている。当該 test の `#[ignore]` 属性自体に

> ubuntu CI で daemon thread join が hang する (2026-05-28 6h timeout 観測)

と書かれており、**既知の hang を ignore で棚上げしたまま CI では実行して恒常 red に
している**という矛盾した状態になっている。

### macOS: `notify_default_does_not_resume_self_stopped_child`

`crates/hyoui-cli/tests/jobcontrol_auto_resume.rs:77` の
`assert!(result.is_err(), ...)` が失敗 = notify (default) なのに `RESUMED_MARKER` が
2s 以内に観測されている。

**未解明**。macOS 開発機 (load ~30 の高負荷下) でローカル実行したところ、CI とは
**逆に** `notify_default_*` が pass し、対の `auto_resume_resumes_self_stopped_child` が
「8s で出力 0 bytes」で fail した。低負荷での再実行と、CI runner との環境差
(macOS バージョン / core 数) の切り分けが必要。

## 受け入れ条件

- [ ] macOS の `notify_default_does_not_resume_self_stopped_child` の真因を特定して直す
      (= 低負荷環境で再現を取り、DR-0019 の期待値と macOS 実挙動のどちらが正しいか判定)
- [ ] ubuntu の `serve_backpressure_disconnects_slow_client` を
      [[2026-06-22-backpressure-writer-pump-drop-sequence-deadlock]] 側で決着させる
- [ ] 上記 2 つの決着後、`continue-on-error: true` を外して恒常 red を検知可能にする。
      外せない test が残るなら、その test だけ除外して残りを blocking にする
      (= 「全部隠す」のをやめる)

## Why (= なぜ放置が有害か)

`continue-on-error` は「不安定な test で workflow を止めない」ための仕組みだが、
現状は **恒常的に壊れている事実を隠す**方向に働いている。この job が緑/赤どちらでも
同じ扱いなので、新しい regression が入っても気づけない (= 検知能力ゼロ)。
