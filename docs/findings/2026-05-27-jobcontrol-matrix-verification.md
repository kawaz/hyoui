# DR-0001 jobcontrol 軸 1/2 マトリクス検証結果 (2026-05-27)

- Date: 2026-05-27
- Scope: DR-0001 軸 1 (`--on-child-suspend=follow|auto-resume`) + 軸 2
  (`--on-parent-suspend=transparent|decouple`) の現状実装の挙動を実機 PTY 上で
  確認した結果
- 関連 task: #32 (= 本 task)、#34 (= DR-0001 軸 1/2 実装、本 findings の修正対応)
- 関連 DR: [DR-0001](../decisions/DR-0001-bgfg-jobcontrol-two-axis.md),
  [DR-0014](../decisions/DR-0014-transparency-and-empirical-verification.md)
- 関連 file:
  - `crates/hyoui-cli/tests/matrix_jobcontrol_axis1.rs` (= 軸 1、6 cell)
  - `crates/hyoui-cli/tests/matrix_jobcontrol_axis2.rs` (= 軸 2、5 cell)
  - `crates/hyoui-cli/tests/matrix_attach_restore.rs` (= attach 復元、4 cell)

## 判明した事実

### 1. DR-0001 軸 1 (= 子の suspend に対する親の挙動) は **未配線**

CLI flag `--on-child-suspend=follow|auto-resume` は parse される (= `cli.rs` で
`OnChildSuspend` enum に格納) が、run 経路に **動作として配線されていない**。
結果:

- 子に外部から SIGSTOP を送って STOPPED 状態にしても、**親 hyoui-cli は Running
  のまま** (= 軸 1 `follow` で期待される「親 SIGSTOP raise」が走らない)
- 子に外部から SIGSTOP を送って STOPPED にしても、**親が即 SIGCONT を送る挙動も
  ない** (= 軸 1 `auto-resume` で期待される「子を強制復帰させる」が走らない)
- `--mode=headless` preset の `auto-resume` も同様に配線なし

検証 cell (= 全 6 cell):

| cell | App | mode | flag override | 期待動作 (DR-0001) | 実態 (= 軸 1 未実装) |
|---|---|---|---|---|---|
| A1 | `/bin/sleep 30` | interactive | default (= follow) | 親 STOPPED に follow | 親 Running のまま |
| A2 | `/bin/sleep 30` | interactive | explicit `=follow` | 親 STOPPED に follow | 親 Running のまま |
| A3 | `/bin/sleep 30` | interactive | explicit `=auto-resume` | 子が即復帰 | 子 STOPPED のまま放置 |
| A4 | `/bin/sleep 30` | headless | default (= auto-resume) | 子が即復帰 | 子 STOPPED のまま放置 |
| A5 | `/bin/cat` | interactive | default | 親 STOPPED に follow | 親 Running のまま |
| A6 | `bash --norc -i` | interactive | default | 親 STOPPED に follow | 親 Running のまま |

### 2. DR-0001 軸 2 (= 親の suspend に対する子の挙動) も **未配線**

CLI flag `--on-parent-suspend=transparent|decouple` も parse されるが配線なし。
結果:

- 親 hyoui-cli に外部 SIGTSTP を送っても、**子 pgrp に SIGSTOP は送られない**
  (= 軸 2 `transparent` で期待される「子も停止」が走らない)
- 結果として interactive default でも headless default でも実態は同じ
  (= 子はそのまま走り続ける = 実質 `decouple` と同じ)
- 親自身の状態については、SIGTSTP の default 動作で STOPPED になっているか、
  PTY raw mode 等の影響で配送が変わっているかは cell ごとに観測結果を残す
  (= test の `eprintln!` で informational に log)

検証 cell (= 全 5 cell):

| cell | App | mode | flag override | 期待動作 (DR-0001) | 実態 |
|---|---|---|---|---|---|
| B1 | `/bin/sleep 30` | interactive | default (= transparent) | 子も STOPPED | 子 Running のまま |
| B2 | `/bin/sleep 30` | headless | default (= decouple) | 子は走り続ける | 子 Running (decouple と一致) |
| B3 | `/bin/sleep 30` | interactive | explicit `=decouple` | 子は走り続ける | 子 Running (一致) |
| B4 | `bash --norc -i` | interactive | default | 子 bash も STOPPED | 子 bash Running |
| B5 | `/bin/cat` | interactive | default | 子 cat も STOPPED | 子 cat Running |

軸 2 の cell は、headless default `decouple` および明示 `=decouple` の cell は
**実態と期待動作が一致**するため、task #34 で実装が入っても test は壊れない想定
(= regression sensor として機能する)。

### 3. POSIX `POSIX_SPAWN_SETSID` + orphan process group の影響

DR-0001 §実装ノートで「子は独立セッションリーダー (= 子の pgid == 子の pid)」と
明記されている通り、`POSIX_SPAWN_SETSID` で起動された子は session leader 兼
process group leader。session 内には子の process group しかいない (= orphaned
process group)。

このため POSIX §3.107 により orphan group メンバーが SIGTSTP / SIGTTIN / SIGTTOU
を受け取ると **kernel が discard** する:

- 「子が自分自身に `kill -TSTP $$` で SIGTSTP を送る」テスト構成は **成立しない**
  (= signal が discard される)
- SIGSTOP は catch 不可で常に届くため、test では SIGSTOP で代替して STOPPED 状態
  を作っている

本来の `Ctrl-Z` 経路 (= PTY line discipline → cooked が SIGTSTP を発火) は、内側に
job control を持つ shell や TUI app がいて初めて意味を持つ。本 task の category
3 種では bash interactive のみがこのケースに該当するが、bash の line discipline
経由 SIGTSTP test は別 task (= harness 側で `\x1a` send + bash 側の job control
への配送確認まで含めて検証が必要) に切り出した方が読みやすい。

### 4. attach 復元 (= screen dump) は正常動作

DR-0013 §3-§4 Phase A の screen state 正本化と attach redraw の最小単位は動作
している:

| cell | 観点 | 結果 |
|---|---|---|
| C1 | `printf "hello\nworld\n"` の出力が screen dump に反映 | ✓ |
| C2 | 同じ socket への連続 screen dump は idempotent | ✓ |
| C3 | `--format=ansi` の出力が ANSI sequence (ESC) を含む | ✓ |
| C4 | normalized snapshot を insta で固定 (regression sensor) | ✓ (snapshot 確定) |

C4 の insta snapshot:

```
[?25h[m[H[Jmarker A
marker B
>[?1l[?2004l
```

(= `[?25h` cursor show、`[H[J` カーソル原点 + screen clear、`marker A` + LF +
`marker B`、`>` prompt 残り、`[?1l[?2004l` mode reset)。

## 実用的な示唆 / ベストプラクティス

### task #34 (= DR-0001 軸 1/2 実装) 着手時の手順

1. 本 findings の **「実態」列を全部「期待動作」列と一致させる** ように実装する
2. 軸 1 配線の test trigger (= 必ず壊れる cell):
   - A1, A2, A5, A6: `!parent_after.is_state('T')` → `parent_after.is_state('T')`
     に反転 (= follow 実装後、親が STOPPED になるはず)
   - A3, A4: `child_after.is_state('T')` → `!child_after.is_state('T')` に反転
     (= auto-resume 実装後、子は STOPPED 滞留しないはず)
3. 軸 2 配線の test trigger:
   - B1, B4, B5: `!child_after.is_state('T')` → `child_after.is_state('T')`
     に反転 (= transparent 実装後、子も STOPPED に follow するはず)
   - B2, B3: そのまま (= decouple は実態と一致、pass し続ける想定)

### test の `wait_for_child` 同期 token

posix_spawn 完了までの待ち時間は固定 sleep ではなく `wait_for_child(parent_pid,
max_ms)` で polling すること。fixed sleep (= 200ms 等) は CI / 高負荷環境で
flaky になりがちで、`hyoui-cli` の test 開発初期に実際に flaky 化した
(= 当初 200ms settle で fail、500ms 以上の polling で安定)。

### macOS の `ps -o sid=` 非対応 workaround

macOS の `ps` は `sid` keyword をサポートしない (Linux は OK)。両 OS 動作する
keyword set は `pid,ppid,pgid,stat,comm`。session leader 判定は `pgid == pid`
で代替する (= DR-0001 §実装ノート「子の pgid == 子の pid」と同じ判定方法)。

これに合わせて harness の `ProcessState` から `sid` field を削除した (= 互換性
破壊だが、harness 自体が DR-0014 制定後の新規 module で外部 dep なし)。

## 検証の詳細

### 実行手順

```bash
cd /Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main
cargo test -p hyoui-cli --test matrix_jobcontrol_axis1
cargo test -p hyoui-cli --test matrix_jobcontrol_axis2
cargo test -p hyoui-cli --test matrix_attach_restore
```

3 file 合計 **15 cell**。実行時間は手元 macOS Darwin 25.5.0 で **5-7 秒** 程度。

### probe で確認した OS 挙動

`Bash` tool 経由で `target/debug/hyoui run -- /bin/sleep 30 &` を直接起動し、
`ps -A -o pid=,ppid=,pgid=,stat=,comm=` で観測した raw 結果:

```text
# 親 hyoui-cli (pid=48809) の child は /bin/sleep (pid=48811)
48811 48809 48811 SNs+ /bin/sleep
# 子の pgid (48811) == pid (48811): session leader 確認

# `kill -TSTP 48809` (= 親 hyoui に外部 SIGTSTP) 直後
48809 48805 48805 SN   target/debug/hyoui     # 親は SN のまま (STOPPED にならない)
48811 48809 48811 SNs+ /bin/sleep             # 子も SNs+ (Sleeping、STOPPED でない)

# `kill -STOP <sleep_pid>` (= 外部 SIGSTOP を子に直接送る)
50026 50023 50026 TNs+ /bin/sleep             # 子 STOPPED (T 始まり)
50023 50020 50020 RN   target/debug/hyoui     # 親 Running のまま (follow しない)

# `kill -TSTP <sleep_pid>` (= 外部 SIGTSTP を子に送る、orphan のため discard)
50026 50023 50026 SNs+ /bin/sleep             # 変化なし (signal discard)
```

### 各 cell の判定

#### Axis 1: matrix_jobcontrol_axis1.rs

##### axis1_sleep_interactive_default_external_sigstop (A1)
- spawn: `hyoui run --mode=interactive -- /bin/sleep 30`
- 操作: 子 sleep に SIGSTOP
- assert: 子 STOPPED ('T') + 親 Running (NOT 'T')
- 結果: pass (= 現実態固定)

##### axis1_sleep_interactive_explicit_follow (A2)
- spawn: `hyoui run --mode=interactive --on-child-suspend=follow -- /bin/sleep 30`
- 操作: 子 sleep に SIGSTOP
- assert: 親 Running (NOT 'T')
- 結果: pass (= flag は parse 済だが配線なし)

##### axis1_sleep_interactive_explicit_auto_resume (A3)
- spawn: `hyoui run --mode=interactive --on-child-suspend=auto-resume -- /bin/sleep 30`
- 操作: 子 sleep に SIGSTOP
- assert: 子 STOPPED ('T') (= auto-resume なら復帰するはず)
- 結果: pass (= 軸 1 auto-resume 未配線)

##### axis1_sleep_headless_default_auto_resume (A4)
- spawn: `hyoui run --mode=headless -- /bin/sleep 30`
- 操作: 子 sleep に SIGSTOP
- assert: 子 STOPPED ('T') (= headless preset auto-resume 未配線)
- 結果: pass

##### axis1_cat_interactive_default_external_sigstop (A5)
- spawn: `hyoui run --mode=interactive -- /bin/cat`
- 操作: 子 cat に SIGSTOP
- assert: 親 Running (NOT 'T')
- 結果: pass (= cat category)

##### axis1_bash_interactive_default_external_sigstop (A6)
- spawn: `hyoui run --mode=interactive -- bash --norc -i`
- 操作: 子 bash に SIGSTOP
- assert: 子 STOPPED ('T') + 親 Running (NOT 'T')
- 結果: pass (= bash REPL category)

#### Axis 2: matrix_jobcontrol_axis2.rs

##### axis2_sleep_interactive_default_external_tstp (B1)
- spawn: `hyoui run --mode=interactive -- /bin/sleep 30`
- 操作: 親 hyoui に SIGTSTP
- assert: 子 Running (NOT 'T')。親の状態は informational log のみ
- 結果: pass

##### axis2_sleep_headless_default_external_tstp_decouple (B2)
- spawn: `hyoui run --mode=headless -- /bin/sleep 30`
- 操作: 親 hyoui に SIGTSTP
- assert: 子 Running (NOT 'T') (= decouple と一致、実装後も pass)
- 結果: pass

##### axis2_sleep_interactive_explicit_decouple (B3)
- spawn: `hyoui run --mode=interactive --on-parent-suspend=decouple -- /bin/sleep 30`
- 操作: 親 hyoui に SIGTSTP
- assert: 子 Running (NOT 'T')
- 結果: pass (= 実装後も pass する想定)

##### axis2_bash_interactive_default_external_tstp (B4)
- spawn: `hyoui run --mode=interactive -- bash --norc -i`
- 操作: 親 hyoui に SIGTSTP
- assert: 子 bash Running (NOT 'T')
- 結果: pass (= bash REPL category)

##### axis2_cat_interactive_default_external_tstp (B5)
- spawn: `hyoui run --mode=interactive -- /bin/cat`
- 操作: 親 hyoui に SIGTSTP
- assert: 子 cat Running (NOT 'T')
- 結果: pass (= cat line-oriented category)

#### Attach restore: matrix_attach_restore.rs

##### restore_simple_echo_visible_in_screen_dump (C1)
- spawn: `hyoui run --mode=headless -- /bin/sh -c 'printf hello\nworld\n; sleep 30'`
- 操作: `hyoui screen dump --format=ansi` を polling で叩く
- assert: normalized dump に `hello` + `world` 両方含まれる
- 結果: pass

##### restore_dump_is_idempotent_across_calls (C2)
- spawn: 同上 (= `one,two,three` の出力)
- 操作: 100ms 間隔で 2 回 dump
- assert: 2 dump の normalized 文字列が完全一致
- 結果: pass

##### restore_dump_contains_ansi_control_sequences (C3)
- spawn: `printf 'marker\n'; sleep 30`
- 操作: dump 取得
- assert: dump bytes に ESC (0x1b) を含む
- 結果: pass

##### restore_snapshot_normalized (C4)
- spawn: `printf 'marker A\nmarker B\n'; sleep 30`
- 操作: dump 取得 → normalize → insta snapshot
- assert: 既存 snapshot file と一致
- 結果: pass (= snapshot 初回登録済)

## 関連

- [DR-0001](../decisions/DR-0001-bgfg-jobcontrol-two-axis.md) — jobcontrol 2 軸の正本
- [DR-0014](../decisions/DR-0014-transparency-and-empirical-verification.md) — 検証主義 self-check
- [DR-0013](../decisions/DR-0013-screen-emulator-and-attach-stability.md) — screen state 正本化
- `docs/findings/2026-05-27-self-audit-after-dr-0014.md` — 本 audit の上位、本 findings の検証 cell リスト 1-10 の起点
- 関連 task: #34 (DR-0001 軸 1/2 実装) — 本 findings の修正対応 task
