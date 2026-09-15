---
title: daemon graceful upgrade (DR-0028) の検証マトリクスと e2e テストが未整備
status: open
category: task
created: 2026-09-15T13:20:00+09:00
last_read:
open_entered: 2026-09-15T13:20:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: DR-0028 の Status を実装実態 (Phase 1〜3 land) に追随させた際、Phase 1/3 の gate (3 category 確認 / マトリクス全セル green + ドッグフーディング) を満たす記録が無いことが判明
---

# daemon graceful upgrade (DR-0028) の検証マトリクスと e2e テストが未整備

## 概要

[DR-0028](../decisions/DR-0028-daemon-graceful-upgrade-self-exec.md) の Phase 1〜3 は実装 land 済みだが、
DR の Phase gate が要求する検証が記録として存在しない。

- **unit test のみ**: `crates/hyoui/src/daemon/upgrade.rs` の `mod tests` に 5 本 (state file の
  round-trip / precheck / env parse 系)。self-exec 自体を跨ぐ検証は無い
- **e2e test 無し**: `crates/*/tests/` に upgrade を扱う test は無い (`web_e2e_api.rs` の
  `upgrade` は WebSocket upgrade で無関係)
- **マトリクス記録無し**: `docs/findings/` に DR-0028 §検証要件 のマトリクスを埋めた記録が無い

## 未達の gate

| Phase | DR の gate | 現状 |
|---|---|---|
| Phase 1 | PTY 越しの子と listener が exec 跨ぎで生きていることを 3 category で確認 | 記録なし |
| Phase 2 | screen 継続 / state 破損 / 版ミスマッチ行が green、再 feed コスト実測 | 記録なし |
| Phase 3 | マトリクス全セル green + ドッグフーディング (走行中 claude セッションで実運用 upgrade) | 記録なし |

## 受け入れ条件

- [ ] DR-0028 §検証要件 のマトリクス (3 category × 8 観点 + suspend 中の子を跨ぐ 1 case) を
      実機で埋め、`docs/findings/` に表形式で残す
- [ ] 自動化できるセル (exec 失敗 / state 破損 / 版ミスマッチ / SIGCHLD 経路) を
      `crates/hyoui-cli/tests/` の e2e test として追加する
- [ ] ドッグフーディング (走行中セッションでの実運用 upgrade) を 1 回以上実施して結果を記録する
- [ ] 検証で不具合が出たら修正し、DR-0028 の Status を実態に追随させる
