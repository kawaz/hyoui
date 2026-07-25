---
title: attach 画面最下行に detach 遅延の progress overlay を出す (DR-0029 §5)
status: open
category: request
created: 2026-07-25T00:00:00+09:00
last_read:
open_entered: 2026-07-25T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 提案 2026-07-25 (DR-0029 起草時に「今回は未実装、issue 化」と裁定)
---

# attach 画面最下行に detach 遅延の progress overlay を出す

## やりたいこと

DR-0029 §2 の Ctrl+Z ガードは、単発 Ctrl+Z を受けてから `ctrlz_guard_delay` (既定 500ms)
待って detach する。この待ち時間中、ユーザには何も見えない (= 押したのに何も起きない窓が
できる)。ここに overlay を出して「今 detach 待ちで、もう一度押せばアプリに届く」を見せる。

kawaz 案の文言とイメージ:

```
[hyoui] デタッチ遅延中、Ctrl+Z をもう一度でアプリに到達
```

- 画面最下行に 1 行、残り時間を示す **500ms のバー**を添える
- バーの見た目は `kawaz/claude-status-line` の 1 段バーのイメージ

## 現状

- `[attach] ctrlz_guard_overlay` config key は **受理されるだけで動作しない**
  (`crates/hyoui/src/config/mod.rs`)。本 issue の実装時に配線する
- 子停止時の 1 行通知 (DR-0029 §1、`draw_child_stopped_notice`) は実装済み。
  `ESC 7` / `ESC 8` で cursor を保存復元して最下行を上書きするだけの最小実装で、
  再描画・消去・他 overlay との調停は持たない

## 論点 (実装前に決めること)

- **一般機構に乗せるか、client ローカルの直描きで済ませるか**:
  [2026-07-21-screen-overlay-general-mechanism](./2026-07-21-screen-overlay-general-mechanism.md)
  が「daemon の screen state に動的仮想 overlay を差す一般機構」を扱っている。detach 遅延
  overlay は **他 client に見せる必要がない** (= 押した本人の窓だけの話) ので client 側
  直描きで足りる可能性が高い。一般機構を待つ理由になるかを判定する
- **消し方**: 500ms 後に detach するなら消す必要はないが、2 発目が来て detach を取り消した
  ときと、他キー割り込みで取り消したときは消す必要がある。何で上書きするか
  (= 子の該当行を daemon から取り直す / 単に `ESC[K` して子の再描画に任せる)
- **バーの更新頻度**: 500ms を何分割で描くか。attach client の poll loop は既に
  deadline まで timeout を張っているので、描画のためだけに短い timeout を張ると
  polling anti-pattern に寄る。フレーム数を決め打ちする根拠が要る
- **`delay = 0` のとき**: 待ち時間がないので overlay も出ない。config 上の整合を明記する

## 関連

- DR-0029 §2 / §5 (docs/decisions/DR-0029-attach-is-a-viewport-ctrl-z-guard.md)
- [2026-07-21-screen-overlay-general-mechanism](./2026-07-21-screen-overlay-general-mechanism.md)
