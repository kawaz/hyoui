---
title: "Feature: tx / lock / unlock CLI subcommand 実装 (DR-0006 §7)"
status: wip
category: task
created: 2026-05-27T00:00:00+09:00
last_read: 2026-06-22T08:15:00+09:00
open_entered: 2026-05-27T00:00:00+09:00
wip_entered: 2026-05-27T00:00:00+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# Feature: tx / lock / unlock CLI subcommand 実装 (DR-0006 §7)

- Priority: Mid (= MVP の自動操作排他は env fallback で機能しているが、外側 wrapper の標準入口が無いと UX が固い)
- 関連 DR: [[DR-0006]] §7 (Lock + tx 仕様正本)、[[DR-0006]] §8.5 (input family との関係)

## 進捗

### Done

- `hyoui lock acquire <session> [--mode=wait|fail] [--timeout=<dur>]`
- `hyoui lock release <session> --token=<T>` (= `HYOUI_LOCK_TOKEN` env fallback あり)
- `hyoui unlock <session> --token=<T>` (= `lock release` alias)
- parser unit test (= cli.rs に追加、20+ ケース)
- integration test (= `crates/hyoui-cli/tests/lock_cli.rs`、6 ケース)

採用 default + open question への解答:

| 項目 | 採用 | 根拠 |
|---|---|---|
| `--mode` default | `wait` | 「待つ」が直感的、`fail` は明示 opt-in (DR-0006 §7 では未明示) |
| `lock acquire` の生存戦略 | **block until SIGINT/SIGTERM/stdin EOF** | daemon は disconnect で auto-release するため、acquire を短命にすると token が即無効化される (= 開いた途端閉じる)。block 中は connection を保持しつつ token を stdout に print、シグナルや stdin EOF で release |
| daemon-side timeout / refcount / process-bound | **未実装のまま** | `LockAcquire` 内の `timeout_abs_ms` / `timeout_idle_ms` / `process_bound` field は daemon が `let _ = req;` で受け流すだけ。timer thread の新規追加は本 task の scope 外。CLI 側 polling で `--timeout` semantics を擬似実現 |
| wait queue (= `LockResult::Queued`) | **未実装のまま** | daemon は `wait=true` でも `Denied` を返す。CLI は `Denied` を見たら `--mode=wait` なら 100ms sleep して再送 polling |
| 別 process からの release | **daemon が holder client 照合で reject (LockNotHeld)** | `handle_lock_release` が `state.lock_holder == Some(ch_id)` を要求するため、新規 connection からの release は通らない。CLI は hint を出して exit 1 (= holder process に SIGTERM を促す) |

### Remaining (= 別 task)

- `hyoui tx <name> [--timeout-* ...] -- cmd args...`
  - 子 process 起動 + env `HYOUI_LOCK_TOKEN` 注入 + 子 exit で auto-unlock
  - `--process-bound` (= 子 PID 紐付け auto-release) は daemon-side timer/refcount が要るので並行 task

## 背景

DR-0006 §7 で「自動操作排他」の CLI として 3 subcommand が確定している:

```bash
hyoui tx <name> [--timeout-* ...] -- cmd args...
  # 起動時 lock 取得 → 子 env に HYOUI_LOCK_TOKEN 注入 → 子 exit で自動 unlock

hyoui lock <name> [--timeout-* ...] [--mode wait|fail]
hyoui unlock <name> [--token T | --force]
```

現状で実装されているのは下記まで:

- **Protocol 層**: `LockAcquire` / `LockResponse` / `LockRelease` message と daemon
  handler は完備 (`crates/hyoui/src/protocol/messages/lock.rs`、
  `crates/hyoui/src/daemon/control.rs` の `handle_lock_acquire` / `handle_lock_release`)。
  cap `"lock"` も MVP_CAPS に含まれている (`protocol/caps.rs`)
- **`ErrorCode::LockDenied` / `LockNotHeld`**: protocol 上の error 表現も既にある
  (`protocol/messages/error.rs`)
- **`--lock-token` flag + `HYOUI_LOCK_TOKEN` env fallback**: `attach` / `tail` /
  `input` / `kill` などの subcommand が handshake.token に流す配線済

未実装:

- `hyoui tx <name> -- cmd...` (= lock 取得 → 子起動 + env 注入 → 子 exit で auto-unlock)

## 求められる仕様 (= DR-0006 §7 から再掲)

### `hyoui tx <name> -- cmd args...`

子 process 起動時に lock 取得 → 子の env に `HYOUI_LOCK_TOKEN` 注入 → 子 exit で
自動 unlock。default timeout:

| flag | default |
|---|---|
| `--timeout-absolute` | 5min (safety net) |
| `--timeout-idle` | 30s |
| `--process-bound` | ⭕ (子プロセス bound、tx 固有) |

## 確認すべき open question

- daemon 側 `LockState` で `--timeout-absolute` / `--timeout-idle` / refcount /
  `--process-bound` は既に管理されているか? (= `control.rs` の handler は granted/denied を
  返すだけに見える、timer 動作は別 thread が要るかも)
- `hyoui lock <name>` を「短命 client」として実装する場合、socket disconnect で lock が
  即解放されないことを担保しているか? (= 普通の attach client が落ちると lock が外れる
  semantics になっているかも、要調査)

## 参考実装

- protocol Lock message 定義: `crates/hyoui/src/protocol/messages/lock.rs`
- daemon Lock handler: `crates/hyoui/src/daemon/control.rs` の `handle_lock_acquire` /
  `handle_lock_release`

## Triage (2026-07-03)

DR-0025 Phase 1a (Lock domain pure reducer 化、feat commit 93102075) が land し、daemon 側
lock の構造基盤が整った。ただし daemon 側 wait queue / `timeout_abs_ms` / `timeout_idle_ms`
は依然未実装 (CLI 側 polling で擬似実現のまま)。timer を message として注入する仕組みは
DR-0025 Q3 (Phase 2-3) で入る予定のため、**残タスクの `hyoui tx` 実装と daemon 側 timeout
semantics の完全化は Phase 2-3 の timer 導入後が適期**。それまで本 issue は wip のまま保持。
