---
title: serve_tail_follow_receives_tail_end_on_child_exit が ubuntu CI で flaky fail する
status: wip
category: bug
created: 2026-07-03T19:50:00+09:00
last_read: 2026-07-29T04:20:00+09:00
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
| 5ms 子 単独 (修正前) | 10/10 FAILED |
| 5ms 子 単独 (修正後) | 30/30 ok |
| serve_tail_follow 2 件 単独 × 10 | 10/10 ok |
| clippy / fmt | clean |

**単独実行では完全に消える** = 本 race に関する限り修正は有効。ただし
suite 併走では下記のとおり別要因で落ちる。

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

#### 第 2 の原因は **macOS でも再現する** (= Linux 固有ではない、2026-07-27 訂正)

当初「Linux でだけ残る」と書いたが、macOS でも suite 併走時に再現する。
`cargo test -p hyoui --lib daemon::session` (= 65 test に絞る) を 20 回:

```
16 ok / 4 FAILED
```

落ちた test は毎回 1 件だが顔ぶれが変わる:

- `serve_tail_request_no_follow_dumps_buffer`
- `serve_tail_follow_receives_tail_end_when_child_exits_immediately`
- `serve_tail_follow_receives_tail_end_on_child_exit`
- `accept_loop_pending_cap_independent_from_clients_cap`

一方、**単独実行なら同じ test が 30/30 green**。したがって第 2 の原因は
「特定 test のロジック」ではなく **同一プロセス内で daemon を多数並走させた
時に起きる何か** (= 前回から疑っている global state 共有) である可能性が高い。
`accept_loop_pending_cap_*` (= tail と無関係な accept 系) まで落ちるのは
その傍証。

#### 試して効果が無かった対処 (= 記録)

drain budget を 100ms → **2000ms** に上げても解決しない (8 回中 1 回失敗、
落ちたのは `serve_tail_request_no_follow_dumps_buffer`)。suite 実行時間だけ
3.0s → 4.9s に伸びるので **100ms のまま据え置いた**。
= 第 2 の原因は「drain 時間が足りない」類ではない。

### ee43651b は回帰の原因ではない (2026-07-27、高負荷 A/B で確定)

`cargo test -p hyoui-cli` の `child_inherits_hyoui_session_id_env` が ee43651b で
決定的に回帰した、という二分探索の報告があったが、**A/B を取り直したところ
ee43651b は無罪**だった。

まず通常負荷 (load ~20〜40) では ee43651b 込みのツリーで **34/34 green** で、
一度も再現しない。再現には人工負荷が要る。CPU burner 40 本で load を 118 まで
上げると、同じツリーで決定的に再現した:

| revision | 高負荷時の結果 |
|---|---|
| HEAD (= ee43651b と code 完全一致) | **6 fail / 12** |
| 4ed57674 (= ee43651b の直前) | **5 fail / 12** |

`session.rs` を親 revision の内容に差し替えて再ビルドし、同一負荷下で 12 回ずつ
回した結果 (作業後ツリーは復元済み)。**親でも同率で落ちる** ため、
「ee43651b が回帰を入れた」は成立しない。二分探索時の
「親は load 120 でも 0.98s で通った」という対照観測は、負荷のかかり方が
その 1 回で偶々軽かった (= サンプル 1) と見るのが妥当。

失敗の様式は毎回同一で、client には attach ヒント行 107 bytes だけが届き、
子の出力が 1 byte も来ないまま 10s timeout:

```
wait_for("MARK=") timed out after 10s; output so far (107 bytes):
"[hyoui] detach: Ctrl+Z  |  子へ Ctrl+Z: 2 連打  |  peek (read-only): hyoui attach <session> --mode=ro\r\n"
```

#### drain 窓が原因でないことの機構的裏付け

疑われていた「drain 窓中に master を poll から外すので子出力が止まる」は、
本 test では原理的に起きない:

- 本 test の子は `echo MARK=...; sleep 30` で 30 秒生きる。`deferred_exit` が
  立つのは `poll_with_transition` が `ChildState::Exited` を返した時だけで、
  これは `waitpid` が実際に reap した場合に限る (`daemon/pty.rs:122-172`。
  `StillAlive` も `Err` も `Alive`/`Stopped` を返すので、生きている子で
  誤発火する経路が無い)。EIO 側の分岐も同じ `poll_with_transition` を通る
- `poll_master = deferred_exit.is_none()` なので、子が生きている限り master は
  常に poll 対象に載る

#### 第 2 の原因と同根の可能性が高い

失敗区間は **handshake 成立 (= ヒント行が出た) から最初の raw_data 受信まで**で、
これは「次の調査方針」3 に書かれている `serve_screen_dump_*` /
`serve_attach_redraw_*` の `read_until_contains: timed out` と同じ区間・同じ様式。
= 第 2 の原因の症状群に本 test も含まれると見るのが自然。

ただし本 test は **既存 2 仮説のどちらでも説明できない**ので、仮説側を
狭める材料になる:

- 「同一プロセス内で daemon を多数並走」説 → 本 test は 1 プロセス 1 daemon
  の別プロセス実行なので該当しない
- self-pipe degraded 説 (= 方針 1) → `SIGCHLD_SELFPIPE_LOCK` は
  `static Mutex` (`session.rs:176`) で **プロセス内でしか効かない**。
  別プロセスの daemon は常に self-pipe を取れるので degraded に落ちない。
  加えて degraded 経路の差は検出 latency 500ms であって、10s timeout の
  説明にならない

= 残るのは「handshake 成立後〜初回 raw_data の区間で、高負荷時に子出力が
client へ配送されない (もしくは子が exec まで到達していない)」という、
より下層の何か。次はこの区間を daemon 側 trace で分解する。

## 第 2 の原因を特定・修正 (2026-07-28、anchor 経路の SIGTTOU race)

前節の「次の調査方針」4 (= `child_inherits_hyoui_session_id_env` を高負荷下で
daemon 側 trace) を実施し、**真因を特定して修正した**。ただし後述のとおり
lib suite (= legacy 経路) の失敗は別原因として残る。

### 観測 (= 3 段階で切り分け)

CPU burner 40 本 (load 60〜250) で再現させ、以下を順に観測した。

**1) 配送ではなく子が喋っていない**: 既存の `--debug-dump-server` (= master
read 直後の生 bytes) と `--debug-dump-client` を test から有効化したところ、
失敗時は **両方 0 byte**。daemon は子から 1 byte も読んでいなかった。

**2) 子は exec に到達していない**: 子 script に PTY を経由しないマーカー
(`: > <dir>/child_exec`) を仕込むと、失敗 3 回すべてで **マーカーが作られない**。
`sh` は最初のコマンドすら実行していない。

**3) 子は fork 済みで T (stopped) のまま**: daemon に trace を入れ、失敗時に
`ps` を撮った。

```
XXTRACE daemon start-ok pid=35983
XXTRACE serve-enter pid=35983 pgid=Pid(35983) sid=Some(Pid(35983)) child=35984
[hyoui] detach: Ctrl+Z  |  子へ Ctrl+Z: 2 連打  |  peek (read-only): ...
```
```
  PID  PPID  PGID STAT TTY      COMMAND
35983 35949 35983 Ss   ttys121  hyoui run --detached -- sh -c ...
35984 35983 35984 T+   ttys121  hyoui run --detached -- sh -c ...
```

子 35984 は **`T` = stopped**、かつ COMMAND がまだ親のもの (= `execve` 前)。
`PGID == 自 PID` なので `setpgid(0,0)` は成功済み。

### 機構

`sys/raw.rs` の `openpty_fork_anchor_exec` の child path:

```rust
libc::setpgid(0, 0);                          // 新 pgrp の leader になる
libc::tcsetpgrp(slave_raw, libc::getpid());   // ← ここで SIGTTOU
```

POSIX では **`tcsetpgrp` を background process group から呼ぶと SIGTTOU が
生成され**、ignore / block されていなければプロセスは **停止する**。
直前の `setpgid(0,0)` で子は新 pgrp に移った一方、slave の foreground pgrp は
**親が `tcsetpgrp` を実行するまで親のまま**なので、親より先にここへ来た子は
background 扱いになり停止する。誰も SIGCONT を送らないので子は exec に到達せず、
daemon は永久に子出力を待つ。

親側 (同関数の parent path) は **同じ罠を認識して `signal(SIGTTOU, SIG_IGN)`
してから `tcsetpgrp` を呼んでいた**。子側にだけこのガードが無い、片面実装。

flaky になる理由も説明がつく: 親と子が同じ `tcsetpgrp` を競走しており、
**親が先なら子は foreground なので無傷、子が先なら停止**する。高負荷ほど
子が先行する確率が上がるため、負荷依存で顔を出す。

### 修正

子側でも `tcsetpgrp` を SIGTTOU 一時 ignore で挟む (= 親側と対称)。exec 後の
子に ignore を漏らさないよう (`execve` は ignored disposition を引き継ぐ)
旧 disposition へ戻す。`signal` は async-signal-safe。

### A/B (高負荷、交互実行で順序バイアス除去)

env switch で修正を無効化できるようにし、A (修正あり) / B (修正なし) を
1 回ずつ交互に 14 iteration:

| 条件 | 結果 |
|---|---|
| **A (fix)** | **0 fail / 14** |
| **B (base)** | **6 fail / 14** |

Fisher 正確検定 両側 **p = 0.016** (= 有意)。switch を除去した clean な
バイナリでも load 247 で **14/14 green**。

### lib suite の失敗は別原因として残る (= 未解決)

burner を止めて `cargo test -p hyoui --lib daemon::session` を 20 回:

```
pass=17 fail=3
  serve_ro_client_lock_acquire_rejected
  serve_tail_follow_receives_tail_end_on_child_exit
  serve_tail_follow_receives_tail_end_when_child_exits_immediately
```

前任の観測 (4/20) と同水準で、**改善していない**。理由は明快で、失敗した
3 run すべてのログに `session anchor 化不可` warning が出ている = lib test は
`Session::start` を直接呼ぶため **legacy 経路 (`forkpty_then_exec_legacy`)**
に落ちており、今回修正した anchor 経路を通らない。

= 本修正が効くのは **production 経路 (`hyoui run` = setsid 済 daemon)** のみ。
CI で落ちている lib 側の flaky には、legacy 経路側の別調査が要る
(= 「試して失敗した修正案」節の `setpgid` + `tcsetpgrp` 設計見直しが該当領域)。

なお本件は test だけの問題ではなく **production の実害**である点に注意:
高負荷マシンで `hyoui run` すると、子が exec 前に停止して永久に起動しない。

### 次の調査方針 (旧、方針 4 は上記で完了)

1. **最有力候補の裏取りから始める**: `SIGCHLD_SELFPIPE_LOCK` を取れなかった
   serve は self-pipe 無しの **500ms polling 経路**に落ちる (`sigchld_pipe`
   が None、`serve_loop` の `NO_SELFPIPE_POLL_CAP_MS`)。lib test は 1 プロセス
   で多数の daemon を並走させるため **self-pipe を取れるのは常に 1 つだけ**で、
   残りは全部 degraded 経路で動く。これは負荷でなく **構造的な差分**であり、
   「単独なら 30/30 green / 併走だと落ちる」「落ちる test の顔ぶれが毎回違う」
   「tail と無関係な accept 系まで落ちる」の 3 点すべてと整合する。
   検証案: daemon ごとに self-pipe 取得の成否を trace し、落ちた test の
   daemon が degraded 側だったかを確認する (= 本 race で使った trace と同じ手)
2. 対象 test は `daemon::session` module に絞れば macOS で 4/20 再現するので、
   Docker を待たずに反復できる (= 調査ループが速い)
3. 子が長命な test 群 (`serve_screen_dump_*` / `serve_attach_redraw_*`) の
   失敗は `read_until_contains` timeout = **handshake → 最初の raw_data 受信**
   の区間で起きている。1 が外れたらこの区間の取りこぼし窓を疑う
4. **本命に格上げ (2026-07-27)**: 上記 3 の区間を `child_inherits_hyoui_session_id_env`
   で調べるのが最も速い。この test は CPU burner で load 100 超にすれば
   **単独実行 + 別プロセスで 50% 再現**し、lib suite の並走も多数 daemon も
   要らない (= 変数が最小)。1 / 「多数並走」説はどちらもこの test を
   説明できないことが確定しているので、切り分け済みの土俵で trace できる

## 第 3 の原因を特定・修正 (2026-07-29、fork〜exec 窓の継承 self-pipe handler)

前節が残した「legacy 経路側の残存要因」を trace で特定した。legacy 経路そのものの
tty 設計とは無関係で、**fork した子が親の self-pipe に signal byte を書き込む**のが
機構だった。したがって「試して失敗した修正案」の `setpgid` + `tcsetpgrp` 見直しは
不要 (= 地雷を踏み直さずに済む)。

### 機構

`SELFPIPE_WRITE_FD` は **プロセス global な raw fd** で、`fork` した子は
signal handler の disposition と write fd の両方を継承する。fd は `FD_CLOEXEC` だが
**`fork` から `execve` までの窓では生きている**。この窓で子に signal が届くと
継承 handler が走り、**親の self-pipe に signal byte を書き込む**。

lib test は 1 プロセスで多数の daemon を並走させるため被害が顕在化する:

1. ある test の `Session::drop` が `kill_pgrp(child, SIGTERM)` を撃つ
2. その子がまだ exec 前なら、子の継承 handler が byte 15 を**共有 self-pipe** に write
3. **無関係な daemon** の `serve_loop` がその byte を drain し「SIGTERM を受けた」と誤認
4. 自分の子を SIGTERM して socket close → 被害 test の client が
   `UnexpectedEof("size header")`

### 観測 (= trace で直接確認、対応は 1:1)

handler 内で `getpid() != owner_pid` を検出する probe (`XXFOREIGN`) を仕込んだ。

```
XXKILL  thread=session_drop_kills_orphan_child child=71784 mode=pgrp sig=SIGTERM target=-71784
XXFOREIGN sig=15 writer_pid=71784 owner_pid=71736      ← 子が親のパイプに書いた
XXSIGTERM-RECV self_pid=71736 child=71777 term_sender=-1 reason=sigterm  ← 無関係な daemon が誤認
```

`term_sender` は `SA_SIGINFO` の `si_pid` を記録したもので、**-1 = SIGTERM は
一度も届いていない**。byte が signal 由来でないことの直接証拠。
予備の 30 run では probe 発火 3 run が失敗 3 run と完全一致した。

### 修正

2 段構え。子側で write fd の登録を外す (`disarm_self_pipe_in_child`) **だけでは
`fork` から disarm 実行までの窓が残り、実測で再発した** (A 群 16 回中 1 回、
XXFOREIGN 付き)。そこで `fork` 自体を `block_handled_signals` で囲み、子は
disarm 後に mask を戻す。`execve` は signal mask を引き継ぐため子側の復元は必須。
anchor / legacy の両 spawn 経路に入れた。

### A/B (交互実行、専有した jj workspace で 100 pair)

`HYOUI_AB_NO_CHILD_DISARM=1` で修正を無効化できるようにし、A (修正あり) /
B (修正なし) を 1 回ずつ交互に 100 iteration (`cargo test -p hyoui --lib daemon::session`)。

| 指標 | A (fix) | B (base) | Fisher 両側 |
|---|---|---|---|
| suite 失敗 | **2/100** | **11/100** | **p = 0.018** |
| 機構マーカー `XXFOREIGN` | **0/100** | **10/100** | **p = 0.0015** |

B の失敗 11 件中 10 件がマーカーを伴い、A では **100 回すべてマーカーが出ない**
(= 機構が消えたことの直接確認)。

### 残る失敗 (= 第 4 の原因、未特定)

マーカーを伴わない失敗が A/B 双方に残る:

- `serve_attach_redraw_preserves_alt_screen_flag` / `serve_screen_dump_ansi_*` の
  `read_until_contains: timed out` (B38 / B48)。子が長命な test 群で、
  「handshake 成立 → 最初の raw_data 受信」区間で止まる既知の様式
- `serve_tail_follow_receives_tail_end_when_child_exits_immediately` の
  `UnexpectedEof` が A 群で 1 回 (A86)。マーカーなしなので別経路
- `run_until_terminates_child_on_pattern_match` が `start: Errno(UnknownErrno)` で
  1 回 (A47)。`Session::start` 自体の失敗で症状群が違う。100 pair 中 1 回のみ

### 測定上の注意 (= 1 回目の A/B を破棄した経緯)

最初 `hyoui/main` で A/B を回したが、**別セッションが同じ workspace に並行 commit**
しており (同一 crate の `client/attach.rs` 等)、毎 iteration の `cargo test` が
異なるソースからビルドしていた。汚染とみなして破棄し、`jj workspace add` で
専有 workspace を作って取り直した。**この種の A/B は専有 workspace で回すこと**。

## 受け入れ条件

- [x] 不安定さの軸が **部分的に** 特定されている (= lib suite 32s が強い増悪要因)
- [x] Linux で再現環境を確保 (= Docker + cpus=4 + taskset、macOS では再現しない)
- [x] product バグを 1 つ特定・修正 (= kill_pgrp の自プロセスグループ誤送信)
- [x] product バグをもう 1 つ特定・修正 (= 子 exit 検出が client frame 処理を
      追い越して tail.request を捨てる race。**決定的再現あり**、regression test 追加)
- [x] product バグをもう 1 つ特定・修正 (= **anchor 経路の子が `tcsetpgrp` を
      background pgrp から呼んで SIGTTOU で停止し、exec に到達しない race**。
      2026-07-28、`ps` で `T` + 未 exec を直接観測。高負荷 A/B で
      A(fix) 0/14 vs B(base) 6/14、Fisher 両側 p=0.016)
- [x] **legacy 経路側の残存要因** — 実体は legacy 固有ではなく
      **fork〜exec 窓の子が継承 self-pipe handler で親のパイプに signal byte を
      書き込む**バグだった (2026-07-29)。`XXFOREIGN` probe で直接観測、専有
      workspace の 100 pair A/B で suite 失敗 2/100 vs 11/100 (p=0.018)、
      機構マーカー 0/100 vs 10/100 (p=0.0015)。anchor / legacy 両経路を修正
- [ ] **第 4 の原因**: マーカーを伴わない残存失敗
      (= `serve_attach_redraw_*` / `serve_screen_dump_*` の
      `read_until_contains: timed out` (= handshake→初回 raw_data 区間)、および
      マーカーなしの `UnexpectedEof` 1 件。100 pair 中 A 2 / B 1 なので
      修正の有無に依らず残る軸)
- [x] ee43651b が回帰原因ではないことを確認 (= 高負荷 A/B で親 5/12 vs
      HEAD 6/12、有意差なし)
- [ ] CI 並列実行で安定して pass する (= 修正 push 後の CI で確認)
