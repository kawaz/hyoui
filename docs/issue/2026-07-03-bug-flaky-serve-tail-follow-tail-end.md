---
title: serve_tail_follow_receives_tail_end_on_child_exit が ubuntu CI で flaky fail する
status: wip
category: bug
created: 2026-07-03T19:50:00+09:00
last_read: 2026-07-27T00:30:00+09:00
open_entered: 2026-07-03T19:50:00+09:00
wip_entered: 2026-07-27T00:30:00+09:00
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

### 決定的な機構の特定 (2026-07-26、C 最小再現で syscall レベル確認)

**`forkpty(3)` は child を親と同じ process group に置く** ことを実機で確認した。
`forkpty` は内部で `login_tty` → `setsid` を呼ぶが、**呼び出し元が既に session leader
だと `setsid` が EPERM で失敗**し、child は親の pgrp に残る。

```c
pid_t pid = forkpty(&m, 0,0,0);
printf("parent pid=%d pgid=%d\n", getpid(), getpgrp());
printf("child  pid=%d pgid=%d\n", pid, getpgid(pid));
```
```
parent pid=20 pgid=20
child  pid=21 pgid=20      ← child は親と同じ pgrp
=> kill(-child_pid) が自爆する
```

hyoui は `anchor 化不可` の時に `forkpty_then_exec_legacy` へ fallback する
(= **失敗した CI ログには必ずこの warning が出ている**)。この経路の child は
親 pgrp に居るため、`kill_pgrp` (= `kill(-pid)`) が **test プロセス自身と兄弟
daemon を撃つ**。daemon 側の trace でも実際に発火を確認:

```
SELF_PGRP_AVOIDED child=287 sig=SIGCONT
SELF_PGRP_AVOIDED child=516 sig=SIGTERM
```

(= `kill_pgrp` 修正で単体送信に落とした回数。修正前はこれが pgrp 送信だった)

### 試して **失敗した** 修正案 (= 記録、再挑戦時の地雷)

`forkpty_then_exec_legacy` の親子両側で `setpgid` を足して child を独立 pgrp に
する案を実装したところ、**同 module 63 test 中ほぼ全部が落ちる大規模 regression**
になった (8/8 で 28〜30 件失敗)。revert 済み。

理由は未分析だが、legacy 経路は `forkpty` が `login_tty` で slave を controlling
tty にする前提で組まれており、そこへ pgrp を動かすと foreground pgrp と
controlling tty の対応が壊れて子への入出力が成立しなくなる、というのが有力な筋
(= `tcsetpgrp` を伴わない `setpgid` 単独では不整合になる)。**修正するなら
`tcsetpgrp` とセットで、legacy 経路全体の tty 設計を見直す必要がある**。

### 次の調査方針

1. **本命**: test 側で `Session::start` を直接呼ぶのをやめ、`setsid` 済の状態で
   anchor 経路に乗せる (= legacy fallback に落ちなければ pgrp 問題は原理的に消える)。
   product の tty 設計に触らずに済むので安全side
2. legacy 経路を直すなら `setpgid` + `tcsetpgrp` をセットで設計し直す
   (= 上記「失敗した修正案」参照、単独 setpgid は不可)
3. ノイズのない環境 (= 専有マシン) で A/B を取り直して有意差を確認する

## 真因の 1 つを特定・修正 (2026-07-27、macOS で決定的再現)

前回までは「Linux でしか再現しない環境依存」と整理していたが、**test の子を
短命化すると macOS でも 10/10 で決定的に再現する**ことが分かった。flaky の
正体は timing 依存の race であって、環境依存ではなかった (= ubuntu では
runner 負荷でこの窓に入りやすいだけ)。

### 再現手順 (決定的、macOS)

`serve_tail_follow_receives_tail_end_on_child_exit` の子を
`sleep 0.2` → `sleep 0.005` に変えるだけ:

```
test daemon::session::tests::serve_tail_follow_receives_tail_end_on_child_exit ... FAILED
panicked at crates/hyoui/src/daemon/session.rs:3601:43:
frame: Protocol(UnexpectedEof("size header"))
```

CI の panic 箇所・message と完全一致する (= `next_control` の
`Frame::decode_from(s).expect("frame")`)。

### 機構 (trace で直接観測)

daemon 側に trace を入れて cleanup 段の状態を観測した:

```
XX exit-site L1677              ← master EOF で子 exit を検出、即 return
XX cleanup outcome=ChildExited(Some(0)) clients=1 followers=0
```

**client は 1 人居るのに follower が 0 人**。つまり `handle_tail_request` が
一度も呼ばれていない。したがって `broadcast_tail_end_to_followers` の送信対象が
0 件になり、client は `TailEnd(ChildExited)` を受け取れないまま socket close
だけを観測する。これが `UnexpectedEof("size header")` の正体。

なぜ呼ばれないか — `serve_loop` の処理順が

1. listener accept
2. **master (= 子 PTY) 読み取り → EOF なら即 `return ChildExited`**
3. client frame の decode → `handle_client_frame`

で、**子 exit の検出が client frame 処理より前にある**。さらに client は
handshake worker 経由で **poll_fds 構築後に登録される**。この 2 つが重なると:

- (a) 同一 poll 周回で既に POLLIN していた client frame
- (b) 登録が exit 検出に間に合わなかった client の frame

がどちらも処理されず捨てられる。実際 trace では `client_revents=[]` で、
tail.request は poll 対象にすら入っていなかった (= (b) のケース)。

子が長生きすれば subscription 登録が先に済むので表面化しない。短命な子ほど
窓に入る = CI で `sleep 0.2` が負荷で相対的に「短命化」すると落ちる。

### 修正 (commit 済み)

子 exit を即 return せず **100ms の drain 窓**に保留し、遅れて届く client frame
も通常経路で処理してから抜ける。実装上の注意点 2 つ (どちらも実測で踏んだ):

- 窓の間は **master を poll 対象から外す**。`POLLHUP` は要求 mask に関係なく
  報告されるため、外さないと poll が即 return し続けて 100ms を busy-spin で
  焼く
- 締切判定は **ループ冒頭**に置く。末尾に置くと `Interrupted` / `Timeout` の
  `continue` 経路が判定を飛ばし、**永久ループになる** (= 実際に suite が
  ハングした)

regression test `serve_tail_follow_receives_tail_end_when_child_exits_immediately`
を追加 (子 5ms)。修正を revert すると RED になることを確認済み。

| 検証 | 結果 |
|---|---|
| 5ms 子 (修正前) | 10/10 FAILED |
| 5ms 子 (修正後) | 18/18 ok |
| macOS lib suite 全 878 | ok (3.00s) |
| clippy / fmt | clean |

### ただし CI flaky はこれで終わらない (= 未解決部分)

Docker (`--cpus=4` + `taskset -c 0-3` + `--test-threads=4`) で Linux 15 回:

```
RESULT pass=3 fail=9  (修正込みのバイナリ)
```

失敗するのは本 test だけでなく `serve_screen_dump_*` /
`serve_attach_redraw_includes_pre_attach_output` /
`serve_tail_request_no_follow_dumps_buffer` を含む同族グループで、様式は
`UnexpectedEof("size header")` か `read_until_contains: timed out`。

**これらは本 race では説明できない**: 該当 test の子は `sleep 30` 等で
生き続けるので「子 exit が client frame を追い越す」窓に入りようがない。
= **独立した第 2 の原因が残っている**。

なお Linux 側の失敗は「32s かかる回」と「4s 前後で終わる回」の両方で起きて
おり、前回特定した 32s 増悪要因とも別軸。

### 次の調査方針 (更新)

1. 第 2 の原因を、本 race と同じやり方で **決定的再現に落とす** のが先決
   (= 「Linux でだけ」「suite 全体でだけ」の条件を、timing パラメータを
   振って決定的な形に還元する。本 race は子の寿命が rev だった)
2. 対象は子が長命な test 群 (`serve_screen_dump_*` / `serve_attach_redraw_*`)。
   共通するのは **handshake → 最初の raw_data 受信**の経路であり、
   `read_until_contains` の timeout がその区間で起きている。handshake worker
   登録と master 出力 broadcast の間に別の取りこぼし窓がある可能性
3. 前セッションが疑っていた「同一プロセス内の daemon 同士が共有する global
   state」も引き続き候補 (= `SIGCHLD_SELFPIPE_LOCK` を取れなかった serve は
   self-pipe 無しの 500ms polling 経路に落ちる。lib test は 1 プロセスで
   多数の daemon を動かすため、**self-pipe を取れるのは常に 1 つだけ**。
   これは負荷でなく構造的な差分なので、有力)

## 受け入れ条件

- [x] 不安定さの軸が **部分的に** 特定されている (= lib suite 32s が強い増悪要因)
- [x] Linux で再現環境を確保 (= Docker + cpus=4 + taskset、macOS では再現しない)
- [x] product バグを 1 つ特定・修正 (= kill_pgrp の自プロセスグループ誤送信)
- [x] product バグをもう 1 つ特定・修正 (= 子 exit 検出が client frame 処理を
      追い越して tail.request を捨てる race。**決定的再現あり**、regression test 追加)
- [ ] **残る要因の特定** (= 上記 2 つの修正後も Linux suite は 9/15 で失敗。
      子が長命な test 群も落ちるため第 2 の原因が独立に存在する)
- [ ] CI 並列実行で安定して pass する (= 修正 push 後の CI で確認)
