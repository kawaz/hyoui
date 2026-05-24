# SIGTSTP の cargo test 環境ブロックと本番 binary の動作差

- Date: 2026-05-22
- 関連: 段階 2 (sys 層 ctty 検証テスト)、段階 4 (agent イベントループ)、DR-0001 (bg/fg ジョブ制御 2 軸)
- 結論: **SIGTSTP の到達がブロックされるのは cargo test ハーネス環境固有。本番 binary では正常動作する**

## 判明した事実

1. **cargo test 環境では SIGTSTP が子プロセスに届かない**ことがある。段階 2 の ctty 検証テスト (`forkpty_child_has_ctty_and_can_be_stopped`) で観測。発生原因は (おそらく) cargo test runner 側の signal mask か、test harness の親プロセス group 構造による。
2. **本番 binary (`target/release/hyoui`) では SIGTSTP が期待通り動作する**。今回の本 finding の smoke で確定。
3. 回避策として段階 2 では **SIGSTOP** (catch/ignore 不能、環境に依存せず deterministic) に切り替えてテストを通した。本質的な ctty 獲得の証明は `tcgetpgrp(master) == child_pid` で別途取れているので、テストの主旨は損なわれていない。

## 実用的な示唆 / ベストプラクティス

- **ジョブ制御の Rust ユニットテスト/統合テストでは SIGSTOP を使う**。SIGTSTP は cargo test 環境で deliverable が不安定なので避ける
- **本番動作の verify はジョブ制御を伴う smoke を別途回す**。`cargo test` だけでは「実環境で Ctrl-Z が効くか」までは保証されない
- `cargo nextest` や test harness を変えれば SIGTSTP も通る可能性あり (未検証、将来の調査候補)
- 同じパターンが他の「ターミナル経由でしか自然に届かないシグナル」(SIGINT, SIGQUIT, SIGHUP の一部) でも起きうるので注意

## 検証の詳細

### smoke 手順

```bash
cargo build --release
./target/release/hyoui run --mode=headless --on-parent-suspend=transparent -- sleep 30 &
parent_pid=$!
sleep 1
child_pid=$(pgrep -P $parent_pid | head -1)

# 状態観測 → SIGTSTP → 観測 → SIGCONT → 観測 → cleanup
ps -o pid,ppid,stat,command -p $parent_pid -p $child_pid
kill -TSTP $parent_pid
sleep 0.5
ps -o pid,ppid,stat,command -p $parent_pid -p $child_pid
kill -CONT $parent_pid
sleep 0.5
ps -o pid,ppid,stat,command -p $parent_pid -p $child_pid
kill $parent_pid
wait $parent_pid
```

### 結果テーブル (macOS Darwin 25.5.0、本番 binary v0.0.0)

| 段階 | 親 hyoui STAT | 子 sleep STAT | 期待 | 結果 |
|---|---|---|---|---|
| 初期起動 | `RN` (running) | `SNs+` (sleeping) | 両者アクティブ | ✓ |
| SIGTSTP 親に送信 | `TN` (stopped) | `TNs+` (stopped) | **親 stop → 子も stop (transparent 伝搬)** | ✓ |
| SIGCONT 親に送信 | `RN` (running) | `SNs+` (sleeping) | 両者再開 | ✓ |
| `kill` で cleanup | (終了) | (終了) | 親の Drop で TtyGuard 復元 / pty close / 子終了 | ✓ |

### 段階 2 の cargo test での観測 (比較)

段階 2 worker の報告から引用:
> "当初 SIGTSTP で stop を観測しようとしたが、Rust cargo test ハーネス環境では SIGTSTP の delivery がブロックされる現象に遭遇 (debug 未深掘り、おそらく test runner 側の signal mask)。SIGSTOP は catch/ignore 不可なので環境に依存せず deterministic、かつ「ctty 獲得」の本質的な証明は `tcgetpgrp(master) == child` 側にあるので test の主旨は損なわれない。"

### 考察

cargo test の test binary は通常以下の特徴を持つ:
- 親が cargo runner (test harness)、その親が cargo (build script + test orchestrator)
- 各 `#[test]` は別 thread で並列実行されることがある (`--test-threads=N`)
- test harness が自前で signal handling を持ち、SIGTSTP/SIGCONT を内部で消費 or 再分配している可能性

本番 binary は端末から直接起動され、`hyoui` 自身が session leader (forkpty + login_tty 経由)。SIGTSTP は親 hyoui のプロセス group / ctty を介して素直に届く。

## 関連

- `docs/decisions/DR-0001-bgfg-jobcontrol-two-axis.md` — ジョブ制御 2 軸の設計、transparent / decouple
- `docs/decisions/DR-0003-rust-only-and-forkpty-login_tty.md` — forkpty + login_tty で ctty 獲得確実化
- `docs/journal/2026-05-22-rust-rewrite.md` — 段階 2 worker の報告原文
