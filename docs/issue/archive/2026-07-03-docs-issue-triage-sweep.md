---
title: "open issue 21 件の triage 棚卸し (外部監査フラグ)"
status: resolved
category: task
created: 2026-07-03T13:45:18+09:00
last_read:
open_entered:
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-07-03T16:01:05+09:00
discard_reason:
pending_reason:
close_reason: ["done:absorb7-blocked-DR-0025/keep14-continue/close-candidate0"]
blocked_by:
origin: claude-rules-personal
---

# open issue 21 件の triage 棚卸し (外部監査フラグ)

## 概要

hyoui の `docs/issue/` 配下の open 状態 issue 数が 21 件で、監査対象リポの中で最多だった
(次点は cache-warden の 13 件)。放置日数順に triage 棚卸し (close / 継続 / 統合の判断) する
ことを推奨する。個々の issue の価値判断は hyoui 担当側に委ねる。

## 背景

2026-07-03 の個人エコシステム横断監査 (claude-rules-personal セッション発) で観測。

これは部外者 (claude-rules-personal セッション) からの観測に基づくフラグであり、実際の
21 件それぞれの内容・放置理由は裏取りできていない。triage の要否・優先度は hyoui 担当側で
確認の上で判断してほしい。

## 受け入れ条件

- [x] 21 件の open issue を放置日数順に一覧し、close / 継続 / 統合のいずれかを判断する

## Triage 結果 (2026-07-03)

21 件を分類: absorb (DR-0025 Phase 吸収) 7 件 / keep (独立継続) 14 件 / close-candidate 0 件。
判定は 1 issue = 1 sonnet agent の並列 triage (19 件) + メイン直判定 (2 件) による。

### absorb 7 件 (→ status=blocked に遷移、DR-0025 該当 Phase を記録済み)

1. `backpressure-writer-pump-drop-sequence-deadlock` → DR-0025 Phase 2 (Client domain Backpressure sub-state)
2. `bug-child-stopped-flag-not-cleared` → DR-0025 Phase 3 (Child state machine 化)
3. `bug-anchor-startup-sigttin-transient` → DR-0025 Phase 3 (ChildLifecycle formal 化)
4. `bug-flaky-serve-propagates-child-exit-code` → DR-0025 Phase 3 (Child reducer への exit code 伝播集約)
5. `refactor-large-file-decomposition` → DR-0025 Phase 1b/3/4 (serve_loop 引数問題。main.rs/cli.rs の CLI 層分割は DR-0025 対象外のため issue に残置)
6. `wait-scrollback-snapshot-coverage` → DR-0025 Phase 5 (Screen reducer 骨格、DR-0013 Phase B 継承)
7. `feature-claude-tui-automation` → DR-0025 Phase 7 (Screen reducer watch region/matcher/flow)

### keep 14 件 (→ 独立継続、status 変更なし)

- `bug-bc-macos-ci-compatibility` (CI 修正・低中)
- `release-yml-latest-release-check` (CI 同期・中)
- `feature-cli-restructure-discussion` (CLI 設計議論・中)
- `feature-icanon-large-input-chunking` (kernel 制約・低中)
- `feature-ack-test-coverage-expansion` (test 基盤・中)
- `tcsaflush-input-discard-in-suspend-resume` (実害未観測・低)
- `child-spawn-sigttou-stop-race` (SIG_IGN 対応方針明示済・中)
- `bug-vt100-zero-size-pty-panic` (入力 validation・低)
- `feature-signal-ack` (protocol 拡張・低)
- `feature-record-redaction-phase5` (DR-0016 系独立機能)
- `bug-wait-fullwidth-padding` (CLI 側変換 bug・中)
- `advanced-feature-jsonl-zstd-domain-dict` (tech-memo)
- `readme-asciinema-cast` (docs)
- `tx-lock-unlock-cli-subcommands` (status=wip 継続)

### close-candidate 0 件

該当なし。
