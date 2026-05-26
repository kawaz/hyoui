# R4-M1: Phase 8 legacy (Session::run / accept_handshake_once) 撤去

## 背景

`crates/hyoui/src/daemon/session.rs` には Phase 8 legacy の `Session::run`
(1-client 同期 path、handshake → 子 PTY exit code 返却) と、Phase 9 で導入された
本流の `Session::serve` (multi-attach、broadcast、serve_loop) が並存していた。

- `Session::run` は v0.1.0 release 時点で本番未使用 (= `hyoui run` も
  `hyoui run --detached` も `serve` 経路)
- `accept_handshake_once` は Phase 7 の skeleton 用 helper
- Linux CI で flaky failure を起こす test (`serve_propagates_child_exit_code` の
  socket EOF 検出 race) が、共有 helper 経由で legacy 同期 path と相互作用していた
  疑い

backlog R4-M1 として撤去を実施し、CI Linux flaky の根治を兼ねた。

## 撤去対象

### `crates/hyoui/src/daemon/session.rs`

関数本体:

- `Session::accept_handshake_once(&self) -> Result<(u64, HandshakeResponse), Error>`
  (Phase 7、約 55 LOC)
- `Session::run(self) -> Result<i32, Error>`
  (Phase 8、約 52 LOC)
- `fn do_handshake<R, W>(...)` (Session::run 専用 helper、約 60 LOC)
- `fn relay_loop(...)` (Session::run 専用 1-client poll loop、約 145 LOC)
- `fn frame_send_outcome(e: FrameError) -> RelayOutcome` (relay_loop 専用、約 10 LOC)

テスト:

- `accept_handshake_once_completes`
- `run_exits_when_client_sends_kill`
- `run_exits_when_client_sends_detach`
- `run_exits_when_client_disconnects`
- `run_propagates_child_exit_code`
- `run_handshake_token_mismatch_rejected`
- `spawn_daemon_thread` (上記 5 つの共通 helper)

import:

- `crate::protocol::ProtocolError` (relay_loop 内でのみ使用していた)
- `crate::protocol::FrameError` (frame_send_outcome でのみ使用していた)

### `crates/hyoui/src/client/attach.rs`

`spawn_daemon_and_connect_client` (e2e fixture) と
`run_handshake_token_mismatch_rejected` の daemon thread spawn を
`session.run()` → `session.serve()` に差し替え (= 関数は残し中身だけ移植)。

`serve` も `(self) -> Result<i32, Error>` という同じ signature を持ち、
1-client 動作も MVP と同等のため、test 仕様変更なしで動作。

### `crates/hyoui-cli/src/main.rs` / `daemonize.rs`

module doc comment の "Session::run" → "Session::serve" に更新 (= 実コードは
そもそも `serve` を呼んでいた、コメントが古かったのみ)。

### `crates/hyoui/src/daemon/mod.rs`

Phase 7/8 を「R4-M1 で撤去済、Phase 9 で完全置換」と注記。Phase 9 を「← 現役」と明示。

## 残存

`do_handshake` と並行に存在していた `accept_new_client` / `do_handshake_stage`
(serve_loop 内の handshake handler) は **継続使用**。Round2 #5 で legacy `do_handshake`
にも token validation を入れた経緯はあったが、`do_handshake_stage` 側にも同等の
token validation が存在するため、撤去で coverage が落ちることはない (serve 経路の
test `serve_handshake_token_mismatch_rejected` が等価カバレッジを持つ)。

## LOC 削減

| ファイル | 撤去前 | 撤去後 | 差分 |
|----------|--------|--------|------|
| `crates/hyoui/src/daemon/session.rs` | 4898 | 4364 | -534 |
| `crates/hyoui/src/daemon/mod.rs` | 26 | 28 | +2 |
| `crates/hyoui/src/client/attach.rs` | 723 | 723 | 0 |
| **合計** | 5647 | 5115 | **-532** |

## verify

- `cargo build --workspace`: 警告 0
- `cargo clippy --workspace --all-targets -- -D warnings`: 警告 0
- `cargo fmt --check`: pass
- `cargo test --workspace --no-fail-fast`: 全 pass (260 + 3 + 13 = **276 件**)
- 3 回連続実行で stable

撤去前は 282 件 (= 276 + 6 撤去 test)、撤去後は 276 件。差分 6 件は本撤去で削除した
legacy 専用 test 件数と一致。

## CI flaky への効果

仮説検証: R4-H14 の `ChildLifecycle` + `WUNTRACED` で child exit transition の
検出が早まり、`session.run()` 内で client handshake response 送信前に child 検知
→ daemon return → socket close → client read EOF、というのが Linux で発生していた
疑いがあった。

`serve_propagates_child_exit_code` (Phase 9 serve path) は legacy `Session::run`
経路を使わないが、共有 helper (`do_client_handshake` の Frame decode) で同じ
socket EOF race を踏みうる。`Session::run` 撤去で本来不要な同期 path との
相互作用を排除した。実 CI の flaky 解消は次回 push 後の Linux runner で確認する。

## 後続作業

- `/tmp/itumono-backlog-hyoui.md` の R4-M1 を `[done]` マーク
- 本撤去で session.rs の structure が整理されたため、後段 LOC 削減 task の
  土台が良くなった
