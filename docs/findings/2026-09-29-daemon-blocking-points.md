# daemon イベントループのブロック点棚卸し (2026-09-29)

対象: v0.9.55 (4688d28e)。daemon の serve loop (`Session::serve` → `serve_loop`、終了時の cleanup / `finalize_child` / `linger_for_late_attach`、upgrade 経路) から到達する IO / 同期呼び出し。issue `docs/issue/2026-09-29-daemon-must-never-hang.md` の「やること 1」の成果物。設計側の扱いは DR-0037。

## 凡例

- **根拠種別**: `observed` = コードを読んで確認した事実 (呼び出し経路を追跡済)、`observed(実機)` = 実機で再現を確認、`inferred` = 推測 (kernel / fs の挙動に依存し未検証)
- **ブロックするか**: 相手 (client / 子 PTY / fs / tty / kernel) が応答しない時に、serve loop のスレッドが戻らなくなるか
- **bound**: 現状の上限。`無` は unbounded
- 行番号は `crates/hyoui/src/` 起点

## 実機確認: 部分 frame で serve loop が止まる

手順 (debug build 0.9.55、namespace を分けた `hyoui run --detached -- cat`):

1. プロキシ経由で `hyoui status` の handshake frame (204 bytes) を採取する
2. 別接続で handshake を送った後、次 frame の size header の先頭 2 bytes (`\x10\x00`) だけを送って止める
3. その状態で `hyoui status` を実行すると 5 秒の `timeout` で打ち切られた (rc=124)。攻撃側の接続を close すると直後の `hyoui status` は 0.08 秒で応答した

main thread が `Frame::decode_from(&mut ch.reader)` の `read_exact` で残りの bytes を待ち続け、poll に戻らないことを示している (下表 C-1)。同じ状態では SIGCHLD の回収も走らないので、2026-09-29 の zombie と同じ「生きているが固まる」状態になる (子の zombie 化そのものは今回実機で確認していない)。

副次観測: `$(hyoui run --detached -- cat 2>&1)` がコマンド置換の EOF 待ちで返らなかった。`lsof -d 2` で daemon の fd 2 が呼び出し元の PIPE のままだった (下表 E-1)。

## 表

### client socket

| ID | 呼び出し (位置) | 相手 | 応答しない時ブロックするか | 現状の bound | 根拠 |
|---|---|---|---|---|---|
| C-1 | `Frame::decode_from(&mut ch.reader)` (`daemon/session.rs` serve_loop の「3. 各 client reader」、`protocol/frame.rs:139` の `read_exact_eof` ×3) | client | **する**。POLLIN は 1 byte 以上で立つが、decode は header 4B + type 1B + body を `read_exact` で読み切るまで戻らない。socket は blocking (`sys/socket.rs:218` の accept は O_NONBLOCK を付けない、`accept.rs:298` で read timeout を `None` に戻す)。reader と writer は `protocol/transports/unix.rs:40-42` の `try_clone` (= dup、同一 open file description) なので、O_NONBLOCK を reader にだけ付けることはできない (file status flag は description 単位で共有される) | 無 | observed(実機) |
| C-2 | `ClientHandle::drop` の writer thread join (`daemon/broadcast.rs` の `impl Drop for ClientHandle`) | client | v0.9.55 で `recv_timeout(DROP_FLUSH_TIMEOUT=500ms)` → `shutdown(Write)` → `done_rx.recv()`。最後の `recv()` は shutdown で send が EPIPE 化する前提で待つ | 500ms + shutdown 後の解除時間 (macOS は即時と実測済のコメントあり、Linux は未確認) | observed (Linux の解除は inferred) |
| C-3 | 待ち合わせ用 thread の生成 (同上) | kernel | しない。ただし client 切断 1 件ごとに thread を 1 本作る | — | observed |
| C-4 | `finalize_accepted_client` の handshake response / reject の `Frame::encode_to(&mut writer_main)` (`daemon/accept.rs:280-400`) | client | 直前に write timeout を `None` に戻した blocking write を **main thread** で行う。handshake 直後で送信 buffer は空なので小さい frame は即時に書ける | 実質有界 (buffer 空が前提) | observed (書けること自体は inferred) |
| C-5 | `listener.accept()` (serve_loop「1. listener」と上限超過時の即 close、`accept.rs:143`) | kernel / client | poll の POLLIN 後に呼ぶ。listener は blocking なので、poll と accept の間に接続が消える (ECONNABORTED 系) と次の接続まで accept が戻らない可能性 | 無 | inferred |
| C-6 | `writer_pump` の `write_all` (`broadcast.rs:493-510`) | client | する (別 thread)。main thread は `queued_bytes` の上限で overflow 判定するだけなので、ここ単体では main を止めない | main には伝播しない (C-2 経由でのみ伝播) | observed |
| C-7 | `enqueue_for_client` / `send_control` / `send_backpressure_error` (`broadcast.rs`) | — | しない (unbounded mpsc への send) | — | observed |
| C-8 | `send_detach_ack_and_flush` の drain 待ち (`daemon/control.rs:523`) | client | 5ms sleep の polling で `queued_bytes == 0` を待つ | 200ms | observed |
| C-9 | handshake worker (`accept.rs:143-175`) | client | 別 thread。read/write timeout 5s、main は `try_recv` のみ | 5s (main は止まらない) | observed |
| C-10 | serve 終了時の per-client drain (`session.rs` serve の cleanup 2 箇所) と `clients.clear()` (= C-2 を client 数だけ直列実行) | client | 5ms sleep polling | 200ms × client 数 × 2 + C-2 × client 数 (最大 64 client) | observed |
| C-11 | `linger_for_late_attach` (`session.rs`) | client | 50ms poll の loop、handshake drain 500ms、ExitNotify drain 1000ms | 約 2s + 1.5s | observed |

### 子 PTY

| ID | 呼び出し (位置) | 相手 | 応答しない時ブロックするか | 現状の bound | 根拠 |
|---|---|---|---|---|---|
| P-1 | master の `read_some` (serve_loop「2. master」) | 子 | しない。master は `Session::start` / upgrade resume で O_NONBLOCK (`session.rs:333,369`)、EAGAIN は素通し | — | observed |
| P-2 | `EffectKind::TtyWrite` の `write_all_with_idle_timeout` (`daemon/reducer/execute.rs:81`、`sys/fd.rs:121`) | 子 | EAGAIN で `poll(POLLOUT)` を待つ。進捗が 500ms 無ければ IdleTimeout。進捗がある限り timer がリセットされるので、遅い読み手には frame 全体を書き切るまで main が留まる | 無進捗 500ms / frame。総時間は frame サイズと子の読み速度で決まる | observed |
| P-3 | master EOF / EIO 時に子が Stopped / Alive なら `thread::sleep(STOPPED_POLL_INTERVAL / ALIVE_RETRY_INTERVAL)` (serve_loop「2. master」) | 子 | main thread の固定 sleep | 1 回ごとに有界 (500ms 等) | observed |
| P-4 | `ioctl(TIOCSWINSZ)` (resize、`sys/pty.rs:159`) | kernel | しない | — | inferred (kernel 同期 API) |
| P-5 | `kill` / `kill_pgrp` / `getpgid` (control.rs の kill・signal・status、session.rs) | kernel | しない | — | inferred (kernel 同期 API) |

### 子プロセスの回収 (kernel)

| ID | 呼び出し (位置) | 相手 | 応答しない時ブロックするか | 現状の bound | 根拠 |
|---|---|---|---|---|---|
| K-1 | `lifecycle.poll_with_transition` の `waitpid(WNOHANG\|WUNTRACED\|WCONTINUED)` (`daemon/pty.rs:125`) | kernel | しない (WNOHANG) | — | observed |
| K-2 | `child_is_stopped_via_waitpid` (`session.rs:1005`) | kernel | しない (WNOHANG) | — | observed |
| K-3 | `procstate::is_stopped` (Linux は F-8 の procfs 読み取り、macOS は kernel の process state 直読み) | kernel | しない | — | inferred |
| K-4 | `finalize_child` の grace loop (`session.rs`) | 子 | 20ms sleep の WNOHANG polling | 5s (`FINALIZE_TERM_GRACE`) | observed |
| K-5 | `reap_blocking` の `waitpid(0)` (SIGKILL 昇格後 / `ChildExited(None)`) | 子 | SIGKILL 後は通常即時。子が uninterruptible sleep (D state) のままなら戻らない | 無 | inferred |
| K-6 | `Session::drop` の reap (`session.rs:871-945`) | 子 | serve 未実行時のみ。WNOHANG polling | 500ms 後 SIGKILL | observed |

### ファイルシステム

| ID | 呼び出し (位置) | 相手 | 応答しない時ブロックするか | 現状の bound | 根拠 |
|---|---|---|---|---|---|
| F-1 | `--debug-dump` の `f.write_all` (serve_loop「2. master」の Ok(n)) | fs | **する**。main thread で blocking write。NFS / FUSE 等が応答しなければ戻らない | 無 | observed (fs の停止は inferred) |
| F-2 | `--debug-dump` の open (`session.rs:510`) | fs | serve 開始前に 1 回。同上 | 無 | observed |
| F-3 | record の `try_push` → `send_timeout(RECORD_PUSH_TIMEOUT)` (`daemon/record.rs:344`)、PTY chunk ごと・sink ごとに `push_bytes_out` から main thread で呼ぶ | record writer thread (→ fs) | writer が fs で止まり queue が満杯になると 1 回 100ms 待つ | 100ms × sink 数 / chunk | observed |
| F-4 | queue 満杯で abort された sink の `join_writer` (`record.rs:736`) と `record.stop` / `stop_all` の `join_writer` (`record.rs:517-540`)。いずれも main thread | record writer thread (→ fs) | **する**。Sender を落としてから writer thread を join する。writer が fs の write で止まっていれば join は戻らない | 無 | observed (fs の停止は inferred) |
| F-5 | `record.start` の `validate_output_path` (`canonicalize`) と `open_record_file` (`record.rs:1165-1260`)、main thread | fs | する (応答しない fs 上の path を指定された場合) | 無 | inferred |
| F-6 | upgrade 経路の fs 操作 (すべて main thread、または exec 前後の単一 thread)。(a) `precheck_path` / `precheck_upgrade_target` の `metadata` (`daemon/upgrade.rs:244-280`)、(b) `write_state_file` の open + CBOR write (`upgrade.rs:160-190`)、(c) exec 準備失敗・execve 失敗からの復帰経路の `remove_file(state_path)` (`upgrade.rs:405,419,485,517`)、(d) prep 失敗時に `UnixSock::drop` が同期実行する socket の `unlink` (`sys/socket.rs:229-232`)、(e) exec 後の新プロセスが resume で行う state file の open / CBOR decode / `remove_file` (`upgrade.rs:202-215` の `read_and_consume_state_file`。`Session::from_upgrade_inherited` 側 `session.rs:361` 付近の resume 経路から呼ばれ、serve 開始前に走る) | fs | する (state dir / socket dir が応答しない fs 上にある場合) | 無 | observed (fs の停止は inferred) |
| F-7 | lock token 生成の `/dev/urandom` の open + `read_exact` 16 bytes (`daemon/lock.rs:523-534`、`control.rs:791` の lock.acquire から main thread で呼ぶ) | kernel (devfs) | 現行の Linux / macOS の `/dev/urandom` は初期化後に read が block しない。open は devfs 上の character device で外部 fs に依存しない。ただし fd 枯渇 (EMFILE) は error として返る (既存で Denied 応答) | — | inferred (man 4 random) |
| F-8 | `procstate::is_stopped` の Linux 実装は `std::fs::read_to_string("/proc/<pid>/stat")` (`sys/procstate.rs:49`) で open / read / close を伴う | kernel (procfs) | procfs は kernel が read 時に生成し外部 fs に依存しない。ただし「fs 経由の IO」なので DR-0037 I-1 上は根拠の明記が必要 | — | inferred (proc(5)) |

### tty / 標準エラー / 同期プリミティブ

| ID | 呼び出し (位置) | 相手 | 応答しない時ブロックするか | 現状の bound | 根拠 |
|---|---|---|---|---|---|
| E-1 | `eprintln!` 全般 (serve / record / upgrade / debug-dump 失敗時など)。daemon の fd 2 は起動した CLI から継承 (`hyoui-cli/src/daemonize.rs:170` の `Stdio::inherit()`、以後の付け替え無し) | 呼び出し元の stderr (tty / pipe) | pipe の読み手が生きたまま読まなくなれば、pipe buffer が埋まった時点で **する**。読み手が消えると EPIPE (Rust の `eprintln!` は書き込み失敗で panic するので、停止ではなく異常終了の経路になる) | 無 | observed(実機: fd 2 が PIPE のまま残ることを確認) / 停止と panic は inferred |
| E-2 | `SIGCHLD_SELFPIPE_LOCK` (`session.rs:176`) の `try_lock` | 同一 process 内 | しない (try_lock) | — | observed |
| E-3 | `RecordRegistry` の `RwLock` / sink の `Mutex` | 同一 process 内 | main thread 以外の保持者は writer thread のみで、writer は tx の Mutex を取らない | — | inferred |
| E-4 | SIGWINCH handler の ioctl (`sys/signal.rs:100`) | kernel | daemon は SIGWINCH forwarder を install しない (attach client 側の機構) | 対象外 | inferred |

## まとめ (相手別の unbounded 箇所)

- client: C-1 (実機で再現)、C-5 (推測)
- fs: F-1、F-2、F-4、F-5、F-6 (upgrade の準備・復帰・resume の全段)
- 標準エラー: E-1
- kernel: K-5 (D state の子のみ)

有界だが main thread を数百 ms 単位で占有するもの: C-2、C-8、C-10、C-11、P-2、P-3、F-3、K-4。これらは「固まる」ではないが、占有中は SIGCHLD 回収・他 client の frame 処理・accept が全て止まる。
