# Ctrl-Z 不達の二層原因と session anchor 案 (daemon 兼任) の PoC 検証

> 検証日: 2026-06-10。検証者: Claude main session + kawaz 実機 (実 terminal での ^Z 押下)。
> バイナリ: v0.2.6 (`/tmp/hyoui-v0.2.6-pre-fix` に退避した修正前 build)。
> 関連 issue: [2026-05-29-bug-claude-tui-ctrl-z-not-stopping](../issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md) (正本)。
> 本 findings は同 issue の「調査結果 2026-06-10 (subagent)」を補完する (= 層 2 の発見と修正方針の更新)。

## 判明した事実

1. **原因は二層構造**。層 1 = orphan process group での SIGTSTP discard (kernel 仕様、
   issue 側マトリクスで確定済)。層 2 = **daemon の auto-resume が、止まった子を即
   SIGCONT で起こす** (`session.rs` の DR-0001 軸 1 実装。`OnChildSuspend::AutoResume`
   に加え、leader 不在時の fallback も無条件 `killpg(child, SIGCONT)`)。
   層 1 をすり抜けて子が止まっても (SIGSTOP / handler 経由 raise)、層 2 が起こす。
2. **tmux も全く同じ制限を持つ** (= PTY ラッパー一般の構造問題で、tmux は放置している)。
   `tmux new -d -- /bin/sleep 600` の sleep は session leader の orphan pgrp になり、
   `send-keys C-z` も直接 `kill -TSTP` も無効 (Ss+ のまま)。tmux ユーザが困らないのは
   典型ユースが shell 起動で、shell が job control を肩代わりするから。
3. **hyoui でも shell 経由なら ^Z は正常動作する** (回避策)。`hyoui run -- zsh` 内で
   `sleep 600` → `input hex:1a` で `zsh: suspended` を screen dump で確認。
   zsh の子は同 session に親 (zsh) がいるので orphan ではない。
4. **session anchor 案 (= daemon 兼任案) は macOS / Linux glibc / Linux musl の
   3 platform 全てで実装可能と PoC で確定**。
   setsid 済みの親が `openpty` + `TIOCSCTTY` で controlling tty を取り、子を
   同 session・別 pgrp・foreground で fork する構造なら、`kill -TSTP` も
   line discipline 経由の ^Z byte も **SIGTSTP 本来のセマンティクス** (catch 可能、
   suspend cleanup が走る) で動作する。
5. kawaz 実機で「^C で prompt に戻ったのに session と子 (sleep) が生きたまま、
   attach プロセスも残存」という現象を観測 (cz1)。^C も子に届いていなかった可能性
   があり、attach の終了経路として別途観測が必要 (未確定、再現手順未確立)。

## 実用的な示唆 (修正方針)

| 案 | 内容 | 評価 |
|---|---|---|
| A4 (issue 推奨) | attach が 0x1a を intercept → daemon が `killpg(SIGSTOP)` | fallback 扱い。SIGSTOP は catch 不可なので **TUI の suspend cleanup (alt screen 解除・termios 復元) が走らず raw のまま凍結**。SIGTSTP セマンティクス喪失 |
| supervisor 分離 | forkpty の中 (新 session 内) に anchor プロセスを挟む | 不採用方向。supervisor (= session leader) を kill -9 すると kernel が foreground pgrp に SIGHUP → **child 巻き添え死**という新故障モード + pid/exit code 中継の実装増 |
| **session anchor (本命)** | forkpty 廃止。daemon (setsid 済) が `openpty` + `TIOCSCTTY` で session anchor になり、child を同 session・別 pgrp・foreground で fork | PoC 済 (macOS)。^Z が本来のセマンティクスで動く。プロセス追加なし。**zsh 直接起動と同型のプロセス構造になるため透過性はむしろ向上** (現行の「child = session leader」の方が直接起動と違う) |
| 割り切り | docs 明記 + 外側 API (`kill -s STOP`) に寄せる | UX が悪い。tmux と同レベルに留まる |

- session anchor 案の残課題: `forkpty` → 手組み化 (DR-0003 改訂)、daemon が tty 状態を
  持つことの整理 (DR-0005 改訂、`tcsetattr` 呼び出し時の SIGTTOU ignore)、
  1 controlling tty 制約 (1 daemon 1 session モデル維持が前提)。
  Linux マトリクス検証は完了済 (下記)。
- **どの案でも層 2 (auto-resume policy) の見直しが必須**。「ユーザ/端末起因の stop は尊重し、
  誰も起こせない状況の救済をどう定義するか」の線引きを DR-0001 改訂で行う。

## 検証の詳細

### 層 2 の分離実験 (daemon auto-resume の直接証明)

対象: `hyoui run --detached -- /bin/sleep 6000` (daemon=5376, sleep=5377, 両者とも独立 session leader)

| 操作 | sleep の stat | 解釈 |
|---|---|---|
| daemon 稼働中に `kill -TSTP 5377` | Ss+ (50ms×30 サンプル全て) | 層 1 discard (または層 2、この実験単独では不分離) |
| daemon 稼働中に `kill -STOP 5377` | Ss+ (50ms×30 サンプル全て、一度も T を観測せず) | SIGSTOP は discard 不可能 → **daemon が即 CONT している** |
| `kill -STOP 5376` (daemon 停止) 後に `kill -STOP 5377` | **Ts+ (止まったまま)** | daemon が止まっていれば子も止まる = auto-resume の証明 |
| daemon 停止中に `kill -TSTP 5377` | Ss+ | **層 1 (orphan discard) も独立に実在** (二層の分離) |
| `kill -CONT 5376` (daemon 再開) | sleep も S に戻る | daemon の invariant 回復 (`handle_suspend_signals`) が子を CONT |

sandbox の signal 遮断は対照実験 (自前の子は `kill -STOP` で TN になる + `dangerouslyDisableSandbox` でも同結果) で棄却済み。

### tmux 対照実験

```
$ tmux new-session -d -s cztest -- /bin/sleep 600
  → sleep: PGID=PID, Ss+ (session leader = orphan pgrp、hyoui と同型)
$ tmux send-keys -t cztest C-z   → Ss+ (変化なし)
$ kill -TSTP <pid>               → Ss+ (discard)
```

### shell 経由の正常動作確認

```
$ hyoui run --detached --session=czsh -- /bin/zsh -i
$ hyoui input czsh "text:/bin/sleep 600" "key:Enter"
$ hyoui input czsh "hex:1a"
$ hyoui screen dump czsh | tail
  → ^Z
     zsh: suspended  ...        # zsh の job control が正常に処理
```

### session anchor PoC (50 行 C、3 platform マトリクス)

| platform | arch | libc | mode A: kill -TSTP | mode A: ldisc ^Z | mode B (forkpty 同等): 両経路 |
|---|---|---|---|---|---|
| macOS (Darwin 25.5) | arm64 | - | STOPPED ✓ (sig=18) | STOPPED ✓ | NOT stopped ✗ (discard 再現) |
| Debian 13.5 (Docker) | aarch64 | glibc | STOPPED ✓ (sig=20) | STOPPED ✓ | NOT stopped ✗ |
| Alpine (Docker) | aarch64 | musl | STOPPED ✓ (sig=20) | STOPPED ✓ | NOT stopped ✗ |

mode A の ldisc 経路 (= master への 0x1a write) が実際の ^Z 経路。

mode A = 親が `setsid` → `openpty` → 親側 `ioctl(slave, TIOCSCTTY)` → fork した子で
`setpgid(0,0)` + `tcsetpgrp(slave, getpid())` (親側でも race 対策の `setpgid` +
`SIGTTOU` ignore + `tcsetpgrp`)。mode B = 子側で `setsid` + `TIOCSCTTY` (forkpty 同等)。

```c
// poc.c (要点のみ。全文は本検証時 /tmp/poc-ctty/poc.c)
setsid();                                  // daemon 役: session leader + ctty 無し
openpty(&master, &slave, NULL, NULL, NULL);
ioctl(slave, TIOCSCTTY, 0);                // daemon が controlling tty を獲得
pid_t c = fork();
if (c == 0) {                              // child: 同 session・別 pgrp・foreground
    setpgid(0, 0);
    tcsetpgrp(slave, getpid());
    dup2(slave, 0); dup2(slave, 1); dup2(slave, 2);
    execlp("/bin/sleep", "sleep", "600", NULL);
}
setpgid(c, c);
signal(SIGTTOU, SIG_IGN);
tcsetpgrp(slave, c);
// → kill(c, SIGTSTP) / write(master, "\x1a", 1) の両経路で WUNTRACED が WIFSTOPPED を返す
```
