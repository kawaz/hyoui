---
title: daemon が「生きているが固まる」状態を構造的に排除する (イベントループのブロック点棚卸し + 再発防止)
status: open
category: bug
created: 2026-09-29T14:25:41+09:00
last_read:
open_entered: 2026-09-29T14:25:41+09:00
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

# daemon が「生きているが固まる」状態を構造的に排除する (イベントループのブロック点棚卸し + 再発防止)

## 概要

kawaz 裁定 2026-09-29: 「daemon が生きているが固まっている状態になること自体が大問題」。v0.9.55 は stopped client への sendto で `ClientHandle::drop` の join が永久ブロックする 1 経路を直しただけで、同種の経路が他に無いことは確認していない。daemon は子プロセスの生殺与奪を握る正本なので、外部 (client / 子 / fs / tty) がどう振る舞ってもイベントループが止まらないことを保証する必要がある。

## やること

1. **ブロック点の棚卸し**: daemon の serve loop から到達する全ての blocking 呼び出し (socket write / join / waitpid / PTY write / record file write / tcsetattr 等) を列挙し、「相手が応答しない時に bounded で返るか」を表にする (`docs/findings/` に記録)
2. 各ブロック点について bounded 化 (timeout / nonblocking + poll / 別 thread 化) するか、「ブロックしない」根拠を書く
3. **再発防止**: daemon 自身が固まったことを検出する手段を検討する (例: serve loop の heartbeat を status に載せ、`hyoui list` の hung 判定に使う。watchdog thread による自己診断 log)。自動 kill のような介入は透過原則 (DR-0014) に照らして必然性がある場合のみ
4. テストで固定: 「client が一切読まない」「client が SIGSTOP」「子が大量出力しつつ exit」「record 先 fs が書けない」等を matrix で回し、serve が bounded で返ること

## 関連

- `docs/issue/2026-09-29-detached-zombie-child-reap.md` (v0.9.55 で修正した経路)
- `docs/issue/2026-06-22-backpressure-writer-pump-drop-sequence-deadlock.md`
- `docs/issue/2026-09-29-list-auto-prune-stale-and-show-hung.md` (固まった daemon の可視化側)
