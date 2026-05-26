# Finding: leader resize 伝播 (TIOCSWINSZ → SIGWINCH)

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/05-resize-propagation.rs`
- 関連: [[DR-0006]] §5 (leader)、§6 (Winsize mode)

## 判明した事実

1. **daemon が `Pty::resize(cols, rows)` (= TIOCSWINSZ ioctl on master fd)** を呼ぶと、kernel が **子 pty の foreground process group に SIGWINCH を送る**
2. PoC: 80x24 で起動した bash → resize 160x48 → SIGWINCH 受信 → trap で `stty size` 出力 → "size: 48 160" (PASS)
3. **bash の trap 発火タイミングは signal handler を spawn してる process 次第**: `sleep 30` で wait 中だと SIGWINCH を受けても trap が即発火しない (たぶん bash 内部の deferred 処理)。`read -t 30` で wait なら即発火
4. **bash の `$COLUMNS`/`$LINES` は non-interactive shell では未設定**: `bash -i` でないと値が空。stty / tput を使えば対話/非対話関係なく取れる
5. **`stty size` は "rows cols" 順** (= bash の `$LINES x $COLUMNS` と同順)

## 実用的な示唆

### daemon の resize 責務

- daemon は **TIOCSWINSZ ioctl を子 pty master fd に対して発行** するだけ
- それ以降は kernel + 子 process の責務 (= SIGWINCH 受信、内部 size update、画面再描画 etc.)
- daemon は子の挙動を待たない、fire-and-forget

### leader resize 伝播の flow ([[DR-0006]] §5)

```
1. leader client が SIGWINCH 受信 (= 自身の terminal が resize された)
2. leader client が tty_size(stdout) で新サイズ取得 (TIOCGWINSZ)
3. leader client が daemon に resize message 送信 (cols, rows)
4. daemon が pty.resize(cols, rows) (= TIOCSWINSZ ioctl on master fd)
5. kernel が子 pty の foreground pgrp に SIGWINCH 送信
6. 子 (TUI app など) が受信 → 内部 size update → 再描画
```

各ステップは独立、daemon は中継のみ。

### MVP 実装の最小限

- daemon: protocol message `RESIZE { cols, rows }` を leader client から受け取ったら `pty.resize` 呼ぶ
- daemon: leader 変更時に新 leader の最新 size で `pty.resize` (= leader 変更直後の size 不一致 を防ぐ)
- client: SIGWINCH handler で resize message を daemon に送る (= self-pipe で signal を main loop に通知 → tty_size 取得 → daemon に send)

### 子 process の signal handling の癖

bash のような shell は signal を deferred で処理することがある (= 現在実行中のコマンドが終わるまで trap 実行を遅らせる)。これは hyoui の責務外、daemon は SIGWINCH を「届ける」だけ。子がそれを処理するかは子次第。

### Other tty 系 ioctl (補足、PoC 範囲外)

- TIOCGWINSZ: 現在の size 取得
- TCSANOW/TCSADRAIN/TCSAFLUSH: termios 反映タイミング
- TIOCSCTTY: ctty 獲得 (forkpty + login_tty で自動処理済)
- これらは hyoui の `sys/tty.rs` で既に wrap 済

## hyoui 本実装への反映

### Protocol message

```rust
enum Message {
    // ... 他
    Resize { cols: u16, rows: u16 },
}
```

leader client のみが送る、daemon は received で `pty.resize` 呼ぶ。non-leader client が送った場合は無視 (or error)。

### Leader 変更時の同期

```rust
fn on_leader_change(&mut self, new_leader: ClientId) {
    if let Some(size) = self.clients[new_leader].last_known_size {
        self.pty.resize(size.cols, size.rows).ok();
    }
}
```

新 leader の terminal size が分かってればその size で resize、不明なら現状維持。

## 検証の詳細

```
$ cargo run --example 05-resize-propagation -- test
[daemon] listening /var/folders/.../sock, child pid 39943 (80x24 initial)
[daemon] client connected
[test] initial output (13 bytes): "size: 24 80\r\n"
[test] sent resize 160x48
[daemon] resize req 160x48
[test] after resize (14 bytes): "size: 48 160\r\n"
[test] initial_ok=true, after_ok=true
[test] PASS
```

`Pty::resize(160, 48)` (= cols, rows) → 子の trap WINCH 発火 → `stty size` 出力 → "48 160" (= rows, cols) 受信。

### bash trap 発火の罠

最初 `sleep 30` で wait させたら trap 発火が遅延した:

```bash
trap 'echo "size: $(stty size)"' WINCH
echo "size: $(stty size)"
sleep 30    # ← ここで SIGWINCH 受けても trap 実行が遅延 (resize 後 1 秒待っても出力なし)
```

`read -t 30` に変えたら即発火:

```bash
trap 'echo "size: $(stty size)"' WINCH
echo "size: $(stty size)"
while read -r -t 30 _; do :; done   # read は signal で即時中断 → trap 即実行 → loop で再 read
sleep 30   # 念のため後置
```

これは bash 内部の signal handling の癖 (= EINTR vs SA_RESTART、bash の loop での deferred dispatch)。hyoui の責務外、ただし「子の挙動はアプリ依存」を本実装の wait/test で意識する必要。

PoC として **TIOCSWINSZ → SIGWINCH の伝播は確実に動く**ことが確認できた。
