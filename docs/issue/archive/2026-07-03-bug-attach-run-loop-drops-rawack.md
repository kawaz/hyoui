---
title: attach run loop が RawAck (DR-0021) を unknown frame 扱いし interactive 打鍵 / pipe 入力で client が即死する
status: resolved
category: bug
created: 2026-07-03T18:45:00+09:00
last_read: 2026-07-03T18:45:00+09:00
open_entered: 2026-07-03T18:45:00+09:00
wip_entered: 2026-07-03T18:45:00+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-07-03T18:55:00+09:00
discard_reason:
pending_reason:
close_reason: "attach.rs run loop に TYPE_RAW_ACK 読み捨て arm を追加 (recv_control の DR-0021 m1 と同じ扱い)。pipe e2e 3 件 + 新規 interactive 打鍵 regression test (attach_interactive_input.rs) で green 確認、実機 PTY 再現 (python openpty 80x24) でも修正後の生存を確認"
blocked_by:
origin: bug-bc-macos-ci-compatibility の真因調査 (session a7761122)
---

# attach run loop が RawAck を unknown frame 扱いし interactive 打鍵 / pipe 入力で client が即死する

## 現象と真因

DR-0021 (v0.9.4) 以降、daemon は client からの raw_data write 完了ごとに
`TYPE_RAW_ACK` (0x02) frame を送信元 client に返す (control.rs
`handle_client_frame`、完了点 ack)。`hyoui input` の `send_raw_bytes` と
`recv_control` は ack を消費するが、**attach の run loop (attach.rs の frame
dispatch) には TYPE_RAW_ACK の arm が無く**、`_ => Err("unknown frame type from
daemon")` に落ちて client が exit 1 する。

結果、v0.9.4 / v0.9.5 では:

- **interactive tty attach は 1 打鍵で即死** (実機 PTY 80x24 + `hyoui run -- cat`
  + "hello\n" 打鍵で再現、修正前 100%)
- pipe stdin (`echo x | hyoui run -- <cmd>`) も入力 forward 直後に同じ死に方
- `--stdin-eof` の EOT (0x04) 送信でも同様

## 2.5 週間検出されなかった理由

1. 自動化経路 (`hyoui input`) は ack 対応済みで無事 → dogfooding が headless 中心
2. **e2e に「attach 経由で通常 byte を打鍵する」test が 1 件も無かった**
   (detach key test は client 内 prefix 処理で frame 非発生)
3. 唯一のカナリア (pipe stdin の `--ignored` e2e) の failure が「macos runner の
   GNU bc 互換性問題」と誤診された ([[2026-06-30-bug-bc-macos-ci-compatibility]]
   も本真因で close)。ubuntu の ignored job は backpressure deadlock が先に fail
   して当該 test まで到達せず「macos のみ」に見えていた

## 修正

- attach.rs run loop に `TYPE_RAW_ACK => { /* 読み捨て */ }` arm を追加
  (= fire-and-forget の stdin forward は完了点同期を必要としない、recv_control の
  silent skip と同じ意味論)
- regression test: `crates/hyoui-cli/tests/attach_interactive_input.rs`
  (2 打鍵目の echo 確認 = ack 受信後も run loop 生存の証跡)

## 残す設計論点 (DR-0025 Phase 2 で扱う)

daemon が「ack を要求していない client」にも無条件で ack を返す現仕様は、
interactive 打鍵のたびに frame を 1 往復増やす。protocol kind 写像の整理
(DR-0025 Phase 2) で ack 要求の opt-in 化を検討する価値がある。
