---
title: outer_token_inheritance_skips_auto_acquire が単独実行でも稀に 30s deadline fail する
status: open
category: bug
created: 2026-07-04T01:07:04+09:00
last_read: 2026-07-20T10:22:16+09:00
open_entered: 2026-07-04T01:07:04+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO — ローカル test 反復実行での flaky 観測 (2026-07-04)
---

# outer_token_inheritance_skips_auto_acquire が単独実行でも稀に 30s deadline fail する

## 概要

`crates/hyoui-cli/tests/input_auto_lock_cli.rs::outer_token_inheritance_skips_auto_acquire`
が、workspace 並列でなく **単独実行でも** 稀に fail する。失敗様式は
`common/pty.rs:96` の「thread did not finish within 30s (無限ハング防止 deadline)」で、
daemon thread が 30s 内に終わらない。

## 観測事実 (2026-07-04)

- v0.9.8 相当のコード (DR-0025 Phase 2-α2 コミット 3414f6d0 後)
- orphan daemon を掃除したクリーン環境で 10 round 単独実行し 1/10 fail
  (round 1 のみ = 直前ビルドの負荷併走時)
- lib test は同条件 12 round 全 pass

## 切り分け済み事項

- この test は外側 `HYOUI_LOCK_TOKEN` 継承で auto-lock を skip する CLI 側経路
  (DR-0022) で、DR-0025 Phase 2-α2 が変更した daemon 側 lock reducer 経路を通らない
- Phase 1a 検証時 (reducer 化以前) にも workspace 並列で同 test の同様式 fail が
  観測されており、既知の「real-process e2e の負荷依存 deadline 超過」族と判定

## 根治方向

DR-0025 が予定する「test expectation を PTY echo 依存から daemon record event の
順序 assert に変更」の族。個別対処より Phase 2-γ/Phase 8 の test 再編で吸収するのが
合理的。

## 関連

- [[2026-06-02-bug-flaky-serve-propagates-child-exit-code]] (Phase 3 吸収予定・blocked)
- a7761122 起票の serve_tail_follow flaky 観測
- [[2026-07-03-bug-macos-ci-flaky-pty-tests]] — 同一 test (`outer_token_inheritance_skips_auto_acquire`)
  が macOS CI matrix 実行でも同じ `pty.rs:96` 30s deadline 様式で flaky fail している。
  本件は単独実行でも再現する観測を追加するもの、真因調査・根治方向は共通

## 受け入れ条件

- [ ] 不安定さの軸が観測データで特定されている
- [ ] 単独実行で安定して pass する (または根拠付きで blocked_by DR-0025 Phase N)
