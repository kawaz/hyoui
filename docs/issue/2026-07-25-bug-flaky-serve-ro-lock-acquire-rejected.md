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

## 根にある環境要因: PTY 枯渇 (2026-07-25 実測)

同日の別 run で、lib test が `start: Errno(ENXIO)` で落ちるのを観測した
(`serve_screen_dump_without_cap_is_rejected` / `serve_signal_unknown_name_rejected`、
いずれも helper `spawn_serve_thread` の `Session::start(cfg).expect("start")`)。
ENXIO は macOS の **PTY 割当失敗**。

```
$ ls /dev/ttys* | wc -l        # 123  (macOS の legacy pty は 128 が上限)
$ lsof -n | grep -c /dev/ttys  # 317  (= 1 pty あたり複数 fd)
$ uptime                       # load average: 46.08 ...
```

= 開発機に常駐している長寿命 hyoui session + 端末 + test suite の並列 daemon が
PTY を食い合っており、**test 側の問題ではなく資源枯渇**で落ちている run が混ざる。
本 issue の 2 系統も、同じ資源圧の下で顕在化している可能性が高い。

対処案 (どれも未着手):
- test 実行時の並列度を絞る (= `--test-threads` 制限、あるいは PTY を使う test に
  serial marker)
- PTY を使う test の後始末を強化して滞留を減らす
- CI と開発機で PTY 上限 (`kern.tty.ptmx_max` 等) を確認・記録する

## CI 実データによる裏取り (2026-07-26)

直近 12 run の blocking job `Test (os / stable)` ログから lib suite の所要時間と
失敗の相関を集計した。**仮説どおり、失敗は 32s の回にしか起きていない**:

| lib suite 所要 | 実行回数 | 失敗 |
|---|---|---|
| **32.0s** (= `/bin/sleep 30` を待った回) | 8 | **4 (50%)** |
| 4.2〜4.8s | 6 | 0 |

失敗内訳は `serve_tail_follow_receives_tail_end_on_child_exit` × 4 と
`serve_ro_client_lock_acquire_rejected` × 1 (本 issue 起票時のローカル観測分を含む)。
= 「32s 居座り daemon が他 test の時間依存 assert を圧迫する」構図が CI でも成立。

## 原因と修正 (2026-07-26)

`connect_token_mismatch_returns_specific_hint` の末尾コメントは
「daemon thread は handshake 拒否で Err 終了」と書いていたが、**これが誤り**。
handshake 拒否は session を畳まない (= 不正 token で daemon を殺せたら脆弱性)。
そのため素の `daemon_handle.join()` が子 `/bin/sleep 30` の自然死を待って 30s block
していた。

修正: 正しい token (`secret-xyz`) で attach して `Kill{signal:SIGKILL, wait:true}` を
送り、決定的に畳んでから join する (`crates/hyoui/src/client/attach.rs`)。

実測 (macOS、`cargo test -p hyoui --lib` 5 連続):

```
test result: ok. 875 passed; ... finished in 2.89s
test result: ok. 875 passed; ... finished in 2.91s
test result: ok. 875 passed; ... finished in 2.86s
test result: ok. 875 passed; ... finished in 2.88s
test result: ok. 875 passed; ... finished in 2.88s
```

= 32s の bimodal が消え、常に ~2.9s。

## 受け入れ条件

- [x] `connect_token_mismatch_returns_specific_hint` の `/bin/sleep 30` 待ちを解消する。
      suite 全体の所要が安定するか確認 (= 32s → 常時 2.9s)
- [ ] 上記でも再現するなら、`serve_ro_client_lock_acquire_rejected` が受け取る
      `SessionExitNotify` の発生源 (= 誰が SIGTERM を送っているか) を特定する
- [ ] 「特定 message が来ないこと」を read timeout で確認する test 群を洗い出し、
      無関係な notify を無視して判定する形に直せるか検討 (= 現在は「何か来たら fail」)
