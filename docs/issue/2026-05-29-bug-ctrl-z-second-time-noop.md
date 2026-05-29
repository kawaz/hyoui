# bug: hyoui run の 2 度目以降の Ctrl-Z が効かない

- Date: 2026-05-29
- Priority: **高** (= 軸 1 follow の続発が壊れてる、Ctrl-Z で bg → fg → 再度 Ctrl-Z できないと運用上の主シナリオが破綻)
- Status: 未着手
- 報告者: kawaz 実機検証 2026-05-29

## 現象

```bash
$ ./target/release/hyoui run --session=test2 -- /bin/sleep 6000
^Z
zsh: suspended  ./target/release/hyoui run --session=test2 -- /bin/sleep 6000

$ fg
[1]  + continued  ./target/release/hyoui run --session=test2 -- /bin/sleep 6000
^Z^Z^Z^Z^Z^Z^C%   # ← 2 度目以降の ^Z 全部効かない、^C で抜けるしかない
```

1 度目の Ctrl-Z で suspended (OK)。fg で resumed (OK)。2 度目の Ctrl-Z が無反応。

## 経路解析 (= 推定)

`hyoui run` 親 process が `exec hyoui attach <session>` に置換され、attach が自プロセスの
stdin を raw mode 化 (ISIG OFF)。よって外側 TTY 経由の Ctrl-Z は line discipline で
SIGTSTP に変換されない。Ctrl-Z byte (0x1a) は raw_data frame として子 PTY に届く。

正常 path (= 1 度目):

1. attach stdin → daemon → 子 PTY の cooked mode line discipline → 子 (sleep) に SIGTSTP
2. 子 STOPPED
3. daemon の `waitpid(WUNTRACED)` が observe (= session.rs::notify_child_stopped)
4. `SessionChildStoppedNotify` を leader (= attach) に送信 (broadcast.rs::send_control)
5. attach の `ClientConnection::run` loop が message を受信 → `raise(SIGSTOP)` (= attach.rs:359)
6. attach process STOPPED → zsh が "suspended" 表示

fg 経路:

7. attach process が SIGCONT で復帰 → run loop に戻る
8. loop が次の処理で `SessionChildResumeRequest` を daemon に送信 (attach.rs:365-371)
9. daemon の `handle_session_child_resume_request` が `killpg(child_pgid, SIGCONT)` (control.rs)
10. 子 sleep が SIGCONT で復帰

2 度目 Ctrl-Z (= 想定):

11. attach stdin → daemon → 子 PTY → 子 (sleep) に SIGTSTP (= 1 度目と同じ経路)
12. 子 STOPPED
13. daemon の waitpid が observe
14. `SessionChildStoppedNotify` を再送信
15. attach が再度 `raise(SIGSTOP)`

**11-15 のどこかで詰まっている**。

## 仮説候補

### 仮説 A: daemon 側 notify が「1 回限り」になっている

- `notify_child_stopped` は state machine 持たず純粋に呼ばれた都度 send するはず
- ただし `lifecycle.poll_with_transition` が「Stopped→Continued→Stopped」の遷移を
  正しく観測してるか? 仮に Continued を見落とすと 2 度目 Stopped を再 Stopped と
  認識しない可能性
- 要確認: `ChildLifecycle` の state 追跡 + WCONTINUED 検出

### 仮説 B: attach の run loop が socket frame を見ていない

- 1 度目 `raise(SIGSTOP)` → SIGCONT 復帰後、resume.request 送信
- その後 loop の poll が socket revents を見て次の frame を受信できているか?
- `raise(SIGSTOP)` 後の reader state が壊れてる可能性

### 仮説 C: 子 PTY 内 cooked mode の state が変わっている

- 1 度目 Ctrl-Z で子 PTY line discipline の state が変化、2 度目以降 Ctrl-Z を
  通常 byte として扱っている?
- `stty -a` で子 PTY の termios 確認すれば分かる

## 再現手順

```bash
# 1. hyoui を build (最新 main)
cargo build --release --workspace

# 2. 別 terminal で sleep 起動
./target/release/hyoui run --session=test2 -- /bin/sleep 6000

# 3. Ctrl-Z → fg → Ctrl-Z (= 2 度目) で再現
```

## 調査手順 (= 仮説検証)

### 仮説 A 検証: daemon 観測

```bash
# 1 度目 Ctrl-Z 後、daemon が waitpid で transition 取れてるか
# strace / dtruss / 内部 log で確認 (= tracing 入ってないので難しい、
# tracing-subscriber 導入が必要 = DR-0011 Phase A)

# 暫定: --debug-dump-server で raw bytes が 2 度目 Ctrl-Z 後に流れるか観測
./target/release/hyoui run --session=test2 \
    --debug-dump-server=/tmp/test2-server.bin \
    -- /bin/sleep 6000
# Ctrl-Z → fg → Ctrl-Z 後に dump 確認
od -c /tmp/test2-server.bin | tail -20
```

### 仮説 B 検証: attach loop の state

- `--debug-dump-client` で client 受信 bytes を観測
- 1 度目 → 2 度目で何 frame 受信したか比較

### 仮説 C 検証: 子 PTY termios

```bash
# 1 度目 Ctrl-Z 直前 + 2 度目 Ctrl-Z 直前で子 PTY tty を観測
ls -la /dev/ttys*  # 子 PTY 特定
stty -a < /dev/ttys<N>
```

## 修正方針 (= 仮説 A の場合)

`ChildLifecycle` の transition 追跡を `Stopped` → `Continued` → `Stopped` で
正しく state 復帰させる。`WCONTINUED` flag を使って Continued transition を
取得 → `lifecycle.stopped = false` にリセット → 次の Stopped を新規として扱う。

## 関連 file

- `crates/hyoui/src/daemon/session.rs::notify_child_stopped` (= line 705 周辺)
- `crates/hyoui/src/daemon/session.rs::serve_loop` (= waitpid 経路)
- `crates/hyoui/src/daemon/pty.rs::ChildLifecycle` (= state 追跡)
- `crates/hyoui/src/client/attach.rs::run` (= SessionChildStoppedNotify 受信 + raise(SIGSTOP))
- DR-0015 §2.2 / DR-0001 軸 1 follow
