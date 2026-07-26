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

## Linux での再現と切り分け (2026-07-26 第 2 弾)

macOS では出ない (= 委譲元ローカルで 5 連続 green) が、**Docker で Linux を用意したら
高頻度で再現**した。CI が ubuntu でだけ落ちる理由はこれ。

再現環境: `docker run --cpus=4 ubuntu:24.04` + `taskset -c 0-3` (= runner 相当)。

### 単独では落ちない、suite 全体でだけ落ちる

| 実行単位 | 結果 |
|---|---|
| 当該 test 単独 × 10 | **10/10 green** |
| `cargo test -p hyoui --lib` (全 878) | 高頻度で失敗 |

失敗するのは本 test だけでなく、`serve_screen_dump_*` / `serve_propagates_child_exit_code`
/ `serve_attach_redraw_*` / `serve_tail_request_*` / `serve_ro_client_lock_acquire_rejected`
を含む **同族グループ**で、様式はすべて `UnexpectedEof("size header")` か
`read_until_contains: timed out` (= 相手 socket が突然閉じる / 応答が来ない)。

### 発見した product バグ (= 部分的原因、修正済み)

`kill_pgrp` が `kill(-child_pid)` で pgrp 送信する際、**child が pgrp leader である前提を
検証していなかった**。`setpgid` 失敗時 (= anchor 化不可経路、失敗ログに必ず出ている
warning) は child の pgid が daemon 自身の pgid のままなので、自プロセスグループを撃つ。
trace で直接観測:

```
kill_pgrp child=1140 sig=SIGTERM child_pgid=Some(Pid(10)) self_pgid=10 DANGER_SELF=true
```

lib test は 1 プロセス内で複数 daemon を動かすため、**1 つの daemon の finalize が test
プロセス自身と兄弟 daemon を SIGTERM** し、無関係な test の socket が閉じる。これは
`UnexpectedEof` の様式と完全に一致する。修正済み (= pgid 検証して単体送信にフォールバック)。

### ただし「これで解決」ではない (= 未完)

- DANGER_SELF の発火は **69 回中 1 回**で、失敗頻度に対して少なすぎる
- 修正後も Linux suite は失敗する。順序バイアスを除去した A/B (A→B / B→A 交互、
  各 22 回) では **A(fix) 13/22 (59%) vs B(base) 7/22 (32%)、Fisher 正確検定 p=0.129**
  = 有意差なしだが点推定は悪化側だった。原因として「leader でないだけで単体送信に
  落ちると孫が刈り取られず PTY を掴んだまま残る」が考えられたため、判定を
  `pgid == getpgrp()` (= 自爆する場合のみ) に絞り込んだ。絞り込み後の再測 (各 14 回)
  は **A 10/14 (71%) vs B 8/14 (57%)、p=0.695** で base と区別できない水準に戻った
- なお同じ base が測定回によって 32% → 57% と大きく振れており、観測に使った開発機が
  他セッションで load 15〜20 だったことによる **環境ノイズが支配的**。有意差を出すには
  専有環境での再測が要る
- `--test-threads=1` (直列) でも 3/6 で失敗するので、**単純な並列度の問題ではない**
  (= 最初の 3/3 green は偶然だった。test-threads を 2/4/8/16 で振っても
  8 だけ 2/3 pass という非単調な結果で、閾値として使えない)

= **残る要因は未特定**。プロセス跨ぎでない何か (= 同一プロセス内の daemon 同士が
共有する global state、あるいは anchor 化不可経路そのものの副作用) が疑わしいが、
裏取りできていない。

### 次の調査方針

1. ノイズのない環境で A/B を取り直す (= 専有マシン or 他セッション停止時)
2. `anchor 化不可` fallback (`forkpty_then_exec_legacy`) が controlling tty / pgrp に
   与える副作用を精査する。全失敗ログにこの warning が出ている点が示唆的
3. 失敗した test の socket が「いつ・誰に」閉じられたかを、daemon 側 fd の
   close 時点まで追う (= 今回は kill_pgrp までしか追えていない)

## 受け入れ条件

- [x] 不安定さの軸が **部分的に** 特定されている (= lib suite 32s が強い増悪要因)
- [x] Linux で再現環境を確保 (= Docker + cpus=4 + taskset、macOS では再現しない)
- [x] product バグを 1 つ特定・修正 (= kill_pgrp の自プロセスグループ誤送信)
- [ ] **残る要因の特定** (= kill_pgrp 修正後も再現。ノイズのない環境での A/B 再測が先)
- [ ] CI 並列実行で安定して pass する (= 修正 push 後の CI で確認)
