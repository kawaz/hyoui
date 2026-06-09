# bug: hyoui run <TUI> の Ctrl-Z が 1 度目すら効かない (= 全 TUI app 共通、orphan group discard 仮説)

- Date: 2026-05-29
- Priority: **最高** (= 主用途 = claude/vim/less 等 TUI を hyoui で運用、これが効かないと DR-0002 命名議論の核 (= 「Ctrl-Z x2 で TUI を bg にして外側 shell で作業」) が **全 TUI app で** 成立しない)
- Status: 未着手
- 報告者: kawaz 実機検証 2026-05-29
- 関連: [2026-05-29-bug-ctrl-z-second-time-noop.md](./2026-05-29-bug-ctrl-z-second-time-noop.md) (= sleep の続発問題、本 issue は **TUI app 全般で** 1 度目すら効かない)

## 更新 2026-05-29: 全 TUI app で同症状確認

検証範囲: claude / vim / less 全て同症状。「claude 限定の独自 Ctrl-Z 処理」ではなく
**hyoui の構造問題が確定** (= TUI app 一般、raw mode + SIGTSTP handler 経路全て)。
orphan group discard 仮説の信憑性が大幅 up。

## 現象

### claude 直接起動 (= zsh から)

```bash
$ claude
^Z
zsh: suspended  claude
$ fg
$ ^Z   # 何度でも OK
```

### hyoui run claude

```bash
$ ./target/release/hyoui run --session=test1 -- claude
# claude 起動後 Ctrl-Z を押しても:
^Z^Z^Z^Z^Z^Z   # 無反応
```

ps で確認すると attach process は **ttys004 で running** (= STOPPED じゃない):

```
501 59850 59849   0  9:14午前 ??         0:00.00 ... hyoui __daemonize-run ... -- claude
501 59849 21564   0  9:14午前 ttys004    0:00.01 ... hyoui attach test1
```

## 推定原因 (= orphan group discard 仮説)

claude TUI の Ctrl-Z 処理 (= 推定):
1. 子 PTY slave を raw mode 化 (= cfmakeraw、ISIG OFF) → Ctrl-Z byte 0x1a として read
2. 自前の Ctrl-Z handler を install して「Claude Code has been suspended」message 表示
3. handler 内で `install_default(SIGTSTP) + raise(SIGTSTP)` で kernel default = STOP を発火

zsh 直接の場合: claude は zsh と同 session、parent process group (= zsh) が同 session 内
→ orphan group ではない → SIGTSTP の SIG_DFL = STOP normal 動作。

hyoui 経由の場合: DR-0003 の `forkpty + login_tty + POSIX_SPAWN_SETSID` で子 (= claude) は
**独立 session leader** に → 子 process group の parent (= daemon) は別 session
→ **orphan process group** と判定される
→ POSIX 仕様:

> SIGTSTP / SIGTTIN / SIGTTOU shall be discarded if the process group is orphaned and the signal action is SIG_DFL.

→ kernel が SIGTSTP を discard、claude は handler で message 表示するが process は running のまま。

## 検証手順

### 1. 仮説確認: 外部 SIGSTOP で確実に止める

```bash
# hyoui run claude を起動した状態で、別 terminal から:
CLAUDE_PID=$(ps -ef | grep -v grep | grep -E 'hyoui run.*claude$|hyoui.*-- claude$' \
  | awk '{print $2}' | sort -u | head -1)
# 上記で取れない場合は: pgrep -P <daemon-pid>
kill -STOP $CLAUDE_PID

# 期待: attach process も follow policy で STOPPED に入る (= 軸 1 follow 経路は正常)
ps -o pid,stat,comm -p $CLAUDE_PID
ATTACH_PID=$(pgrep -f 'hyoui attach test1' | head -1)
ps -o pid,stat,comm -p $ATTACH_PID
```

- attach が `T+` なら: 軸 1 follow 経路は正常 → claude の raise(SIGTSTP) が orphan discard で消えてるが確定
- attach が running のままなら: 別の問題

### 2. 仮説確認: claude の process group / session

```bash
# claude pid + その pgid + sid を確認
ps -o pid,ppid,pgid,sess,stat,comm -p $CLAUDE_PID
# claude が独立 session leader なら sid == pid
# parent (daemon) の sid と比較
ps -o pid,ppid,pgid,sess,stat,comm -p $(pgrep -f __daemonize-run | head -1)
```

両者の sid が違う = claude は別 session → orphan group 仮説と整合。

## 修正方針 (= 仮説 A: orphan discard 回避)

### 案 A1: daemon が attach を持ってる時は子の session を attach の session に合わせる

実装難易度高。POSIX_SPAWN_SETSID を取らず、attach 経由で setsid する経路が必要。
そもそも子 PTY を持つには子が session leader である必要 (= DR-0003 の理由)。

→ **不可** (= 子 PTY 制御端末獲得と orphan 回避は両立しない)。

### 案 A2: claude (= 子) の SIGTSTP handler を hyoui 側から override する経路

不可能 (= claude process の handler install は claude プロセス内で完結)。

### 案 A3: TTY line discipline 経由で SIGTSTP 配信 (= 既存 DR-0001 §1 の経路)

DR-0001 §1 で「内側は既に正しい」と書いてるのは、**外側 shell の cooked mode line
discipline が Ctrl-Z を SIGTSTP に変換して claude に送る** 想定。実際:

- 外側 (= attach の stdin) = raw mode、ISIG OFF
- 内側 (= 子 PTY slave) = cooked か raw かは子 (= claude) の設定次第

claude が子 PTY slave を raw 化してるなら、子 PTY line discipline 経由でも SIGTSTP は
配信されない。byte 0x1a として読まれる。

→ 子 PTY line discipline 経路は **claude が raw mode 化してる時点で機能しない**。

### 案 A4: attach 側で Ctrl-Z byte を捕まえて daemon に「子も止めて」要求 (= 軸 2 を復活)

DR-0015 §2.3 で軸 2 廃止したが、claude TUI 等「raw mode + Ctrl-Z 自前処理」の app の
ために **attach client が Ctrl-Z byte (0x1a) を detect → daemon に
SessionChildSuspendRequest 送信 → daemon が `killpg(child, SIGSTOP)`** の経路を入れる。

- detach prefix (= Ctrl-A D) と同じレイヤで Ctrl-Z byte を attach client が intercept
- detach key 同様 env で disable 可能に
- raise(SIGSTOP) は catch 不可 → orphan group でも確実に STOP

#### 課題

- 「Ctrl-Z byte を attach が intercept」=「Ctrl-Z を子 (= claude) に送らない」= claude
  自身の Ctrl-Z handler は走らない → 「suspended」message 出ない
- ただし実際に止まるので機能的には OK
- ユーザ体験: zsh の "suspended" 表示は出る (= attach process が STOPPED に入るため)

### 案 A5: claude code 上流に「raise(SIGTSTP) ではなく raise(SIGSTOP)」要求

claude code 開発元に「orphan group safety のため SIGSTOP も対応してね」と issue 立てる。
ただし orphan group は POSIX 標準で multiplexer 全部が引っかかる。

## 推奨

**案 A4** (= attach 側 Ctrl-Z intercept) が最も実用的。tmux / screen の Ctrl-Z 動作
(= prefix key 経由で attach detach する) と類似の経路。ただし「Ctrl-Z で全部止める」
ユーザ体験は維持できる (= zsh が "suspended" 表示)。

DR-0015 §2.3 で廃止した「軸 2 transparent」を **client 起動の局所機能として復活**:
- 旧軸 2: daemon thread が SIGTSTP 受けて子に SIGSTOP forward
- 新案 A4: attach client が **Ctrl-Z byte を stdin から detect**、daemon に message
  送って子 pgrp に SIGSTOP。client 自身も raise(SIGSTOP)

protocol: 既存 `signal` message を「pgrp 送信」に変える (= codex 過去指摘 #2 の方向)、
もしくは新 `session.child.suspend` message を追加。

## 関連 file

- `crates/hyoui-cli/src/main.rs::install_attach_signal_thread` (= attach SIGTSTP handler、
  現状は process 自身の SIGTSTP を捕まえてる)
- `crates/hyoui/src/client/attach.rs::run` (= stdin → daemon の raw_data 経路、
  ここで Ctrl-Z byte intercept 経路を入れる)
- DR-0015 §2.3 軸 2 廃止判断 (= 本 issue で部分 revisit する可能性)
- DR-0001 §1 「内側 (= 子) は既に正しい」(= claude のような raw mode TUI では成立しない)
- DR-0002 命名議論の核 (= 「Ctrl-Z x2 で claude を bg」)

---

## 調査結果 2026-06-10 (= 実機検証で orphan group discard 仮説を確定)

> 検証者: Claude subagent (= 実機マトリクス検証)。バイナリ `target/release/hyoui`
> v0.2.6。Rust コードは未変更 (= 調査のみ)。検証 harness は Python `pty.fork()` で
> 実 PTY を作り、`hyoui run --session=ID -- CMD` を attach client 化、master fd に
> Ctrl-Z byte (0x1a) を注入して子 stat / termios / foreground pgrp / session を観測。

### 結論: orphan process group discard が **真の根本原因** (= 確定)

issue の「推定原因 (= orphan group discard 仮説)」は **実機で確定した**。
案 A1〜A3 が不可で、**案 A4 (= attach client が Ctrl-Z byte を intercept → daemon
経由で子 pgrp に SIGSTOP) が唯一の実用解** という issue の推奨も裏取りできた。

### 観測事実マトリクス (= 5 カテゴリ、winsize 80x24 設定済)

| app | カテゴリ | 子 session | 内側 Ctrl-Z byte x2 | 外部 `kill -TSTP` → 子pgrp | 外部 `kill -STOP` → 子pgrp |
|---|---|---|---|---|---|
| cat | line-oriented | 子=leader (≠親) | Ss+ (止まらず) | **Ss+ (discard)** | **Ts+ (止まる)** |
| less /etc/hosts | pager | 子=leader | Ss+ | **Ss+ (discard)** | **Ts+** |
| vim | TUI alt-screen | 子=leader | Ss+ | **Ss+ (discard)** | **Ts+** |
| python3 -u | REPL | 子=leader | Ss+ | **Ss+ (discard)** | **Ts+** |
| bash --norc -i | shell | 子=leader | Ss+ | **Ss+ (discard)** | **Ts+** |

全カテゴリで完全に一致 (= app 固有の独自 Ctrl-Z 処理は無関係、hyoui の構造問題で確定)。

### 確定した因果 (= 推測ではなく観測 + コード裏取り)

1. **子 PTY の line discipline は正常**: `ps -o tty` で子 tty を特定 → 別プロセスから
   `O_NOCTTY` で open して `tcgetattr` で観測。`ISIG=True ICANON=True ECHO=True`、
   `VSUSP=0x1a (26)`。**Ctrl-Z → SIGTSTP の変換設定自体は正しい**。
   (= `forkpty(&ws, None)` で termios 未指定 = kernel default cooked、`raw.rs:102`)
2. **子は独立 session leader**: 全 app で `pgid==pid==sid`、親 (= daemon) は別 session。
   daemon は `daemonize.rs:358` で `setsid()` 済 (= controlling tty 切り離し)、子は
   `forkpty + login_tty` で別 session leader。
   → POSIX orphaned process group 定義 (= 「グループの全メンバーの親が、グループ
   メンバーでもグループの session メンバーでもない」) を **両条件とも満たす**。
3. **orphan group の SIGTSTP は kernel が discard**: 外部から `kill -TSTP` を子 pgrp に
   直接送っても子は `Ss+` のまま (= 止まらない)。`kill -STOP` (= discard 不可) なら
   `Ts+` に止まる。両者の差が orphan discard を直接証明。
   POSIX 規範 (= 流し読み回避、正確に):
   > If the process group is orphaned and the action of SIGTSTP, SIGTTIN, or
   > SIGTTOU is SIG_DFL, the signal shall be discarded.

   line discipline が VSUSP で生成する SIGTSTP の action は子側 default (SIG_DFL)、
   かつ子グループは orphan → **生成された SIGTSTP は kernel が握り潰す**。

### 棄却した仮説と根拠

- **「子 PTY が raw mode 化されてて 0x1a を byte 消費」(= issue 推定 step 1)**:
  ✗ 棄却。実測で子 PTY は cooked (ICANON=True, ISIG=True, VSUSP=0x1a)。
  raw 化されていない。0x1a は SIGTSTP に変換される (= が orphan で discard)。
- **「app (claude) 固有の独自 Ctrl-Z 処理が原因」(= issue 1 の当初疑い)**:
  ✗ 棄却。cat/less/python/bash の SIG_DFL app でも同症状。app 非依存。
- **「signal 配送経路 (daemon→子) 自体が壊れている」**:
  ✗ 棄却。`kill -STOP` は確実に届いて子が `Ts+`。配送は健全、discard は SIGTSTP
  限定の orphan 仕様。

### 修正方針案 (= issue 推奨 A4 を実機で裏付け)

**案 A4 (= attach client が Ctrl-Z byte 0x1a を intercept → daemon に suspend 要求 →
daemon が `killpg(child_pgid, SIGSTOP)`)** が実機裏付けで唯一実用的:

- SIGSTOP は orphan group でも discard されず確実に止まる (= 実機で `Ts+` 確認済)
- 既存に `SessionChildResumeRequest` → `killpg(child, SIGCONT)` の **resume 経路は
  存在する** (`control.rs:274 handle_session_child_resume_request`、`session.rs` の
  `kill_pgrp(child, SIGCONT)`)。対称な suspend 経路 (= `SessionChildSuspendRequest`
  → `killpg(child, SIGSTOP)`) を追加するだけで済む
- DR-0015 §2.3 が「daemon が能動的に子を SIGSTOP する経路は本 DR では一切存在しない」と
  明記して廃止した経路を、**子 self-stop ではなく client intercept 起点で復活**させる形

### 該当コード箇所 (= file:line)

- `crates/hyoui/src/sys/raw.rs:102` — `forkpty(&ws, None)` で子 PTY 起動 (= 子が
  独立 session leader になる根本)。termios は default cooked
- `crates/hyoui-cli/src/daemonize.rs:358` — daemon `setsid()` (= 子グループを orphan に
  する要因。daemon と子が別 session)
- `crates/hyoui/src/client/attach.rs:run` (= 約 387 行目以降の stdin→socket raw_data
  経路) — ここで Ctrl-Z byte (0x1a) を detect して suspend 要求に変える intercept を
  入れる (= detach prefix `process_detach_prefix` と同じレイヤ)
- `crates/hyoui/src/daemon/control.rs:274` — 既存 `SessionChildResumeRequest` handler。
  対称に `SessionChildSuspendRequest` handler を追加する箇所
- `crates/hyoui/src/protocol/messages/session_lifecycle.rs` — `SessionChildResumeRequest`
  の隣に `SessionChildSuspendRequest` 構造体 + cap flag を追加

### 修正規模見積り (= 中規模)

- 新 protocol message `SessionChildSuspendRequest` 1 個 + cap flag (= 既存
  `child-state-v1` に相乗りも可) + round-trip test
- `ControlMessage` enum variant 1 個追加
- attach.rs の stdin 経路に Ctrl-Z intercept state machine (= detach prefix と同様、
  env で disable 可能に。byte 0x1a 単発を見て suspend frame 送信)
- daemon control.rs に suspend handler (= `killpg(child_pgid, SIGSTOP)`、resume の
  対称形なので数十行)
- attach client 自身も `raise(SIGSTOP)` して外側 shell に "suspended" を見せる
  (= ユーザ体験維持。これは既存 follow policy の `raise(SIGSTOP)` 経路を流用)
- → **新規実装は概ね resume 経路のミラー**。protocol + attach intercept + daemon
  handler の 3 箇所、テスト込みで中規模

### 注意 (= 検証中に観測した別バグ、本 issue とは独立)

winsize 未設定 (= grid 0) の PTY で `hyoui run` を起動して子に出力させると
**vt100-0.16.2 `screen.rs:827` で `drawing_cell(pos).unwrap()` が None で panic** し
daemon が crash する (= 子も巻き込んで死ぬ)。winsize を 80x24 に設定すると再現しない。
kawaz の実機 (= 正常な端末サイズ) では本 panic は通常踏まないが、別 issue として
起票推奨 (= grid 0 / 極小サイズ耐性、vt100 への upstream 報告 or 自前ガード)。
Ctrl-Z の orphan discard とは無関係 (= 切り分け済)。

## 追加調査 2026-06-10 (= main session + kawaz 実機): 層 2 の発見と修正方針の更新

> 詳細マトリクス・PoC コードは
> [findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md](../findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md)
> を正本とする。本節は issue としての結論差分のみ。

### 原因は二層構造 (= 上記 orphan discard だけでは説明が閉じない)

- **層 1**: orphan pgrp の SIGTSTP discard (= 上記調査の通り)
- **層 2**: **daemon の auto-resume が、止まった子を即 SIGCONT で起こす**。
  `kill -STOP` (= discard 不可) ですら子が止まったままにならないことを実機で確認
  (daemon を先に SIGSTOP で止めた場合のみ子が Ts+ を維持 → daemon 再開で子も CONT
  される)。該当: `session.rs` の DR-0001 軸 1 実装、leader 不在時 fallback の無条件
  `killpg(child, SIGCONT)`。**案 A4 を実装しても層 2 と衝突する** (= detached 時の
  suspend が即解除される) ため、auto-resume policy の見直しが必須。

### 修正方針の更新 (= A4 は fallback に降格、session anchor 案を本命に)

上記「案 A4 が唯一の実用解」は **層 2 未発見時点の結論**であり、以下で更新する:

1. **本命: session anchor 案** (= forkpty 廃止、daemon が `openpty` + `TIOCSCTTY` で
   controlling tty を持ち、child を同 session・別 pgrp・foreground で fork)。
   macOS / Linux glibc / Linux musl の 3 platform PoC で `kill -TSTP` /
   line discipline ^Z の両経路とも **SIGTSTP 本来のセマンティクス** (catch 可能 =
   TUI の suspend cleanup が走る) で動作確認済み。
   A4 の弱点 (= SIGSTOP は catch 不可なので vim/claude の alt-screen 解除・termios
   復元が走らず raw のまま凍結) を持たない。プロセス構造も zsh 直接起動と同型に
   なり透過性が向上する。残課題: DR-0003/0005/0001 改訂
2. **fallback: 案 A4** (= session anchor 案に未知の障害が出た場合)
3. supervisor 分離案は不採用方向 (= supervisor kill -9 で foreground pgrp に SIGHUP
   → child 巻き添え死の新故障モード)
4. tmux も同一制限を持つ (= 実証済)。本修正が入れば **「直接起動した子の ^Z が効く
   PTY ラッパー」は hyoui の差別化点になる**
