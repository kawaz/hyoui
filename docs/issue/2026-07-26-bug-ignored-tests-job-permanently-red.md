---
title: "CI の ignored-tests job が continue-on-error で恒常 red を隠している (ubuntu/macOS それぞれ固定 test が 100% fail)"
status: wip
category: bug
created: 2026-07-26T09:40:00+09:00
last_read: 2026-07-29T18:45:00+09:00
open_entered: 2026-07-26T09:40:00+09:00
wip_entered: 2026-08-21T11:50:50+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: CI flaky 根治タスク中に GitHub API で直近 12 run × 全 attempt の job 結果を集計して発見 (2026-07-26)
---

# CI の ignored-tests job が恒常 red を隠している

## 概要

`.github/workflows/ci.yml` の `ignored-tests` job (= `cargo test --workspace -- --ignored`)
は `continue-on-error: true` が付いており、**失敗しても workflow は緑になる**。

直近 12 run × 全 attempt の job 結果を GitHub API で集計したところ、この job は
**サンプルした全 run で ubuntu / macOS 両方とも失敗していた**。flaky ではなく恒常 red。
`continue-on-error` によって誰も気づかない状態が継続している。

## 集計結果 (2026-07-26、直近 12 run)

失敗テストは OS ごとにほぼ固定:

| OS | テスト | 件数 | 様式 |
|---|---|---|---|
| ubuntu | `daemon::session::tests::serve_backpressure_disconnects_slow_client` | 12 | 毎回 **31.0s** = テスト内 `join_with_deadline(30s)` の deadline hang |
| macOS | `notify_default_does_not_resume_self_stopped_child` | 7 | **0.14〜0.28s で即死** (= タイミング依存でも資源枯渇でもない決定的失敗) |
| macOS | `smoke_hyoui_run_echo` | 2 | |
| macOS | `sys::raw::anchor_tests::session_anchor_makes_child_stoppable` | 1 | |
| macOS | `pipe_send_eof_default_terminates_bc` | 1 | |

## 個別の状況

### ubuntu: `serve_backpressure_disconnects_slow_client`

既に [[2026-06-22-backpressure-writer-pump-drop-sequence-deadlock]] (status: blocked) が
扱っている。当該 test の `#[ignore]` 属性自体に

> ubuntu CI で daemon thread join が hang する (2026-05-28 6h timeout 観測)

と書かれており、**既知の hang を ignore で棚上げしたまま CI では実行して恒常 red に
している**という矛盾した状態になっている。

### macOS: `notify_default_does_not_resume_self_stopped_child`

`crates/hyoui-cli/tests/jobcontrol_auto_resume.rs:77` の
`assert!(result.is_err(), ...)` が失敗 = notify (default) なのに `RESUMED_MARKER` が
2s 以内に観測されている。

**未解明**。macOS 開発機 (load ~30 の高負荷下) でローカル実行したところ、CI とは
**逆に** `notify_default_*` が pass し、対の `auto_resume_resumes_self_stopped_child` が
「8s で出力 0 bytes」で fail した。低負荷での再実行と、CI runner との環境差
(macOS バージョン / core 数) の切り分けが必要。

## 真因確定と修正 (2026-07-26 第 2 弾)

### (A) ubuntu `serve_backpressure_disconnects_slow_client` → **解決**

Docker (ubuntu:24.04) + `--cpus=4` + `taskset -c 0-3` で **5/5 決定的に再現**した
(= flaky ではなく Linux での確定的 hang。macOS では出ない)。daemon に計装を入れて観測:

```
promoted id=0 mode=Rw
ENQ reject id=0 payload=4100 cur=0 limit=4096 single_frame_too_big=true
READY-branch drop id=0
poll:enter nclients=0 ...   ← 以降 31s 間 nclients=0 のまま yes(1) を空回り
```

真因は **product バグ**: `enqueue_for_client` が `cur + size > limit` だけを見ていたため、
`size > limit` の単一 frame で **queue が空の client すら即 disconnect** していた。
子 PTY の読み取り chunk は 8 KiB 固定なので、`client_buffer_bytes` が 8 KiB 未満だと
全 client が attach 直後に切られ、**誰も接続できない daemon** になる (= kill も届かない)。

修正: 空 queue なら limit 超の単一 frame も受け入れる (前進保証)。併せて test 側の
`client_buffer_bytes = 4096` (< chunk 8 KiB) という自己矛盾も 12 KiB に修正。

検証 (Linux 4core): 修正前 31.01s FAILED × 5/5 → 修正後 1.06〜1.12s ok × 8/8。

### (B) macOS `notify_default_does_not_resume_self_stopped_child` → **裁定済み・実装済み**

真因は **DR-0019 と DR-0029 の規定衝突**:

- DR-0019 §3: `on-child-suspend` default = `notify` = 「daemon は勝手に起こさない」
- DR-0029 §5: `[attach] resume_on_reattach = true` (default) で rw attach 時に resume 要求
- `hyoui run` は DR-0015 で「fork daemon + attach client」の合成なので、**run した瞬間に
  attach 経路が発火して子を起こす**。daemon は notify を守っているが同居 client が起こす

2026-07-29 kawaz 裁定 (👺RS-Q1) により [[DR-0030]] を起票・実装済み。原則は
「rw attach client が存在する間、hyoui は子を停止させたままにしない」で、
DR-0029 §5 の resume 発火点を「handshake 時に stopped」に加えて「attach 中の
`SessionChildStoppedNotify` 受信」にも拡張した。旧 test
`notify_default_does_not_resume_self_stopped_child` は「default では起こされない」
という、この裁定前の (誤った) 期待値だったため、`run_resumes_child_that_is_already_stopped_at_attach`
等に置き換えて期待値を反転済み (`crates/hyoui-cli/tests/jobcontrol_auto_resume.rs`)。

## 受け入れ条件

- [x] macOS の `notify_default_does_not_resume_self_stopped_child` の真因を特定
      (= DR-0019 と DR-0029 の規定衝突)
- [x] 修正方針の裁定 (👺RS-Q1、2026-07-29 kawaz) と [[DR-0030]] による実装
      (= resume 発火点の拡張、対象 test の期待値反転)
- [x] ubuntu の `serve_backpressure_disconnects_slow_client` を解決
      (= 単一 frame > buffer_limit で誰も attach できなくなる product バグ)
- [ ] 上記の決着を受けて `continue-on-error: true` を外して恒常 red を検知可能にする。
      外せない test が残るなら、その test だけ除外して残りを blocking にする
      (= 「全部隠す」のをやめる)

## Why (= なぜ放置が有害か)

`continue-on-error` は「不安定な test で workflow を止めない」ための仕組みだが、
現状は **恒常的に壊れている事実を隠す**方向に働いている。この job が緑/赤どちらでも
同じ扱いなので、新しい regression が入っても気づけない (= 検知能力ゼロ)。

## 追記 (2026-07-29): §(B) の後日談 — 「実機で動かない」は誤診だった

DR-0030 land 後、`hyoui status` の `child-state` が resume 後も stopped のままなのを見て
「DR-0030 が実機で効いていない」と報告が上がったが、実機で切り分けた結果 **DR-0030 の
resume 自体は正しく動いていた** (子は `ps` で `S+` = 走行中、rw client 無しの対照では
`T+` のまま維持される)。stopped 表示は別 bug
([[2026-06-12-bug-child-stopped-flag-not-cleared]]、2026-07-29 に root cause 特定・修正) の
症状で、それを resume 失敗と読み違えていた。

この誤診が成立してしまった構造的な理由が 2 つあり、本 issue の射程に直接効く:

1. **`#[ignore]` された test の green を「green」と読んでいた**。
   `cargo test -p hyoui-cli --test jobcontrol_auto_resume` は DR-0030 の 3 test を
   ignored のまま skip して `ok` を返す。`-- --ignored` を付けない限り検証していない。
   「test green なのに実機 red」という前提自体が、実際には「test を走らせていない」だった。
2. **test が daemon 側の観測可能な state を一切見ていなかった**。
   3 test はいずれも子の標準出力 (`RESUMED_MARKER`) だけを assert しており、
   `hyoui status` が報告する `child-state` を見ていない。人間が実機で最初に見るのは
   status の表示なので、test が green でも「見え方」の回帰は素通しになる。
   回帰 test `status_child_state_returns_to_running_after_resume` を追加して塞いだ。

## 調査 2026-07-29 (macOS notify_default_*)

### 「7/7」の単位

集計表の `7` は 7 個の test ではなく、直近 CI 12 run のうち macOS で
`notify_default_does_not_resume_self_stopped_child` が失敗した **7 run 全件**を指す。
旧コードに該当 test は 1 個だけで、現 HEAD では DR-0030 に従う 4 個の ignored test に
置き換わっている。

### 旧 assert の fresh 再現

現 HEAD の product code に DR-0030 裁定前の旧 test ファイルだけを一時コピー上で戻し、
次を 7 回個別実行した。リポの追跡ファイルは変更していない。

```console
cargo test -p hyoui-cli --test jobcontrol_auto_resume \
  notify_default_does_not_resume_self_stopped_child \
  -- --ignored --exact --nocapture
```

| run | 結果 | 所要時間 | `result` の実値 |
|---:|---|---:|---|
| 1 | FAILED | 0.82s | `Ok("[hyoui] detach: ...\\r\\nRESUMED_MARKER\\r\\n")` |
| 2 | FAILED | 0.81s | 同上 |
| 3 | FAILED | 0.82s | 同上 |
| 4 | FAILED | 0.79s | 同上 |
| 5 | FAILED | 0.84s | 同上 |
| 6 | FAILED | 0.82s | 同上 |
| 7 | FAILED | 0.81s | 同上 |

全 7 回で `wait_for("RESUMED_MARKER", 2s)` は `Err` ではなく同一内容の `Ok` を返した。
失敗 assert は旧 test の `assert!(result.is_err())` であり、marker は timeout 前に必ず観測された。
CI runner の負荷・macOS version・資源枯渇で説明する余地のない決定的失敗をローカルでも再現した。

### 現 HEAD の関連 test

置換後の `crates/hyoui-cli/tests/jobcontrol_auto_resume.rs` の ignored test を個別実行した。

| test | 結果 | 所要時間 | 検証経路 |
|---|---|---:|---|
| `auto_resume_resumes_self_stopped_child` | ok | 1.06s | daemon policy が resume |
| `run_resumes_child_that_is_already_stopped_at_attach` | ok | 0.93s | handshake snapshot が stopped |
| `run_resumes_child_that_stops_while_attached` | ok | 1.82s | attach 中の stopped notify |
| `status_child_state_returns_to_running_after_resume` | ok | 1.87s | resume 後の daemon state |

`pgrep -fl hyoui` でテスト由来の daemon 残骸 process が無いことを確認した。調査前から
走行している別セッションの `hyoui` process には触れていない。

### 真因と分岐判定

**test の前提不備**。product bug でも CI 環境固有でもない。

旧 test は daemon policy の default `notify` を「`hyoui run` 全体では誰も子を起こさない」
と解釈していた。しかし実装上は次の 3 点が同時に成立する。

1. `hyoui run` は detached daemon を起動した後、自身を `hyoui attach` に置換する
   (`crates/hyoui-cli/src/main.rs:569-604`)。
2. attach 設定は stopped child の resume を default `true` とする
   (`crates/hyoui/src/config/mod.rs:135`, `:192`)。
3. rw attach は handshake 時に `child_stopped` なら `SessionChildResumeRequest` を送る
   (`crates/hyoui-cli/src/main.rs:819-829`)。

したがって daemon は `notify` 規定どおり自発的に resume していなくても、`run` に合成された
rw attach client が resume を要求し、旧 test の `RESUMED_MARKER` が出る。旧 assert は
DR-0019 の daemon policy だけを検証対象とみなし、DR-0015 の `run = daemon + attach` と
attach 側の規定を前提から落としていた。

### 修正の妥当性

単に assert を実測値へ緩めるのではなく、DR-0030 で現行仕様を
「rw attach 中は子を停止させたままにしない」と裁定した上で、旧 test の検証意図を
現行仕様へ置き換えるのが正しい。現 HEAD は以下を満たしている。

- 起動時点で stopped の経路と、attach 成立後に stop する経路を別 test で検証する。
- daemon の明示 `auto-resume` policy も独立 test のまま維持する。
- marker 出力だけでなく `child-state: running` への復帰も検証する。
- resume 判定を `should_resume_stopped_child` に集約し、handshake と stopped notify の
  2 call site で条件を共有する (`crates/hyoui/src/client/attach.rs:128-139`, `:799-823`)。

### 追加調査: `daemon_sigcont_wakes_stopped_child`

- 現 HEAD / macOS Darwin 25.5.0 で `cargo test -p hyoui-cli --test jobcontrol_daemon_cont_wakes_child daemon_sigcont_wakes_stopped_child -- --ignored --exact --nocapture` を無負荷で20回反復し20/20 FAILED。run1 16.32s、run2-20 は12.30〜12.55s、全回 panic は `attach client が leader として daemon に接続しない`。無負荷で決定的再現したためCPU高負荷試験は不要。
- 対照 `daemon_cont_wakes_child_stopped_during_daemon_stop` は5/5 green、2.66〜3.08s。
- 一時コピーへの観測計装(追跡ファイル無変更)で、外部 daemon SIGCONT 前に `wait_for("RESUMED_BY_DAEMON_CONT", 3s)` が `Ok("[hyoui] detach: ...\r\nRESUMED_BY_DAEMON_CONT\r\n")`。最初の status は `SessionExitNotify { exit_status: 0 }` を unexpected response として失敗、以降は socket ENOENT。daemon_pid=None。
- 機構: test の子は起動直後 `kill -STOP $$`。`hyoui run` はrw attachを合成し、DR-0030により handshake snapshot の stopped child を defaultでresumeする(main.rs:819-829、config/mod.rs:135,192)。子はmarker出力後即exitする。したがって外部SIGCONT前に対象sessionとleaderが消える。`wait_for_leader_ready` は status polling 50ms(common/pty.rs:500-524)だが、消滅済leaderは観測不能。
- deadline: 外側 leader wait 10s。各 status helper は capture deadline 10sだが、statusの `connect_with_retry` は100ms×20 attempts≒2s(main.rs:164-211)。socket消滅後は1 attemptが約2sを消費し、外側deadlineはblocking status call後にしか評価されないため全体約12.3〜12.5s(cleanup込み)になる。CI 12.73sと一致。固定sleepが真因ではない。500ms sleep(test:55)は到達しない。
- 履歴: 対象test最終更新は2026-06-12 commit 389d5a146e09、DR-0030は2026-07-29 commit c767388f892a。仕様変更時にこのtestの前提更新が漏れた。
- 分岐判定は「test の前提不備」。product bugでもCI環境固有でもない。productのrw attach resumeはDR-0030どおり正しい。
- 修正案(実装しない): testの目的であるdaemon SIGCONT防衛策を隔離するため、このtestだけ runner-scoped config で `[attach] resume_stopped_child = false` にしてrw leader接続は維持しつつattach側auto-resumeを無効化する。その後 marker未出力を同期的に確認しdaemon SIGCONTを送る。単にtimeout延長・poll間隔短縮は既に消滅したleaderを探すだけなので不適切。別案はdetached daemon + ro attachだがleaderを持たず元test構造から変わるため第一案を推奨。
- `pgrep -fl` で調査由来の残骸processなし。既存process未接触。一時コピー削除済み。

#### 修正 2026-07-29

`daemon_sigcont_wakes_stopped_child` だけに runner-scoped config
`[attach] resume_stopped_child = false` を適用した。rw leader 接続は維持し、DR-0030 の
attach 側 resume だけを opt-out することで、この test が保証する daemon
`SIGCONT` 防衛経路を他の resume 経路から隔離した。

config は `HyouiTestRunner::spawn_hyoui_with_config` が runner 固有の
`<runtime_dir>/xdg-config/hyoui/config.toml` に書き、spawn した process だけへ
`XDG_CONFIG_HOME` を渡す。process-global env を変更しないため、並列 test や既存 config に
干渉しない。

修正後の macOS 実機結果:

| test | 反復結果 | 所要時間 |
|---|---:|---:|
| `daemon_sigcont_wakes_stopped_child` | 20/20 PASS | 1.81〜2.03s |
| `daemon_cont_wakes_child_stopped_during_daemon_stop` | 5/5 PASS | 2.73〜2.86s |
| 同 integration test binary の ignored test 全体 | 2/2 PASS | 2.55s |

`cargo fmt --all -- --check` も成功。`pgrep -fl` で test session 名に一致する残骸 process が
無いことを確認した。timeout 延長や poll 間隔短縮ではなく、仕様変更で無効になった test の
前提だけを明示的に固定しており、失敗を隠す変更ではない。

## ignored test 総ざらい 2026-07-29

`crates/` 配下の `#[ignore]` test 全 29 件を列挙し、DR-0029 と DR-0030、および各 test が
参照する現行 DR と突き合わせた。

| test | 判定 | 根拠・扱い |
|---|---|---|
| `headless_stdin_eof_terminates_child_reading_bc` | (a) | stdin EOF の event 経路。attach stop/resume 仕様と独立 |
| `notify_child_stopped_does_not_auto_resume_without_leader` | (a) | DR-0030 §4 の無人時 `notify` を直接検証 |
| `serve_backpressure_disconnects_slow_client` | (a) | backpressure 仕様。ubuntu 側は既知 issue のため本調査の実装スコープ外 |
| `session_anchor_makes_child_stoppable` | (a) | DR-0017 の session anchor。attach client 不在 |
| `list_marks_stale_socket_when_no_ping_response` | (a) | stale socket 判定。jobcontrol と独立 |
| `interactive_typing_survives_raw_ack` | (a) | DR-0021 RawAck regression |
| `daemon_sigterm_terminates_child_and_unlinks_socket` | (a) | daemon graceful shutdown |
| `daemon_second_sigterm_during_shutdown_completes_unlink` | (a) | shutdown 中の再 SIGTERM 耐性 |
| `auto_resume_resumes_self_stopped_child` | (a) | 裁定により `resume_stopped_child=false` で client 経路から隔離し、daemon `auto-resume` policy を単独検証 |
| `run_resumes_child_that_is_already_stopped_at_attach` | (a) | DR-0030 発火点1 |
| `run_resumes_child_that_stops_while_attached` | (a) | DR-0030 発火点2 |
| `status_child_state_returns_to_running_after_resume` | (a) | resume 後の daemon state regression |
| `daemon_sigcont_wakes_stopped_child` | (a) | runner-scoped config で attach resume から隔離済み |
| `daemon_cont_wakes_child_stopped_during_daemon_stop` | (b) | attach resume が daemon fallback 不発を隠す偽 green。`resume_stopped_child=false` で隔離 |
| `follow_child_self_stop_makes_attach_stopped` | (b) | DR-0029 §1 が client follow を撤回。新仕様の attach 継続＋通知行 test へ置換 |
| `lock_acquire_prints_token_and_blocks_until_sigterm` | (a) | lock lifecycle。jobcontrol と独立 |
| `restore_simple_echo_visible_in_screen_dump` | (a) | DR-0013 screen state |
| `restore_dump_is_idempotent_across_calls` | (a) | DR-0013 dump 冪等性 |
| `restore_dump_contains_ansi_control_sequences` | (a) | DR-0013 attach redraw |
| `restore_snapshot_normalized` | (a) | DR-0013 snapshot regression |
| `wait_explicit_session_wins_over_env` | (a) | DR-0020 session 解決順。ignore 理由は scrollback 未対応 |
| `wait_single_positional_resolves_self_with_env` | (a) | DR-0020 self session 解決。ignore 理由は同上 |
| `smoke_hyoui_run_echo` | (a) | DR-0015 run/attach round-trip |
| `pipe_send_eof_default_terminates_child` | (a) | DR-0019 stdin EOF `send-eof` |
| `pipe_detach_leaves_child_under_daemon` | (a) | DR-0019 stdin EOF `detach` |
| `pipe_dev_null_no_spin_and_terminates` | (a) | pipe POLLNVAL/POLLHUP regression |
| `idle_timeout_terminates_silent_child` | (a) | DR-0019 idle timeout |
| `overall_timeout_terminates_busy_child` | (a) | DR-0019 overall timeout |
| `no_timeout_keeps_silent_child_alive` | (a) | timeout 未指定の対照 |

裁定反映後の集計は (a) 27 件、(b) 2 件、(c) 0 件。

### (b) の修正

- `follow_child_self_stop_makes_attach_stopped` を
  `stopped_child_keeps_attach_running_and_draws_notice` に置換し、test file も
  `jobcontrol_stopped_notice.rs` へ改名した。test 固有 config で
  `resume_stopped_child=false` とし、子が実際に `T` の間も attach が `T` にならず、
  DR-0029 §1 の停止通知行が描画されることを e2e で検証する。
- `daemon_cont_wakes_child_stopped_during_daemon_stop` に同じ runner-scoped config を適用した。
  daemon の同一 drain batch fallback が不発でも、後段の rw attach resume で子が起きて
  green になる経路を除外した。
- test harness と production comment に残っていた `follow → raise(SIGSTOP)` の現行仕様扱いを
  DR-0029/0030 の責務分担へ更新した。

### (c) の裁定反映

`auto_resume_resumes_self_stopped_child` は `--on-child-suspend=auto-resume` が指定する
**daemon policy 自体**の regression test である。DR-0030 §4 の rw attach resume でも同じ
marker が出る構成では daemon policy が壊れても検出できないため、runner-scoped config で
`resume_stopped_child=false` とし、daemon が単独で子を起こす経路へ隔離した。

### fresh 検証

| 検証 | 結果 | 所要時間 |
|---|---:|---:|
| `auto_resume_resumes_self_stopped_child` (daemon 経路隔離後) | 20/20 PASS | 初回 compile 込み 5.82s、以降 0.95〜1.08s |
| `stopped_child_keeps_attach_running_and_draws_notice` | 20/20 PASS | 初回 compile 込み 4.12s、以降 1.17〜1.39s |
| `daemon_cont_wakes_child_stopped_during_daemon_stop` | 20/20 PASS | 2.67〜2.90s |
| 対照 `run_resumes_child_that_stops_while_attached` | 5/5 PASS | 初回 compile 込み 4.92s、以降 2.09〜2.14s |
| 対照 `daemon_sigcont_wakes_stopped_child` | 5/5 PASS | 1.91〜1.97s |

CI と同一コマンドを macOS で 3 周実行した。共有 workspace には別作業の未完了差分が
存在したため、親 commit に本 test 変更だけを載せた一時 jj workspace で検証し、他の差分を
混入させていない。

```console
cargo test --workspace -- --ignored
```

| round | 結果 | ignored test 数 | 全所要時間 |
|---:|---|---:|---:|
| 1 | PASS | 29/29 | 45.98s (compile 込み) |
| 2 | PASS | 29/29 | 18.60s |
| 3 | PASS | 29/29 | 19.35s |

`cargo fmt --all -- --check` も成功。test session 名で `pgrep -fl` を確認し、調査由来の
残骸 process が無いことを確認した。

## 追加観測 2026-08-21: 新たな恒常 red 対象と build tree 依存の再現

恒常 red の対象に `menu_client_suspend_item_wakes_child_on_fg` (DR-0032、2026-07-30 追加) が
加わっていることを確認した。

裏取り: v0.9.35 の CI run 30666359346 (ubuntu) で同 test が FAILED (8 passed; 1 failed)、
v0.9.36 の run 32438811356 では ubuntu / macOS 両方で FAILED。macOS は v0.9.34 / v0.9.35 では
success だったため、macOS 側は今回の run で初めて顕在化した。**v0.9.36 の変更内容
(size 正規化 / SIGPIPE) が原因ではない** — v0.9.35 時点で ubuntu は既に FAILED していたため。

なお `ignored-tests` job は `continue-on-error` のため workflow 自体の red の原因ではない。
v0.9.36 の workflow red は clippy `result_large_err` (rustc 1.98 の新規 lint) が真因で、
別途修正中。

### build tree 依存の再現 (原因調査継続中)

ローカルでも main workspace で決定的に再現した。v0.9.35 の commit をチェックアウトしても
FAILED し、`state=S+` のまま client 側が `T` にならない。一方 flaky4b workspace では
同一 commit でも pass する。

cwd ではなく **build tree 側に依存する**ことまで確認済み:

- cwd=flaky4b × build=main → FAILED
- cwd=main × build=flaky4b → ok

原因調査を継続中。

## 追記 2026-08-21: 恒常 red の性質が変化した

### 解消したもの

1. `menu_client_suspend_item_wakes_child_on_fg` — 真因 (attach client が初回 redraw を
   resume 証拠と誤認して menu focus を閉じる) を特定して v0.9.39 で修正。修正後の CI で
   3 attempt 連続 pass、macOS の ignored job も 3 回とも success。
2. `serve_backpressure_disconnects_slow_client` — 起票時 ubuntu 12/12 失敗だったが、
   v0.9.37/38/39 の 3 run × 2 OS で全て ok。
3. `notify_default_does_not_resume_self_stopped_child` (macOS 7 回失敗) も現在は macOS
   job 全体が success なので解消。

### 残っているもの (性質が変わった)

ubuntu のみ、**同一 commit の 3 attempt で毎回違うテストが 1 本ずつ落ちる**:

- attempt1 = `ctrlz_x1_select_on_demand_prompt_ctrl_c_quits_client` (`ctrlz_suspend_client.rs`)
- attempt2 = `daemon_second_sigterm_during_shutdown_completes_unlink` (`daemon_sigterm_graceful.rs:142`)
- attempt3 = `daemon_sigterm_terminates_child_and_unlinks_socket` (`daemon_sigterm_graceful.rs:51`)

つまり起票時の「OS ごとに固定テストが 100% fail」= 決定的 bug ではなく、**負荷依存の
flaky に移行した**。attempt2/3 が同一ファイル (SIGTERM graceful shutdown 系) に集中している
点は傾向として記録しておく。ローカル macOS では該当テスト群 5/5 pass、ignored 全体も
45 passed / 0 failed。

### 次にやるとよいこと

ubuntu runner の負荷特性 (並列実行時の CPU/IO 競合) と SIGTERM 系テストの timing 依存を
調べる。決定的 bug が残っていないなら、`continue-on-error` を外して flaky retry に
切り替える方針も検討できる (= 恒常 red を隠す構造自体の解消が本 issue の目的)。

## 追記 2026-08-24: flaky サンプルが 4 件に増加

同一 commit の 3 attempt (v0.9.39) + 別 commit (v0.9.40) で毎回違うテストが 1 本落ちる傾向は
変わらず継続中:

1. `ctrlz_x1_select_on_demand_prompt_ctrl_c_quits_client`
2. `daemon_second_sigterm_during_shutdown_completes_unlink`
3. `daemon_sigterm_terminates_child_and_unlinks_socket`
4. `handshake_snapshot_menu_remains_usable_after_initial_resize_flushes_redraw`
   (v0.9.39 で新規追加した e2e)

### (4) の詳細 (要調査、優先度中)

失敗内容: `wait_for("子プロセスが停止中") timed out after 10s`。出力は attach バナー
158 bytes のみで menu ヘッダが出ていない。

テスト側の同期は取れている (`wait_stopped(child_pid, 5s)` で kernel の T 状態を確認して
から attach している)。したがって単純な「停止前に attach した」race ではない。

仮説 (未検証):

- (a) kernel が T になってから **daemon が child_stopped を内部状態に反映するまでのラグ**
  があり、handshake snapshot が `child_stopped=false` を返している。ただしその場合でも
  後続の STOP_NOTIFY 経路で menu が出るはずだが 10s 待って出ていないので、これだけでは
  説明が付かない
- (b) ubuntu runner の負荷で単純に 10s を超えた
- (c) sync update 中 (`?2026h` 送出後に SIGSTOP) という特殊状態が daemon 側の停止検知に
  影響している

macOS では success、ローカル macOS では 10/10 pass。自分たちが追加したテストなので放置
しない。調査時は (a) を最初に潰す (daemon の status が stopped を返すまで待ってから
attach する形にテストを直すか、それとも daemon 側の検知遅延そのものが実装課題か を
切り分ける)。

