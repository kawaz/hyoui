# bug: termios 復元漏れによる cmux freeze (suspend 時)

- Date: 2026-05-28
- Priority: **最重要** (= 実機で完全 freeze 誘発、kawaz がアクティビティモニタ強制終了)
- Status: 未着手

## 現象

実機 `hyoui run -- claude` を **cmux (= manaflow-ai/cmux + libghostty)** 内で起動した状態で:

1. claude TUI で Ctrl-Z 押下 (= 直接押下経路)、または
2. kawaz が外部から `kill -TSTP <hyoui pid>` 実験 (= 外部 signal 経路)

→ hyoui (stat `T`) + claude (stat `Ts+`) が STOPPED (= 軸 2 transparent は動作確認 ✓)

→ `kill -CONT <hyoui pid>` で復帰試行

→ **cmux (= manaflow-ai/cmux + libghostty) 全体が freeze**
→ kawaz がアクティビティモニタで強制終了するしかなくなる

## 真因仮説 (= 確証高い)

hyoui の SIGTSTP handler に **termios 戻し処理が一切ない**:

- `crates/hyoui/src/daemon/session.rs` の `handle_suspend_signals` (line 603 周辺) で `raise(SIGSTOP)` を呼ぶだけ
- grep でこの経路の termios / tcsetattr 呼び出しゼロヒット
- `crates/hyoui/src/sys/tty.rs::TtyGuard::drop` (= line 63) は **serve_loop 抜け時のみ** termios 復元
- → suspend 中は外側 TTY が raw mode のまま放置

結果として:
- 外側 TTY = raw mode のまま STOPPED 状態
- cmux/libghostty が raw bytes (= line discipline OFF の入力) を受信待ち
- TTY の line discipline state と libghostty の期待が食い違って **無限待ち** → cmux freeze

## 修正方針

SIGTSTP handler 経路に termios 復元/再設定を組み込む:

```
SIGTSTP handler 経路:
  1. termios を「raw 前の状態」(= TtyGuard.saved) に tcsetattr で復元
  2. raise(SIGSTOP)
  ↑ kernel STOPPED
  3. (SIGCONT 復帰時) raw mode 再設定 (= cfmakeraw + IUTF8)
```

実装の助け:
- `TtyGuard::suspend()` / `TtyGuard::resume()` の helper を新設
- `handle_suspend_signals` から呼ぶ
- `tty.rs::TtyGuard` の `saved` field は既に存在 (= drop で使ってる経路を流用可能)

## 関連 file / line

- `crates/hyoui/src/daemon/session.rs` line 587-680 (= handle_suspend_signals)
- `crates/hyoui/src/sys/tty.rs` line 36-66 (= TtyGuard)

## 関連 DR

- DR-0001 §実装ノート — termios 言及なし (= 設計時の漏れ、本 issue 修正後に追記)
- DR-0014 §self-check — 「kernel / PTY / shell の標準機能を再発明していないか?」項に「termios state も観測したか?」を追加すべき (= Issue 5 で議論)

## 検証マトリクス (= 修正後に埋める)

| 起動環境 | 経路 | 修正前 | 修正後期待 |
|---|---|---|---|
| 素 terminal (Terminal.app) | claude TUI Ctrl-Z | ? | 復帰可 |
| 素 terminal | 外部 kill -TSTP | ? | 復帰可 |
| cmux 内 | claude TUI Ctrl-Z | freeze | 復帰可 |
| cmux 内 | 外部 kill -TSTP | freeze | 復帰可 |
| tmux 内 | claude TUI Ctrl-Z | ? | 復帰可 |
| tmux 内 | 外部 kill -TSTP | ? | 復帰可 |

DR-0014 §検証主義に従い、最低 3 種類 category (= 素 / cmux / tmux) でマトリクス検証する。

## 注意

- 本 issue 修正は **Issue 2 (SIGCONT invariant 回復) と独立**、ただし関連する (= 復帰経路は同じ handler)
- 修正後は `harness test` (= matrix_test.rs) にも `stty -a < /dev/tty<N>` 観測を追加 (= Issue 5)
