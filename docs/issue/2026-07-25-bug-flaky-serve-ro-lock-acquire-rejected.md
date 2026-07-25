---
title: "flaky: serve_ro_client_lock_acquire_rejected / input_auto_lock_cli が高負荷時に落ちる"
status: open
category: bug
created: 2026-07-25T00:00:00+09:00
last_read:
open_entered: 2026-07-25T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: DR-0029 実装中に local (macOS) の `cargo test --workspace` で断続観測
---

# flaky: serve_ro_client_lock_acquire_rejected が suite 高負荷時に落ちる

## 症状

```
---- daemon::session::tests::serve_ro_client_lock_acquire_rejected stdout ----
panicked at crates/hyoui/src/daemon/session.rs:4437:
unexpected broadcast received on s1: SessionExitNotify(SessionExitNotify { exit_status: 143, signal: None })
```

test は「s1 に何も broadcast が来ないこと」を 500ms の read timeout で確認するが、
その窓に **子の SIGTERM 死 (= 143) による `SessionExitNotify`** が飛び込む。

## 観測 (macOS、local、2026-07-25)

| 実行単位 | 所要 | 結果 |
|---|---|---|
| `cargo test -p hyoui --lib` (全 860) | 2.98s | ok (3/3 連続) |
| `cargo test -p hyoui --lib` (全 860) | 32.0s | FAILED (2 回観測) |
| `cargo test -p hyoui --lib daemon::session` のみ | 2.7s | ok (5/5 連続) |
| `cargo test -p hyoui --lib client::` のみ | 32.0s | ok |

**失敗は必ず suite 全体が 32s かかった回**で起きている。32s の正体は
`client::attach::tests::connect_token_mismatch_returns_specific_hint` が
`/bin/sleep 30` を子に持ち、末尾で `daemon_handle.join()` して子の自然終了を待つ設計
(= 実行のたび 30s 待つ回と待たない回があり、これ自体が非決定的)。

**切り分け**: 当該 test だけ除外すると suite は常に 2.9s で green になる。

```
$ cargo test -p hyoui --lib -- --skip connect_token_mismatch   # 3 回連続
test result: ok. 860 passed; 0 failed; ... finished in 2.89s
test result: ok. 860 passed; 0 failed; ... finished in 2.87s
test result: ok. 860 passed; 0 failed; ... finished in 2.88s
```

= 30s 居座る daemon が他 test の session 終了タイミングを押し出し、当該 test の
500ms 窓に `SessionExitNotify(143)` が滑り込む、という筋。**SIGTERM の送出元までは
未特定**。

## 併発する別 flaky: `input_auto_lock_cli` の deadline fail

同じ高負荷環境 (= `load average 42`) で `crates/hyoui-cli/tests/input_auto_lock_cli.rs` の
3 test (`single_input_succeeds_with_auto_lock` / `parallel_input_serialized_by_auto_lock` /
`outer_token_inheritance_skips_auto_acquire`) が 5s / 15s / 30s の deadline で fail する。
`outer_token_*` は既知 ([2026-07-04-bug-flaky-outer-token-e2e-deadline](./2026-07-04-bug-flaky-outer-token-e2e-deadline.md))
だが、**同 binary の他 2 test も同様に落ちる**ことを確認した。

DR-0029 の変更が原因でないことを、変更前 revision (`7c15f5a1`) の jj workspace を作って
同一環境・交互実行で確認した:

| コード | ok | fail |
|---|---|---|
| DR-0029 適用後 | 6 | 3 |
| 変更前 (7c15f5a1) | 8 | 1 |

**変更前でも落ちる** (= 本 flaky は DR-0029 起因ではない)。回数差は n=9 では有意でなく、
load average 42 の環境ノイズと区別できない。

## 受け入れ条件

- [ ] `connect_token_mismatch_returns_specific_hint` の `/bin/sleep 30` を短命な子に
      置き換える (= 30s 待ちの必然性がないなら削る)。suite 全体の所要が安定するか確認
- [ ] 上記でも再現するなら、`serve_ro_client_lock_acquire_rejected` が受け取る
      `SessionExitNotify` の発生源 (= 誰が SIGTERM を送っているか) を特定する
- [ ] 「特定 message が来ないこと」を read timeout で確認する test 群を洗い出し、
      無関係な notify を無視して判定する形に直せるか検討 (= 現在は「何か来たら fail」)
