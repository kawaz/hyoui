# Finding: multi-attach + broadcast + multiplex の動作

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/02-multi-attach.rs`
- 関連: [[DR-0006]] §5 (複数 attach), §1 (Architecture)、[[DR-0007]] v0.0.1

## 判明した事実

1. **poll ベースの単一 thread 実装で multi-attach broadcast + stdin multiplex が動く**: listener / 子 pty master / 各 client socket を `nix::poll::poll` で多重待ち、READY なものを処理する古典パターンで十分
2. **PoC: 2 client 同時接続 + 双方向 broadcast 検証**: client A が "hello\n" write → 子 pty (cat) が echo → 全 client (A, B) が "hello\r\n" 受信。client B から "world\n" でも同様。**PASS**
3. **pty の tty flags は default で ONLCR 有効** = 子の stdout の `\n` が `\r\n` に変換されて master に届く (= "hello\n" 送ったのに "hello\r\n" 来る)。bytes 透過したい場合は明示で disable (`stty -onlcr`) 必要
4. **`UnixSock::listen` は parent directory が mode 0700 必須**: 既存 hyoui の安全性チェック (= world/group writable な dir に socket 置くと攻撃面増大)。`/tmp` 直接は NG、`$TMPDIR/hyoui-<name>` の subdir を 0700 で作る運用 (= [[DR-0006]] §2 の socket 配置と整合)
5. **poll の borrow checker**: `PollFd::new(c.as_fd(), ...)` が clients を borrow し続けると後で clients を mutate できない。**poll 直後に revents を別 Vec にコピーして polls を drop** すれば clients を mutate 可能 (古典的な workaround)

## 実用的な示唆

- daemon の event loop は poll ベースで OK、tokio 等の async ランタイム不要
- nonblocking I/O (`fcntl O_NONBLOCK`) で全 fd 設定、poll で READY 待ち、ready なものを read/write
- 1 thread でも十分捌ける (= claude/TUI app の出力レートは数百 KB/s 程度、poll の overhead は無視できる)
- PoC のシンプルさ確認 → 本実装でも poll loop を採用

## hyoui 本実装への反映

### Event loop の構造 (擬似コード)

```rust
loop {
    let revents = {
        let mut polls = build_pollset(&listener, &master, &clients, &self_pipe);
        poll(&mut polls, timeout)?;
        polls.iter().map(|p| p.revents().unwrap_or(empty())).collect::<Vec<_>>()
    };
    // ここで polls drop、clients を mutate 可能

    if revents[LISTENER].pollin() { accept_new_client(&mut clients); }
    if revents[MASTER].pollin() { broadcast_to_clients(&mut clients); }
    if revents[SELF_PIPE].pollin() { handle_signal(); }
    for (i, c) in clients.drain(..).enumerate() {
        if revents[CLIENT_BASE + i].pollin() { handle_client_input(&c, &master); }
        // ... keep alive ones in a new vec
    }
}
```

### Broadcast 失敗時の取り扱い

PoC では write 失敗 client を drop。本実装では:
- 部分書き込み (= TCP buffer full 相当) は backoff + retry (= client buffer に貯める)
- 完全失敗 (= EPIPE 等) は client を drop + 通知 (`connection lost`)

### ONLCR の主体 (= kernel tty driver、nix も hyoui --line-ending も無関係)

PoC 02 で観察した `\n → \r\n` 変換は **POSIX の tty/pty 仕様、kernel の tty driver が自動変換**。
hyoui の `--line-ending` (= hyoui のアプリレイヤで paste 入力を変換) とは別レイヤ。

#### レイヤ図

```
[hyoui]
  ├ paste/keys 入力                  ← --line-ending/--trailing-newline (hyoui レイヤ、optional)
  └ master fd へ write
        ↓
  [kernel tty driver]
        ├ c_iflag (input modes)      ← 子の stdin 側 (ICRNL 等)
  [子の slave fd で read]
  [子 process (cat / shell / TUI app)]
  [子の slave fd へ write "hello\n"]
        ↓
  [kernel tty driver]
        ├ c_oflag (output modes)     ← ★ ONLCR がここ
        │   ONLCR | OPOST → "\n" を "\r\n" に変換
[hyoui master fd で read] → "hello\r\n"
```

#### default termios

`forkpty(3)` / `openpty(3)` が作る pty の default termios は「対話的端末 (cooked mode)」相当:

```c
c_iflag = BRKINT | ICRNL | IXON | ...
c_oflag = OPOST | ONLCR | ...      // ← \n → \r\n 変換
c_cflag = CS8 | CREAD | ...
c_lflag = ISIG | ICANON | ECHO | ...
```

歴史的経緯: pty はリモートログイン (telnet/ssh) のため設計、ssh で接続した人間が shell を対話的に使う前提の cooked mode が default。
hyoui のようなデータチャネル用途は後発、必要に応じて子の termios を変える or hyoui レイヤで吸収する。

#### nix の役割

nix の `Pty::spawn` は forkpty を呼ぶだけ、変換は kernel 内で起こる。**nix は無関係**、ライブラリの問題ではなく POSIX 仕様。

#### TUI app は自分で disable

vim / claude code 等の TUI は起動時に termios を raw mode に変える:

```c
struct termios t;
tcgetattr(STDIN_FILENO, &t);
t.c_oflag &= ~OPOST;      // ← ONLCR 含む output 変換 OFF
t.c_lflag &= ~(ECHO | ICANON | ISIG);
tcsetattr(STDIN_FILENO, TCSANOW, &t);
```

これで TUI app が `write(stdout, "\n")` しても master 側は `\n` のまま。

一方 `cat` のような termios いじらないシンプルプログラムは default cooked mode のまま → `\n → \r\n` 変換される (= PoC 02 で観察)。

#### hyoui の対応 (3 案)

| 案 | 動作 | 評価 |
|---|---|---|
| **A. 子に任せる** | TUI app は \n、cooked モード子は \r\n のまま | ⭕ 子の意図尊重、wait/match で `\r?\n` 規約 |
| **B. daemon が ONLCR 無効化** (起動時 master の termios で `c_oflag &= ~ONLCR`) | 全 case で \n、cooked モード子の表示が崩れる可能性 | △ 強制干渉、副作用大 |
| **C. wait/match レイヤで CRLF→LF 正規化** | hyoui のアプリ層で吸収 | ⭕ 子に干渉せず、match 安定 |

**推奨: 案 A + C の組合せ**:

- daemon は子の termios に干渉しない (= bytes 透過)
- wait/match で **CRLF → LF 正規化 option** (= `--newline-convert=preserve|lf` 等の別 flag、装飾除去とは責務分離)

これで「子が cooked モードで `\r\n` 吐く」「子が raw モードで `\n` 吐く」のどちらにも対応、wait/match の text match が `\r?\n` regex 書かなくても安定する。

詳細は本 finding 末尾 + [[DR-0006]] §11 微修正で確定 (= [[2026-05-26-ansi-strip]] でも CRLF→LF を装飾除去から分離すべきと判明)。

## 検証の詳細

### 実行結果 (test role)

```
$ cargo run --example 02-multi-attach -- test
[test] starting daemon: ... daemon /var/folders/.../T/hyoui-poc-02-10104/sock
[daemon] listening /var/folders/.../sock, child pid 10243
[test] socket appeared in 55.046708ms
[daemon] +client, total 1
[test] both clients connected
[daemon] +client, total 2
[test] wrote 'hello\n' from client A
[daemon] client read 6 bytes: "hello\n"
[daemon] master read 7 bytes: "hello\r\n"
[test] client A received (7 bytes): "hello\r\n"
[test] client B received (7 bytes): "hello\r\n"
[test] wrote 'world\n' from client B
[daemon] client read 6 bytes: "world\n"
[daemon] master read 7 bytes: "world\r\n"
[test] (after B write) A received (7 bytes): "world\r\n"
[test] (after B write) B received (7 bytes): "world\r\n"
[test] result: A_recv_from_A=true, B_recv_from_A=true, A_recv_from_B=true, B_recv_from_B=true
[test] PASS
```

socket 作成から `--socket` 経由で daemon 起動 → client 接続まで **55ms 程度**。十分速い。

### socket 配置の留意

`/tmp/hyoui-poc-02.sock` を直接使うと `Precondition("socket parent directory must be mode 0700")` で失敗。`$TMPDIR/hyoui-poc-02-PID/` のような subdir を `mkdir -p` + `chmod 0700` してから socket 置く必要あり。

本実装の socket 配置 ([[DR-0006]] §2):
- Linux: `$XDG_RUNTIME_DIR/hyoui/<name>.sock` — `$XDG_RUNTIME_DIR` は systemd-logind 由来で既に user-private 0700
- macOS: `$TMPDIR/hyoui-$UID/<name>.sock` — `$TMPDIR` 自体は 0700、`hyoui-$UID/` も初回作成時に 0700 chmod

`UnixSock::listen` の precondition は既存の安全性チェック、本実装でもそのまま生きる。

### 拡張ポイント (本実装で必要、PoC でカバーしてない)

- self-pipe (シグナル受信通知)
- attach/detach の protocol (= 単純な bytes 中継ではなく、handshake → data → control の状態機械)
- leader 内部実装 (= rw/ro 区別、winsize broadcast)
- lock/tx の token 検証 ([[PoC 06]] 範疇)
- in-flight paste state (= bracketed paste end の best-effort 保証、[[PoC 03]] 範疇)

これらは別 PoC または本実装で詰める。本 PoC では **broadcast/multiplex の基本動作**を確認したのみ。
