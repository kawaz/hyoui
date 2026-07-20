---
title: "bug: `serve_propagates_child_exit_code` が full workspace 並列実行時に flaky fail"
status: blocked
category: bug
created: 2026-06-02T00:00:00+09:00
last_read:
open_entered: 2026-06-02T00:00:00+09:00
wip_entered:
blocked_entered: 2026-07-03T16:01:05+09:00
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by: DR-0025 Phase 3
origin: DR-0016 record stop hang fix の pkf run push 時に test deps で 1 件 fail
---

# bug: `serve_propagates_child_exit_code` が full workspace 並列実行時に flaky fail

- Priority: 低 (= 再実行で通る、機能影響なし、ただし push / CI の不安定要因)

未調査、観測のみ。

## 症状

`pkf run push` の test deps (= `cargo test --workspace`) で稀に:

```
daemon::session::tests::serve_propagates_child_exit_code
test result: FAILED. 593 passed; 1 failed; 2 ignored
```

## flaky 確定の根拠

- 単体実行 `cargo test -p hyoui --lib daemon::session::tests::serve_propagates_child_exit_code -- --exact` を **3 回連続で全 pass**
- 直前の手動 `cargo test --workspace` では 594 全 pass
- = **full workspace 並列実行時のみ** race で fail、単体では再現しない

## 推定原因 (= 未検証)

`serve_propagates_child_exit_code` は child PTY の exit code 伝搬を検証する test。PTY / waitpid / signal 系で、並列実行時に他 test と以下のいずれかで干渉する可能性:

- SIGCHLD handler の global state 共有 (= プロセス単位の signal 配信)
- waitpid の race (= 別 test の子プロセスを reap してしまう)
- PTY fd / socket の資源競合

CLAUDE.md §検証主義の「signal は process group / session 単位の broadcast」の罠と同種の可能性。

## 関連

- 過去 `docs/issue/2026-05-26-bug-flaky-agent-tests.md` (= 廃止済、agent.rs 削除で症状消滅) と同じ「並列 test の race」class
- DR-0014 §Anti-patterns「CI 6h hang を matrix test 由来と推測 → 真因は backpressure test」= flaky test の真因特定は慎重に

## 対処 (= 暫定)

- push の test deps で fail したら **再実行** (= flaky なので次は通ることが多い)
- 恒久対処は signal test の serialize (= 既存に static Mutex で signal test を serialize する pattern あり、`R4-C4` commit `cf2bbe50` 参照) を本 test にも適用する調査が必要

## 次のアクション

- `serve_propagates_child_exit_code` が他のどの test と並列で走ると fail するか特定 (= `cargo test -- --test-threads=1` で pass するか確認 → race 確定)
- signal serialize Mutex を適用するか、test 設計を見直す

## Triage (2026-07-03)

DR-0025 Phase 3 (Child state machine 化) で child exit code 伝播の状態管理が Child reducer に
集約される見込み。signal handler の global state 共有 / waitpid race といった推定原因は
Phase 3 の single-writer 化で構造的に解消される対象のため、Phase 3 完了待ちとして blocked に
遷移する。

## 再観測 (2026-07-19, v0.9.10)

- Release run: https://github.com/kawaz/hyoui/actions/runs/29694355729 (commit 9180fd8e)
- job: `ci / Test (macos-latest / stable)`
- 失敗テスト: `child_exit_propagates_code` (crates/hyoui-cli/tests/daemon_death_exit.rs:92)
- panic: `assertion \`left == right\` failed: 子の exit 7 が client に伝搬するはず (= ChildExited)`, `left: None`, `right: Some(7)`
- 注: 本 issue の対象 test は `daemon::session::tests::serve_propagates_child_exit_code` (lib test) だが、今回の失敗は CLI e2e `child_exit_propagates_code` (daemon_death_exit.rs) — 同一の「child exit code 伝搬」flaky family として本 issue に記録。DR-0025 Phase 3 待ちの blocked 状況は変わらず
