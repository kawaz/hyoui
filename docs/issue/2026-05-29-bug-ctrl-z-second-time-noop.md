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

---

## 調査結果 2026-06-10 (= 「2 度目以降」以前に「子が一度も止まらない」が確定)

> 検証者: Claude subagent (= 実機マトリクス検証)。バイナリ v0.2.6、Rust 未変更。

### 結論: 本 issue の前提「1 度目の Ctrl-Z で子が suspend した」が **実は成立して
いない**。真因は姉妹 issue の orphan group discard

実機検証で、`hyoui run -- sleep` の **子 (sleep) は 1 度目の内側 Ctrl-Z でも止まらない**
ことが確定した (= 子は常に `Ss+`、外部 `kill -STOP` でのみ `Ts+`)。よって仮説 A/B/C
(= ChildLifecycle の transition 取りこぼし / attach loop の state / 子 PTY termios 変化)
は **いずれも本 issue の主因ではない**。

→ 観測マトリクス・確定した因果・修正方針は
**`2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md` の「調査結果 2026-06-10」に集約**。
本 issue (= sleep の 2 度目) はその orphan discard 問題の一症状。

### 仮説の棄却根拠 (= 実機 + コード)

- **仮説 A (= daemon notify が 1 回限り / WCONTINUED 取りこぼし)**: ✗ 棄却。
  `pty.rs::ChildLifecycle::poll_with_transition` は `WUNTRACED|WCONTINUED` で
  Stopped/Continued を正しく state 追跡しており (= `stopped` flag を Continued で
  false に戻す)、`session.rs` serve_loop の複数箇所が transition 毎に
  `notify_child_stopped` を呼ぶ。コード上 2 度目 Stopped は再 notify される設計。
  そもそも 1 度目で子が STOPPED しない (= notify 自体が発火しない) ので、この経路は
  本症状に到達しない。
- **仮説 C (= 子 PTY termios が 1 度目で変化)**: ✗ 棄却。子 PTY termios は
  `ISIG=True ICANON=True VSUSP=0x1a` で安定 (= Ctrl-Z 前後で不変)。子は常に
  cooked。SIGTSTP は生成されるが orphan group で discard される。
- **仮説 B (= attach loop の socket 受信 state)**: △ 主因ではない。1 度目で notify が
  来ないので attach の受信 state を疑う前段で詰む。

### 「kawaz の sleep 報告では 1 度目 suspended に見えた」件 = 残る仮説 (= 未確定)

kawaz の実機 (= zsh から `hyoui run -- sleep`、`zsh: suspended` 表示) と、本検証
(= Python pty で attach を session leader 化、`zsh: suspended` 相当が出ない) で
**1 度目の見かけが食い違う**。本検証では外側 shell (bash --norc -i, `set -m`) から
`hyoui run -- sleep` を起動し外側 tty に 0x1a を送っても、attach は `S+` のまま
止まらず `^Z` が echo されるだけだった (= 1 度目も suspended しない再現)。

- 観測事実: 本検証環境では「1 度目から止まらない」(= attach は外側 tty を raw 化
  済で ISIG off、0x1a は byte として子へ → orphan discard)
- 残る仮説 (= 未検証): kawaz の zsh では (a) `hyoui run` 親が `exec attach` に変身する
  **前**の一瞬 (= 外側 tty まだ raw 化前) に Ctrl-Z が当たると、外側 line discipline が
  SIGTSTP に変換し run 親が STOPPED → `zsh: suspended` 表示、もしくは (b) zsh と bash の
  job control / raw mode タイミング差。**この 1 度目の差は orphan discard の本筋とは
  別レイヤ**で、修正 (= 姉妹 issue 案 A4) が入れば「1 度目から確実に子も止まる」ため
  食い違い自体が解消する見込み

### 次の検証手順 (= 1 度目の食い違いを詰めるなら)

1. kawaz 正規 path (zsh) で `hyoui run -- sleep` → Ctrl-Z 直後に
   `ps -o pid,ppid,stat -p <attach>,<sleep>` を即取得 (= attach が `T` か、sleep が
   `Ss` のままか)。attach が `T` で sleep が `Ss` なら「attach だけ止まる」= 上記
   仮説 (a)/(b) が裏付く
2. `--debug-dump-client` / `--debug-dump-server` を付けて 1 度目の byte 経路を比較
3. ただし優先度は低 (= 根本 fix で吸収される)。主対応は姉妹 issue 案 A4
