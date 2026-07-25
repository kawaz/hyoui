---
title: detach key (Ctrl-A d) が実端末で発火しない疑い — keyboard protocol 起因の可能性
status: discarded
category: bug
created: 2026-07-20T01:16:37+09:00
last_read: 2026-07-25T00:00:00+09:00
open_entered: 2026-07-20T01:16:37+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered: 2026-07-25T00:00:00+09:00
resolved_entered:
discard_reason: ["obsolete","DR-0029 で `Ctrl-A d` detach prefix と `HYOUI_DETACH_PREFIX` を機能ごと全廃したため、本 bug の対象が消滅した。detach は Ctrl+Z 単発に置換 (DR-0029 §2/§3)。keyboard protocol (kitty CSI-u 等) と単一 byte 入力の相互作用そのものは、必要になった時点で別 issue として立て直す"]
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# detach key (Ctrl-A d) が実端末で発火しない疑い — keyboard protocol 起因の可能性

## 概要

detach key (Ctrl-A d) が実端末で発火しないケースが報告されている。端末の keyboard
protocol (kitty CSI-u 等) が原因の可能性がある。自動テストでは完全に再現しない
(default / disabled / Ctrl-B split の 3 matrix × line-cat/python REPL/vim の 3 app
category、全通過。Homebrew 版でも同結果)。

過去の同種 bug (`TCSAFLUSH` → `TCSANOW`、commit `43732d851365`) は既に修正済みで
今回の疑いとは別。

## 背景

仮説: kitty CSI-u 等 keyboard protocol が有効な端末では、Ctrl-A が単一 byte
`0x01` ではなく `\e[97;5u` 相当の CSI escape 列として届く。detach prefix の判定
(`process_detach_prefix`) は 1 byte 単位で認識する実装のため、CSI escape 列が
その判定を素通りしてしまう可能性がある。

副次発見: `TtyGuard::Drop` の `tcsetattr(TCSAFLUSH)` が外側 PTY 出力未消費の
状態で block する実害を観測した。これは既存 issue
`2026-06-12-tcsaflush-input-discard-in-suspend-resume` の裏付けになる。

関連: DR-0026 §Context (kawaz 提示、本件は独立調査として切り出し)

## 受け入れ条件

- [ ] 実端末で attach 中に Ctrl-A を押下した際の実 bytes を採取する
      (デバッグ版 build で `attach.rs:866` 直前に hex dump、または子プロセスを
      `cat -v` にして escape 列を確認)
- [ ] kitty / ghostty / wezterm / iTerm2 等、端末 emulator ごとの keyboard
      protocol 設定 (有効/無効) を確認するマトリクス検証を行う
- [ ] 原因が CSI-u escape 列の場合、`process_detach_prefix` に CSI-u パース
      対応を追加するか、attach 起動時に keyboard protocol を disable する
      ANSI シーケンスを送出するかを判断・実装する

## TODO

<!-- wip 時のみ -->
