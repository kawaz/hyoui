---
title: "open issue 21 件の triage 棚卸し (外部監査フラグ)"
status: idea
category: task
created: 2026-07-03T13:45:18+09:00
last_read:
open_entered:
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: claude-rules-personal
---

# open issue 21 件の triage 棚卸し (外部監査フラグ)

## 概要

hyoui の `docs/issue/` 配下の open 状態 issue 数が 21 件で、監査対象リポの中で最多だった
(次点は cache-warden の 13 件)。放置日数順に triage 棚卸し (close / 継続 / 統合の判断) する
ことを推奨する。個々の issue の価値判断は hyoui 担当側に委ねる。

## 背景

2026-07-03 の個人エコシステム横断監査 (claude-rules-personal セッション発) で観測。

これは部外者 (claude-rules-personal セッション) からの観測に基づくフラグであり、実際の
21 件それぞれの内容・放置理由は裏取りできていない。triage の要否・優先度は hyoui 担当側で
確認の上で判断してほしい。

## 受け入れ条件

- [ ] 21 件の open issue を放置日数順に一覧し、close / 継続 / 統合のいずれかを判断する
