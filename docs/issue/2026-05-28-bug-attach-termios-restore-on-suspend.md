# bug: hyoui attach プロセスでも termios 復元が必要 (Issue #1 派生)

- Date: 2026-05-28 (Issue #1 修正後に切り出し)
- Priority: 中 (= run --detached → 別端末 attach のシナリオで cmux freeze 再発の可能性)
- Status: 未着手
- 関連: [2026-05-28-bug-termios-restore-on-suspend.md](./2026-05-28-bug-termios-restore-on-suspend.md) (= 親 Issue)

## 現象

Issue #1 (= hyoui run 非 detached 経路) は修正済 (commit 2751ff28)。ただし以下の
シナリオは未解決:

```bash
# 端末 A
$ hyoui run --detached -- claude
/path/to/sock.sock

# 端末 B (別 terminal、cmux 内など)
$ hyoui attach run-XXX     # ← この attach プロセスが raw mode 化
# 端末 B 内で Ctrl-Z または 別端末から kill -TSTP <attach-pid>
# → attach プロセスが raw mode のまま STOPPED
# → 端末 B の cmux / libghostty が freeze
```

attach プロセスは **daemon と別 process** のため、Issue #1 修正 (= daemon thread の
SIGTSTP handler から TtyGuard.suspend() callback) が効かない。

## 真因

`crates/hyoui-cli/src/main.rs::attach_command` (line 461 周辺) で `_raw_guard` を
保持しているが、SIGTSTP handler が install されていない。SIGTSTP のデフォルト動作
(= プロセスが STOPPED になる) では termios 復元が走らない。

## 修正方針

### 制約

- `crates/hyoui-cli` は `#![forbid(unsafe_code)]` (= main.rs:13)。sigaction install /
  AtomicPtr 経由の termios 受け渡しは hyoui ライブラリ側 (sys/signal.rs) に置く必要
- handler 内処理は async-signal-safe 限定。`tcsetattr` は POSIX で async-signal-safe、
  `raise` も async-signal-safe。ただし Rust の `Mutex` / `Arc<Mutex>` は ❌
- → handler 内で参照する termios は **`AtomicPtr<Termios>` で leak 経由**保持

### 案 A: sa_handler 内で直接処理 (scope 最小)

`hyoui::sys::signal` に `install_attach_suspend_handlers(saved: Termios)` を追加。
内部で:

1. `saved` を `Box::leak` して `AtomicPtr<Termios>` に store
2. SIGTSTP handler を install:
   - handler: `tcsetattr(STDIN_FILENO, TCSAFLUSH, &saved)` → SIGTSTP を `SIG_DFL` に
     戻す → `raise(SIGTSTP)` で kernel に STOP させる
3. SIGCONT handler を install:
   - handler: `cfmakeraw(saved.clone())` → `tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw)`

### 案 B: ClientConnection::run に self-pipe 統合 (筋は良いが API 変更大)

`ClientConnection::run` の poll loop に SIGTSTP/SIGCONT self-pipe fd を追加し、
handler 外で同期的に処理。`raise(SIGSTOP)` も loop 内同期 path で実施。
- 利点: handler 内 unsafe 最小、設計上きれい
- 欠点: ClientConnection の API 変更、`hyoui::client` library 利用者 (= hyoui-serve 等) に影響

### 推奨

案 A を MVP、案 B は API 安定化フェーズで再評価。

## 関連 file

- `crates/hyoui-cli/src/main.rs::attach_command` (line 461)
- `crates/hyoui/src/sys/signal.rs` (= install_self_pipe / register_self_pipe 既存、参考)
- `crates/hyoui/src/sys/tty.rs::TtyGuard` (= saved field 流用)

## 検証マトリクス

Issue #1 と同じマトリクスを attach プロセスで実施:

| 起動環境 | 経路 | 修正前期待 | 修正後期待 |
|---|---|---|---|
| 素 terminal | hyoui attach + Ctrl-Z | freeze の可能性 | 復帰可 |
| 素 terminal | hyoui attach + 外部 kill -TSTP | freeze の可能性 | 復帰可 |
| cmux 内 | hyoui attach + Ctrl-Z | 既知 freeze | 復帰可 |
| cmux 内 | hyoui attach + 外部 kill -TSTP | 既知 freeze | 復帰可 |
| tmux 内 | 同上 | ? | 復帰可 |
