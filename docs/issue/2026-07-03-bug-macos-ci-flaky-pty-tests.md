---
title: macos CI で PTY 系 e2e が flaky fail する (outer_token_inheritance_skips_auto_acquire / child_inherits_hyoui_session_id_env)
status: open
category: bug
created: 2026-07-03T18:25:00+09:00
last_read: 2026-07-20T10:18:31+09:00
open_entered: 2026-07-03T18:25:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: CI run 実ログ裏取り (session a7761122、初期 subagent 報告の訂正過程で発見)
---

# macos CI で PTY 系 e2e が flaky fail する

## 概要

`Test (macos-latest / stable)` job (= blocking、main red の実原因) で以下 2 test が
fail した。同一 code の後続 run では pass しており flaky。

- `outer_token_inheritance_skips_auto_acquire` (input_auto_lock_cli.rs、DR-0022 系) —
  `crates/hyoui-cli/tests/common/pty.rs:96` で panic、当該 suite 30.58s
- `child_inherits_hyoui_session_id_env` (self_session_id_env.rs:34、DR-0020 系) — panic

## 観測事実

- fail: CI run 28415355152 (2026-06-30、commit 004fc122) job 84196977050
- pass: CI run 28644829018 (2026-07-03、commit 834a5742 = docs のみの差分) の
  macos Test job 5m18s green
- → 同系 code で fail/pass が割れており flaky。共通項は「PTY + 実 daemon 起動を伴う
  e2e が CI runner 上で時間依存の失敗をする」
- 注意: macos-latest label は 2026-06-15 以降 macOS 26 へ移行中 (runner image の
  世代差が fail/pass を割った可能性も否定できない)

## 真因調査の方針 (flaky ラベルで打ち切らない)

1. pty.rs:96 の panic 内容 (= read timeout / expect 失敗) を特定し、何を待って
   いたかを把握する
2. 2 test をローカル + CI で反復し、再現条件 (並列度 / runner 世代 / load) の軸を切る
3. DR-0025 Phase 2 (Client domain) / Phase 3 (Child domain) の reducer 化で
   吸収されるか evaluate (吸収されるなら blocked 遷移)

## 再観測 (2026-07-19, v0.9.10)

- Release run: https://github.com/kawaz/hyoui/actions/runs/29694355729 (commit 9180fd8e)
- job: `ci / Test (macos-latest / stable)`
- 失敗テスト: `single_input_succeeds_with_auto_lock` (crates/hyoui-cli/tests/common/pty.rs:96)
- panic: `single-input daemon: thread did not finish within 30s (= 無限ハング防止のため deadline fail)`
- 同一 pty.rs:96 の deadline hang 様式。本 issue が既に扱う `outer_token_inheritance_skips_auto_acquire` / `child_inherits_hyoui_session_id_env` に加え、同ファイル内の 3 件目のテストで同型 flaky が発生

## 再観測 (2026-07-21, commit a9e123c1 後)

- 本日 commit a9e123c1 直後の workspace test を 3 回連続実行し全て green
  (input_auto_lock_cli 含む)
- diff (auto_lock helper を `crates/hyoui-cli/src/main.rs` から
  `crates/hyoui/src/client/auto_lock.rs` へ切り出し) を精読。
  `LOCK_RECV_TIMEOUT=5s` / `POLL_INTERVAL=100ms` / control-flow
  (LockAcquire→poll→recv→match result) / release 経路すべて意味論同一。
  error 型が `String` → `AutoLockError` enum 化されただけで CLI 側は
  Display 経由で同じ eprintln 文字列を出す
- ローカルでは a9e123c1 起因の regression と判定できず、macOS CI 側で
  観測されていた既存 flaky (30s deadline hang @ pty.rs:96) と同族と推定。
  並列 workspace test の CPU 資源競合による露出頻度上昇の可能性は残るが、
  実装側の意味論変更に起因する反証は得られていない

## 真因 (2026-07-26) — 本 issue の 2 test は **別原因**だった

本 issue は起票時に 2 test を束ねていたが、調査の結果 **失敗様式が違う別物**と判明した。
片方だけ解決済み。

### (1) `outer_token_inheritance_skips_auto_acquire` → 解決済み

`pty.rs:96` の 30s join deadline 様式 (= `single_input_*` / `parallel_input_*` も同族)。
daemon serve_loop への一時計装で直接観測し、
[[2026-07-25-bug-daemon-drops-pending-frames-on-client-close]] と同一原因と確定した
(= 「送信して即 close」した client の Kill frame を daemon が読まずに捨てる product バグ)。
詳細と修正内容は当該 issue、検証は [[2026-07-04-bug-flaky-outer-token-e2e-deadline]] を参照。

macOS 固有ではなく **ubuntu でも同頻度で発生**していた (下記 CI 集計)。
`macos-latest` の runner 世代交代は無関係だった。

### (2) `child_inherits_hyoui_session_id_env` → **未解決 (別原因)**

`self_session_id_env.rs:34` の `wait_for("MARK=", 10s)` timeout 様式で、上記の
join deadline とは無関係。2026-07-26 にローカル full-workspace 並列実行で再現させ、
失敗時の PTY 出力を採取した:

```
wait_for("MARK=") timed out after 10s; output so far (107 bytes):
"[hyoui] detach: Ctrl+Z  |  子へ Ctrl+Z: 2 連打  |  peek (read-only): hyoui attach <session> --mode=ro\r\n"
```

= client の attach 自体は成功して banner を出している。しかし **attach 前に子が出力した
`MARK=...` が attach redraw で再生されていない** (= 107 bytes は banner のみ)。

`hyoui run` は DR-0015 で「fork daemon + attach client」の合成なので、
子の `echo` が client の attach より先に走るのは正常系。その場合 attach redraw
(DR-0013) が screen state から再生する責務を負うが、そこが高負荷時に取りこぼしている。

- 単独実行では 20/20 green (= 並列 contention 依存)
- 未検証: screen state への取り込みが間に合っていないのか、redraw の送信側で
  落ちているのか。`serve_attach_redraw_preserves_alt_screen_flag` と同じ経路なので
  そちらの観点で追うのが早い

## CI 実データ (2026-07-26、直近 12 run の blocking job `Test (os / stable)`)

失敗テスト頻度 (= 21 件中):

| テスト | 件数 |
|---|---|
| `single_input_succeeds_with_auto_lock` | 5 |
| `outer_token_inheritance_skips_auto_acquire` | 4 |
| `serve_tail_follow_receives_tail_end_on_child_exit` | 4 |
| `parallel_input_serialized_by_auto_lock` | 3 |
| `e2e_input_returns_409_while_external_client_holds_lock` | 2 |
| `child_inherits_hyoui_session_id_env` / `child_exit_propagates_code` / `attach_emits_discovery_hint_with_actual_prefix` | 各 1 |

= `input_auto_lock_cli` の 3 test で **12/21 (57%)**。本 issue の修正が最頻の
blocking failure を潰す。`serve_tail_follow_*` (4 件) は
[[2026-07-25-bug-flaky-serve-ro-lock-acquire-rejected]] の 32s 問題側で対処済。

## 受け入れ条件

- [x] `outer_token_*` 側の不安定さの軸が観測データで特定されている
      (= WriterDead 起因の frame 破棄、計装で直接観測)
- [ ] `child_inherits_hyoui_session_id_env` の真因特定 (= attach redraw が attach 前の
      子出力を取りこぼす経路。上記 (2) 参照)
- [ ] 2 test が CI で安定して pass する (= 修正 push 後の CI で確認)
