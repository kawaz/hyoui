---
title: serve_tail_follow_receives_tail_end_on_child_exit が ubuntu CI で flaky fail する
status: open
category: bug
created: 2026-07-03T19:50:00+09:00
last_read:
open_entered: 2026-07-03T19:50:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: Release run 28655240907 の ci gate fail 観測 (session a7761122)
---

# serve_tail_follow_receives_tail_end_on_child_exit が ubuntu CI で flaky fail する

## 観測事実 (2026-07-03)

- Release run 28655240907 の `ci / Test (ubuntu-latest / stable)` (job 84982620400) で
  `daemon::session::tests::serve_tail_follow_receives_tail_end_on_child_exit` が
  panic fail (`session.rs:3267:43`、suite 32.03s、779 passed / 1 failed)
- **同一 commit (9b174349) の並走した単独 CI workflow (28655240807) では同 test は pass**
  → code 起因の deterministic fail ではなく環境/timing 依存の flaky
- 同日、同一 runner 世代の ubuntu で別の daemon/PTY 系 flaky も観測されている
  ([[2026-07-03-bug-main-unittest-hang-ubuntu-ci]] /
  [[2026-06-02-bug-flaky-serve-propagates-child-exit-code]])。CI 上で 2 workflow が
  同時に full suite を回した時間帯であり、runner 負荷との相関が疑われる (未検証)

## 真因調査の方針 (flaky ラベルで打ち切らない)

1. session.rs:3267 の panic 箇所 (= 何の expect / deadline か) を特定する
2. ローカル高負荷並列 (`cargo test --workspace` 複数同時) で再現を試みる
3. serve/tail 系 flaky 群 (本件 + flaky-serve-propagates-child-exit-code) と失敗軸が
   共通か比較し、共通なら統合して DR-0025 Phase 2/4 (Serve/Client reducer 化) への
   blocked 遷移を検討する

## 受け入れ条件

- [ ] 不安定さの軸が観測データで特定されている
- [ ] CI 並列実行で安定して pass する (または根拠付きで blocked_by DR-0025 Phase N)
