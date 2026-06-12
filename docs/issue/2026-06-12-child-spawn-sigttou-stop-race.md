# daemon 子プロセスが fork〜exec 間で停止する race (SIGTTOU/SIGTTIN 系)

- Date: 2026-06-12
- Status: open
- Priority: 中 (= 発生すると daemon が「子が起動しない」状態で永久待ち、attach がハング)
- Origin: Fable review 対応中の workspace test 実機観測 (2026-06-12)

## 観測した現象

`cargo test --workspace` 実行中 (= 外側端末が hyoui PTY 配下の dogfooding 環境)、
`self_session_resolve::attach_other_session_from_inside_is_allowed` がハング。観測:

```
  PID  PPID  PGID   TT  STAT COMMAND
62181     1 62181 s029  Ss   hyoui run --detached -- sh -c sleep 30   (= daemon、正常)
62187 62181 62187 s029  T+   hyoui run --detached -- sh -c sleep 30   (= daemon の子、停止!)
```

- 62187 = daemon が fork した子 PTY プロセスで、**COMMAND が exec 前のまま 12 分停止 (T+)**
- TT が外側端末 (s029) のまま = setsid / TIOCSCTTY 到達前に停止
- `kill -CONT 62187` で復帰し、即 `sleep 30` まで exec 完了 → テストも完走
- 子が止まったままなので handshake 後の attach client は応答を永久に待つ

## 推定 root cause

fork 直後の子は `setpgid(0, 0)` で新 process group になる (= session.rs の child pgid
設計)。この時点で子は **親 session の background pgrp** であり、`setsid` で新 session
に移るまでの間に制御端末 (= 外側端末) への端末操作 (write / tcsetattr / ioctl) が
入ると **SIGTTOU / SIGTTIN で停止** する。

通常環境では窓が極小で顕在化しないが、外側端末が hyoui PTY (= raw/cooked が
頻繁に切り替わる) + test 並列実行のような条件でヒットした。

## 修正の方向 (実装時に検証)

- fork 直後の子で `SIGTTOU` / `SIGTTIN` を `SIG_IGN` にしてから setsid までの
  シーケンスを進める (= login_tty 系の常套手段)、または
- fork〜exec 間の子で外側端末に触る経路を排除する (= 何が SIGTTOU を発生させて
  いるか dtruss / ktrace で特定してから)
- 再現テスト: 外側を raw mode PTY にした harness で run --detached を高頻度起動

## 関連

- `crates/hyoui/src/sys/raw.rs` — `openpty_fork_anchor_exec` (= fork〜exec シーケンス)
- DR-0017 — session anchor 構造 (= 子は fork 直後 `setpgid(0,0)`)
