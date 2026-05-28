# DR-0015: `hyoui run` を fork daemon + attach client の合成に再定義 — client/server 同居廃止

- Status: Active
- Date: 2026-05-28
- Related: DR-0005 (思想), DR-0001 (jobcontrol), DR-0007 (段階リリース、本 DR で部分覆し), DR-0009 (session 分割、影響なし), DR-0014 (透過原則、本 DR の justify)

## Context

現状の `hyoui run -- cmd` は **1 プロセス内 2 thread モデル**:

- main thread = CLI client (= stdin/stdout 中継、外側 TTY の raw mode 管理)
- daemon thread = daemon (= 子 PTY 起動、socket bind、broadcast、SIGTSTP/SIGCONT/SIGCHLD handler 配線)

このモデルは「同プロセス内 fork なし」で実装が単純に見えるが、実際は **役割境界が混濁**:

- POSIX signal は **プロセス単位**で配信される。daemon thread が SIGTSTP handler を install すると、CLI client (= main thread) の termios state を **`Arc<Mutex<TtyGuard>>` 経由で共有**する必要が出る (= [Issue #1 修正、commit 2751ff28] で実際に発生した複雑さ)
- `hyoui attach` (= 単独プロセス) は daemon と別 process なので、Issue #1 修正の callback は効かない (= [attach 派生 issue] が必要)
- 同じ「外側 TTY を SIGTSTP 時に復元する」処理を **2 箇所** (= run 内 callback inject / attach 内 sigaction install) で実装する必要

この複雑さは「client と server を 1 プロセスに詰め込んだ」結果。**役割分離した方が単純**:

| 観点 | 現状 (= 1 プロセス 2 thread) | 提案 (= 2 プロセス) |
|---|---|---|
| signal 配信 | プロセス共有、`Arc<Mutex<TtyGuard>>` で thread 越し receive | 各プロセスが自分の signal を独立で受ける、共有不要 |
| 外側 termios 管理 | daemon thread が client thread の TtyGuard を触る | client process だけが触る (= role 明快) |
| 子 PTY 出力中継 | daemon thread → memory broadcast → main thread stdout | daemon process → Unix socket → client process stdout (= attach と同じ経路) |
| exit code 伝搬 | daemon thread の waitpid 結果を main が thread join で受け取る | daemon process → protocol message → client process (= 新 message 必要) |
| 起動 error 経路 | main process が exec error を直接 stderr | daemon process → ready pipe / 起動 error notification (= 新経路必要) |
| Issue #1 / attach 派生 issue | 2 箇所修正 | client process 側 SIGTSTP handler 1 箇所で吸収 |

「既に `hyoui run --detached -- cmd` で fork pattern は実装済」(= `daemonize::run_detached_parent`)。これを `--detached` フラグ有無で挙動分岐するのを廃止し、**常に fork + 親が attach** の合成に統一する。

## Decision

### 1. `hyoui run -- cmd` の動作 = fork + attach の合成

```
hyoui run -- cmd
  ├─ 親 process: detached daemon を spawn (= fork+exec "hyoui run --detached --session=... -- cmd")
  │                ↓ ready 通知待ち
  │              親自身を attach に exec で置換 (= exec "hyoui attach <session>")
  │                ↓
  │            (親 process = hyoui attach として動く)
  └─ 子 process (= daemon): "hyoui run --detached" exec 経路、子 PTY 起動 + socket bind + broadcast
```

挙動:
- `hyoui run -- cmd` (= non-detached) = **`hyoui run --detached -- cmd` と `hyoui attach <session>` の完全合成**。親 process が前者を spawn、後者に exec で置換
- `hyoui run --detached -- cmd` = daemon 子 spawn + 親即 exit (= 既存挙動)
- `hyoui attach <session>` = 既存 daemon に接続 (= 既存挙動)

`ps` で見える形:
- `hyoui run --detached --session=... -- cmd` (= daemon process、常駐)
- `hyoui attach <session>` (= 親 process、exec 後の姿)
- 「run」コマンド名は **どこにも見えない** (= parent が attach に exec で変身済)

利点:
- daemon process は外側 TTY / 親 termios を **一切知らない**
- attach client process が外側 TTY raw mode + 自分の SIGTSTP/SIGCONT を独立管理
- **memory 共有事故ゼロ** (= fork+exec で memory image 完全置換、global static の thread 罠回避)
- **コード重複ゼロ** (= 既存 `--detached` + `attach` 実装をそのまま流用)
- `run_command` 実装が wrapper 数十行に圧縮 (= spawn → ready 待ち → exec)
- debug 観測単純 (= parent を strace/lsof しても attach の挙動だけ見える)
- ps で role が一目瞭然

### 2. Protocol 拡張

> **codex review 2026-05-28 致命指摘 3 件への対応**を反映済。
> 草案ではこの section は `session.exit.notify` のみだったが、DR-0001 軸 1/2 を
> 別プロセス構成で維持するために以下の追加 message + cap が必須と判明。

#### 2.0 cap-aware broadcast helper

新 message を未対応 client に送ると decode error になる (= 既存 `broadcast_control`
は `negotiated_caps` を見ずに全員に投げる)。Phase A で **cap-aware broadcast helper**
を新設 (= `broadcast_control_with_cap("session-exit-v1", msg)` 等)、本 DR で
追加する全 message はこの helper を経由する。MVP_CAPS にも追加するが、helper 経由で
将来 client の意思 (= cap negotiate 結果) を尊重できる構造にしておく。

#### 2.1 `session.exit.notify` (= 新 message、cap flag `session-exit-v1`)

daemon が子 PTY exit を観測した瞬間、daemon shutdown 前に全 attached client へ broadcast:

```cbor
{
  "kind": "session.exit.notify",
  "exit-status": <int>,       // 親 client がそのまま `process::exit(status)` する shell convention 値
                              // = 通常 exit: 子の exit code (0..255)
                              // = signal 死亡: 128 + signum (= shell convention、bash 等と整合)
  "signal": <text> | null     // signal で死んだ場合の補足情報 (= "SIGTERM" 等、DR-0012 形式)
                              // signaled=true 時のみ非 null、debug / 表示用
}
```

client は本 message 受信時に **`exit-status` をそのまま自プロセスの exit code として
伝搬** (= 既存 `finalize_child` の `Ok(WaitStatus::Signaled(_, sig, _)) => 128 + sig`
慣習と整合)。`signal` field は補足情報で、client が「signal 死亡」を区別したい場合の
optional info (= 多くの場合は `exit-status` だけで足りる)。

daemon は本 message broadcast 後に socket close + process exit。

#### 2.2 `session.child.stopped.notify` + `session.child.resume.request` (= DR-0001 軸 1 維持、cap flag `child-state-v1`)

`Ctrl-Z` は外側 TTY の SIGTSTP ではなく **byte (`0x1a`) として子 PTY に流れる**
(= line discipline 経由、cooked モードで子側で SIGTSTP に変換)。よって
attach client process には **SIGTSTP が来ない**。client 側 SIGTSTP handler だけでは
DR-0001 軸 1 `follow` を発火できない。

owner 再設計:

```cbor
// daemon → leader (= rw + leader 取得 client)
// 子 PTY が SIGTSTP / SIGSTOP で stopped した瞬間に送る
{
  "kind": "session.child.stopped.notify",
  "pid": <uint>,
  "signal": <text> | null      // "SIGTSTP" / "SIGSTOP" / null=不明
}

// leader → daemon (= follow から復帰した時、もしくは外部 fg で leader が起き上がった時)
{
  "kind": "session.child.resume.request"
}
```

挙動:
- daemon: `waitpid(WUNTRACED|WCONTINUED)` で子 transition を観測 → stopped なら
  `notify` を leader にだけ送る (= broadcast ではない、leader 単一 receiver)
- **DR-0001 軸 2 廃止** (= kawaz 確定 2026-05-28): 新構成では daemon 常駐前提 +
  複数 client 同時 attach 可能のため、「client が外部 SIGTSTP で止まる」は
  単に **その 1 client が止まるだけ**で子プロセスとは無関係。よって daemon が
  client 要求で子 pgrp に SIGSTOP/SIGCONT を中継する経路は不要 (= 旧 §2.3
  `session.child.signal` 廃止)。これにより daemon が `killpg(child, SIGSTOP)`
  する経路自体が **本 DR では一切存在しない** ことになり、子 stopped transition は
  100% 子 self-stop 起因 = `child_stop_origin` のような衝突回避 state も不要
  (= codex v2 指摘 #1 #2 が自然消滅)
- **leader cap check** (= codex 2026-05-28 v2 指摘 #3 対応): leader が
  `child-state-v1` cap を持たない場合は notify 送信せず、daemon 内部で
  即 `killpg(child_pgid, SIGCONT)` の **auto-resume fallback** を実行
  (= 「leader 不在」と同じ扱い)。これで follow policy は cap 持つ leader 時のみ
  有効、cap 不足 client が leader の場合は実質 auto-resume になる
- leader (= attach client): `notify` 受信時、`on-child-suspend` policy に従って:
  - `follow` (= interactive default): 自プロセスを `raise(SIGSTOP)` (= 親 fg・子 stop 禁止
    invariant 維持、外側 shell に制御戻る)
  - `auto-resume` (= headless default): `session.child.resume.request` を daemon に
    送り返す (= 子に SIGCONT、self-stop を許さない)
- leader 不在 (= `hyoui run --detached` で attach なし): daemon 側で `auto-resume`
  にフォールバック (= leader notify 不能のため自動復帰)
- leader 復帰 (= 外側 `fg` で leader process が走り始める): leader が
  `session.child.resume.request` を送って子を復帰させる (= invariant 回復ルール)

policy 場所:
- `on-child-suspend` (= follow / auto-resume) は **client 側 attach 時の cap negotiate
  payload** に含める。daemon は leader の policy を覚えてフォールバック判断に使う
- 既存 `OnChildSuspend` enum (= `crates/hyoui/src/cli.rs`) はそのまま流用、新構造に
  追従させる形

#### 2.3 DR-0001 軸 2 (= 親 hyoui 自身の外部 SIGTSTP 経路) は **本 DR で廃止**

新構成では daemon が常駐 + 複数 client 同時 attach 可能。「client が外部 SIGTSTP で
止まる」シナリオは新構成下では単にその client 1 つが止まるだけで、子プロセスや
他 client には何の影響もない (= kawaz 確定 2026-05-28)。

```
[BEFORE = DR-0001 軸 2 (= 1 プロセス 2 thread モデル前提)]
hyoui process が外部 TSTP 受ける = daemon thread + main thread 両方止まる
→ 「子も止めるか?」が問題になる (= transparent / decouple 政策)

[AFTER = 本 DR]
client process が外部 TSTP 受ける = その client process だけが止まる
→ daemon は別 process なので何も止まらない、子も走り続ける
→ 他 client が attach 中なら何も影響を受けない
→ 「子も止める」要求自体が思想と矛盾 (= 複数 attach 前提では client 1 個の停止が
   子に波及するのは非対称、複数 client のうちどれが止まったら子を止めるか不定)
```

廃止される機能:
- `--on-parent-suspend=transparent|decouple` フラグ (= 意味不在に)
- DR-0001 軸 2 の handler 経路 (= 全部削除)
- 軸 2 用の新 message (= 草案 v2 で計画した `session.child.signal` は不要)

新構成での client SIGTSTP 受信時の挙動:
- client process は **自分の termios を suspend → `raise(SIGSTOP)`** するだけ
- daemon に対しては **何もリクエストしない** (= 子は無関係)
- 復帰時は termios を resume するだけ
- これは旧 `decouple` 相当だが、新構成では「唯一の正解」(= 政策を選ばせる必要すらない)

#### 2.3.1 `hyoui run` 親 client の死亡時 semantics (= kawaz 確定 2026-05-28)

「`hyoui run` で起動した親 client が死んだら子も止めるべきか?」を検討した結果、
**親 client も他 attach client と完全に同じ扱い** (= 案 B) で確定。

挙動:
- `hyoui run` 親 client が SIGHUP / SIGTERM / SIGKILL / panic / abrupt close で死ぬ
  → socket close → daemon は **何もしない** (= 子も他 client も無事)
- 残る attach client が居れば session 継続、居なくなっても daemon は alive
- 別端末から `hyoui attach <session>` で再接続可能

理由:
- 業界 standard (= tmux / abduco / shpool / zellij、Agent 調査 2026-05-28 で確認)
- 「複数 attach」と「親死で子も死」の semantics 衝突を回避
- daemon = 「独立した憑依対象」(= DR-0002 命名思想と整合)、attach client は来訪者
- `hyoui run` ≒ `hyoui run --detached <session>` + `hyoui attach <session>` の **完全合成**
  (= --detached 有無で daemon の寿命を分けない)
- 子の寿命は **子自身の exit** だけが決める (= daemon は子の lifecycle に介入しない)
- daemon が exit するのは 子 PTY exit 時のみ (= `SessionExitNotify` broadcast 後)

`pkill hyoui` で誤って親を殺したら子残留 → shell 直感とは外れるが、`hyoui list` /
`hyoui kill <session>` で明示クリーンアップ可能。daemon 残留事故は `hyoui list` で
発見できる (= R5-H3 stale socket 判定で死活確認可能)。

DR-0001 への影響:
- §軸 2 全体を「本 DR で廃止」と annotate
- §デフォルト表から軸 2 列を削除
- §仕様の限界 で「親が SIGSTOP 受けた時の子への影響」の記述削除
- §invariant の「親が死ねば子も」を **「子が死ねば daemon と client は exit」のみ**に修正
  (= 親 client 死 → 子は無影響、子 exit → daemon exit + 全 client exit、の片方向)

#### 2.3.5 採用する実装パターン (= 類似 OSS 調査 2026-05-28 由来 + exec attach pattern)

DR-0015 起票直後に agent で tmux / abduco / dtach / zellij / shpool / mosh の
client/server 分離パターンを調査。本 DR の **exec attach pattern** (= §1) と
組み合わせて以下を採用:

**起動 race 対策 = tmux 流 lock + retry**:
- 親プロセスは最初に socket connect 試行 → ENOENT/ECONNREFUSED なら
  `<socket>.lock` を flock で取得 → daemon を spawn → 再 connect
- 二段構えで「別 client が同時に同 session を起こす」 race を回避
- 既存 hyoui の `--session` 名空間と整合

**daemon 起動 handshake = 既存 ready pipe + stderr inherit** (= exec attach pattern で
socketpair 不要に):

exec attach pattern (= §1) では parent が `hyoui run --detached` を **`Command::spawn`**
で起こすため、起動 handshake は標準的な child process 起動と同じ仕組みで足りる:

- **既存 ready pipe (= 1 byte 通知)** で「socket bind 完了」を待つ (= 現行
  `daemonize::run_detached_parent` 実装そのまま)
- daemon child の **stderr を parent が inherit** (= `Command::stderr(Stdio::inherit())`
  もしくは pipe で capture)。daemon が起動失敗時に stderr に error 文字列を吐けば
  parent / ユーザに自然に伝わる
- ready pipe が EOF (= daemon が ready 通知前に死亡) → parent は spawn の
  exit status を `wait()` で取り、stderr から拾った error と合わせてユーザに表示
- **新 protocol message (= `DaemonStartupResult` 等) は不要** (= 既存 OS 機能で完結)

これは abduco / dtach 流の pipe + CLOEXEC pattern を **stderr で自然に実現** したもの
(= stderr 自体が pipe、CLOEXEC は標準動作)。

**子 exit code 伝搬 = tmux/abduco 流 `MSG_EXIT` + buffer drain 待機**:
- daemon は子 exit を観測しても **すぐに socket close しない**
- 残った PTY 出力 (= 子の最後の stdout/stderr) を全 client に flush
- buffer drain 完了 (= 全 client の queued_bytes が 0 になる、もしくは drain budget timeout)
- それから `session.exit.notify` を broadcast → daemon exit
- 既存 serve_loop の `DRAIN_BUDGET_PER_CLIENT` (= 200ms) と整合する

### 3. 廃止される実装

#### 3.1 Issue #1 修正の callback inject

- `DaemonConfig::on_suspend` / `on_resume` フィールド削除
- `crates/hyoui/src/daemon/session.rs::handle_suspend_signals` / `handle_child_transition::Follow` 内の callback 呼び出し削除
- `crates/hyoui-cli/src/main.rs::run_command` の `Arc<Mutex<Option<TtyGuard>>>` 共有削除
- daemon は外側 termios を一切知らない構造に戻す

#### 3.2 同プロセス 2 thread モデル

- `Session::start` + `daemon_handle = thread::spawn(|| session.serve())` の同プロセス起動 path 削除
- `hyoui run` は常に `daemonize::run_detached_parent` 相当の fork path を通る
- ただし fork 後の親は `--detached` の場合は exit、無しの場合は `hyoui attach <session>` 相当に進む

#### 3.3 既存 DR の調整

- **DR-0007 v0.1.0 scope**: 「daemon thread を main thread と同居」記述を廃止、本 DR で覆し
- **DR-0009 (session 分割)**: daemon **内部** module 構成の話で本変更と独立、影響なし
- **DR-0001 §実装ノート**: termios 復元の記述を「client process 側責務」に書き換え
- **CLAUDE.md**: 該当言及があれば更新

### 4. 構造の対応図

```
[BEFORE = 現状]
hyoui run -- cmd
  └─ process(pid=X)
       ├─ main thread (= CLI client)
       │    ├─ TtyGuard (= 外側 TTY raw mode)
       │    ├─ conn.run (= stdin/stdout 中継)
       │    └─ daemon_handle.join() で exit code 取得
       └─ daemon thread
            ├─ child PTY (= cmd 子 process)
            ├─ socket bind + listener
            ├─ SIGTSTP/SIGCONT/SIGCHLD handler (= self-pipe)
            └─ broadcast loop
       ↑ signal 共有が問題

[AFTER = 本 DR、exec attach pattern + 軸 2 廃止]
hyoui run -- cmd
  ├─ parent process: Command::spawn で daemon child 起動 → ready pipe 待ち
  │                  → exec("hyoui attach <session>") で自プロセス置換
  │
  ├─ process A (= exec 後の姿、`ps` で "hyoui attach <session>")
  │    ├─ TtyGuard (= 外側 TTY raw mode)
  │    ├─ SIGTSTP/SIGCONT handler (= 自プロセスの termios 管理のみ)
  │    │    └─ 外部 TSTP 受信 → TtyGuard.suspend() + raise(SIGSTOP) で自分だけ止まる
  │    │       (= daemon には何も送らない、§2.3 軸 2 廃止)
  │    ├─ socket connect (= 通常 attach、negotiate cap で on-child-suspend policy 渡す)
  │    ├─ conn.run (= stdin/stdout 中継)
  │    ├─ session.child.stopped.notify 受信 → 軸 1 follow/auto-resume 発動
  │    │    └─ follow なら TtyGuard.suspend() + raise(SIGSTOP)、復帰後
  │    │       session.child.resume.request を送って子も復帰
  │    └─ session.exit.notify 受信 → exit-status で親 (= ex parent) が exit
  │
  └─ process B (= daemon、`ps` で "hyoui run --detached --session=...")
       ├─ Command::spawn で起動、stderr inherit で起動 error は parent / ユーザ stderr へ
       ├─ ready pipe (= 1 byte) で socket bind 完了通知 → parent が exec attach に進む
       ├─ child PTY (= cmd 孫 process、line discipline で Ctrl-Z → 子 SIGTSTP)
       ├─ socket bind + listener
       ├─ SIGCHLD + waitpid(WUNTRACED|WCONTINUED) handler
       │    ├─ 子 stopped (= 100% self-stop 起因、§2.3 で軸 2 廃止)
       │    │    → leader cap check → notify or auto-resume fallback
       │    └─ 子 exit → session.exit.notify を全 client に (cap-aware broadcast)
       ├─ session.child.resume.request 受信 → killpg(child_pgid, SIGCONT)
       └─ broadcast loop (= daemon が能動的に子 pgrp を SIGSTOP する経路は本 DR では一切不存在)
```

`ps` で見える形:
- `hyoui run --detached --session=<id> -- cmd args...` (= daemon、process B)
- `hyoui attach <session>` (= ex-parent、process A = exec 後の姿)
- → 「run」コマンド名は parent が短命 (= spawn → ready 待ち → exec) のため
  通常 `ps` snapshot では見えない。daemon は常に "run --detached"、parent は
  常に "attach" として観測される

**Ctrl-Z 配信経路の整理**:
- 子 PTY 内で Ctrl-Z byte 入力 → PTY line discipline が SIGTSTP を子 pgrp に送る
  → 子 stopped → daemon の `waitpid(WUNTRACED)` が observe
  → leader (= process A) に `session.child.stopped.notify` 送信
  → leader が `on-child-suspend` policy 発動 (= follow / auto-resume)
- **外部 `kill -TSTP <process A>` の場合は process A だけが止まる** (= 軸 2 廃止、§2.3)。
  daemon には何も伝わらず、子も他 client も無影響

## Rejected alternatives

### (a) 現状維持 (= 1 プロセス 2 thread)

却下理由 (= 本 DR Context):
- Issue #1 / attach 派生 issue が 2 箇所に分散
- signal handling が process 共有で本質的に複雑
- 役割境界が混濁、debug 困難

### (b) fork 後の親プロセスを「特殊 client」として扱う (= protocol 拡張なし)

通常 attach + 既存 protocol だけで全て賄う案。

却下理由:
- 子 exit code を attach client に伝える正規 message が無い (= `tail.end::ChildExited` は tail subscriber 限定で全 attach に届かない)
- daemon startup error を親 client に詳細伝達する経路が無い
- 「とりあえず親も attach」では起動 race / error 経路が脆い

→ protocol 新 message + socketpair handshake は最小限の追加

### (c) `--detached` を default にしない (= flag 有無で path 分岐維持)

「短命 batch 用途で fork overhead を避けたい」観点。

却下理由 (= kawaz 確認 2026-05-28):
- 短命 batch 用途は想定外 (= 「そもそもそんな用途で使うものじゃない」)
- 分岐維持は role 混濁の温存
- 起動 overhead は数 ms、許容範囲

## Consequences

### 実装への波及

- `hyoui-cli/src/main.rs::run_command` の構造大改修 (= 現状 100+ 行のロジックを fork + attach の合成に書き換え)
- `hyoui-cli/src/daemonize.rs::run_detached_parent` を ready pipe → socketpair handshake に変更、ready 通知に startup metadata を含める形に拡張
- `hyoui/src/daemon/session.rs::serve_loop` に `session.exit.notify` broadcast を追加 (= 子 exit 観測直後)
- `hyoui/src/protocol/messages/` に `SessionExitNotify` 構造体追加
- `hyoui/src/protocol/messages/mod.rs::ControlMessage` enum に新 variant 追加
- `hyoui/src/daemon/config.rs` から `on_suspend` / `on_resume` field 削除
- `hyoui-cli/src/main.rs::TeeWriter` (= debug dump) は client process でそのまま再利用
- `hyoui/src/sys/tty.rs::TtyGuard` の `suspend()` / `resume()` API は client process で使う (= 維持)
- attach client (= run の親 / 単独 attach 両方) の SIGTSTP/SIGCONT handler を新規実装 (= sigaction + self-pipe、async-signal-safe 範囲)

### 廃棄するもの

- DaemonConfig.on_suspend / on_resume callback (= Issue #1 修正の経路、本 DR で不要に)
- Arc<Mutex<Option<TtyGuard>>> 共有経路
- `hyoui run` の同プロセス daemon thread spawn 経路
- 旧 ready pipe (1 byte) の単純化前提のコード
- 同プロセス前提の smoke / matrix test fixture (= 丸ごと再構築)

### 関連 DR 整理

| DR | 影響 |
|---|---|
| DR-0007 | v0.1.0 scope の「daemon thread を main thread と同居」を本 DR で覆し、annotate 追加 |
| DR-0009 | 影響なし (= daemon **内部** module 分割) |
| DR-0001 | §実装ノート の termios 取扱記述を「client process 側」に修正 |
| DR-0014 | self-check 通過 (= POSIX prefork model = kernel 標準機能、再発明ではない) |
| Issue #1 | 修正 commit 2751ff28 は本 DR 実装フェーズで削除予定 (= 不要に) |
| Issue #1 派生 (attach SIGTSTP) | 本 DR の「client SIGTSTP handler 実装」に統合・吸収 |

## 実装 Phase

### Phase A: protocol 拡張 + daemon 側

1. **新 message 追加** (= mod.rs + tail.rs に倣う form):
   - `SessionExitNotify` (= cap `session-exit-v1`)
   - `SessionChildStoppedNotify` (= cap `child-state-v1`)
   - `SessionChildResumeRequest` (= cap `child-state-v1`)
   - (= **新 protocol message は上記 3 個のみ**、起動 handshake は OS 標準機能で済む、§2.3.5)
2. MVP_CAPS に `session-exit-v1` / `child-state-v1` を追加
3. **cap-aware broadcast helper** 新設 (= `broadcast_control_with_cap`、negotiated_caps に
   含まない client は skip)
4. daemon serve_loop で:
   - 子 exit 観測時に `SessionExitNotify` を **cap-aware broadcast** で全 client へ
     (= buffer drain 完了後、§2.3.5 採用パターン)
   - 子 stopped 観測時 (= 100% 子 self-stop 起因、§2.3 で軸 2 廃止のため衝突なし):
     - leader が `child-state-v1` cap を持たない or leader 不在なら
       `killpg(child_pgid, SIGCONT)` で **auto-resume fallback**
     - それ以外は `SessionChildStoppedNotify` を leader へ送信
   - `SessionChildResumeRequest` 受信時に `killpg(child_pgid, SIGCONT)`
5. daemonize.rs は **現行 ready pipe (= 1 byte 通知) を維持**。stderr は inherit して
   起動 error 文字列を parent / ユーザに伝える (= §2.3.5、新 message 不要)

### Phase B: client process 側 (= exec attach pattern、§1)

1. `hyoui-cli/src/main.rs::run_command` を以下に書き換え (= wrapper 数十行):
   - `cfg.detached` 時: 既存 `daemonize::run_detached_parent` をそのまま呼ぶ (= 既存挙動)
   - 非 detached 時:
     1. session_id 採番 + socket path 解決
     2. `Command::new(current_exe()).args(["run", "--detached", "--session=...", ...])` で
        daemon child を spawn、stderr は inherit、ready pipe で wait
     3. ready 受信後、`exec("hyoui", ["attach", "<session>", "--debug-dump-client=..."])`
        で **自プロセスを attach バイナリに置換**
     4. exec 後は戻らない (= attach バイナリの `attach_command` が走る)
   - 既存の `Session::start` + `thread::spawn(...)` + `daemon_handle.join()` の同プロセス
     経路は **削除**
2. `attach_command` 側に追加実装:
   - sigaction install + self-pipe で **SIGTSTP / SIGCONT を自プロセスのために**監視
   - **client 自身の SIGTSTP 受信時** (= 旧軸 2 廃止後の挙動):
     - `TtyGuard.suspend()` → `raise(SIGSTOP)` → 復帰時 `TtyGuard.resume()`
     - daemon に対しては **何もリクエストしない** (= §2.3、子は無関係)
   - **子 self-stop 経路 = `session.child.stopped.notify` 受信時**:
     - `follow` policy: `TtyGuard.suspend()` → `raise(SIGSTOP)` → 復帰時 (= 外側 fg)
       `TtyGuard.resume()` → daemon に `session.child.resume.request` (= invariant 回復)
     - `auto-resume` policy: 即 daemon に `session.child.resume.request` を返す
       (= 子の self-stop を許さない)
   - `session.exit.notify` 受信時に `exit-status` をそのまま `process::exit(status)` で
     伝搬
   - cap negotiate payload に `on-child-suspend` policy を含めて daemon が leader 不在時の
     fallback (= auto-resume) を判定できるようにする
3. `--on-child-suspend` flag は attach にも追加 (= run の wrapper が引き渡す)。
   `--on-parent-suspend` は削除 (= §2.3 で軸 2 廃止)

### Phase C: 廃棄物清掃

1. DaemonConfig.on_suspend / on_resume 削除
2. session.rs の callback 呼び出し削除
3. run_command の Arc<Mutex> 共有削除
4. 同プロセス daemon thread spawn 経路削除

### Phase D: test 全面改修

1. smoke_pty.rs / matrix_test.rs を fork ベースに改修 or 丸ごと再構築
2. 既存 callback inject 系 test 削除
3. 新 protocol message の round-trip test 追加
4. CI green 回復 (= 既存 Linux flaky 含めて再確認)

## 関連

- DR-0005 — 思想 (= 透明性最優先、本 DR は role 分離で透明性を強化)
- DR-0001 — jobcontrol 2 軸 (= client process 側 SIGTSTP/SIGCONT 実装の根拠)
- DR-0007 — MVP scope (= 本 DR で部分覆し)
- DR-0008 — protocol design (= 本 DR で新 message + socketpair handshake 追加)
- DR-0009 — session 分割 (= 影響なし)
- DR-0014 — 透過原則 + 検証主義 (= 本 DR の self-check 通過)
- Issue #1 (termios 復元) — 本 DR 実装フェーズで修正経路を変更
- Issue #1 派生 (attach SIGTSTP) — 本 DR に統合・吸収
