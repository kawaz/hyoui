---
title: 端末サイズ 0x0 の tty 上で hyoui run が vt100 grid の subtract overflow で panic
status: wip
category: bug
created: 2026-07-30T15:20:00+09:00
last_read: 2026-08-21T10:31:00+09:00
open_entered: 2026-07-30T15:20:00+09:00
wip_entered: 2026-08-21T10:33:24+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: DR-0032 実装の実機 probe 中に偶発発見 (= python `pty.fork()` で winsize を設定せず起動した場合)
---

# 端末サイズ 0x0 の tty で起動すると daemon が panic する

## 現象

winsize を設定していない PTY (= rows=0, cols=0) を stdin/stdout に持つ状態で
`hyoui run -- /bin/cat` を起動すると、起動直後に panic して session が立ち上がらない。

```text
thread 'main' panicked at vt100-0.16.2/src/grid.rs:26:28:
attempt to subtract with overflow
```

socket も作られないため、`hyoui status` は `ENOENT` になる。

## 再現

```python
import os, pty, subprocess
mfd, sfd = pty.openpty()          # winsize を設定しない (= 0x0)
subprocess.Popen([HYOUI, "run", "--socket=/tmp/p.sock", "--", "/bin/cat"],
                 stdin=sfd, stdout=sfd, stderr=sfd)
print(os.read(mfd, 4096))          # 上記 panic が出る
```

`fcntl.ioctl(sfd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))` を
exec 前に入れると正常に起動する (= 同 probe で確認済み)。

## 観測できたこと / できていないこと

- 観測済み: 0x0 で panic、24x80 なら正常 (macOS、debug build v0.9.30)
- 未確認: どの経路のサイズが 0 のまま vt100 に渡っているか (= `run` の initial_size
  解決は「tty size が取れなければ daemon default 80x24」のはずで、`Some((0, 0))` が
  そのまま通っている疑い)。`daemon` の resize clamp (= `handle_resize` の
  `clamp(1, 4096)`) と同じ下限 1 の clamp が初期サイズ経路に無いのではないか
- 未確認: 実端末でこの状況が起きるか (= 通常の端末は 0x0 を報告しない。CI / 自動化 /
  library 経由の PTY 生成で起きうる)

## 想定される直し方 (= 当事者判断に委ねる)

初期サイズ経路でも 0 を弾く (= 下限 1 clamp、または 0 は「取れなかった」扱いにして
daemon default に倒す) のが素直に見える。panic を戻り値エラーに変えるだけでは
「サイズ 0 で起動して画面が壊れる」に化けるので、値の正規化側で塞ぐ方が良さそう。

## 進捗

- opus5-medium worker (lightbugs) に修正実装を委譲 (2026-08-21)。初期サイズ経路の
  0 正規化 (経路特定してから修正、`handle_resize` の clamp と整合させる方針)
- 6 月の類似 issue (`2026-06-11-bug-vt100-zero-size-pty-panic`) との同根判定は
  worker 報告待ち

## 関連

- DR-0013 (screen state 正本 / resize 経路)
- docs/issue/2026-07-30-bug-stdout-epipe-panic.md (= panic を正常系に倒す別件)
