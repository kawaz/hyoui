---
title: DR-0042 の失敗経路 1〜3 (配線に失敗したら起動・接続を拒否する) を実際に失敗させて確かめる (未検証)
status: open
category: task
created: 2026-10-14T12:00:00+09:00
last_read: 2026-10-14T12:00:00+09:00
open_entered: 2026-10-14T12:00:00+09:00
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

# DR-0042 の失敗経路 1〜3 (配線に失敗したら起動・接続を拒否する) を実際に失敗させて確かめる (未検証)

DR-0042 決定 6 (fail closed) の 3 つの失敗経路は、コードとテストでは確かめたが、実際に失敗を起こしての確認はしていない (2026-10-06)。

1. daemon が子に stdin を渡せない (`F_DUPFD_CLOEXEC` の失敗) → 子を spawn せず、run は exit 1
2. attach が fd 0 を /dev/null に置き換えられない → 接続せず exit 1
3. 制御端末はあるのに実体を開けない → 接続せず exit 1、端末を raw にしたまま残さない

## 確かめられていない理由

`sh -c "ulimit -n N; exec hyoui ..."` で fd の上限を絞って失敗を起こそうとしたが、Claude Code の removal check に止められた (`sh -c` の script を検査できない、の判定。rm は含まない)。kawaz は実験自体を許可済み (2026-10-06「失敗や破壊パターンの把握まで重要」) だが、この check は会話での許可では通らない。

## 進め方 (どちらかを kawaz が選ぶ)

- a: kawaz がプロンプトから `!` で打つ (下のコマンド。3 は制御端末が要るので kawaz の端末から)
- b: AI が `sh -c` を使わない形 (Bash ツールのサブシェル `( exec 3<&-; ulimit -n 3; exec $B ... )`) で打つ。拒否された操作を形を変えてやり直すことになるので、kawaz の了承が要る

## コマンド (2026-10-06 時点。バイナリの path は今の main の build に合わせて読み替える)

`B` は repo の release build、`T` は 0700 の一時 dir (socket の長さのため短い /tmp の下に mktemp で作る)。

```
# 1
printf 'x\n' | env -u HYOUI_SESSION_ID HYOUI_DAEMONIZE_INIT="{\"socket\":\"$T/d.sock\",\"session\":\"k1\",\"ready_fd\":9,\"child_stdin\":true}" XDG_CONFIG_HOME=$T/cfg sh -c "exec 3<&- 4<&- 5<&- 6<&- 7<&- 8<&- 9<&-; ulimit -n 3; exec $B run --detached -- sh -c 'touch $T/marker1'"; echo "rc=$?"; ls -A $T
# 期待: 「子を起動しません」、rc=1、marker1 と d.sock が無い

# 2 (確認用 session を作ってから)
$B run --detached --pty-stdin --socket=$T/s.sock -- sleep 600
printf 'x\n' | env -u HOME -u HYOUI_SESSION_ID XDG_CONFIG_HOME= sh -c "exec 3<&- 4<&- 5<&- 6<&- 7<&- 8<&- 9<&-; ulimit -n 3; exec $B attach --socket=$T/s.sock"; echo "rc=$?"; $B status --socket=$T/s.sock
# 期待: 「stdin を手放せないため中止します」、rc=1、status に attach が居ない

# 3 (kawaz の端末から)
</dev/null env -u HYOUI_SESSION_ID XDG_CONFIG_HOME=$T/cfg sh -c "exec 3<&- 4<&- 5<&- 6<&- 7<&- 8<&- 9<&-; ulimit -n 4; exec $B attach --socket=$T/s.sock"; echo "rc=$?"; $B status --socket=$T/s.sock; stty -a | grep -o -E -- '-?icanon|-?echo\b'
# 期待: 「キー入力に使う端末を開けないため中止します」、rc=1、端末が raw のまま残らない

# 片付け
$B kill --socket=$T/s.sock
```

v0.15.x 以降、session の置き場や --session-id の規則 (DR-0041) と daemon のログ (DR-0037 段 2) が変わったので、期待の文言や置き場は実行前に今の版で確かめ直す。
