---
title: 呼び出し元で無視 (SIG_IGN) されている signal が、daemon を経て子にそのまま引き継がれる ($(hyoui run --detached ...) の子が ^Z で止まらない)
status: open
category: design
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

# 呼び出し元で無視 (SIG_IGN) されている signal が、daemon を経て子にそのまま引き継がれる ($(hyoui run --detached ...) の子が ^Z で止まらない)

**裁定済み (2026-10-09、kawaz、SIG-Q1 = a)**: 子の exec の前に、SIGINT / SIGQUIT / SIGTSTP / SIGTTIN / SIGTTOU / SIGPIPE を既定 (SIG_DFL) に戻し、signal mask も空にする。DR-0042 の「hyoui は bash の位置に立つ」に揃える (対話の bash が前景の job に対してするのと同じ)。

## 観測 (2026-10-09、0.14.2)

`I=$(hyoui run --detached -- cat)` で起動した子は、^Z も外からの `kill -TSTP` も効かず `S+` のままだった。`hyoui run --detached -- cat >file` の形で起動し直すと `kill -TSTP` で `T+` になる。プロセスの構造は DR-0017 のとおり (daemon が session leader、子は同じ session の別 pgrp)。

## 原因 (コードで確認、2026-10-09 統括)

`crates/hyoui/src/sys/raw.rs` の `openpty_fork_anchor_exec` の子の側は、`tcsetpgrp` の前後で SIGTTOU を一時的に無視して戻すだけで、exec の前に signal の扱いを既定 (SIG_DFL) に戻していない。execve は「無視」の設定を引き継ぐので、呼び出し元の設定が daemon を経て子に届く。

- bash のコマンド置換 `$(...)` の中は、job control の signal (SIGTSTP / SIGTTIN / SIGTTOU) が無視の設定になる
- 非対話の shell で `cmd &` とすると SIGINT / SIGQUIT も無視の設定になる。この形で起動した子は ^C で終わらないはず (未確認)
- signal mask (block) も、fork の前に保存した呼び出し元の mask に戻して exec しているので、呼び出し元が block していた signal は子でも block されたまま (未確認)

## 論点 (STDIN-Q1 と同じ考え方で裁定を仰ぐ: SIG-Q1)

対話の bash は前景の job を起動する時、これらの signal を既定に戻してから exec する。DR-0042 で「hyoui は bash の位置に立ち、bash と同じように配線する」と決めたので、それと揃えるなら hyoui も子の exec の前に既定へ戻す。tmux も子の signal を既定に戻している。

一方、これは子への介入を新しく足すことになる (CLAUDE.md の介入判断 self-check、DR-0014)。「直接実行で `$(cmd)` とした時も子は無視を引き継ぐ」ので、透過を「直接実行と同じ」と読むなら今のままが正しい。ただ hyoui の子は端末 (PTY) を持つ対話のプロセスとして起動されるので、「端末で起動された時と同じ」を基準にするなら既定に戻すのが透過になる。
