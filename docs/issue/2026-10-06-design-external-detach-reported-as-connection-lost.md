---
title: 外から `hyoui detach` された attach client が「daemon との接続が失われました」で exit 9 になり、daemon の消滅と区別できない
status: open
category: design
created: 2026-10-06T15:20:00+09:00
last_read: 2026-10-06T15:20:00+09:00
open_entered: 2026-10-06T15:20:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# 外から `hyoui detach` された attach client が「daemon との接続が失われました」で exit 9 になり、daemon の消滅と区別できない

## 観測 (2026-10-06、DR-0042 の実装 worker の実機確認)

`hyoui detach S` (DetachTarget の Others / All) で切られた attach client は、`RunOutcome::ConnectionLost` に落ち、stderr に「daemon との接続が失われました (daemon が終了した可能性があります)」を出して exit 9 (`EXIT_CONNECTION_LOST`) で終わる。daemon は対象 client の接続を drop するだけで、切られた側に理由を伝えない (`crates/hyoui/src/daemon/control.rs` の `handle_detach_target`、`detach.ack` は要求元にしか返らない)。

`crates/hyoui-cli/src/main.rs` の exit code の設計 (exit 9 は「子の正常 exit と daemon の消滅を取り違えない」ための予約) からすると、意図して切り離された場合まで「daemon が終了した可能性」と言うのは誤報になる。

## 重みが増した理由

DR-0042 で、端末の無い起動元 (agent の Bash ツール等) から `hyoui run` / `hyoui attach` すると、attach client はキー入力なしで出力だけを中継する。この形では外からの `hyoui detach` が主な終わらせ方になり、毎回 exit 9 と誤報が出る。

## 論点 (決めない)

- 切られる側に理由を伝える経路: daemon が drop の前に通知 frame を送る (新 message になるなら CLAUDE.md の self-check で必然性を DR に書く) か、既存の frame で表せるか
- 外から detach された時の exit code (自発 detach と同じ 0 か、区別できる別の値か)
- backpressure での切断 (`BackpressureDisconnected`、今は同じ exit 9 で stderr だけ出し分け) との揃え方

## 追記 (2026-10-09): 送信 queue の上限で切られた client でも同じ文言が出る

HANG-C1 の確認 (0.14.2) で、^Z で止めた attach client を抱えたまま子が大量に出力すると、daemon は送信 queue の上限でその client を切る (status の clients から消える)。その後 client を `fg` すると、溜まっていた出力を流した後に「daemon との接続が失われました (daemon が終了した可能性があります)」を出して終わった。daemon は live のまま。`RunOutcome::BackpressureDisconnected` には別の文言があるのに、この経路ではそれが出ていない (切断の理由が client に届いていないか、判定に落ちていない)。
