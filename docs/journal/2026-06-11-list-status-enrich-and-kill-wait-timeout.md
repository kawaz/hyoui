# journal: list/status 子情報拡充 + kill --wait timeout/escalation

- Date: 2026-06-11
- 解決した issue (削除済み):
  - docs/issue/2026-06-11-feature-list-child-process-info.md
  - docs/issue/2026-06-11-feature-kill-wait-escalation.md

## 機能 1: list/status 子プロセス実行時情報

StatusResponse に追加 (全て serde default で後方互換、cap flag 不要):

- `daemon_pid: u32` — daemon process の pid (= ps 突き合わせ起点)
- `child_pgid: Option<u32>` — 子 PTY の pgid (= 子は session leader なので通常 pid と一致、
  daemon は `getpgid(2)` で実測して載せる)
- `child_state: ChildLiveState` — running / stopped / exited(code)。既存
  `child_pid` (None=exited) + `child_stopped` (bool) を包含する整理表現。
  後方互換のため旧 2 field も残し、`ChildLiveState::from_legacy` が導出の正本。

表示:

- `hyoui status` (plain): daemon-pid / child-pid + pgid / child-state を追加
- `hyoui status --format=json`: daemon_pid / child_pgid / child_state を追加
- `hyoui list` (plain): PID 列を追加 (8ch)。STATUS は live | stopped | stale (従来通り)
- `hyoui list --format=jsonl`: child_pid / child_pgid / child_state を追加

## 機能 2: kill --wait の timeout + SIGKILL 昇格

### CLI 仕様

- 裸 `--wait`: default timeout 10s (`KILL_WAIT_DEFAULT_TIMEOUT_MS`)。
  根拠: 対話 app の graceful shutdown に十分 + TERM 無視の子で無駄に待たない閾値
- `--wait=<DUR>`: 既存 `parse_duration_ms` 形式 (inline `=` のみ。`--wait demo` の
  次 arg は session-id 扱い)
- timeout 超過 = **exit 3** (= 子は生かす、session は無傷で残る)。
  exit code 3 の根拠: 1 (= connect/reject 失敗) と区別して「昇格付きで再実行すれば
  解決する」ことをスクリプトが判定できるようにする
- `--kill-on-timeout`: timeout 後 SIGKILL に昇格して見届け。`--wait` なしは parse エラー

### daemon 側 deadline の設計判断 (= 採用: client 駆動 + daemon は wait しない)

kawaz 提示の 2 案 (protocol で deadline を渡す / client 切断を daemon が検知) の
**どちらでもない第 3 案**を採用した:

> `--wait` でも client は `Kill { wait: false }` (= 即時 ack mode) を送る。
> daemon は signal 送信 + KillAck だけで serve を継続し、「見届け」は client が
> `SessionExitNotify` / EOF を自前 deadline 付きで待つことで実現する。
> 昇格は client が SIGKILL の Kill をもう 1 発送るだけ。

理由:

1. **timeout 時に session を無傷で残せるのは client 駆動だけ**。daemon 側
   deadline 案 (当初実装した) は `TerminateSession` 経路に入った時点で session の
   teardown が始まっており、timeout で「やっぱり止める」と daemon は exit する
   しかない (= 子は生きるが session が消える)。「エラー終了 = 子は生かしたまま、
   何も変わらない」という仕様に合うのは client 駆動
2. **daemon に blocking 見届けが一切無い** = 孤児 daemon が構造的に発生しない
3. **protocol 変更ゼロ** (= 既存 Kill{wait:false} + SessionExitNotify の組合せで成立)。
   最小介入 (DR-0014 self-check)

加えて daemon 側の安全網として、`finalize_child` (= legacy `Kill{wait:true}` client と
`--until` match の経路) に **bounded escalation** を入れた:
SIGCONT+SIGTERM → 5s grace (`FINALIZE_TERM_GRACE`) を waitpid(WNOHANG) polling →
超過で SIGKILL → blocking reap。従来は timeout なし blocking waitpid で、TERM を
ignore する子を撃つと daemon が永久 block していた (= 2026-06-11 孤児 daemon の根源)。
SIGKILL は catch 不能なので finalize は必ず有限時間で返る。

### stopped な子への SIGCONT 併送 (shell 慣行)

stopped な子に terminate 系 signal を送っても pending のまま配送されない。
shell の job control 慣行に倣い、**殺す意図の signal (TERM/INT/HUP/QUIT/ABRT) に
SIGCONT を併送**して起こしてから効かせる (`signal_should_cont_first`)。
CONT/KILL/STOP/TSTP/USR 系は併送しない。`handle_kill` (default 即時 kill) と
`finalize_child` の両方に配線。

### client 側 --wait loop の要点

- 通算 deadline 管理 (= broadcast 受信のたびに read timeout を「残り時間」で
  設定し直す。LeaderNotify 等で wait が延びない)
- `SessionExitNotify` 受信 = 見届け完了 (exit 0)。EOF も成功扱い (= wait:false kill
  では daemon は子が死んだ時しか session を畳まないため)
- deadline 到達 → `--kill-on-timeout` なら SIGKILL Kill を送って見届け継続
  (昇格後 budget 10s、超えたら D-state 等の異常として exit 1)。なしなら exit 3

## 実機検証 (2026-06-11、release build、namespace=verify-feat)

| # | 内容 | 結果 |
|---|---|---|
| 1 | list に PID 列 / status に daemon-pid・child-pid+pgid・child-state | ✓ ps -o pid,pgid と一致 |
| 1' | SIGTSTP 後: status child-state=stopped / list STATUS=stopped / jsonl child_state | ✓ |
| 2 | `kill --wait=2s` + `trap '' TERM` の子 → 2.06s で exit 3、子生存、daemon 健在、session 無傷 (live のまま) | ✓ |
| 3 | `kill --wait=2s --kill-on-timeout` → 2s で昇格メッセージ → 2.42s で exit 0、子死亡、daemon 終了、残骸なし | ✓ |
| 4 | stopped (stat=TN+) の子に default kill → SIGCONT 併送で TERM が効いて死亡 | ✓ |
| 5 | 正常系 bare `--wait` (sleep 子) → 0.055s で見届け exit 0 (timeout を待たない) | ✓ |

検証後の掃除済み (verify-feat namespace 空、孤児プロセスなし)。

ignored テスト: `lock_acquire_prints_token_and_blocks_until_sigterm` が並列実行で
fail したが、`#[ignore = "flaky: SIGTERM タイミング依存"]` 明記済みの既知 flake で、
単独実行 0.71s pass を確認 (= 本変更とは無関係)。jobcontrol_follow のハングは
既知 issue (別 agent が修正中) のため対象外。

## 同日の他修正 (並行 3 波)

- **jobcontrol_follow ハング根治**: 二段構造 (① テストハーネス kill() の無限 child.wait —
  stopped child を抱えた daemon が ctty を保持し leader が exit しきれない、② attach の
  tcsetattr(TCSAFLUSH) が「master を読まないテスト」と相互待ち)。ハーネスに
  pump_pty / deadline 群を導入し ignored 12 件が 2 連続 all pass (旧 14-27 分ハング → 数秒)。
  副産物で実装バグ 2 件修正: lock acquire の SIGTERM handler 設置順 race (flaky の正体)、
  lock テストの Ro-mode kill no-op (28s → 0.7s)
- **cwd 透過 (dogfooding 初日の最優先バグ)**: daemon の chdir("/") が子に継承されていた。
  DaemonConfig.cwd (表示専用だった) を fork〜exec 間の chdir に配線。chdir 失敗は
  _exit(127) で明確に失敗
- **socket /tmp 化 (ENAMETOOLONG)**: macOS の TMPDIR (~50 文字) + ns + session 名が
  sun_path 上限 104B を超えた。base を /tmp/hyoui-<uid> に固定 (tmux 前例)、
  resolve 時 + bind/connect 直前の二重事前チェックで friendly error。breaking (v0.x)
