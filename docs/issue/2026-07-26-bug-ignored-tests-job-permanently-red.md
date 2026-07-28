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

## 真因確定と修正 (2026-07-26 第 2 弾)

### (A) ubuntu `serve_backpressure_disconnects_slow_client` → **解決**

Docker (ubuntu:24.04) + `--cpus=4` + `taskset -c 0-3` で **5/5 決定的に再現**した
(= flaky ではなく Linux での確定的 hang。macOS では出ない)。daemon に計装を入れて観測:

```
promoted id=0 mode=Rw
ENQ reject id=0 payload=4100 cur=0 limit=4096 single_frame_too_big=true
READY-branch drop id=0
poll:enter nclients=0 ...   ← 以降 31s 間 nclients=0 のまま yes(1) を空回り
```

真因は **product バグ**: `enqueue_for_client` が `cur + size > limit` だけを見ていたため、
`size > limit` の単一 frame で **queue が空の client すら即 disconnect** していた。
子 PTY の読み取り chunk は 8 KiB 固定なので、`client_buffer_bytes` が 8 KiB 未満だと
全 client が attach 直後に切られ、**誰も接続できない daemon** になる (= kill も届かない)。

修正: 空 queue なら limit 超の単一 frame も受け入れる (前進保証)。併せて test 側の
`client_buffer_bytes = 4096` (< chunk 8 KiB) という自己矛盾も 12 KiB に修正。

検証 (Linux 4core): 修正前 31.01s FAILED × 5/5 → 修正後 1.06〜1.12s ok × 8/8。

### (B) macOS `notify_default_does_not_resume_self_stopped_child` → **裁定済み・実装済み**

真因は **DR-0019 と DR-0029 の規定衝突**:

- DR-0019 §3: `on-child-suspend` default = `notify` = 「daemon は勝手に起こさない」
- DR-0029 §5: `[attach] resume_on_reattach = true` (default) で rw attach 時に resume 要求
- `hyoui run` は DR-0015 で「fork daemon + attach client」の合成なので、**run した瞬間に
  attach 経路が発火して子を起こす**。daemon は notify を守っているが同居 client が起こす

2026-07-29 kawaz 裁定 (👺RS-Q1) により [[DR-0030]] を起票・実装済み。原則は
「rw attach client が存在する間、hyoui は子を停止させたままにしない」で、
DR-0029 §5 の resume 発火点を「handshake 時に stopped」に加えて「attach 中の
`SessionChildStoppedNotify` 受信」にも拡張した。旧 test
`notify_default_does_not_resume_self_stopped_child` は「default では起こされない」
という、この裁定前の (誤った) 期待値だったため、`run_resumes_child_that_is_already_stopped_at_attach`
等に置き換えて期待値を反転済み (`crates/hyoui-cli/tests/jobcontrol_auto_resume.rs`)。

## 受け入れ条件

- [x] macOS の `notify_default_does_not_resume_self_stopped_child` の真因を特定
      (= DR-0019 と DR-0029 の規定衝突)
- [x] 修正方針の裁定 (👺RS-Q1、2026-07-29 kawaz) と [[DR-0030]] による実装
      (= resume 発火点の拡張、対象 test の期待値反転)
- [x] ubuntu の `serve_backpressure_disconnects_slow_client` を解決
      (= 単一 frame > buffer_limit で誰も attach できなくなる product バグ)
- [ ] 上記の決着を受けて `continue-on-error: true` を外して恒常 red を検知可能にする。
      外せない test が残るなら、その test だけ除外して残りを blocking にする
      (= 「全部隠す」のをやめる)

## Why (= なぜ放置が有害か)

`continue-on-error` は「不安定な test で workflow を止めない」ための仕組みだが、
現状は **恒常的に壊れている事実を隠す**方向に働いている。この job が緑/赤どちらでも
同じ扱いなので、新しい regression が入っても気づけない (= 検知能力ゼロ)。
