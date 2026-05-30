# BUG: agent.rs tests がフレーキー (`agent_run_echo_output_visible_via_observer`, `agent_socket_input_reaches_child`)

- Status: **廃止 (2026-05-30 整理)** — 該当 test 2 件は agent.rs ごと削除済 (commit `numkmlolnkkl`、2026-05-26)、症状消滅。根本 race condition は v0.1.0 daemon 再実装計画 (= 当時の根本対処方針) とともに撤回されたため、本 issue としては closed。historical reference として保存。
- Date: 2026-05-26
- Priority: Middle (CI を fail させるが、v0.1.0 daemon 再実装で根本対処予定)
- 発見元: PoC 完了後の push (`pkf run push`) で test 失敗、CI でも別の test (agent_socket_input_reaches_child) で fail

## 症状

`cargo test --workspace` の並列実行で以下 2 test が偶発的に fail:
- `crates/hyoui/tests/agent.rs::agent_run_echo_output_visible_via_observer`
- `crates/hyoui/tests/agent.rs::agent_socket_input_reaches_child` (line 207)

単独実行 (`cargo test --test agent <name>`) では確実に PASS。**並列実行時の race condition**。

CI ログ (2026-05-26):
```
test agent_socket_input_reaches_child ... FAILED
thread 'agent_socket_input_reaches_child' (3389) panicked at crates/hyoui/tests/agent.rs:207:69
```

ローカル (macOS):
```
test agent_run_echo_output_visible_via_observer ... FAILED  (並列の偶発失敗)
```

## 原因 (推定)

- agent イベントループの初期化と test 側 client connect/observer の同期が緩い
- 特に socket-based test は「daemon が socket bind 完了」を待たずに client connect すると失敗
- 並列 test で OS リソース (pty, fd, socket) 競合の可能性

## 暫定対応

両 test に `#[ignore = "..."]` を付けて disable。`cargo test -- --ignored` で個別検証可。
push (CI 経由) は通るようになる。

## 根本対処方針 (v0.1.0 daemon 再実装で吸収)

v0.1.0 で daemon module を 再実装する際:
- socket bind 完了を pipe で親通知してから client connect する (= PoC 01b で確認した pattern)
- agent.rs の test 自体を v0.1.0 の daemon 用 integration test に書き直す
- 古い `tests/agent.rs` は廃止 or 大幅書き換え

それまでは `#[ignore]` で push 通せる状態を維持。

## 関連

- `crates/hyoui/tests/agent.rs` — 該当 test 群
- `crates/hyoui/src/agent.rs` — 元実装 (v0.1.0 で daemon module に置き換え予定)
- `docs/findings/2026-05-26-daemon-fork.md` — daemon socket 起動の正しい sync pattern
- `docs/findings/2026-05-26-multi-attach.md` — multi-client + socket の動作モデル
- v0.1.0 implementation で根本対処
