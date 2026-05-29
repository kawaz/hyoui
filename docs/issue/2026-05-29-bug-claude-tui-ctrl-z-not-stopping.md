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
