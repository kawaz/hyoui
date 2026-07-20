---
title: screen 仮想スクリーンの部分切り出し API + 監視エリアのマッチング検出インターフェース
status: idea
category: request
created: 2026-07-21T00:58:05+09:00
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
origin: 自リポ TODO
---

# screen 仮想スクリーンの部分切り出し API + 監視エリアのマッチング検出インターフェース

## 概要

hyoui screen (仮想スクリーン) に対して、以下 2 つの新規外部インターフェースを追加する:

1. **region 指定の screen 部分取得 API**: 全画面ではなく行範囲等の部分だけを取得する。
   CLI と web (DR-0027) の両方に露出する。例: `GET /api/sessions/:id/screen?region=rows:20-24`
2. **watch 登録 + 検出通知の外部インターフェース**: 特定の監視エリアがマッチ条件を満たしたら
   通知する仕組み。web なら `POST watches` 登録 + WS/long-poll での通知、CLI なら既存
   `hyoui wait` (pattern match) の拡張との関係整理が必要。

設計の母体は DR-0025 の Screen domain `WatchRegistration` (region / matcher / flow の 3 軸、
matcher は AnyWrite / Literal / Regex)。今回追加するのは region 指定の取得 API と、
watch 登録・通知を外部 (CLI/web) から使える形にするインターフェース設計。

## 背景

kawaz 提案 (2026-07-20)。web ターミナル (DR-0027) 完了後に着手したい優先度の item。
DR-0025 の Screen domain 側の型設計は既にあるが、それを CLI / web から実際に叩ける
外部インターフェースとしてはまだ無い。

## 受け入れ条件

- [ ] DR-0025 の実装 Phase 進捗を確認し、WatchRegistration (region/matcher/flow) が
      どこまで実装済みか grep で裏取りする (B 方向整合性チェック)
- [ ] 既存 `hyoui wait` (pattern match) との責務分担を設計時に明確化する
      (= 新規 watch API が wait を置き換えるのか、共存するのか)
- [ ] region 指定の部分取得 API を CLI + web の両方に実装する
- [ ] watch 登録 + 検出通知 (web: POST watches + WS/long-poll、CLI: wait 拡張 or 新規)
      を実装する
- [ ] DR-0027 (web ターミナル) 完了後に着手する (依存関係)

## 設計制約

設計制約 (kawaz 2026-07-20): 場当たり実装を禁止。DR-0025 の形式化路線に従い、(1) 全て
message 駆動で設計する (screen 内部状態への直接アクセス経路を作らない)、(2) コンポーネント間は
公開インターフェースのみで結合し、同一プロダクト内でも他コンポーネントの内部実装に直接手を
突っ込まない、(3) 実装前に DR-0025 の該当 domain (Screen) の event/message カタログへの追加
として定式化し、必要なら DR を先に書く。

## TODO

<!-- wip 時のみ -->
