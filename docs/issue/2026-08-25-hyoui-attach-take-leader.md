---
title: hyoui attach --take-leader を実装する
status: open
category: request
created: 2026-08-25T16:00:22+09:00
last_read:
open_entered: 2026-08-25T16:00:22+09:00
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

# hyoui attach --take-leader を実装する

## 概要

`hyoui attach --take-leader` を実装する。attach 開始時に `leader.request` を自動送信し、
自分の attach 接続を leader にする入り口を CLI に用意する。

## 背景

kawaz 裁定 2026-08-25 (QUESTIONS.md LR2-Q1 = a)。DR-0033 (leader.request 奪取) の CLI 後続として、
`hyoui attach --take-leader` を実装する。attach 開始時に `leader.request` を自動送信する形。

### 選ばれなかった案 (再提案しないこと)

- b: attach 中の in-attach 操作として追加 (DR-0032 child action menu 等に項目追加)。menu は
  「子 suspend 時」の文脈なので置き場が歪む
- c: 保留 (web の「leader になる」で足りる)

### 意味論の前提 (DR-0033 より)

leader は「接続」に付く属性で、切断すると cascade で次の rw client へ移動する。したがって
**standalone な `hyoui leader request <session>` は成立しない** (一瞬 leader を取って切断 →
即 cascade するだけで意味がない)。また他 client への leader 付与は DR-0033 で導入しないと
決定済み。よって CLI 表面は「自分の attach 接続を leader にする」入り口に限られ、
`attach --take-leader` がその最小形になる。

### 実装時の注意

- `Mode::Ro` では `leader.request` が `mode.not-allowed` で拒否される (DR-0033 §1)。
  `--mode=ro` と `--take-leader` の同時指定はエラーにするか、CLI 層で弾くか設計判断が要る
- `rw-no-leader` からの要求は mode を `rw` に遷移させた上で leader を付与する
  (DR-0033 §1、LR-Q3=b の裁定)。`--mode=rw-no-leader --take-leader` の組み合わせをどう扱うか
  (矛盾としてエラーか、要求を優先して rw 昇格か) を決める
- **help / completion / 実装の 3 点同期** が必要 (cli-design-preferences)。`--help` テキストと
  zsh completion 定義を同時に更新する
- 既存の `leader.notify` 受信経路 (crates/hyoui/src/client/attach.rs) が昇格を検知して初回
  Resize を送る仕組みがあるので、それに乗る形になるはず

### 関連

DR-0033 (leader.request 奪取、§1 が protocol の正本) / DR-0007 (v0.3.0 「leader CLI」枠) /
DR-0006 (CLI ground rules)

## 受け入れ条件

- [ ] `hyoui attach --take-leader` が attach 開始時に `leader.request` を自動送信する
- [ ] `--mode=ro --take-leader` の扱いを決定・実装する
- [ ] `--mode=rw-no-leader --take-leader` の扱いを決定・実装する
- [ ] `--help` とzsh completion を実装と同時更新する
