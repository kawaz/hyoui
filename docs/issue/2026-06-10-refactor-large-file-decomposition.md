---
title: "Refactor: 巨大ファイル解体 (session.rs serve_loop / main.rs / cli.rs)"
status: open
category: task
created: 2026-06-10T00:00:00+09:00
last_read:
open_entered: 2026-06-10T00:00:00+09:00
wip_entered:
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

# Refactor: 巨大ファイル解体 (session.rs serve_loop / main.rs / cli.rs)

- Priority: Mid (= 動作には影響しないが、3 ファイルが 4000-8000 行に肥大化し、変更時の認知負荷・コピペ起因のバグ混入リスクが高い)
- 関連 DR: [DR-0009](../decisions/DR-0009-session-module-split.md) (= daemon/session.rs 責務分割の継続)、[DR-0006](../decisions/DR-0006-cli-ground-rules.md) (= CLI 設計地盤)

## 背景

主要 3 ファイルが肥大化:

| ファイル | 行数 (2026-06-10) | 問題 |
|---|---|---|
| `crates/hyoui/src/daemon/session.rs` | ~4170 | `serve_loop` が 12 個の `&mut` 引数を取る巨大関数。poll_fds 構築 / client integration が複数箇所にコピペ |
| `crates/hyoui-cli/src/main.rs` | ~4210 | 全 subcommand executor が 1 ファイルに同居 |
| `crates/hyoui/src/cli.rs` | ~8260 | usage (41 関数) / parse (186 箇所) / unit test が 1 ファイルに混在 |

## 提案する解体

### 1. session.rs: `serve_loop` の `ServeContext` 化

`serve_loop` は現在 12 引数:

```rust
fn serve_loop(
    pty: &Pty, child: Pid, listener: &UnixSock,
    clients: &mut Vec<ClientHandle>, next_client_id: &mut u64,
    config: &DaemonConfig, state: &mut SessionState,
    scrollback: &mut Scrollback, screen_state: &mut ScreenState,
    pending_redraws: &mut Vec<u64>, sigchld_pipe: Option<&SelfPipe>,
    debug_dump: Option<&mut std::fs::File>,
) -> RelayOutcome
```

→ これらを `ServeContext` struct にまとめ、loop 本体を `impl ServeContext` の
メソッド群に分割する。引数渡しの煩雑さが消え、loop 内のヘルパー抽出も容易になる。

### 2. poll_fds 構築 / client iteration のコピペ統合

`PollFd::new(...)` + `for ch in clients.iter()` の poll fd 構築パターンが
session.rs 内の 4 箇所 (line 443 / 528-585 付近 / 986-995 付近) に散在。
1 つの helper (例: `build_poll_fds(&ServeContext) -> Vec<PollFd>`) に統合する。

### 3. main.rs: `commands/` への subcommand 分割

`crates/hyoui-cli/src/commands/` は既に存在 (`completion.rs` / `daemonize.rs` /
`input_handlers.rs` / `socket_path.rs` / `wait_core.rs`)。main.rs に残る各 subcommand
executor (`record_start_command` / `lock_acquire_command` / 等) を
`commands/<subcommand>.rs` に移し、main.rs は dispatch に専念させる。

### 4. cli.rs: usage / parse / test の分離

- usage_* (41 関数) → `cli/usage.rs`
- parse_* dispatcher → `cli/parse.rs` (or subcommand 別)
- `#[cfg(test)]` unit test → `cli/tests.rs` (or `tests/` 統合)

## 単一定義導出構想 (= 別検討、本 issue で議論のみ)

usage text / completion script / argument parse は同じ「subcommand × option」情報を
3 重に手書きしている。整合性ズレが起きやすい。subcommand 仕様を 1 つの宣言から
help / completion / parse を導出する仕組み (= declarative spec table) を将来検討する。
本 issue では「将来構想」として記録のみ、実装は別 issue に切り出す。

## 進め方

- **動作変更ゼロのリファクタ** (= 純粋な分割・抽出)。各ステップ後に
  `cargo check --workspace` + 全 test pass を確認
- 1 ステップ = 1 commit で小さく。session / main / cli を別 commit に
- ServeContext 化 (1) → poll_fds 統合 (2) は session.rs 内で連続して行うと整合確認しやすい
- 単一定義導出 (将来構想) は本 issue では着手しない
