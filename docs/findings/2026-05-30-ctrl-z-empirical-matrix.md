# Ctrl-Z bug 実機マトリクス検証 (Claude 代理、2026-05-30)

- Date: 2026-05-30
- 発見元: Ctrl-Z bug 調査 workflow (wfmln0gmq) の Phase 1 観測フェーズを Claude が代理実行
- Env: macOS 26.5 (Build 25F71) / Darwin 25.5.0 / hyoui v0.2.1 (6d9ca3a)
- Binary: `./target/release/hyoui`

## 判明した事実

1. **hyoui run --detached で起動した子プロセスは独立 session leader**
   - 実機観測: `sleep 6000` の child は `pgid==pid==51150`、`Ss+` 表記。daemon (51149) は別 process group (`pgid=51149`)。
   - → 子の process group は親 (daemon) と異なる session に属する = **POSIX 用語の "orphan process group" の条件 (= group 内全 member の親が、同 session の別 group に属さない) を満たす**。

2. **POSIX の `raise(SIGTSTP)` orphan group discard は macOS XNU 25.5.0 で確認された**
   - 子 process が **自分自身に SIGTSTP を投げる** path (= `kill -TSTP $$` or `os.kill(pid, SIGTSTP)`) では、子は STOPPED にならず実行続行した。
   - `python3` で `os.kill(os.getpid(), signal.SIGTSTP)` を実行 → 戻り値 `None` (= 例外なし) で「`after raise: ... still alive`」が出力された。
   - `bash -c 'kill -TSTP $$; echo "still-alive: $?"; sleep 30'` でも同様に `still-alive: 0` が出力され、続く `sleep 30` を `Ss+` で実行。
   - **leader 在りでも leader 不在でも結果同じ** (= leader 不在時の daemon auto-resume fallback とは独立に、kernel layer で discard されている)。
   - → **Ctrl-Z bug の Issue 1 (= 1回目 Ctrl-Z 無応答 in TUI/raw mode) の根本原因として `orphan_group_discard` 仮説が survive**。

3. **外部からの `kill -STOP` は orphan 判定を bypass し、leader 在り状態では `T` (STOPPED) に遷移する**
   - leader (= attach client, mode=rw, cap=child-state-v1) を attach した状態で `kill -STOP <child_pid>` → `Ts+` に遷移。
   - leader 不在状態では SIGSTOP 直後に daemon の auto-resume fallback (`session.rs:835`) が走って `killpg(child, SIGCONT)` し、`Ss+` のままに戻る。
   - SIGSTOP は catch / orphan-discard 不能の signal なので **DR-0001 軸1 follow path の動作確認に使える** (Test B 成立)。

4. **PTY slave (= 子の controlling terminal) は cooked (canonical) mode + ECHOCTL**
   - `stty -a < /dev/ttys007` で `icanon`, `isig`, `echoctl`, `susp = ^Z` を確認。
   - hyoui の input で 0x1a (VSUSP) を送ると → PTY line discipline は SIGTSTP を foreground process group に生成、ECHOCTL で `^Z` をリテラル echo back する path に乗る。
   - 子は orphan group なので SIGTSTP は kernel discard → `^Z` 2 byte だけが scrollback に残り、子は `Ss+` のまま実行続行。
   - → **PTY 内で発生する Ctrl-Z (= 内側 terminal で実 Ctrl-Z 押下、or input hex:1a 投入) も orphan discard 経路に乗る**。

5. **attach client の termios 復元 path (DR-0015 §2.3 signal monitor thread) は対称的だが 2 度目 Ctrl-Z race の余地あり (= code lens)**
   - `suspend()`: saved (pre-raw) termios を `tcsetattr(TCSAFLUSH)` で復元 (`tty.rs:71-75`)。
   - `resume()`: saved を clone → `cfmakeraw` + IUTF8 → `tcsetattr(TCSAFLUSH)` (`tty.rs:82-94`)。
   - SIGTSTP handler (`main.rs:98-110`) の流れ: `g.suspend()` → `install_default(SIGTSTP)` → `raise(SIGTSTP)` → **(STOPPED, SIGCONT で復帰)** → `register_self_pipe(SIGTSTP)` → `g.resume()`。
   - **race の余地**: SIGCONT 復帰直後 `register_self_pipe(SIGTSTP)` が完了する**前**に外側 Ctrl-Z (SIGTSTP) が attach process に届くと、disposition が `default` のまま → kernel が attach を直接 STOP → handler 通らず → termios が raw のまま固着。これは **Issue 2 (= 2度目 Ctrl-Z で attach が無応答) と整合する仮説**だが、Claude 環境では interactive Ctrl-Z 押下を再現できないため、本 finding では code lens の対称性記述に留める。

## 実用的な示唆 / ベストプラクティス

### orphan_group_discard 仮説の status

- macOS XNU 25.5.0 で **実機 confirmed**。
- POSIX spec (= SUS / `kill(2)` / signal.h):
  - process が orphan process group のメンバーの場合、process group に対する SIGTSTP / SIGTTIN / SIGTTOU は **discard される** (= `kill(2)` は成功扱い 0 を返すが、kernel が signal を deliver しない)。
  - hyoui の子は session leader = 自身の process group のみ + 親が別 session = orphan 判定 ✓。

### Issue 1 (= 1回目 Ctrl-Z 無応答 in raw mode TUI) の root cause

子 TUI が raw mode で Ctrl-Z byte (0x1a) を直接読んで `raise(SIGTSTP)` する流れ (= TUI 自前の job control suspend) は、子 pgrp が orphan なので必ず discard される。これは TUI 側の bug ではなく **hyoui 側の構造的問題** (= setsid で session leader 化することの副作用)。

### 候補対策 (DR で検討すべき)

- A. **daemon が child を own session の foreground group として持つよう構造変更** → 子は orphan でなくなり SIGTSTP 通常配信。ただし daemon が controlling terminal を持つ複雑性が増す。
- B. **TUI raw mode で Ctrl-Z byte を hyoui daemon が intercept → 軸1 follow path を kick** → child raise なしで stop notify → leader 側で適切に処理。これは DR-0001 軸 1 / DR-0015 §2.2 の延長で実現可能。
- C. **hyoui run --no-setsid 等の opt-out flag** → orphan 化を回避、ただし shell job control の干渉が再発する可能性 (= 過去 DR で setsid 採用した経緯を再評価必要)。

選択は DR で決める領域 (= Phase 1 観測の scope 外)。

### Issue 2 (= 2度目 Ctrl-Z 無応答 in attach client) の status

code lens で signal handler の race 余地を発見したが **未確証**。Claude 環境では interactive Ctrl-Z 押下不能。kawaz の手元検証で再現実験が必要 (= 後述「Claude 代理で再現できなかった範囲」)。

## 検証の詳細

### A. 子pgrp orphan 状態確認

```bash
$ ./target/release/hyoui run --detached --session=test-orphan -- /bin/sleep 6000
/tmp/claude-501/hyoui-501/test-orphan.sock

$ ./target/release/hyoui status test-orphan
session-id: test-orphan
child-pid: 51150
scrollback-bytes: 0
lock-holder: (none)
clients:
  - id=0 mode=Ro

$ ps -o pid,ppid,pgid,stat,comm -p 51149 51150
  PID  PPID  PGID STAT COMM
51149     1 51149 Ss   /Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main/target/release/hyoui
51150 51149 51150 Ss+  /bin/sleep
```

解釈:
- daemon (51149): `pgid=51149`, `Ss` (session leader, sleeping)
- child sleep (51150): `pgid=51150`, `Ss+` (session leader, sleeping, foreground)
- 子の pgid (51150) ≠ daemon の pgid (51149) → **別 process group 確定**
- 子は session leader (= sid==pid、`s` flag) → 自身の session の単一 process group
- 子の親 (daemon, 51149) は別 session に属する → **orphan process group の POSIX 定義を満たす**

macOS の `ps -o sess` は session pointer を 0 表示するクセがあるが、`pgid == pid` + `s` flag + `ppid` 経由の session 比較で同じ結論を得られる。

### B. 外部 SIGSTOP で軸1 follow 検証

#### B-1. leader 不在状態 (= --detached 直後)

```bash
$ kill -STOP 51150; ps -o pid,stat,comm -p 51150
  PID STAT COMM
51150 Ss+  /bin/sleep    # T (STOPPED) にならず Ss+ のまま
```

解釈: daemon が SIGCHLD 経由で stop transition を観測 → `notify_child_stopped` (`session.rs:826`) → leader 不在判定 → `killpg(child, SIGCONT)` で auto-resume fallback (`session.rs:836`) → child 即復帰 → `Ss+` 維持。

#### B-2. sanity check (= macOS ps の `T` 表記確認)

```bash
$ /bin/sleep 1000 &
[1] 54768
$ ps -o pid,stat,comm -p 54768   # SN
$ kill -STOP 54768; ps -o pid,stat,comm -p 54768
  PID STAT COMM
54768 TN   /bin/sleep             # T が出る (= ps STAT の T 表記は機能している)
$ kill -CONT 54768; ps -o pid,stat,comm -p 54768
  PID STAT COMM
54768 SN   /bin/sleep
```

→ macOS の `ps STAT` は STOPPED を `T` で表記する。B-1 で `Ss+` が維持されたのは daemon auto-resume fallback の結果である。

#### B-3. leader 在り状態 (= attach client 起動後)

```bash
$ ./target/release/hyoui attach test-orphan --mode=rw < /dev/null > /tmp/attach-stdout.log 2>&1 &
[1] 58751
$ ./target/release/hyoui status test-orphan
clients:
  - id=7 mode=Rw leader
  - id=8 mode=Ro

$ kill -STOP 51150; sleep 0.5; ps -o pid,ppid,pgid,stat,comm -p 51150
  PID  PPID  PGID STAT COMM
51150 51149 51150 Ts+  /bin/sleep   # T (STOPPED) に遷移、+ で foreground 維持
```

解釈: leader (id=7) が在ると `notify_child_stopped` は SIGCONT fallback ではなく `SessionChildStoppedNotify` を leader へ送って判断を委ねる (`session.rs:849`)。child は STOPPED のまま維持される。

attach client の stdout には CBOR control message が乗らない (= 別 channel) ため log file には変化なし。これは設計通り。

### C. raise(SIGTSTP) orphan discard 実機検証

#### C-1. bash kill -TSTP $$ (= leader 不在)

```bash
$ ./target/release/hyoui run --detached --session=test-raise -- bash -c 'sleep 0.5; kill -TSTP $$; echo "still-alive: $?"; sleep 30'

$ # 1.5s 後
$ ./target/release/hyoui screen dump test-raise --format=text
still-alive: 0
(以下空行)

$ ps -o pid,ppid,pgid,stat,comm -p 61468
  PID  PPID  PGID STAT COMM
61468 61467 61468 Ss+  sleep   # bash は STOPPED にならず sleep 30 を実行中
```

解釈:
- `kill -TSTP $$` (= bash 自身に SIGTSTP) が `exit 0` を返した = signal は `kill(2)` の入り口で受理された
- だが bash は STOPPED にならず `echo "still-alive: $?"` を実行 → `still-alive: 0` 出力 → `sleep 30` を fork
- これは **kernel の orphan group discard** が `raise` を無効化した証拠

#### C-2. bash kill -TSTP $$ (= leader 在り)

```bash
$ ./target/release/hyoui run --detached --session=test-raise2 -- bash -c 'sleep 2; kill -TSTP $$; echo "still-alive: $?"; sleep 30'
$ ./target/release/hyoui attach test-raise2 --mode=rw < /dev/null > /tmp/attach2.log 2>&1 &
$ # 2.5s 後
$ ./target/release/hyoui screen dump test-raise2 --format=text
still-alive: 0
```

解釈: leader 在りでも結果同じ (= still-alive: 0 出力)。**leader 不在時の auto-resume fallback とは独立に、kernel layer で discard されている**ことの証明。

#### C-3. python3 os.kill(pid, SIGTSTP) (= bash 由来の job control 設定を完全に排除)

```bash
$ ./target/release/hyoui run --detached --session=test-py -- python3 -c '
import os, signal, time
print(f"pid={os.getpid()}, pgid={os.getpgrp()}, sid={os.getsid(0)}, ppid={os.getppid()}", flush=True)
time.sleep(0.5)
print(f"before raise SIGTSTP", flush=True)
r = os.kill(os.getpid(), signal.SIGTSTP)
print(f"after raise: returned {r}, still alive", flush=True)
time.sleep(30)
'
$ # 2s 後
$ ./target/release/hyoui screen dump test-py --format=text
pid=64234, pgid=64234, sid=64234, ppid=64233
before raise SIGTSTP
after raise: returned None, still alive

$ ps -o pid,ppid,pgid,stat,comm -p 64234
  PID  PPID  PGID STAT COMM
64234 64233 64234 Ss+  Python
```

解釈 (= 決定的観測):
- python は `pid=64234, pgid=64234, sid=64234` を自己報告 → **session leader を実機確認**
- `os.kill(64234, SIGTSTP)` は例外なく `None` 返却 (= syscall 成功)
- だが python は STOPPED にならず `still alive` を出力、`time.sleep(30)` に進んだ
- bash 由来の SIGTSTP disposition 操作とは無関係に、**kernel が orphan group 判定で SIGTSTP を discard した**

これは **`orphan_group_discard` 仮説の macOS XNU 25.5.0 における実機 confirmed**。

### D. line-oriented app の Ctrl-Z 経路

```bash
$ ./target/release/hyoui run --detached --session=test-sleep -- /bin/sleep 6000
$ ./target/release/hyoui input test-sleep hex:1a   # 0x1a = VSUSP / Ctrl-Z

$ ./target/release/hyoui screen dump test-sleep --format=text | head -3
^Z
(空行)

$ ./target/release/hyoui tail test-sleep --last-bytes=500
[?1049l[?25h[m[H[J^Z>[?1l[?2004l^Zhyoui: tail: stream ended (eof (= scrollback flush done))

$ ps -o pid,ppid,pgid,stat,comm -p 65326
  PID  PPID  PGID STAT COMM
65326 65325 65326 Ss+  /bin/sleep   # sleep は STOPPED にならず Ss+
```

PTY termios 状態 (`stty -a < /dev/ttys007`):

```
speed 9600 baud; rows 24; columns 80;
intr = ^C; quit = ^\; erase = ^?; kill = ^U; eof = ^D; eol = <undef>;
eol2 = <undef>; start = ^Q; stop = ^S; susp = ^Z; dsusp = ^Y; rprnt = ^R;
werase = ^W; lnext = ^V; discard = ^O; status = ^T; min = 1; time = 0;
isig icanon iexten echo echoe -echok -echonl -noflsh -tostop -echoprt echoctl
echoke -flusho -extproc
```

解釈:
- PTY slave は `icanon` (cooked / canonical mode) + `isig` (signal generation 有効) + `echoctl` (control char visible echo)
- `susp = ^Z` (VSUSP = 0x1a)
- hyoui の input hex:1a で送った 0x1a が PTY slave に届くと、line discipline は:
  1. SIGTSTP を foreground process group (= sleep の pgrp 65326) に生成する
  2. ECHOCTL の効果で `^Z` リテラル (= 0x5E 0x5A の 2 byte) を slave 側に echo back
- (1) の SIGTSTP は子 (65326) が orphan group なので kernel discard
- (2) の echo は signal とは独立 path で実行 → scrollback に `^Z` が残る

→ **TUI app だけでなく cooked mode の line-oriented app (sleep, cat 等) でも Ctrl-Z byte injection 経路は orphan discard に遭う**。これは Issue 1 と整合する別 app による cross-check (= サンプル数 ≥3 の category 確保: bash, python, sleep)。

### E. termios 復元対称性 (code lens)

#### `TtyGuard` (`crates/hyoui/src/sys/tty.rs:47-104`)

```rust
pub fn suspend(&self) {                              // lines 71-75
    if let Some(fd) = self.fd.as_ref() {
        let _ = termios::tcsetattr(fd.as_fd(), SetArg::TCSAFLUSH, &self.saved);
    }
}

pub fn resume(&self) {                               // lines 82-94
    let Some(fd) = self.fd.as_ref() else { return };
    let mut raw_t = self.saved.clone();
    termios::cfmakeraw(&mut raw_t);
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        raw_t.input_flags |= termios::InputFlags::IUTF8;
    }
    let _ = termios::tcsetattr(fd.as_fd(), SetArg::TCSAFLUSH, &raw_t);
}

impl Drop for TtyGuard {                             // lines 97-104
    fn drop(&mut self) {
        if let Some(fd) = self.fd.as_ref() {
            let _ = termios::tcsetattr(fd.as_fd(), SetArg::TCSAFLUSH, &self.saved);
        }
    }
}
```

- `suspend`: saved (= 退避済み pre-raw termios) を `tcsetattr(TCSAFLUSH)` で復元
- `resume`: saved を clone → `cfmakeraw` + IUTF8 → `tcsetattr(TCSAFLUSH)` で raw 再設定
- 両 API とも同じ fd / saved を base に対称的に動く → **対称性 OK**

#### `install_attach_signal_thread` (`crates/hyoui-cli/src/main.rs:75-118`)

SIGTSTP handler (`lines 98-110`):

```rust
if signum == Signal::SIGTSTP as i32 {
    // (1) 外側 TTY を pre-raw に戻す → SIGTSTP を kernel default で処理させて STOPPED へ
    if let Ok(g) = guard.lock() {
        g.suspend();                                  // (1a) termios pre-raw 復元
    }
    let _ = install_default(Signal::SIGTSTP);        // (1b) disposition を default に
    let _ = raise(Signal::SIGTSTP);                  // (1c) 自分を STOPPED に raise
    // ──── STOPPED → SIGCONT で復帰 ────
    let _ = register_self_pipe(Signal::SIGTSTP);     // (2a) disposition を self-pipe に戻す
    if let Ok(g) = guard.lock() {
        g.resume();                                   // (2b) raw 再設定
    }
}
```

- (1a, 1b, 1c) と (2a, 2b) は対称的構造
- SIGCONT signal は handler 内では explicit に no-op (line 112 comment)。kernel が process を起こすだけで termios resume は SIGTSTP handler の continuation で実施

#### Race 余地 (2度目 Ctrl-Z 仮説)

SIGCONT で復帰した直後の **(2a) 完了前**に外側で Ctrl-Z が押されると:
- attach process の SIGTSTP disposition はまだ (1b) で設定した `default` のまま
- kernel が attach process を直接 STOP (= handler 通らず)
- (2b) `g.resume()` が走らないので termios も pre-raw のままだが、attach process 自体は STOPPED
- 外側 shell が `fg` で attach を起こすと、(2a)(2b) が走るが、その前にユーザが Ctrl-Z を再度押した順序によっては race

これは **Issue 2 「2度目 Ctrl-Z で attach が無応答」と整合する仮説**だが、code lens のみでは確証不可。Phase 2 で kawaz の手元検証が必要。

仮にこの race が真因なら、対策候補:
- (2a) を (2b) より先に、かつ raise() 直後 (= 復帰直後の最初の動作) に置く
- raise() の前後で signal mask を block(SIGTSTP) して critical section を作る (= sigprocmask)

## Claude 代理で再現できなかった範囲

以下は kawaz の手元 (interactive terminal) でしか観測できない:

1. **実 Ctrl-Z 押下による Issue 1 の再現**: 外側 terminal で claude/vim の TUI を立ち上げ → 実 Ctrl-Z 押下 → 子の orphan discard で無応答になるかの体感確認。本 finding の C-1/C-2/C-3 で代理検証済みだが、TUI app のフル文脈 (= alt screen + raw mode + 内部 SIGWINCH handler 等) を含む状況での再確認は kawaz 手元で。
2. **Issue 2 の race 再現**: attach client を起動 → 1 度 Ctrl-Z で抜ける → fg で復帰 → すぐ 2 度目 Ctrl-Z → attach が無応答になるかの体感確認。code lens で race 余地は示したが、実機での race window が十分大きいか (= 50ms sleep の中で signal が届くか) は不明。
3. **外側 terminal multiplexer (cmux/tmux/libghostty) との相互作用**: hyoui attach が外側 terminal の job control にどう interact するかは Claude 環境では PTY を持たないため検証不能。

これらは Phase 2 で kawaz の手元検証を依頼する範囲。

## 関連

- workflow wfmln0gmq (Ctrl-Z bug 調査)
- DR-0001 jobcontrol 2 軸 (= 軸1 follow / 軸2 廃止 with auto-resume fallback)
- DR-0015 §2.2 (= child stopped notify path), §2.3 (= attach signal monitor thread)
- POSIX SUS `kill(2)`, `signal.h` (orphan process group rule)
- 過去 finding: `2026-05-27-jobcontrol-matrix-verification.md` (= 同テーマの先行検証)
