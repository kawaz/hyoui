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

## 再観測 (2026-07-19, v0.9.10)

- Release run: https://github.com/kawaz/hyoui/actions/runs/29694355729 (commit 9180fd8e)
- job: `ci / Test (ubuntu-latest / stable)`
- 失敗テスト: `daemon::session::tests::serve_tail_follow_receives_tail_end_on_child_exit`
- panic 箇所: crates/hyoui/src/daemon/session.rs:3310 (前回観測時は :3267、その後のコード変更で行がずれている)
- panic 内容: `frame: Protocol(UnexpectedEof("size header"))`
- suite: 806 passed; 1 failed; finished in 32.03s

## 不安定さの軸を特定 (2026-07-26、CI 実データ集計)

直近 12 run の blocking job ログから **lib suite の所要時間**と失敗の相関を取ったところ、
きれいな bimodal で相関していた:

| lib suite 所要 | 実行回数 | 失敗 |
|---|---|---|
| **32.0s** | 8 | **4 (50%)** |
| 4.2〜4.8s | 6 | 0 |

本 test の失敗 4 件は **すべて 32.0s の回**。前回観測 (v0.9.10) の
`finished in 32.03s` も同じ。

32s の正体は `client::attach::tests::connect_token_mismatch_returns_specific_hint` が
子 `/bin/sleep 30` の自然死を 30s 待っていたこと (= 詳細と修正は
[[2026-07-25-bug-flaky-serve-ro-lock-acquire-rejected]])。30s 居座る daemon が
他 test の時間依存 assert を圧迫していた。

修正後は lib suite が常時 ~2.9s になり (macOS 5 連続実測)、32s モード自体が消えた。

### ただし「32s 除去 = 本 test 解決」ではない (2026-07-26 反証)

32s 除去後のローカル `cargo test --workspace` 8 連続で、本 test が **再び 1 回失敗**した
(`session.rs:3543`、round 1/8)。同ランでは `serve_tail_request_follow_switches_subscription`
(session.rs:3763) も別 round で失敗している。

= 32s 居座りは **増悪要因ではあるが唯一の原因ではない**。full-workspace 並列の
contention 自体でも落ちる。CI の相関データ (32s: 4/8 失敗、4.3s: 0/6) は
「32s だと失敗率が跳ね上がる」ことは示すが、4.3s 側のサンプルが 6 件しかないため
「4.3s なら落ちない」までは主張できない。

注: 観測に使った開発機は他セッション由来の常駐 hyoui 46 process + load 20〜40 という
CI より遥かに過酷な条件。CI (= 専有 runner) で同率で落ちるとは限らない。

## 受け入れ条件

- [x] 不安定さの軸が **部分的に** 特定されている (= lib suite 32s が強い増悪要因)
- [ ] 32s 除去後も残る contention 由来の失敗の真因特定 (= tail follow subscriber へ
      TailEnd を送る経路と client drop の順序を疑う)
- [ ] CI 並列実行で安定して pass する (= 修正 push 後の CI で確認)
