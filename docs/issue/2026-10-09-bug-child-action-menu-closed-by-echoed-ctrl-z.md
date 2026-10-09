---
title: 子が ^Z をエコーする時 (cat 等)、^Z×2 で出た child action menu がキーを受け付けず子へ素通りする
status: open
category: bug
created: 2026-10-09T11:00:00+09:00
last_read: 2026-10-09T11:00:00+09:00
open_entered: 2026-10-09T11:00:00+09:00
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

# 子が ^Z をエコーする時 (cat 等)、^Z×2 で出た child action menu がキーを受け付けず子へ素通りする

## 観測 (2026-10-09、0.14.2、入れ子の hyoui で 4 回中 4 回)

`[session] on_child_suspend = "show_child_action_menu"` で、子が `cat` の時に ^Z×2 で止めると、menu は描かれるのに `c` / `Esc` 等のキーが子の PTY に届き、menu の操作にならない (子は `T+` のまま)。内側の `hyoui tail` の末尾は `^Z q c` で、kernel のエコーに送ったキーが出ている。

同じ設定でも、子が vim / python REPL の時の ^Z×2、外から `kill -STOP` で止めた時は期待どおり動く。

## 真因の見立て (推測、コードは読んだが未検証)

子が canonical mode で echoctl が有効な時、line discipline が出す `^Z` のエコーが停止通知より後に daemon から client へ届く。`crates/hyoui/src/client/attach.rs` の 1287 行付近は「menu 表示中に届いた子の出力は子が resume した証拠」とみなして focus を Child に戻すので、このエコーを resume と取り違えている。同じ箇所のコメントは「早く閉じる側に倒れる (安全側)」と書いているが、menu の描画は画面に残ったままキーが素通りするので、利用者には「menu が出ているのに効かない」と見える。

## 付随して見えたこと

- 素通りの後に外から `kill -CONT` しても、menu の描画が画面に残る
- menu で `d` (detach) した後、および select_on_demand のプロンプトで `^C` した後も、menu / プロンプトの行が端末に残る

## 直す方向 (未検討)

resume の判定を「子の出力が来た」でなく「daemon が子の continue を観測した」(SIGCHLD の CLD_CONTINUED、status の child_state) に寄せる。menu を閉じる時は描画も消す。
