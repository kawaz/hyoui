# client / daemon / child のシグナル・サスペンド相互作用 監査結果

- Date: 2026-06-11
- 調査方法: Fable サブエージェント + Codex の独立 2 系統で静的監査し、main セッションで突き合わせ
- **全てコード読みベース (実機未検証)**。実機マトリクス検証が必要な項目は末尾に明記

## 判明した事実

### 1. 相互作用マトリクス (現状実装)

役割: client = `hyoui attach` プロセス / daemon = detached daemon (DR-0017 で session anchor = session leader + controlling tty 保持) / child = PTY 配下の対象プロセス。

#### client 起点

| 事象 | daemon への影響 | child への影響 | 根拠 |
|---|---|---|---|
| SIGTSTP 受信 | なし (何も送らない) | なし (走り続ける) | `main.rs:68-117` (termios cooked 復元 → raise(SIGTSTP))、`main.rs:701-703` |
| SIGSTOP 受信 | なし | なし | handler 不可 (kernel 制約)。termios raw のまま停止し外側表示が崩れる |
| SIGCONT 復帰 (外部 TSTP から) | なし (通知なし) | なし | `main.rs:109-115`。raw 再設定のみ、**画面再描画なし** |
| SIGCONT 復帰 (child follow 停止から) | `SessionChildResumeRequest` 送信 | daemon が redraw push **後に** `killpg(SIGCONT)` (順序保証) | `attach.rs:437-474`、`control.rs:313-371` |
| detach (Ctrl-A d) | DropClient + leader cascade | なし | `attach.rs:520-537`、`session.rs:1365-1404` |
| 死亡 (SIGKILL 含む) | socket EOF → DropClient。**client 0 でも daemon 常駐継続** | なし | DR-0015 §2.3.1。SIGKILL 死では外側端末が raw 残留 (`reset` 必要) |
| suspend 中の出力蓄積 | per-client queue 8 MiB 超過で daemon が **client を切断** (`backpressure.disconnect`) | — | `broadcast.rs:44-46, 163-190`。長時間 ^Z + 大量出力で fg 時に接続消失 |
| SIGINT / SIGTERM / SIGHUP / SIGWINCH | handler 未登録 (default 挙動) | — | `main.rs:86-88`。raw mode 中の Ctrl-C は byte として child へ透過 |

#### child 起点

| 事象 | daemon の挙動 | client の挙動 | 根拠 |
|---|---|---|---|
| self-suspend (^Z / kill -STOP) | SIGCHLD self-pipe で検知 → `child_stopped=true` + record + **leader (cap `child-state-v1`) にのみ notify**。介入しない (auto-resume コード現存せず) | leader は **無条件 follow ハードコード**: 安全側 escape 全解除 → cooked → raise(SIGSTOP)。非 leader は通知されず画面停止に見えるだけ | `session.rs:815-879`、`attach.rs:437-455` |
| 外部 SIGCONT で再開 | `child_stopped=false` + record のみ。**client への Continued 通知 message が protocol に存在しない** | follow 停止中の client は**止まりっぱなし** (復帰は外側 shell の `fg` のみ) | `session.rs:890-906`、`protocol/messages/mod.rs` |
| exit / signal 死 | 200ms drain → reap (signal 死は 128+signum) → `SessionExitNotify` broadcast → linger 2s → daemon exit | exit code をそのまま伝搬して終了 | `session.rs:460-601`、`attach.rs:434-435`、`main.rs:770-781` |

#### daemon 起点

| 事象 | child への影響 | client への影響 | 根拠 |
|---|---|---|---|
| SIGTSTP 受信 | **実装上シグナルが吸い込まれる** (self-pipe 登録済みだが処理側が byte を無視 = default stop にもならない)。コメントは「kernel default で STOPPED」と主張しており実装と乖離 | — | `session.rs:177-181, 787-812`、`sys/signal.rs:217-225` (codex 発見) |
| SIGCONT 受信 | waitpid で child stopped なら child pgrp に SIGCONT | 通知なし | `session.rs:795-803` |
| SIGTERM / SIGINT / SIGHUP | **handler 未登録 → 即死**。graceful shutdown (escalation / ExitNotify / socket unlink) が全て走らない | socket EOF → **exit 0** (正常 detach と区別不能) | `session.rs:173-181`、`attach.rs:483-493` |
| SIGKILL 死 | **【要実機検証】child SIGHUP 巻き添え死の疑い**: daemon = session leader + controlling tty 保持のため、POSIX 上 controlling process 死亡で fg pgrp (= child) に SIGHUP。DR-0017 が supervisor 案を却下した理由 (leader kill -9 → child 巻き添え) を anchor 案自身が持つ自己矛盾候補 | exit 0 + stale socket 残留 (`list --prune-stale` で掃除) | DR-0017 柱1 / Rejected alternatives、`sys/pty.rs:77-135` (Fable 発見) |
| `--until` match | `killpg(SIGTERM)` → finalize escalation (CONT+TERM → grace → KILL) | ExitNotify | `session.rs:957-960, 1271-1283, 1441-` |

#### サイズ伝搬 (SIGWINCH)

**完全に未配線 (実装漏れ)**。daemon 側 `Resize` handler (leader 限定、TIOCSWINSZ + ScreenState resize) は実装済みだが、**CLI 全体に Resize を送る経路が 1 つも無い**。attach client は SIGWINCH handler を install せず (`sys/signal.rs:97` の `install_winch` は production 呼び出しゼロ)、サイズが伝わるのは run 起動時の initial size のみ。DR-0006 §6 の MVP 主軸 `--window-size=leader` が受け手だけ実装の dead protocol path。— `control.rs:638-668`

### 2. オプション棚卸し (parse vs 消費)

| オプション | 状態 | 詳細 |
|---|---|---|
| `run --detached` | ✅ 配線済 | `main.rs:456-476` |
| `run --until` | ✅ 配線済 | DaemonizeInit → DaemonConfig → UntilWatcher。**daemon 側終了条件の唯一の正しい先例** |
| `attach --mode=rw\|ro\|rw-no-leader` | ✅ 配線済 | `main.rs:646-655` |
| kill/wait/lock 系 (--wait, --signal, --timeout, --mode 等) | ✅ 配線済 | 健全 |
| `run --mode=interactive\|headless` | ❌ no-op | 消費箇所ゼロ。preset の参照先 (軸1 default / 軸2) が DR-0015/0017 で両方消滅 |
| `run --on-child-suspend=follow\|auto-resume` | ❌ no-op | attach exec に不伝搬、HandshakeRequest に field 無し、client は follow ハードコード。**AutoResume は到達不能** |
| `run --timeout` / `--idle-timeout` | ❌ no-op | DaemonConfig に field すら無い。help/completion には現役記載 |
| `attach --exclusive` / `--detach-others` | ❌ dead field | wire には乗るが daemon 側に読む production コード無し |
| `screen dump/snapshot --timeout` | ❌ no-op | `let _ = cfg.timeout_ms` + NOTE 明示 |
| `--on-parent-suspend` | ❌ help 残骸 | parser から削除済みなのに `usage_run()` に記載残存 (`cli.rs:3277-3279`) = 指定するとエラーになるオプションが help に載っている |
| `StdinEofAction::SendEof` (API) | ❌ dead code | production call site ゼロ。`echo "1+2" \| hyoui run -- bc` は stdin EOF で切断するだけで **bc が daemon 配下に残る** |
| `SessionExitNotify.signal` (protocol field) | ❌ 常に None | DR-0015 の設計では補足情報だが実送信コードが常に None (codex 発見) |

### 3. 2 系統監査の一致点・相違点

- マトリクス・no-op リスト・`--mode` 削除推奨・pipe-through の「stdin tty 判定 + 明示フラグ」化は **完全一致**
- **唯一の意見相違 = auto-resume の再配置先**: Fable「daemon 側に配線すべき (auto-resume が本当に必要なのは誰も attach していない時で、client policy では発動者不在)」 vs Codex「daemon が覚える必然性は薄く client ローカルに縮退」。main 判断は Fable 支持 — codex 案は headless ユースケースで発動者がいない。ただし「ユースケースが出るまで削除して待つ」(外側 API `hyoui kill --signal=CONT` で代替可能、DR-0017 記載) が透過原則的には最有力
- Fable のみの発見: backpressure 8MiB 切断罠 / daemon kill -9 → child SIGHUP 巻き添え疑い / Continued 通知欠落 / daemon 死 = client exit 0 問題
- Codex のみの発見: daemon SIGTSTP 吸い込み / SessionExitNotify.signal 常時 None

## 実用的な示唆 (整理提案)

| 対象 | 提案 |
|---|---|
| `--mode` | **削除** (`pub enum Mode` ごと)。`attach --mode=rw\|ro` / `LockMode` との多重定義も解消 |
| follow (client が子に追従) | client ローカル挙動として**ハードコードのままが妥当** (attach 中の人間に follow 以外の合理挙動なし)。オプション不要 |
| auto-resume | 第一候補: **削除して待つ** (外側 API で代替可、透過原則整合)。残すなら **daemon 配線** (DaemonizeInit → DaemonConfig、notify の代わりに killpg(SIGCONT))。client 側 flag は作らない |
| `--timeout` / `--idle-timeout` | `--until` と同経路で **daemon 配線** (または削除)。終了条件の発動者は daemon に統一 |
| pipe-through | stdin 非 tty なら default `SendEof` + `--stdin-eof=detach\|send-eof` で opt-out (raw mode TUI に 0x04 が刺さるため opt-out 必須)。run → attach 伝搬 |
| `--exclusive` / `--detach-others` | 実装するか parse 段で「未実装」エラー化。silent no-op 放置は不可 |
| SIGWINCH | 既存 signal thread に WINCH を 1 本足して leader 時 `Resize` 送信 + attach 成立時に初回 Resize。**新規介入でなく DR-0006 §6 の実装漏れ修復なので最優先級** |

## 実機検証結果 (2026-06-11、hyoui 0.6.1 / macOS 26.5 arm64)

上記「要実機検証」3 項目を専用 jj workspace でビルドした実機バイナリで検証した。

### 仮説 1: daemon kill -9 → child SIGHUP 巻き添え死 — **確定 (全カテゴリ巻き添え死)**

anchor 構造の実機確認: daemon は `ppid=1` の session leader (`Ss`、PID=PGID=SID)、child は別 pgrp の foreground (`S+`)。

| child | カテゴリ | 期待 (仮説) | 実態 |
|---|---|---|---|
| `vim -u NONE -N` | TUI alt screen | 死亡 | **死亡** |
| `cat` | line-oriented | 死亡 | **死亡** |
| `python3` | interactive REPL | 死亡 | **死亡** |
| `sh -c 'trap ... HUP; while sleep'` | SIGHUP 直接観測 | HUP 受信 | **HUP 受信 (trap ログで直接観測、生存)** |

`kill -TERM <daemon>` でも同様 (handler 無し → 即死 → child SIGHUP 巻き添え)。いずれも **socket file が残骸として残る** (graceful cleanup 不走)。daemon kill -9 時の attach client は exit code **0** (コード読み通り、正常終了と区別不能)。

→ DR-0017 の supervisor 案却下理由 (「leader kill -9 → child 巻き添え死」) を anchor 案自身が持つことが確定。DR-0017 §Rejected に訂正注記、Consequences に帰結を追記済み。

### 仮説 2: daemon への SIGTSTP 吸い込み — **確定**

`kill -TSTP <daemon>` → daemon は `Ss` のまま (T にならない)、child も無影響。handler 登録 + 処理側 byte 無視により default stop も起きず、シグナルが実質吸い込まれる。

### 派生発見: daemon SIGCONT → child 連動起こしが不発 (コードと実機の乖離)

「daemon が SIGCONT で再開した時、child が stopped なら killpg(SIGCONT)」の防衛策コード (session.rs:795-803) が実機で発火しない:

- child を `kill -STOP` (`T+`) → daemon に `kill -CONT` → child は `T+` のまま
- daemon を `kill -STOP` → `kill -CONT` で実際に stop/resume させても → child は `T+` のまま
- child へ直接 `kill -CONT` すれば即復帰 (= child 自体は正常)

self-pipe → drain → killpg 経路のどこかで切れている疑い (SIGCONT 受信時に self-pipe write が走らない、または waitpid が Stopped を返さない)。→ docs/issue/2026-06-11-bug-daemon-signal-robustness.md に起票。

## 関連

- 発端 issue (mode-headless-not-wired) は DR-0019 に昇華して削除済
- DR-0001 (旧 2 軸 preset) / DR-0015 (run = fork + exec attach、軸2廃止) / DR-0017 (notify-only default、anchor 化)
