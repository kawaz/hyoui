---
title: 子 suspend 時の動作設定の統合 + child action menu (attach 内操作メニュー)
status: open
category: design
created: 2026-07-30T13:30:00+09:00
last_read: 2026-07-30T13:30:00+09:00
open_entered: 2026-07-30T13:30:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 提案 (2026-07-30 ccmsg r92 m18-20)。^Z 単発 = client suspend (DR-0029 改訂) の実機確認直後の発展提案
---

# 子 suspend 時の動作設定の統合 + child action menu

## kawaz 裁定済みの骨子 (2026-07-30)

1. **メニューは `resume_stopped_child=false` 相当の時のみの機能** (既定の auto-resume 下では出番なし)
2. **子 suspend 時の動作を 1 つの enum に統合**する方向 (kawaz 原案、語彙は「他との整合性を考えて調整」= AI 側に委任):
   - `on_child_suspend_action = auto_resume_always | auto_resume_on_attached | show_child_action_menu`
   - 前 2 値は既存の `[session] auto_resume` (daemon 側) / `[attach] resume_stopped_child` (attach 側) と 1:1 対応
     → **bool 2 個の統合リファクタを兼ねる**
3. **`ctrlz_action = client_suspend | client_detach`** (既定 client_suspend)。detach 派の選択肢を復活
4. メニュー項目 (kawaz 原案、signal 名の整理は AI 側):
   - client の detach (client 終了、fg 不可)
   - client の suspend (fg で復帰、復帰と同時に子も起こす)
   - child を起こす (SIGCONT)
   - child へ SIGINT / SIGHUP / SIGKILL (中断・終了系として別グループに整理)

## 設計論点 (DR 起草で解くもの)

- **enum の跨ぎ**: `auto_resume_always` は daemon 責務、他 2 値は attach client 責務。ユーザ向けには
  単一 enum ([session] 配下が候補)、内部で daemon policy + attach policy に写像
- **attach 不在時に子が止まった場合** (`show_child_action_menu` 選択時): メニューを出す先が無い
  → notify のまま待つ、を明文化 (次の attach 時にメニュー表示、が自然か)
- **メニュー中のキー入力**: 停止中の子の PTY に溜まると CONT 時に流れ込む → メニュー表示中は
  hyoui が input を飲む。DR-0029 で狭めた in-band 解釈の再拡張になるため「子が停止中 =
  アプリが入力を消費しない状態に限定」で justify する
- **描画**: DR-0029 の停止通知 1 行の拡張として実装するか、screen-overlay 一般機構
  ([[2026-07-21-screen-overlay-general-mechanism]]) を先に作るか
- **語彙**: 既存 CLI `--on-child-suspend=notify|auto-resume` (daemon) / config `ctrlz_guard*` との整合

## 実装済み (2026-07-30)

DR-0032 (Active) の §1-§4 を実装。close 判定は統括に委ねる。

- **config 統合**: `[session] on_child_suspend` (enum 3 値、default `auto_resume_on_attached`)。
  旧 `[session] auto_resume` / `[attach] resume_stopped_child` は起動拒否 + migration hint
  (`--no-scrub-env` でも迂回不可)。写像は `OnChildSuspendSetting::daemon_policy()` と
  `client::stopped_child_action(mode, setting)` (= Resume / Menu / Nothing の 3 値)
- **child action menu**: rw attach + `show_child_action_menu` + 子 stopped 検知 + raw tty で表示。
  キーは 1 項目 1 mnemonic 文字 (`c` SIGCONT / `z` client suspend / `d` detach / `i` SIGINT /
  `h` SIGHUP / `k` SIGKILL / `Esc`・`q` 閉じる)。**1 byte chunk のみ受理** (= 貼り付け中の
  1 文字で SIGKILL 等が発火する事故を実測で踏んだため)。終了系は既存 `signal` message の
  2 連送 (SIGCONT 併送)、起こすのは既存 `SessionChildResumeRequest` (= killpg + redraw)。
  新 protocol message / cap flag なし
- **`[attach] ctrlz_x1_action`**: `client_suspend` (default) / `client_detach` /
  `select_on_demand` (= ^Z / ^C / Esc の明示キーのみ、他キーは破棄、timeout なし)
- **検証**: unit (写像 3×3 マトリクス / キー表 / 描画) + e2e `child_action_menu.rs` 5 本、
  `ctrlz_suspend_client.rs` に §3 4 本 + menu の `z` 1 本 (いずれも `--ignored` で実機 green)
- **後続 issue 対象のまま**: menu のキーバインド確定 (今回は最小形)、web UI 側の同等機能、
  `hyoui set` の enum 対応、screen-overlay 一般機構への描画移行

## 関連

- DR-0029 (Revised 2026-07-30: 単発 ^Z = client suspend) / DR-0030 (resume_stopped_child) / DR-0019 (auto_resume)
- docs/issue/2026-07-25-request-attach-overlay-progress.md (overlay の先行 request)
- docs/issue/2026-07-21-screen-overlay-general-mechanism.md (一般機構)
