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

### ONLCR 等の tty flag 設定

子 pty の default tty flags は `ONLCR | OPOST | ECHO | ICANON | ISIG | ...`。
hyoui の主用途 (= TUI app の pty 中継) では:
- shell/TUI app は自分で tty を raw mode にする (`tcsetattr` で ECHO/ICANON 等 disable)
- なので daemon は default flags のまま起動して OK (= 子が必要に応じて変更)
- ただし `cat` のような単純コマンドだと cooked mode のまま、改行変換が起きる

wait の text/pattern match で改行は `\r\n` で来る可能性を考慮:
- regex は `\r?\n` で書く慣習を doc 推奨、または
- 装飾除去 ([[DR-0006]] §11) で CRLF → LF 正規化を含める

CRLF 変換は ANSI escape ではないが、wait の text match のノイズになるので **装飾除去の一部として CRLF → LF も含める**のが筋。doc 明示。

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
