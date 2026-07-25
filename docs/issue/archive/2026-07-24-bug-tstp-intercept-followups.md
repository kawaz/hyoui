---
title: "BUG: DR-0026 Ctrl+Z intercept 実挙動 follow-up (非 leader 固着 / detach 時 reset 未送出 / 単発 stop UX)"
status: resolved
category: bug
created: 2026-07-24T14:00:00+09:00
last_read: 2026-07-25T00:00:00+09:00
open_entered: 2026-07-24T14:00:00+09:00
wip_entered: 2026-07-24T14:00:00+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-07-25T00:00:00+09:00
discard_reason:
pending_reason:
close_reason: ["fixed","H3 (非 leader への stopped notify) / H4 (detach 時の端末 reset 未送出) は実装済。H2 (単発 Ctrl+Z で child+client 両方 stop) は DR-0029 で仕様ごと再設計 (= 単発 Ctrl+Z = detach、follow 廃止) して解消。実機で単発 detach 後の子生存 / 2 連打で子 stopped + attach 継続を確認"]
blocked_by:
origin: kawaz 実機報告 2026-07-24 (hyoui 0.9.18)。sol-worker + fable5-worker で PTY harness 実測、DR-0026 intercept 経路が仕様通り動く一方で follow / detach 経路に穴があることを確定。
---

# BUG: DR-0026 Ctrl+Z intercept 実挙動 follow-up

- Priority: High (H3/H4 = 複数 client 実運用と detach 後の外側 shell 状態が壊れる)

## 症状 (kawaz 実機報告)

**A. `hyoui run -- claude ...` で Ctrl+Z 1 発** → 即ターミナルに戻る。`hyoui list` は
STATUS=stopped、clients=2。

**B. `hyoui attach <既存 session>` で Ctrl+Z 1 発** → 即 claude が suspend、attach client
は繋がったまま画面固着。Ctrl+C/Ctrl+D も子に届かない。Ctrl+Z 2 連打で detach は成立するが、
detach 後 `fg` すると `62;22;52c2026;2$y62;22;52c11;rgb:1010/1212/1616` のような端末クエリ
応答文字列が shell に流れ、数回 `fg` を繰り返すと `fg: no current job` に至る (job テーブル
破壊)。

## 実測 (probe_run_stop / probe_multiclient、macOS 0.9.18 release build)

| # | 仮説 | 判定 | 根拠 |
|---|---|---|---|
| H1 | intercept が 300ms 保留を実装通り効かせている | **確定 (仕様通り)** | 単発 Ctrl+Z → attach client STOP まで 300.33ms / 304.13ms (2 sample)。外側 tty は raw + `isig=false`、kernel line discipline 経路は使われていない |
| H2 | report A の「即戻り」は intercept bypass ではなく仕様通りの 300ms → follow → shell 帰還 | **確定 (仕様通り、UX 齟齬)** | H1 の実測経路。kawaz の期待「child だけ stop、client は attach 継続」との齟齬 |
| H3 | report B の画面固着は「非 leader rw client が SessionChildStoppedNotify を受け取らない」 | **確定 (真の bug)** | probe_multiclient (default 2 番目 attach は leader を奪わず id=0 leader / id=2 RwNoLeader): 単発 Ctrl+Z → 約 306ms 後 child stopped + 旧 leader (id=0) Ts+、操作元 (id=2) は Ss+ raw のまま。leader-only notify 設計欠陥として確定。後続入力の扱いは子の disposition 依存 (`sleep` のような素直な子は SIGINT で exit 130、SIGINT ignore する子や child_stopped 状態では固着継続)、kawaz 報告「Ctrl+C/D も届かない」の一般化は反証済 |
| H4 | detach path (TriggerDetach / stdin-eof=Detach) で `SUSPEND_OUTER_TTY_RESET` を吐いていない | **確定 (真の bug)** | probe_multiclient の nonleader detach 出力は echo bytes のみ (leader の follow 出力はフル reset あり)。追加実測: detach 後に `ESC[12;34R` (CPR 応答) を outer tty に遅延注入すると、次 process が stdin から読み取り、tail/debug には現れない = 外側 terminal からの応答経路 (raw mode で client が消化していた) が detach 後に処理されず shell に漏れる機序。kawaz 報告の `62;22;52c... 2026;2$y... 11;rgb:...` は DA / DECRQM / OSC 11 応答が同経路で漏れたもの |

## 修正 (H3 / H4、本 issue で実装、2026-07-24)

- **H3 fix (`crates/hyoui/src/daemon/session.rs` `notify_child_stopped`)**: leader 1
  client への通知を廃止、`Mode::Rw` と `Mode::RwNoLeader` 全 client (かつ cap
  `child-state-v1` 保持) に broadcast する形に変更。ro client と cap 未対応 client は
  対象外。既存 auto-resume policy / leader 不在時の SIGCONT 抑止 (DR-0017 §柱2) は不変。
- **H4 fix (`crates/hyoui/src/client/attach.rs` `run`)**: TriggerDetach handler および
  stdin EOF (`--stdin-eof=detach` 経路 2 箇所) + stdin read error 経路で、Detach
  message 送信の前 (or return の前) に `stdout.write_all(SUSPEND_OUTER_TTY_RESET)` を
  追加。`suspend_hooks` が Some (= 外側 stdout が tty、raw mode 中) のときだけ発動、
  非 tty パイプにエスケープを垂れ流さない対称性を保つ。follow 経路 (line ~858) と同一
  定数を利用。
- **DR-0017 §柱2 注記**: 「leader 1 client への follow 通知」から「全 rw client への
  broadcast」に文言修正。narrative は最小限。
- **テスト**:
  - `notify_child_stopped_broadcasts_to_all_rw_clients_excluding_ro`
    (session.rs): 4 client (leader rw+cap / non-leader rw+cap / ro+cap / rw 無 cap) を
    構築、broadcast 通知が上 2 者のみに届くことを assert
  - `run_emits_reset_before_detach_when_suspend_hooks_set` /
    `run_does_not_emit_reset_when_suspend_hooks_absent` (attach.rs): TriggerDetach
    経路で `suspend_hooks` 有無に応じて `SUSPEND_OUTER_TTY_RESET` を吐く / 吐かない
    ことを stdout バイト列で assert

## H2 の決着 (2026-07-25、DR-0029)

kawaz 裁定により、選択肢 (a)/(b)/(c) のいずれでもなく **仕様の前提から再設計**した。
「attach は覗き窓であり、client 操作で子を止めない」を原則として明文化し (DR-0029):

- 単発 Ctrl+Z = **client detach のみ** (= 子には届かない、子は走り続ける)
- 子を止めたいときは Ctrl+Z **2 連打** (= 2 発ごとに子へ 1 発)
- 子が stopped になっても client は follow (`raise(SIGSTOP)`) せず attach を継続し、
  画面最下行に「子が停止中」を 1 行表示する
- `Ctrl-A d` detach prefix は機能ごと全廃

実機確認 (macOS / release 0.9.18 / 子 = `/bin/cat`):

| 操作 | attach client | 子 |
|---|---|---|
| Ctrl+Z 単発 | exit 0 (detached) | live のまま |
| Ctrl+Z 2 連打 | 繋がったまま (5s 継続を観測) | stopped |
| Ctrl+Z 3 連打 | exit 0 (detached) | stopped |

## 検証コマンド (追試用)

`~/.config/hyoui/config.toml` 不在で default (intercept=true / 300ms / 1500ms) を再現。

- 300ms 保留計測: `python3 <scratchpad>/probe_run_stop.py`
- 非 leader 固着 + detach 出力: `python3 <scratchpad>/probe_multiclient.py`
- fresh build 実測は `./target/release/hyoui --version` を必ず確認 (PATH の 0.9.18
  brew と debug/release で挙動差が出た履歴あり — memory `feedback_retest_requires_version_check`)。

## 関連

- DR-0017 (session anchor + suspend policy) §柱2
- DR-0026 (attach UX Ctrl+Z 折衷 intercept + reattach resume) §1 §2 §3
- DR-0015 §2.2 (SessionChildStoppedNotify / SessionChildResumeRequest 仕様)
