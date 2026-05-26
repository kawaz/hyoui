# Finding: daemon 化 (double-fork) の動作と stdio detach の必須性

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/01-daemon-fork.rs`, `01b-daemon-fork-detached.rs`
- 関連: [[DR-0006]] §1 (Architecture screen 型)、[[DR-0007]] v0.0.1 (daemon 化)

## 判明した事実

1. **double-fork + setsid で daemon を init/launchd の子に切り離せる**: grandchild の PPID は中間プロセス exit 後すぐに 1 になる
2. **stdio を /dev/null に detach しないと、daemon が parent shell の pipeline を抱え続ける**: cargo run の出力を tail にパイプした実験で、stdio 継承だと 30 秒 (= daemon の sleep 終了まで) パイプが解放されない、detach すると 0.43 秒で即解放
3. **macOS (Darwin 25.5.0) では init/launchd への reparent が即座に成立** (intermediate process exit と同時に PPID=1)

## 実用的な示唆

- daemon は **必ず stdin/stdout/stderr を /dev/null にリダイレクトする** (= 親プロセス連鎖から完全切断)
- ただし debug 用途では log file にリダイレクトでも OK (= 子の `/dev/null` 化はあくまで parent shell の pipeline 解放のため)
- 中間プロセスでの setsid は新セッション作成のため必須 (= 子プロセスが端末から完全切り離される、子に対する Ctrl-C/HUP が伝搬しない)

## hyoui 本実装への反映

`hyoui run` の daemon 化シーケンス:

```
1. fork() → 中間プロセス
2. 中間プロセスで setsid()
3. fork() → grandchild (= daemon)
4. 中間プロセスは即 exit
5. grandchild は stdin/stdout/stderr を /dev/null に dup2
6. 以降 daemon としてイベントループ
```

`hyoui run` (= 親プロセス) は first fork 後に中間プロセスを waitpid (ゾンビ防止) して exit。これで shell prompt 即返り。

**注意**: detach タイミングは「socket bind 完了後」にする必要あり (= 親プロセスが exit する前に daemon の socket が listen 状態になってないと、`hyoui list` 等の後続コマンドが「socket 無いよ」になる)。pipe で完了通知する pattern が筋:

```
1. fork() で中間プロセス起動 + pipe(read_fd, write_fd) を準備
2. 中間プロセスが double-fork で daemon 起動 + socket bind
3. daemon が socket bind 完了したら pipe に 1 byte 書く
4. 親プロセスは pipe を read で待つ (timeout 付き)
5. read 成功で「daemon 起動完了」確認 → 親 exit
```

これで `hyoui run --detached` の場合に「daemon 起動完了を確認してから親 exit」が綺麗に組める。

## 検証の詳細

### 実行 1: PoC 01 (stdio 継承)

```
$ time cargo run --example 01-daemon-fork | tail
=== PoC 01: daemon double-fork ===
parent pid: 91906
first fork: spawned intermediate pid 91917
intermediate exited with code 0
parent exiting; shell prompt should return immediately
cargo run --example 01-daemon-fork  0.02s user 0.02s system 9% cpu 0.421 total
tail  0.00s user 0.00s system 0% cpu 30.426 total
```

cargo 自体は 0.42s で exit、ただし pipeline 全体は **30.4s 待った** (= daemon が stdout fd を握ったまま 30 秒 sleep)。

### 実行 2: PoC 01b (stdio detach)

```
$ time cargo run --example 01b-daemon-fork-detached
=== PoC 01b: daemon double-fork + stdio detach ===
parent pid: 96780
parent exiting; shell prompt + pipeline should return immediately
cargo run --example 01b-daemon-fork-detached  0.02s user 0.02s system 9% cpu 0.430 total

$ cat /tmp/hyoui-poc-01b-daemon.log
[1779770599.534] daemon (pid 96809) detached stdio, PPID = 1
[1779770604.535] daemon (pid 96809) alive tick 0, PPID = 1
...

$ ps -p 96809 -o pid,ppid,stat,command
  PID  PPID STAT COMMAND
96809     1 S    target/debug/examples/01b-daemon-fork-detached
```

pipeline 即解放 (0.43s)、PPID=1 で生存確認、STAT=S (sleep)。

### PPID 遷移

- daemon が起動直後: PPID = intermediate process pid (= 中間プロセスがまだ生きてる時点)
- 中間プロセス exit 後: PPID = 1 (init/launchd に reparent)

PoC 01 のログ:
```
[1779770478.162] daemon (pid 91918) started, my PPID = 91917
[1779770483.163] daemon (pid 91918) alive tick 0, PPID = 1
```

= 起動時に PPID=91917 (intermediate)、5 秒後の最初の tick で PPID=1 (intermediate exit 済)。orphan 化成功。

## nix crate の罠

nix 0.31 の `nix::unistd::dup2` は `&mut OwnedFd` を要求するように API 変更されてる:

```rust
pub fn dup2<Fd: AsFd>(oldfd: Fd, newfd: &mut OwnedFd) -> Result<()>
```

stdin/stdout/stderr の fd (= raw 0/1/2) を `OwnedFd` でラップするのは循環依存 (= OwnedFd::from_raw_fd は unsafe、かつ drop で close される)。**libc::dup2 を直接使うほうがシンプル**:

```rust
unsafe {
    libc::dup2(nullfd, 0);
    libc::dup2(nullfd, 1);
    libc::dup2(nullfd, 2);
}
```

これは hyoui の `sys/raw.rs` (unsafe 封じ込め) に既存方針として合致。
