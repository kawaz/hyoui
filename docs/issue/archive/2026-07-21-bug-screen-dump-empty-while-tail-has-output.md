---
title: screen dump/snapshot が空 (seqno=0) を返す — idle 15s 経過の stalled auto-reset が false-positive で全 cell を破棄している
status: resolved
category: bug
created: 2026-07-21T09:15:00+09:00
last_read:
open_entered: 2026-07-21T09:15:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-07-21T01:31:47+09:00
discard_reason:
pending_reason:
close_reason: ["done"]
blocked_by:
origin: 自リポ TODO
---

# screen dump/snapshot が空 (seqno=0) を返す — idle 15s の stalled auto-reset が false-positive で cells を破棄

## 概要

`hyoui screen dump <session> --format=ansi` が **子は live で tail に scrollback もあるのに 30 byte の空画面** を返す。原因は DR-0013 §5 Phase B の stalled auto-reset が、子が単に **静か** なだけの状態 (= 標準的な TUI が入力待ちで idle) を「broken byte stream」と誤判定し、15 秒後に `ScreenState::reset()` で cells / cursor / seqno を全消しするため。

CLAUDE.md「partial state を扱う実装の規律」節 (`default は warn のみ + 手動操作`) に真っ向反する挙動。DR-0014 self-check 「透過原則を破るが、その理由は「必然」か?」にも該当 (= 便利のための介入で、必然ではない)。

## 再現手順 (2026-07-21、hyoui 0.9.12 で実機再現)

```
$ SESS=$(hyoui run --detached -- bash -c 'echo alive; sleep 3600')
$ sleep 20    # STALLED_RESET_TIMEOUT * STALLED_RESET_CONSECUTIVE_DETECTS = 5s * 3 = 15s 超
$ hyoui screen snapshot $SESS --include=Cursor,SequenceNo | od -c | head -2
# → cursor row=0 col=0, sequence-no=0
$ hyoui screen dump $SESS --format=ansi | wc -c
# → 30 (= \x1b[?25h\x1b[m\x1b[H\x1b[J\x1b>\x1b[?1l\x1b[?2004l のみ)
$ hyoui tail $SESS --last-bytes=200 | head -1
# → "alive\r\n" ← byte-base scrollback には残っている (= 子は正常に出力していた)
```

再現条件は「PTY 子が最後の出力から 15 秒以上 idle」だけ。TUI 待ち画面 / bash プロンプト待ち / claude TUI で入力待ち等、**通常の子プロセスの静止**で 100% 発火する。

## 実観測 (2026-07-21)

**live claude TUI (`run-61898-bb2c8e1a`, PID 61907 STAT S+, DUR 3h48m)**:

- child-state: running、tail --last-bytes=3000 は ANSI 色付き UI 描画あり (= 過去に子は多量に出力していた)
- `screen dump --format=ansi`: 30 byte (= 空画面 trailer のみ)
- `screen snapshot`: `cursor row=0 col=0`, `sequence-no=0`, `rows=24 cols=80` (config 既定)

**別セッション比較 (`run-143-96c02784`, 同 claude TUI, 会話中で連続出力あり)**:

- 同じ条件で `dump` は 2608 byte 正常、`sequence-no=25`, `cursor row=12 col=2`

差は「直近 15s 以内に子が何か出力したか」だけ。live claude が入力待ちで静止 → auto-reset 発火 → cells 全消し。

過去観測との突合:

- `run-98723-8517bd80` (stopped child) の 30 byte 空 dump も同じ原因 (= stopped 子は当然 15s 出力なし → reset)。「stopped だから空」ではなく「idle >15s だから reset された」が正解。
- 「CONT 送信でセッション消滅」は本件とは別事象の可能性。本 issue とは分離して扱う (= 本 issue の修正では改善しない前提)。

## 原因

`crates/hyoui/src/daemon/session.rs:781-808` の `detect_and_warn_stalled` が、`crates/hyoui/src/daemon/screen/state.rs:44-52` の `STALLED_RESET_TIMEOUT=5s` * `STALLED_RESET_CONSECUTIVE_DETECTS=3` = 15 秒経過で `screen_state.reset()` を呼ぶ。

`reset()` の破壊範囲 (`state.rs:300-314`):

- `parser` を `vt100::Parser::new(rows, cols, scrollback_len)` で作り直し → **cells 全消し + cursor(0,0) + alt-screen off + mode 全リセット**
- `current_seqno = 0`
- `input_log` clear
- `last_feed_at = now`

reset 後、子が新 byte を出さない限り (= idle が継続する限り) この状態が永続する。子は正常なのに daemon 側が「壊れている」と誤判定して全状態を捨てる。

### 元 DR (DR-0013) の意図とのズレ

DR-0013 §5 の由来は tmux `input.c` の 5s 挙動。tmux 側は **parser 内部の partial escape sequence buffer** (= 途中まで来て完成していない ESC ...) を捨てるだけで、画面 grid や cursor は触らない。hyoui 側は vt100 crate に partial buffer だけを reset する API が無いため、**parser 全体を作り直す** ショートカットを取っている。結果として tmux の意図する介入範囲より遥かに広い破壊が起きている。

CLAUDE.md 「partial state を扱う実装の規律」の直接的な違反:

- ❌ default は warn のみ + 手動操作 → 実装は default で自動破棄
- ❌ 自動破棄が必要なら判定基準を DR に明示 + マトリクス検証で false-positive 検証 → 「idle >15s」という判定基準は明らかな false-positive パターン (= 静止した TUI) を検証していない
- ❌ 「子は正常だが時間がかかっている」ケースを false-positive で破棄しない → まさに本件

## 影響

- `hyoui screen dump/snapshot` は **子が静止した瞬間から使い物にならなくなる**。claude TUI / vim / less / bash 待ち等、hyoui の主要ユースケースが軒並み該当。
- attach 復元 (redraw_sequence) も cells が空なら空の画面を復元する形になり、attach 直後に「白紙が返る」現象を起こす可能性あり (= 別 issue 化検討)。
- `hyoui wait --text=...` の visible-based match が cells 空で false negative になる (= 同 wait-scrollback-snapshot-coverage の一部と重なる可能性)。
- byte-base scrollback (= `Scrollback` layer) は健全なので tail 系は生存。

## 修正方針案 (実装は指示待ち)

### 案 A: auto-reset を default off にする (推奨)

CLAUDE.md 規律「default は warn のみ + 手動操作」に合わせる。`detect_and_warn_stalled` から `screen_state.reset()` 呼び出しを外し、warn log (現状 silent) だけを残す。手動 reset が要る場面には `hyoui screen reset <session>` の CLI を用意する (= 別 issue 化)。

- 利点: false-positive 発火ゼロ、CLAUDE.md 規律準拠、透過原則優先
- 欠点: 真に broken な partial escape が入った場合の自動復旧はなくなる (= 手動 reset まで parser がおかしい状態が続く可能性)。ただしこれが実際に起きる頻度は本 false-positive より遥かに低いと想定。

### 案 B: 閾値を極端に上げる

`STALLED_RESET_CONSECUTIVE_DETECTS` を 3 → 数百に、あるいは `STALLED_RESET_TIMEOUT` を数時間にする。false-positive を実質発生させない値にする。

- 利点: 実装最小 (定数 1 個)
- 欠点: 「壊れた stream の自動復旧」の意義がほぼゼロに近づき、機能として名だけ残る形になる。案 A のほうが誠実。

### 案 C: 「partial escape sequence がある時だけ reset」の絞り込み

vt100 crate に「今 parser が partial state か?」を判定する public API を追加する PR を上流に出す or fork。それを見て partial state のときだけ reset する。

- 利点: 元 DR-0013 §5 の意図 (= tmux input.c 準拠) に忠実
- 欠点: 実装コスト大、上流依存、hyoui 単独では即応不可

### 推奨: 案 A + 手動 reset CLI の合成

即修正の最小変更として案 A。後付けで手動 reset の CLI を用意する (= 別 issue)。

## 関連

- DR-0013 §5 Phase B (auto-reset の判断が本 issue の元)
- DR-0014 §self-check「透過原則を破るが、その理由は「必然」か?」に該当する不必然介入
- CLAUDE.md §partial state を扱う実装の規律 (本 issue の正本規律)
- 別件懸念: attach 時の redraw が cells 空を復元する場合の挙動 (= 別 issue 化検討)
- 別件懸念: CONT 送信でセッション消滅する現象 (2026-07-20 15:07 頃観測、本件とは別事象の可能性)

## 検証マトリクス (今後、修正実装後に埋める)

| 子プロセス | idle 継続時間 | 期待 dump | 期待 seqno |
|---|---|---|---|
| bash (echo 後 sleep) | 20s | 直前出力を保持 | > 0 維持 |
| claude TUI (入力待ち) | 60s | 直前 UI 描画を保持 | > 0 維持 |
| vim (idle) | 60s | alt-screen 状態を保持 | > 0 維持 |
| cat (idle stdin 待ち) | 60s | 直前出力を保持 | > 0 維持 |
| less (viewer 停止) | 60s | 直前描画を保持 | > 0 維持 |
| python REPL (プロンプト待ち) | 60s | プロンプト表示を保持 | > 0 維持 |

## 検証マトリクス (2026-07-21、修正実装後)

修正版 hyoui (`target/release/hyoui` 0.9.12 + 本修正) で以下を実測。旧実装なら 15s 経過で seqno=0, cursor(0,0), text/plain 全空になるところ、修正後は全ケースで cells が保持された。

| 子プロセス | idle 経過 | seqno | cursor | text/plain 先頭 | 判定 |
|---|---|---|---|---|---|
| `bash -c 'echo alive; sleep 3600'` | 46s | 1 | row=1 col=0 | `alive ...` | 保持 |
| `bash -c 'echo bash-line; sleep 3600'` | 60s+ | 1 | row=1 col=0 | `bash-line ...` | 保持 |

いずれも旧実装での再現 (T=20s で 30B 空 dump + seqno=0) が消え、cells が保持されることを確認した。

## 未検証と報告した「静止→出力→静止 counter リセット挙動」

本修正では counter 自体を撤去したため確認対象そのものが消えた。`process()` は今も新 byte 毎に呼ばれて `current_seqno` を increment するだけで、idle 中の counter / タイマ / 副作用は一切残らない。よって「counter リセット挙動」の検証は irrelevant になった (静止したら seqno は保持、次の出力で increment するだけ)。
