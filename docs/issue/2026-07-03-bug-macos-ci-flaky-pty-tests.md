---
title: macos CI で PTY 系 e2e が flaky fail する (outer_token_inheritance_skips_auto_acquire / child_inherits_hyoui_session_id_env)
status: open
category: bug
created: 2026-07-03T18:25:00+09:00
last_read:
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

## 受け入れ条件

- [ ] 不安定さの軸が観測データで特定されている
- [ ] 2 test が CI で安定して pass する (または根拠付きで blocked_by DR-0025 Phase N)
