# bug: hyoui run / attach 起動時の PTY size が外側 terminal と不一致

- Date: 2026-05-29
- Priority: 中 (= TUI app の表示崩れ、resize 対応 app は救えるが non-resize app は崩れたまま)
- Status: 未着手
- 報告者: kawaz 実機検証 2026-05-29

## 現象

- `hyoui run -- vim`: vim は **全画面で表示** (= 起動後 vim 自身が screen size を再取得して resize?)
- `hyoui run -- less /etc/passwd`: **画面の上半分くらいまでしか出ず、下半分はまっさら**
  - Enter を押していくと下に伸びていき、一番下に行き当たったら `:` プロンプトが表示される (= less が「24 行 PTY」を信じて 24 行分しか描画していない)

## 原因 (= 推定)

`DaemonConfig` の既定値:
```rust
cols: 80,
rows: 24,
```

実機 terminal の size (= 例 80x48 等) と一致せず、子 PTY が 80x24 で作られる。

attach 接続時に外側 terminal の size を取得して daemon に伝える経路:
- attach client が起動 → 自分の TTY size を `ioctl(TIOCGWINSZ)` で取得
- daemon に `Resize` control message 送信 (= cols/rows)
- daemon が `ioctl(TIOCSWINSZ)` で子 PTY master の size 変更
- 子 PTY slave 側に SIGWINCH 配信
- 子 (= less / vim) が SIGWINCH catch → 自分で resize

**この経路のどこかで脱落 or 遅れている**:
- vim は SIGWINCH catch + 全画面再描画する → 結果的に救われる
- less は起動時 size を信じる (もしくは SIGWINCH 後の再描画が遅延) → 上半分のまま

## 検証案

### 1. attach 起動直後の Resize message 送信を確認

attach の `connect` 直後に `ioctl(TIOCGWINSZ)` で外側 size を取得 + `Resize` 送信する
経路が実装されているか確認。実装されていれば、送信タイミングを早める / 同期にする。

### 2. `--debug-dump-server` で子 PTY 出力を確認

```bash
./target/release/hyoui run --debug-dump-server=/tmp/less.bin -- less /etc/passwd
# 上半分まで表示後 Ctrl-C で抜ける
od -c /tmp/less.bin | head -50
# less が「24 行分」しか書いてないか確認
```

### 3. 子 PTY size を直接確認

```bash
# hyoui run -- less 起動中に別 terminal で
DAEMON_PID=$(pgrep -f '__daemonize-run.*less' | head -1)
LESS_PID=$(pgrep -P $DAEMON_PID | head -1)
# /dev/ttyXXX を特定して stty 観測
TTY=$(ps -o tty= -p $LESS_PID | tr -d ' ')
stty -a -F /dev/$TTY 2>&1 | grep -E 'rows|columns'
# 期待: 外側 terminal と同じ rows/cols
# 実態: 24x80 のまま (= 既定値) の可能性
```

## 修正方針 (= 仮説確定後)

### 案 A: attach connect 直後に Resize を **同期送信**

`ClientConnection::connect` の handshake 直後で:
1. 外側 TTY size を ioctl で取得
2. `Resize` message を送信 (= 既存実装)
3. **daemon が Resize 適用するまで wait** (= ack 待ち)
4. その後 `run` loop に入る

これで子 (= less) が起動した瞬間には PTY size が外側 terminal と一致してる。

### 案 B: `hyoui run` の `--cols` / `--rows` を外側 TTY size に default

`run_command` の冒頭で:
1. stdin が TTY なら `ioctl(TIOCGWINSZ)` で size 取得
2. `cfg.cols / cfg.rows` 未指定なら取得 size を使う (= 80x24 fallback ではなく)
3. `spawn_detached_daemon_and_wait_ready` に渡す

これで daemon spawn 時点で子 PTY が外側 size で作られる。

### 案 C: Resize を attach 接続時に send + daemon 側 ack を待つ

案 A の subset。Resize message は既存だが ack response が無い。新 message を増やすか
`status.query` で確認する形にするか。

## 推奨

**案 A + 案 B の組み合わせ**:
- 案 B で daemon 起動時に正しい size で子 PTY を作る (= 主経路、最も自然)
- 案 A で attach 接続時にも resize 送信 (= run と別経路の attach、または run 後の terminal
  resize 対応)

run 側で外側 size を取れない (= TTY じゃない / pipe) 場合は既定 80x24 fallback。

## 関連 file

- `crates/hyoui-cli/src/main.rs::run_command` (= daemon spawn 前に size 取得経路)
- `crates/hyoui-cli/src/daemonize.rs::spawn_detached_daemon_and_wait_ready` (= cols/rows 引数)
- `crates/hyoui/src/daemon/config.rs::DaemonConfig` (= 既定 80x24)
- `crates/hyoui/src/client/attach.rs` (= attach 時の Resize message 送信タイミング)
- `crates/hyoui/src/sys/tty.rs::tty_size` (= TIOCGWINSZ wrapper)
- `crates/hyoui/src/sys/signal.rs::install_winch` (= 既存 WINCH handler、attach 側)
