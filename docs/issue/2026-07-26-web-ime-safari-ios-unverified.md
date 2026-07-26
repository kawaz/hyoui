---
title: web ターミナルの IME 追従: 実機 Safari/iOS での未検証範囲
status: open
category: task
created: 2026-07-26T12:27:20+09:00
last_read:
open_entered: 2026-07-26T12:27:20+09:00
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

# web ターミナルの IME 追従: 実機 Safari/iOS での未検証範囲

## 概要

web ターミナル (hyoui のブラウザ attach 画面) の IME (日本語入力等) カーソル追従について、
実機 Safari / iOS 環境での動作が未検証。

## 背景

web ターミナル実装 (web-ime-cursor 関連作業) の一環で IME カーソル追従機構を実装したが、
検証は他ブラウザ / デスクトップ環境が中心で、実機 Safari (macOS) および iOS Safari
(モバイル) での挙動確認が手つかずのまま残っている。

hyoui は DR-0014 の検証主義に従い「推測で実装しない」「マトリクス検証は最低 3 category」を
求めており、IME 追従のようなブラウザ実装依存の強い機能は特に実機差異が出やすい
(compositionstart/update/end のタイミング、input event の発火順序、visualViewport の
挙動などが Safari/iOS で他ブラウザと異なることが知られている)。この検証が漏れたまま
放置されるとリグレッションに気づけない。

## 受け入れ条件

- [ ] 実機 macOS Safari で日本語 IME 入力時のカーソル追従を確認 (変換候補表示中の位置ズレ有無)
- [ ] 実機 iOS Safari (iPhone/iPad) で同様のカーソル追従を確認
- [ ] 上記で問題が見つかった場合は挙動を記録し、修正 issue を別途起票するか本 issue に追記
- [ ] 問題がなければ「検証済み・問題なし」を本ファイルに記録して close

## TODO

<!-- wip 時のみ -->
