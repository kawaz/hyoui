# 2026-07-03 プロジェクト監査 → 正直化 fix 群 + CI hang 観測 (session a7761122)

kawaz の「プロジェクト確認 + 不備指摘」依頼から始まった監査と、承認を受けた fix 群の記録。
DR-0025 Phase 1a / triage 本体は並行 session 8442a619 が担当 (本 journal は a7761122 分)。

## 監査で確定した不備 → 対応

| 不備 | 対応 |
|---|---|
| release.yml が CI と独立でテスト未実行 (CI red のまま v0.9.5 出荷の実害) | ci.yml に `workflow_call` 追加 + release.yml に `ci` job (`uses: ./.github/workflows/ci.yml`)、`build-release` の needs に配線 |
| semver gate が latest-tag 単独 (DR-0039 canonical から乖離) | latest-release / latest-tag 並列 check に同期。trigger が paths [Cargo.toml] のため「eq = 良性 skip / lt = error」に適応 (canonical の一律 error は非 bump push を赤にするので不採用) |
| `record --input-secrecy` が 3 値とも飾り (default `redact-after-prompt` でも stdin 素通し記録、header は policy 名を虚偽申告) | interim 正直化: default を `record-all` へ、`redact-after-prompt` は CLI parse + daemon 双方で未実装エラー、`never-record-stdin` は実実装。Phase 5 本実装は issue 継続 |
| **interactive 打鍵 / pipe 入力で attach client が即死** (v0.9.4 以降の critical、下記) | attach run loop に RawAck 読み捨て arm + interactive 打鍵 regression test 新設 |
| bc test の failure (上記の症状だった。「bc 互換性」は誤診) | test を bc 非依存 (POSIX sh read-loop) に書き換え + 真因訂正で close |
| wait / lock --mode=wait の polling 実装、daemon lock の半実装 (`let _ = req;`) | DR-0025 が構造対処 (Phase 1a は peer が land 済)。tx issue に Phase 2-3 適期の注記 |
| repo root の claude*.cast 3 件 | testdata/ へ移動、findings の参照も更新 |

## CI 健康状態の訂正 (= 初期調査 subagent 報告の誤りを実ログで訂正)

初期の CI 調査 subagent は「main red の原因 = bc test (macos Test job)」と報告したが、
実ログでの裏取り結果は異なる (= subagent 報告も検証主義の対象という教訓):

- bc test は `#[ignore]` 付きで **non-blocking の ignored-tests job でしか走らない**。
  main red の原因ではない (fix の価値自体は不変)
- 6/30 の main red の実体 = cargo-audit fail + **macos Test job の PTY 系 flaky 2 件**
  (`outer_token_inheritance_skips_auto_acquire` pty.rs:96 panic /
  `child_inherits_hyoui_session_id_env` self_session_id_env.rs:34 panic)。
  両者とも 2026-07-03 の run では pass (= flaky 確定、issue 起票)
- cargo-audit は 2026-07-03 run で green (解消済)
- ubuntu ignored-tests の backpressure deadlock は 2026-07-03 も 30s panic で継続
  (既知 issue、Phase 2 blocked)

## Critical 発見: attach run loop の RawAck 未処理 (= interactive 打鍵の全滅)

bc test の bc 非依存化を検証したら**書き換え後も同一 failure** (stdout 空 + exit 1)。
stderr を観察すると `unknown frame type from daemon` — bc は無関係だった。

- 真因: DR-0021 (v0.9.4) で daemon が raw_data write 完了ごとに RawAck (0x02) を返す
  ようになったが、attach run loop の frame dispatch に arm が無く unknown frame 扱いで
  client が exit 1。`hyoui input` 経路は ack 対応済みなので自動化だけ無事だった
- 実機再現: python openpty (80x24) + `hyoui run -- cat` + 打鍵 1 回で 100% 再現。
  brew の v0.9.4 でも同一 → **リリース済みバイナリの interactive 打鍵は 2.5 週間壊れていた**
- e2e の盲点: attach 経由で通常 byte を打鍵する test が 0 件 (detach key は client 内
  prefix 処理で frame 非発生) → regression test を新設 (attach_interactive_input.rs)
- 詳細: docs/issue/archive/2026-07-03-bug-attach-run-loop-drops-rawack.md
- 副産物: `script -q /dev/null` (0x0 winsize) で既知の vt100 zero-size panic
  (bug-vt100-zero-size-pty-panic) の再現手順を発見、issue に追記予定

## CI hang の観測 (run 28644829018)

docs-only commit 834a5742 の CI で `Test (ubuntu-latest / stable)` が 1h50m 無出力 hang。
cancel してログ回収した結果:

- hyoui-cli main.rs unit tests 136 件中 134 完走、未完了 2 件を ok 出力との差分で特定:
  `send_raw_bytes_partial_byte_race_regression` / `list_marks_stale_socket_when_no_ping_response`
- cancel 時 orphan に `cat` (= test が spawn した子) 残留
- 既知の backpressure deadlock (lib 側 --ignored) とは別事象。直近 3 run の ubuntu は green
  だったので flaky
- 起票: `docs/issue/2026-07-03-bug-main-unittest-hang-ubuntu-ci.md`
- 暫定: ci.yml test job に `timeout-minutes: 30` (6h 空焼き防止)

ハング test 特定の手順 (再利用可):

```bash
# 完走した test 名を抽出
gh api repos/kawaz/hyoui/actions/jobs/<job_id>/logs |
  awk '/Running.*<binary>/{s=1} s' |
  grep -oE 'test [a-z_:]+ \.\.\. ok' | sed 's/^test //; s/ \.\.\. ok//' | sort > ok.txt
# 全 test 名 (当該 commit の source から) と comm -23 で差分
```

## 並行 session との調整

- 16:14-17:49 に peer (session 8442a619) が DR-0025 Active 化 / Phase 1a / triage 反映 /
  Release v0.9.6 準備を同一 workspace で進行。当方の未 commit 編集は peer が wip change に
  退避 (`jj describe` + `jj new`) → 当方が `jj squash --from <wip> --into @` で回収
- cmux-msg で直接調整 (peer の sid は project dir の session jsonl 更新時刻から特定)。
  合意: v0.9.6 push は保留、当方 fix を同梱して当方が `just push` まで実施。
  peer の 2 commit (93102075 / ce1793c2) は rebase / 改変しない
- 教訓: 同一 workspace 並行作業では cmux-msg の生存 peer 登録が dead でも
  session jsonl の mtime で相手を特定できる

## 上流への dogfooding 還元

- `/local-issue:list` / `read` が Skill tool 経由の fork 実行で $ARGUMENTS を受け取らず
  空振り (2 回再現)。上流に起票済み:
  **claude-local-issue `docs/issue/2026-07-03-skill-tool-fork-invocation-drops-arguments.md`**
  (commit 1dea916、push 済)。本リポでの issue 直接 Read/Write は当該 bug のワークアラウンド
