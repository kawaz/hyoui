---
title: screen state への動的仮想オーバーレイ一般機構
status: open
category: design
created: 2026-07-21T00:59:38+09:00
last_read:
open_entered: 2026-07-21T00:59:38+09:00
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

# screen state への動的仮想オーバーレイ一般機構

## 概要

子 PTY・TUI アプリには一切影響を与えず(バイト送信なし、透過原則維持)、hyoui が配信する
仮想 screen の見た目にだけダイアログ・通知等を重畳表示できる一般機構を設計・実装する。
DR-0013 (screen state 正本) の延長として位置づける。

## 背景

kawaz 提案 (2026-07-20)。以下 2 用途が既に確定している:

1. DR-0029 Phase 2 の Ctrl+Z intercept 時の detach 案内 overlay
   (DR-0029 本文で「一般機構の整備後」と明示保留済み)
2. web ターミナル (DR-0027) でのダイアログ/通知表示

着手は web ターミナル (DR-0027) 完了後を想定。

## 設計論点

- overlay の配信先: attach client 毎に個別か、全 client 共通か
- screen dump/snapshot への overlay 合成の有無: 自動化 API は素の screen を見たいはず
  なので、合成しない側が透過原則的に自然という仮説がある
- z-order / 領域指定と WatchRegistration
  (`docs/issue/2026-07-21-screen-region-watch-api.md`) との構造共有

## 受け入れ条件

- [ ] overlay 配信先(client 個別 / 共通)の設計判断が DR に記録される
- [ ] screen dump/snapshot と overlay 合成の扱いが確定する
- [ ] WatchRegistration との構造共有方針が確定する
- [ ] DR-0029 Phase 2 detach 案内 overlay が本機構上で実装される
- [ ] web ターミナル (DR-0027) のダイアログ/通知表示が本機構上で実装される

## 設計制約

設計制約 (kawaz 2026-07-20): 場当たり実装を禁止。DR-0025 の形式化路線に従い、(1) 全て
message 駆動で設計する (screen 内部状態への直接アクセス経路を作らない)、(2) コンポーネント間は
公開インターフェースのみで結合し、同一プロダクト内でも他コンポーネントの内部実装に直接手を
突っ込まない、(3) 実装前に DR-0025 の該当 domain (Screen) の event/message カタログへの追加
として定式化し、必要なら DR を先に書く。
