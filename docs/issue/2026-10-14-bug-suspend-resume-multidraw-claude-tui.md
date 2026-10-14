---
title: "hyoui run -- claude で Ctrl+Z → fg を繰り返すと banner が多重描画 + 文字インターリーブ (DR-0029 で再現経路が消えた見込み、現版で未確認)"
status: open
category: bug
created: 2026-10-14T09:20:00+09:00
last_read: 2026-10-14T09:20:00+09:00
open_entered: 2026-10-14T09:20:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# bug: `hyoui run -- claude` で Ctrl+Z → fg を繰り返すと banner が多重描画 + 文字インターリーブ

## 現状 (2026-10-14)

DR-0029 (2026-07-25) で client が子の停止に追従する経路が廃止されたため再現経路が消えた見込み。現版での再現確認は未実施。確認して出なければ close。

以下は 2026-06-14 起票時 (hyoui 0.6.5) の記録。

- Date: 2026-06-14
- Severity: 中 (= 機能は復帰するが表示が崩れる、UX 障害)
- Reporter: kawaz
- バイナリ: hyoui 0.6.5 (= brew, repo head と同 version)

## 現象

`hyoui run -- claude` で claude code を起動し、claude 内で **Ctrl+Z で suspend → 外側 shell で `fg` で resume** を 3 回繰り返すと、claude の welcome banner が画面に多重残留し、banner と statusline の文字が **1 文字単位でインターリーブ**して読めなくなる。

観測例 (= 3 cycle 後の外側端末):

```
 ▐▛███▜▌   Claude Code v2.1.177
▝▜█████▛▘  Opus 4.7 (1M context) with high effort · Claude Max
  ▘▘ ▝▝    ~/.local/share/repos/github.com/kawaz/hyoui/main

──────────────────────────────────────────────
❯
Claude Code has been suspended. Run `fg` to bring Claude Code back.───
Note: ctrl + z now suspends Claude Code, ctrl + _ undoes input.   0 tokens
 ▐▛███▜▌odeClaudezCodeuv2.1.177skpkolsx b8d67dfd 💬...      ● high · /effort
▝▜█████▛▘ssOpusm4.7i(1M context)twith highyeffort · ClaudeeMax  /rc active
  ▘▘ ▝▝    ~/.local/share/repos/github.com/kawaz/hyoui/main
[以下同様にもう 1 セット繰り返し]
```

「odeClaudezCodeuv2.1.177skpkolsx」のような文字列は、claude welcome banner の `Claude Code v2.1.177` と statusline の `skpkolsx b8d67dfd 💬...` が **同一行に書き込み合っている** ように見える (= 連続書き込みで前文字を完全消去せずに上書き)。

## 想定される構造

claude TUI が Ctrl+Z handler で `?1049l` + 「suspended」メッセージを primary に出してから `raise(SIGSTOP)` する流れ。`SessionChildStoppedNotify` を受けた client は `SUSPEND_OUTER_TTY_RESET` を吐いて自身も SIGSTOP。外側 fg で client が SIGCONT → `SessionChildResumeRequest` → daemon が `build_attach_redraw` (= `?1049l` or `?1049h` prepend + `state_formatted`) を client に push → `killpg(child, SIGCONT)`。

ここで:

1. daemon の screen state は suspend 時点で **primary buffer + suspended msg** を抱えている可能性が高い (= claude が `?1049l` してから raise した直後の vt100 state)
2. resume 時 redraw は **primary buffer の state_formatted** を流す → outer terminal は primary mode のまま
3. その後 claude が SIGCONT で復帰 → `?1049h` + alt screen に banner + statusline を draw
4. cycle を繰り返すと alt screen の前 cell 内容が完全 clear されないまま新規 banner が書き込まれる?

または:

- claude が SIGCONT 復帰時に `?1049l` → `?1049h` を高速で切替するが、その途中 daemon redraw bytes も混ざる
- vt100 state が primary/alt 切替を取りこぼし、`state_formatted` が **alt buffer cells を primary buffer に書く** ような出力をしている

## 重要な観察ポイント

- 「文字インターリーブ」(= 1 文字単位での混在) が起きるのが特異。普通の重ね描画ならブロック単位で重なる
- claude 側の redraw 出力 (`?1049h` + 全文字列) と hyoui daemon の `state_formatted` (= cell-by-cell 出力) が **同一行に並列に届いて** カーソル位置を取り合っている可能性
- 「Ctrl+Z 直後の vt100 state は primary だが、claude SIGCONT 復帰後はすぐ alt」なので、redraw bytes と claude 自身の出力が **異なる buffer 想定で書く** ことが root cause かも

## 再現手順

1. `hyoui run -- claude`
2. claude プロンプトが出たら Ctrl+Z で suspend
3. 外側 shell で `fg` で resume
4. step 2-3 を 3 回繰り返す
5. claude UI が崩れて読めなくなる

## 検証 (要実施)

- [ ] hex:1a を `hyoui input` で送信して同じ再現を非対話的に作る (= 再現ハーネス化)
- [ ] `hyoui screen dump --format=ansi` で各 cycle の screen state を保存 → 差分を見る
- [ ] `hyoui tail --since-strict` で daemon が送った redraw bytes 列を取り出して解析
- [ ] vim / less / cat / bash でも同じ pattern (Ctrl+Z → fg ×3) を試して、claude 特有か汎用問題か切り分け
- [ ] claude の SIGTSTP handler 出力 bytes 列を asciinema で記録、`?1049l` / `?1049h` の出現パターンを把握

## 関連

- DR-0013 §4 attach redraw (= `build_attach_redraw`、primary/alt prepend)
- DR-0015 fork daemon + attach client 構造
- DR-0014 §self-check (= 透過原則、推測実装禁止)
- `crates/hyoui/src/client/attach.rs` SUSPEND_OUTER_TTY_RESET / SessionChildResumeRequest 送信
- `crates/hyoui/src/daemon/control.rs` handle_session_child_resume_request (redraw → SIGCONT)
- `crates/hyoui/src/daemon/screen/redraw.rs` build_attach_redraw
- 既存類似 issue: 2026-06-12-tcsaflush-input-discard-in-suspend-resume (= 同じ suspend/resume hook 経路だが別問題)

## TODO

- [x] 非対話再現ハーネス試行 — `--detached` では再現せず (理由は調査結果参照)
- [ ] 実機 (= attach client あり) で kawaz が `hyoui run --debug-dump-client=/tmp/c.bin --debug-dump-server=/tmp/s.bin -- claude` を実行し 3 cycle 後の bytes file を提供
- [ ] 取得 bytes を解析 → 仮説 A/B/C を切り分け
- [ ] vim でも attach client 経由で同様に再現するか実機確認
- [ ] root cause 確定 → DR/journal に記録 → fix 実装

## 調査結果 2026-06-14 (subagent)

### 非対話再現が不可能な理由

`--detached` で `kill -CONT $child_pid` 直送ルートは:
- `attach.rs::SUSPEND_OUTER_TTY_RESET` 書き出し経路を bypass
- `daemon/control.rs::handle_session_child_resume_request` (= `send_attach_redraw → killpg` の 2 段階) を **通らない**
- → 多重描画 path 自体が走らない

実バグは `hyoui run -- claude` の attach client 経路で `SessionChildResumeRequest` を daemon が受けて `send_attach_redraw` を push したときにのみ起きる。

### root cause 仮説 (優先度順)

**仮説 A (最有力)**: client socket buffer の pending raw_data frame が SIGSTOP に追い越される
`client/attach.rs::run` L538-720 の poll loop は 1 iteration で 1 frame しか処理しない。`SessionChildStoppedNotify` 受信 iteration で socket buffer に未処理の claude PTY 出力 (= suspend cleanup 描画 bytes、`?1049l` 等) が残っているケースで、resume 後にそれらが attach_redraw の前後で stdout に流れて累積する。

**仮説 B**: `send_attach_redraw` enqueue → `killpg(SIGCONT)` 間の race
`daemon/control.rs::handle_session_child_resume_request` L375-381 で enqueue → killpg の順だが、write thread の scheduling 次第で claude SIGCONT 復帰時の `?1049h` 描画が client への redraw 配信を追い越す可能性。同一 socket FIFO のはずだが、daemon→client socket と child→daemon PTY は別 fd 別経路。

**仮説 C**: claude の SIGTSTP handler が `?1049l` を発行しない
tail 65536 bytes に `?1049l` が無いという observation あり (= 非対話 detached のみ確認、要実機追検証)。claude が alt screen のまま suspended msg を primary 想定で書いている可能性。`build_attach_redraw` が alt-prepend `?1049h` を被せた上で `state_formatted` を流すため、外側 shell の primary scrollback に alt buffer cells が描画される。

### 副次 bug (= 道具側、別途追跡)

1. **`child_stopped` latch が auto-resume 経路で下りない**: `session.rs::notify_child_stopped` L984-987 は AutoResume で `killpg(SIGCONT)` 後即 return、`set_child_stopped(false)` は `record_child_continued` (= `WaitStatus::Continued` 観測時) でのみ呼ばれる。子が実際 STOP しないケース (= python3 で観測) では `Continued` も来ず latch が立ったまま残り、`hyoui list` の `stopped` 表示が嘘になる。本セッション (= run-21491-7b6e6acb) も `stopped` 表示で即時再現中
2. **bash 起動直後の `seqno=0` race**: `ScreenState::process()` が 0 回呼ばれた状態で server raw が 436 bytes 観測されるタイミング依存の race

### 次の決定打 (= kawaz への依頼)

`hyoui run --debug-dump-server=/tmp/srv.bin --debug-dump-client=/tmp/cli.bin -- claude` で起動し 3 cycle 後に file 提供。これで claude PTY → daemon (server.bin) と daemon → client (cli.bin) の raw bytes が全部取れ、仮説 A/B/C を decisive に分離可能。
