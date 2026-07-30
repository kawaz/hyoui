---
title: web ターミナルに attach/embed/session 情報のフローティングパネル (表示 → 変更操作の 2 段階)
status: open
category: request
created: 2026-07-30T15:10:00+09:00
last_read: 2026-07-30T15:10:00+09:00
open_entered: 2026-07-30T15:10:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
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
