---
title: "flaky: serve_ro_client_lock_acquire_rejected が suite 高負荷時に SessionExitNotify(143) を拾う"
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
(= 実行のたび 30s 待つ回と待たない回があり、これ自体が非決定的)。この 30s の間
他 test の daemon が滞留し、当該 test の session が SIGTERM されるタイミングと
500ms 窓が重なる、という筋が有力 (= 未確定、機序の裏取りは未実施)。

## 受け入れ条件

- [ ] `connect_token_mismatch_returns_specific_hint` の `/bin/sleep 30` を短命な子に
      置き換える (= 30s 待ちの必然性がないなら削る)。suite 全体の所要が安定するか確認
- [ ] 上記でも再現するなら、`serve_ro_client_lock_acquire_rejected` が受け取る
      `SessionExitNotify` の発生源 (= 誰が SIGTERM を送っているか) を特定する
- [ ] 「特定 message が来ないこと」を read timeout で確認する test 群を洗い出し、
      無関係な notify を無視して判定する形に直せるか検討 (= 現在は「何か来たら fail」)
