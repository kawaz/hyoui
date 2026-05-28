# bug: SIGCONT invariant 回復が動かない

- Date: 2026-05-28
- Priority: 高 (= DR-0001 軸 1/2 の根幹挙動)
- Status: 未着手

## 現象

実機検証手順:

1. `hyoui run -- claude` 起動 (= cmux 内など、任意環境)
2. `kill -TSTP <hyoui pid>` 送信 → hyoui + claude 両方 STOPPED 確認 ✓
3. `kill -CONT <hyoui pid>` 送信 → **hyoui だけ復帰** (= stat `S`)
4. **claude は STOPPED のまま** (= stat `Ts+`)

DR-0001 の §invariant 回復ルール (= 親 SIGCONT 復帰時に子が STOPPED なら `killpg(child, SIGCONT)`) が
**動いていない**。

## 確認済 / 未確認

### 確認済
- grep で SIGCONT handler 実装は存在 (= `crates/hyoui/src/daemon/session.rs` line 587-680 周辺)
- agent #34 (DR-0001 軸 1/2 実装) 完了報告: 「SIGCONT byte 観測時に waitpid で子確認 → STOPPED なら `killpg(SIGCONT)`」
- harness test (= matrix_test.rs) は 15 cell pass、ただし **「軸 2 transparent: 親 SIGTSTP 時に子も連動 STOPPED」までしか観測してない**
- 復帰側 (= invariant 回復) は test されてない (= B1/B4/B5 は STOPPED まで観測、CONT 後の状態は未検証)

### 未確認 (= 新 session が調査する内容)
- self-pipe に SIGCONT が乗っているか (= signal handler が write しているか)
- serve_loop の poll 経路で SIGCONT byte を読んで dispatch しているか
- `waitpid(WNOHANG | WUNTRACED)` の return value 解釈
- fg 経路 (= shell job control 経由の continue) は動くか? 外部 kill -CONT 経路だけ動かないのか?

## 調査方針

1. **signal handler** で SIGCONT を catch して self-pipe に書いてるか確認
   - `crates/hyoui/src/daemon/session.rs` 内で `signal_hook` 系の register を grep
2. **serve_loop の dispatch** が SIGCONT を `killpg(child_pgid, SIGCONT)` 経路に流すか確認
3. **waitpid 観測** が WUNTRACED で子の STOPPED 状態を見えているか確認 (= WSTOPPED + WIFSTOPPED)
4. **fg 経路 vs 外部 kill -CONT 経路** の挙動差を実機で観測
   - fg: `Ctrl-Z` → shell `fg %1` → `tcsetpgrp` + `SIGCONT` to pgrp
   - 外部: `kill -CONT <hyoui pid>` のみ (= 子の pgrp に CONT が届かない、これが root cause かも)

### 仮説候補

「外部 kill -CONT は **hyoui の pid にしか CONT を送らない**、hyoui の SIGCONT handler が子の pgrp にも
forward する必要があるが、その forward が動いていない」

→ POSIX 仕様確認: kernel が自動で子に SIGCONT を伝播するか? (= 答え: しない、明示 forward 要)

→ 確認 → 修正 → harness test に CONT 後の状態観測 cell を追加

## 修正後の検証

harness test (= `crates/hyoui/tests/matrix_test.rs`) を拡張:

| cell | 既存 | 追加 |
|---|---|---|
| B1/B4/B5 | TSTP → STOPPED 確認 | + CONT → 両方 stat `S` 確認 |
| 新 cell C1 | (新規) | 外部 kill -CONT で子も連動復帰 |
| 新 cell C2 | (新規) | fg 経路 (= tcsetpgrp + SIGCONT to pgrp) で復帰 |

DR-0014 §検証主義に従い、最低 3 種類 category (= sh / claude / vim 等) で確認。

## 関連 file / line

- `crates/hyoui/src/daemon/session.rs` line 587-680 (= handle_suspend_signals 周辺)
- `crates/hyoui/tests/matrix_test.rs` (= 既存 15 cell、CONT 検証は未実装)
- DR-0001 §invariant 回復ルール
- findings/2026-05-27-jobcontrol-matrix-verification.md (= matrix tests 結果記録、CONT 側未検証を明記)

## 注意

- Issue 1 (= termios 復元漏れ) とは **独立**、ただし修正経路は同じ handler に手を入れる
  → 1 つの commit にまとめる判断もあり
- ただし論理単位は分離した方が DR との対応がクリア (= Issue 1: termios、Issue 2: signal forward)
