# bug: termios 復元漏れによる cmux freeze (suspend 時)

- Date: 2026-05-28
- Priority: **最重要** (= 実機で完全 freeze 誘発、kawaz がアクティビティモニタ強制終了)
- Status: **修正済 (hyoui run 非 detached 経路、commit 2751ff28)** / hyoui attach 経路は別 issue
- 派生 issue: [2026-05-28-bug-attach-termios-restore-on-suspend.md](./2026-05-28-bug-attach-termios-restore-on-suspend.md) (= attach プロセスは別 process なので別途修正必要)

## 現象

実機 `hyoui run -- claude` を **cmux (= manaflow-ai/cmux + libghostty)** 内で起動した状態で:

1. claude TUI で Ctrl-Z 押下 (= 直接押下経路)、または
2. kawaz が外部から `kill -TSTP <hyoui pid>` 実験 (= 外部 signal 経路)

→ hyoui (stat `T`) + claude (stat `Ts+`) が STOPPED (= 軸 2 transparent は動作確認 ✓)

→ `kill -CONT <hyoui pid>` で復帰試行

→ **cmux (= manaflow-ai/cmux + libghostty) 全体が freeze**
→ kawaz がアクティビティモニタで強制終了するしかなくなる

## 真因仮説 (= 確証高い)

hyoui の SIGTSTP handler に **termios 戻し処理が一切ない**:

- `crates/hyoui/src/daemon/session.rs` の `handle_suspend_signals` (line 603 周辺) で `raise(SIGSTOP)` を呼ぶだけ
- grep でこの経路の termios / tcsetattr 呼び出しゼロヒット
- `crates/hyoui/src/sys/tty.rs::TtyGuard::drop` (= line 63) は **serve_loop 抜け時のみ** termios 復元
- → suspend 中は外側 TTY が raw mode のまま放置

結果として:
- 外側 TTY = raw mode のまま STOPPED 状態
- cmux/libghostty が raw bytes (= line discipline OFF の入力) を受信待ち
- TTY の line discipline state と libghostty の期待が食い違って **無限待ち** → cmux freeze

## 修正方針

SIGTSTP handler 経路に termios 復元/再設定を組み込む:

```
SIGTSTP handler 経路:
  1. termios を「raw 前の状態」(= TtyGuard.saved) に tcsetattr で復元
  2. raise(SIGSTOP)
  ↑ kernel STOPPED
  3. (SIGCONT 復帰時) raw mode 再設定 (= cfmakeraw + IUTF8)
```

実装の助け:
- `TtyGuard::suspend()` / `TtyGuard::resume()` の helper を新設
- `handle_suspend_signals` から呼ぶ
- `tty.rs::TtyGuard` の `saved` field は既に存在 (= drop で使ってる経路を流用可能)

## 関連 file / line

- `crates/hyoui/src/daemon/session.rs` line 587-680 (= handle_suspend_signals)
- `crates/hyoui/src/sys/tty.rs` line 36-66 (= TtyGuard)

## 関連 DR

- DR-0001 §実装ノート — termios 言及なし (= 設計時の漏れ、本 issue 修正後に追記)
- DR-0014 §self-check — 「kernel / PTY / shell の標準機能を再発明していないか?」項に「termios state も観測したか?」を追加すべき (= Issue 5 で議論)

## 修正内容 (= 2026-05-28 commit 2751ff28)

`hyoui run` 非 detached 経路のみ対応 (= 1 プロセス内に main thread + daemon thread):

1. `TtyGuard::suspend()` / `resume()` API 追加 (= `sys/tty.rs`)
2. `DaemonConfig::on_suspend` / `on_resume` callback 追加 (= `daemon/config.rs`)
3. `session.rs::handle_suspend_signals` + `handle_child_transition::Follow` で
   `raise(SIGSTOP)` の前後 callback 呼出
4. `main.rs::run_command` で `Arc<Mutex<Option<TtyGuard>>>` 経由で callback inject

詳細: commit 2751ff28、DR-0001 + DR-0014 self-check 通過済。

`hyoui attach` 経路は別 issue (= 2026-05-28-bug-attach-termios-restore-on-suspend.md)。

## 検証マトリクス + 検証手順

DR-0014 §検証主義に従い、最低 3 種類 category (= 素 / cmux / tmux) で実機検証。
**`--debug-dump-server` + `--debug-dump-client` で raw bytes も並行取得** (= post-mortem
+ 修正効果の bytes 単位の証拠固め)。

### 修正前ベースライン (= 修正前のバイナリで実施、修正後と差分比較用)

```bash
# 修正前バイナリは v0.2.0 release が brew tap で取得可能 (= 修正は v0.2.x で publish 予定)
brew install kawaz/tap/hyoui  # v0.2.0 = 修正前
# 端末 A (= test 対象 env)
hyoui run --session=bugtest --debug-dump-server=/tmp/v020-server.bin --debug-dump-client=/tmp/v020-client.bin -- claude
```

### 修正後検証 (= 本リポの cargo build --release 経由)

```bash
cargo build --release --workspace
# 端末 A (= test 対象 env)
./target/release/hyoui run --session=bugtest \
    --debug-dump-server=/tmp/fix-server.bin \
    --debug-dump-client=/tmp/fix-client.bin \
    -- claude
# claude TUI が起動したら Ctrl-Z (= 直接押下経路)
# 端末 B (別 terminal)
HYOUI_PID=$(pgrep -f 'hyoui run.*bugtest' | head -1)
CLAUDE_PID=$(pgrep -f 'bugtest' | grep -v "$HYOUI_PID" | tail -1)
ps -o pid,ppid,pgid,sid,stat,comm -p "$HYOUI_PID,$CLAUDE_PID"
# 期待: hyoui = T (stopped) + claude = T
stty -a < /dev/tty   # 端末 A の termios を観察 (= 復元されているか)
# 復帰 (端末 A で fg、もしくは別端末から):
kill -CONT "$HYOUI_PID"
# 期待: 端末 A の cmux/libghostty が freeze しない、claude TUI が再描画される
```

### 検証マトリクス

| # | 起動環境 | 経路 | 修正前期待 | 修正後期待 | 修正前実機 | 修正後実機 |
|---|---|---|---|---|---|---|
| 1 | 素 terminal (Terminal.app/iTerm) | claude TUI Ctrl-Z (= 子 self-stop = `OnChildSuspend::Follow`) | ? | 復帰可、外側 shell に制御戻る | (kawaz 検証) | (kawaz 検証) |
| 2 | 素 terminal | 外部 `kill -TSTP <hyoui-pid>` (= 外部 SIGTSTP = `OnParentSuspend::Transparent`) | ? | 復帰可 | (kawaz 検証) | (kawaz 検証) |
| 3 | cmux 内 | claude TUI Ctrl-Z | **freeze** | 復帰可、cmux 健全 | (既知 freeze) | (kawaz 検証) |
| 4 | cmux 内 | 外部 `kill -TSTP` | **freeze** | 復帰可、cmux 健全 | (既知 freeze) | (kawaz 検証) |
| 5 | tmux 内 | claude TUI Ctrl-Z | ? | 復帰可 | (kawaz 検証) | (kawaz 検証) |
| 6 | tmux 内 | 外部 `kill -TSTP` | ? | 復帰可 | (kawaz 検証) | (kawaz 検証) |

「kawaz 検証」セルは実機で:
- 修正後実機 = 「OK / freeze」のいずれかを記入
- 修正前実機 = ベースライン取得 (= 修正効果の対照群、v0.2.0 brew 経由バイナリで)

各 cell に対応する dump file pair (`/tmp/v020-*-{server,client}.bin` vs
`/tmp/fix-*-{server,client}.bin`) を残しておけば、後段で diff を取って bytes
単位の差を検査できる。

### 検証完了の条件

- cell 3 / cell 4 (= cmux 内、既知 freeze) で **修正後 = 復帰可** が確認できれば本 issue
  解消。それ以外の cell は regression check として実施。
- 「cmux 内 freeze」以外で **修正後に new freeze** が見つかったら別 issue として起票。

## 注意

- 本 issue 修正は **Issue 2 (SIGCONT invariant 回復) と独立**、ただし関連する (= 復帰経路は同じ handler)
- 修正後は `harness test` (= matrix_test.rs) にも `stty -a < /dev/tty<N>` 観測を追加 (= Issue 5)
