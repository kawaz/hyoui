---
title: web terminal の touch 再タップで focus を解除する
status: wip
category: request
created: 2026-07-30T21:32:08+09:00
last_read: 2026-07-30T21:32:08+09:00
open_entered: 2026-07-30T21:32:08+09:00
wip_entered: 2026-07-30T21:32:08+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 要望 (2026-07-30 ccmsg r92 m45)
---

# web terminal の touch 再タップで focus を解除する

## 要望

touch 端末では terminal が画面の大部分を占め、xterm.js の helper textarea に focus して
ソフトウェアキーボードを開いた後、focus を外すためのタップ先がない。

terminal 領域への touch tap を focus toggle として扱う。

- 未 focus なら terminal を focus
- focus 済みなら `term.blur()` でソフトウェアキーボードを閉じる
- mouse click は従来どおり focus
- drag / 選択 / scroll は tap と判定しない
- FAB の入力タブにある textarea には影響させない
- FAB panel 表示中の terminal tap / click は panel close を優先し、同じ touch で focus toggle しない
- FAB panel の表示専用 `Terminal` ヘッダ行を削除し、× と drag 操作をタブ行へ統合する

## 実装

`#term` の touchstart / touchmove / touchend / touchcancel を監視する。単一 touch かつ
開始点からの最大移動量が 10px 以下の場合だけ tap と判定し、touchstart 時点の
helper textarea focus 状態を反転する。focus 済み tap では touchend の default を抑止して
synthetic click による再 focus を防ぎ、次 task で `term.blur()` を実行する。未 focus tap と
移動 gesture は default を抑止しない。

start / move / cancel は passive listener で観測し、既存の選択・scroll gesture は維持する。

panel 表示中の短い touch tap は `closePanel()` を実行して focus toggle 分岐を終了する。
mouse click も `#term` の click listener から同じ close 経路を使う。panel の表示専用ヘッダは削除し、
タブと × を 1 行に統合する。panel drag handle もタブ行へ移す。

## 検証

- Chromium touch emulation: 未 focus → touch tap で focus、再 tap で blur
- Chromium touch emulation: 10px を超える移動では focus 状態を変更しない
- Chromium mouse: click 後は常に focus
- Chromium touch / mouse: panel 表示中の terminal tap / click で panel が閉じる
- × button とタブ行 drag はヘッダ削除後も機能する
- panel 内に `.input-panel-head` / 表示専用 `Terminal` 行がなく、タブ行が先頭になる
- iOS / iPadOS 実機: 未検証。ソフトウェアキーボードの表示・格納は kawaz 実機確認待ち
