---
title: 子が ^Z をエコーする時 (cat 等)、^Z×2 で出た child action menu がキーを受け付けず子へ素通りする
status: pending
category: bug
created: 2026-10-09T11:00:00+09:00
last_read: 2026-10-09T11:00:00+09:00
open_entered: 2026-10-09T11:00:00+09:00
wip_entered:
blocked_entered:
pending_entered: 2026-10-09T12:00:00+09:00
discarded_entered:
resolved_entered:
discard_reason:
pending_reason: kawaz 2026-10-09「menu は使っていないので要らない。自前の仮想セル層 (DR-0040) ができた時に考え直す」。menu は既定では出ない (on_child_suspend の既定は auto_resume_on_attached)
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

## 調べて分かったこと (2026-10-09、修正は中止)

- 真因 (入れ子の hyoui の record で確認): daemon の serve_loop は 1 周の中で SIGCHLD の処理 (StoppedNotify の送信) を master の読み取りより先に行う。停止前に line discipline が出した `^Z` のエコーが StoppedNotify の後に client へ届き、attach.rs の「menu 表示中の raw_data = resume」の判定が focus を Child に戻す
- 反例: 外から `kill -STOP` で止めて menu を出した後、別の client から `hyoui input` で文字を送ると、停止中でも canonical mode のエコーが raw_data として届いて menu が閉じる。「menu 表示中の raw_data = resume の証拠」という判定は原理的に成り立たない
- 直すなら: daemon が continue の観測 (record_child_continued の全経路) を rw client に通知する message (`session.child.continued.notify`) を足し、client はそれで menu を閉じる。DR-0032 §2 の本文の意図 (running への遷移で menu を消す) に沿う。master を読み切ってから StoppedNotify を送る案は、Linux の n_tty では競合が残り、上の反例も直らない
