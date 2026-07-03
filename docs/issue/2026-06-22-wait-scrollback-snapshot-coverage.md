---
title: hyoui wait の StateSnapshotRequest が scrollback を含まず viewport 外の出力を見逃す
status: blocked
category: bug
created: 2026-06-22T23:15:49+09:00
last_read:
open_entered: 2026-06-22T23:15:49+09:00
wip_entered:
blocked_entered: 2026-07-03T16:01:05+09:00
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by: DR-0025 Phase 5
origin: 自リポ TODO
---

# hyoui wait の StateSnapshotRequest が scrollback を含まず viewport 外の出力を見逃す

## 概要

`hyoui wait <pattern>` が内部で発行する `StateSnapshotRequest` は visible rows (viewport) のみを配信し、scrollback は含まない (DR-0013 Phase B 未完)。子プロセスが瞬時に exit して出力が viewport から流れた場合、wait が daemon に接続して snapshot を読む頃には対象行が観測できず timeout fail する race がある。

## 背景

2026-06-22 CI (ubuntu-latest) で以下のテストが flaky fail として観測された:

- `wait_single_positional_resolves_self_with_env` 等 (`crates/hyoui-cli/tests/self_session_resolve.rs`)

再現パターン:
- 子: `echo selfhello; sleep 30` — echo が瞬時完了、その後 sleep
- wait: `hyoui wait selfhello --timeout=5s` を別プロセスで起動
- daemon への接続・snapshot 取得が echo 完了後になると `selfhello` は viewport 外

local では proc spawn が速くて race が短く通過、CI (ubuntu-latest) では遅延が大きく再現しやすい。

**回避策 (適用済)**: test 側で `while sleep 0.2; do echo ...; done` の継続出力に変更 (line 300, 326 of `crates/hyoui-cli/tests/self_session_resolve.rs`)。回避策でテストは安定するが根本問題は残る。

DR-0013 Phase B (scrollback) の実装漏れとして位置づける (= CLAUDE.md 「既存 DR の実装漏れは新規介入より優先」)。

## 根治案候補

- **A. `StateSnapshotRequest` に scrollback last N rows を含める**: cap negotiation で旧 client 互換を維持しつつ scrollback を付加する。DR-0013 Phase B の正規実装
- **B. `hyoui wait` が初回接続時に attach 復元 redraw 経路を呼んで過去 row を取得**: attach の redraw 経路を流用する
- **C. `screen dump --layer=both` 経路を `wait` から使う**: 既存の dump 経路をパターンマッチに転用する

候補 A が DR-0013 Phase B の正規実装であり最も根治度が高い。

## 受け入れ条件

- [ ] `hyoui wait <pattern>` が接続前に出力された行 (scrollback 内) もマッチ対象にできる
- [ ] 上記 self_session_resolve.rs の各テストで継続出力 workaround を外しても CI が安定 pass する
- [ ] マトリクス検証: TUI alt screen 系 / line-oriented 系 / interactive REPL 系 で動作確認

## TODO

<!-- wip 時のみ -->

- [ ] DR-0013 Phase B の実装要件を確認して根治案 A/B/C の採否を決定
- [ ] 採用案を DR-0013 または新 DR に記録
- [ ] 実装 + テスト (継続出力 workaround を除去して安定を確認)

## Triage (2026-07-03)

DR-0025 Phase 5 (Screen reducer 骨格、DR-0013 byte-base tail/history と rows-base virtual
screen の分離を継承) のスコープで `StateSnapshotRequest` への scrollback 統合が扱われる見込み。
根治案 A (StateSnapshotRequest に scrollback last N rows を含める) は Phase 5 の実装対象と
整合するため、Phase 5 完了待ちとして blocked に遷移する。
