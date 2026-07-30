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

## 関連

- DR-0029 (Revised 2026-07-30: 単発 ^Z = client suspend) / DR-0030 (resume_stopped_child) / DR-0019 (auto_resume)
- docs/issue/2026-07-25-request-attach-overlay-progress.md (overlay の先行 request)
- docs/issue/2026-07-21-screen-overlay-general-mechanism.md (一般機構)
