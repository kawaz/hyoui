---
title: web ターミナルに attach/embed/session 情報のフローティングパネル (表示 → 変更操作の 2 段階)
status: resolved
category: request
created: 2026-07-30T15:10:00+09:00
last_read: 2026-08-21T10:25:01+09:00
open_entered: 2026-07-30T15:10:00+09:00
wip_entered: 2026-07-30T15:40:39+09:00
blocked_entered: 2026-08-21T10:26:06+09:00
pending_entered:
discarded_entered:
resolved_entered: 2026-08-25T16:00:39+09:00
discard_reason:
pending_reason:
close_reason: ["done"]
blocked_by:
origin: kawaz 要望 (2026-07-30 ccmsg r92 m33)
---

# web ターミナルに情報フローティングパネル

## kawaz 要望 (原文要旨)

embed されたターミナル内に、以下を表示するフローティングウィンドウを展開できる機能:

1. 現在のアタッチに関する情報 (ro/rw, leader, ...)
2. 現在の embed に関する情報 (embed 初期パラメータ)
3. アタッチ中の hyoui セッション情報 (attach count, pid, hyoui_session_id, ...)

**まず表示 (Phase 1)、その後、変更可能なものは変更操作 (Phase 2)** — 例: leader 昇格 /
フォントサイズ変更 / ambw 変更。

## UI の入り口 (kawaz 指示 2026-07-30 m34)

**既存のキーボード FAB を拡張する。FAB は増やさない** — ボタンは 1 つのままで、開いた
フローティングが「入力モード」と「情報モード」を持ち、タブ等で切り替える。

## Phase 1 (表示) の素材

- attach 情報: gateway の WS 側 daemon 接続の handshake 結果 (mode / leader)。WS の JSON
  制御チャネル (resize で新設済み) に info メッセージを足すか、接続時に 1 回 push
- embed 情報: frontend 自身の URL params (ローカルで完結)
- セッション情報: daemon StatusResponse (attach count / pid / child-state 等)。gateway に
  status 中継があるか確認、無ければ WS 経由で追加 (新 daemon protocol は不要のはず)

## Phase 2 (変更操作) の依存

- fontsize / ambw / その他表示パラメータ: frontend 内で再適用 (xterm options 更新 + refit /
  provider 再 register)。URL 書き換えとの整合 (リロードで残るか) を設計
- **leader 昇格: `leader.request` (DR-0008 §2.2 で v0.2.0+ 予約、未実装) の実装が前提**。
  daemon protocol 拡張になるので DR 起草必須

## 実装状況

Phase 1 (表示) + Phase 2a (表示設定変更) + Phase 2b (leader 昇格) + Phase 3 (テキスト選択) を実装済み。

- 既存キーボード FAB のフローティングを「入力」「情報」の 2 タブ化。入力機能は維持
- gateway 内部の WS text frame `attach.info` で daemon handshake の mode / leader を表示し、
  `leader.notify` / `mode.change` を受信したら更新
- unicode / ambw / fontsize / lineheight / scrollback / fontfamily / bg / fg の実効値と、
  `URL 指定` / `default` / `embed 中に変更` の出自構造を表示
- 既存 `/api/sessions` から hyoui_session_id / child pid / child-state / attach client 数を表示
- **Phase 2a (表示設定変更、実装済み)**: 情報タブから unicode / ambw / fontsize /
  lineheight / scrollback / fontfamily / bg / fg をその場で変更可能。font 系
  (fontsize / lineheight / fontfamily) は xterm options 更新 + 既存 fit/resize 経路で反映、
  unicode / ambw は provider 再選択 + screen dump 再描画で反映。変更した項目の出自表示は
  「embed 中に変更」になる。URL / storage へは永続化せず、reload すると URL 指定 or
  default にリセットされる
- **Phase 2b (leader 昇格、実装済み)**: 情報タブは leader でない接続にだけ
  「leader になる」ボタンを表示する。WS `leader.request` / `leader.result` で gateway を
  経由して daemon の `leader.request` (cap `leader-request-v1`) を呼び、成功時は
  `attach.info` で新旧 browser の leader 表示を更新する。takeover 直後は後発 browser の
  viewport に対する grid 提案へ PTY と xterm.js を揃える ([[DR-0033]])
- **Phase 3 (テキスト選択、実装済み)**: 情報タブの「テキスト選択」で xterm.js の
  active buffer 全行 (scrollback + viewport) を `line.translateToString(true)` から抽出し、
  terminal 上の独立 `user-select: text` DOM に静止 snapshot として表示する。OS native の
  選択・コピーを使い、閉じるボタンまたは Esc で通常表示へ戻る。表示中は FAB/panel を
  隠し、WS binary input と HTTP input の両送信経路を遮断する
