---
title: 非 tty の stdin は子の stdin にそのまま渡す (PTY に流し込まない) 方が直接実行と同じになる — DR-0019 §5 の見直し
status: open
category: design
created: 2026-10-05T13:30:00+09:00
last_read: 2026-10-06T11:40:00+09:00
open_entered: 2026-10-05T13:30:00+09:00
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

# 非 tty の stdin は子の stdin にそのまま渡す (PTY に流し込まない) 方が直接実行と同じになる — DR-0019 §5 の見直し

議論の素材 (2026-10-05)。未裁定。v0.11.0 で入れた「--detached でも非 tty の stdin を daemon が PTY に流す」(DR-0019 §5 の拡張) を見直す提案。

## 発端

`claude <<<'準備して待て'` は直接実行だとプロンプトが送信されるが、`hyoui run` 経由だと入力欄に残って送信されない (kawaz 報告)。

## 調査で分かったこと (worker が v0.11.0 で実機観測)

- 直接実行の claude は、非 tty の stdin を**最初のプロンプトとして読み**、その後のキーボードは **/dev/tty** から読む対話になる (制御端末が無いと「can't read the keyboard」で exit 1)。非対話モードではない
- hyoui 経由では子の stdin が PTY なので、同じ bytes が「キー入力」として届く。bytes は claude が raw mode に入る前に届くので、末尾が CR でも ICRNL で LF になり、LF は claude では送信にならない。raw に入った直後に CR を届けても、claude が入力を受け付ける準備は raw 化より遅れるので送信にならない
- 転送時に LF を CR に変える案、子が raw になるまで待って送る案は、どちらも claude では直らない (実測)
- 回避の書き方 (実機で送信を確認): `hyoui run -- claude 'プロンプト'` (argv で渡す) / `hyoui input S wait:❯ text:... key:Enter`

## 見直しの提案 (統括、未検証)

**呼び出し元の stdin が tty でない時は、その fd を子の stdin にそのまま渡し、PTY は制御端末と stdout / stderr にだけ使う。** stdin が tty の時は今どおり子の stdin も PTY。

- 直接実行で `echo x | prog` とした時、prog の stdin は pipe で tty ではない。hyoui がそれを PTY に変えて流し込む今の形の方が、子から見た環境を直接実行と変えている (非透過)。「stdin を PTY に保つ」根拠にしていた「isatty(0) が偽になると TUI が壊れる」は、直接実行でも同じ条件なので理由にならない
- pipe でデータを受けつつキーボードを使う TUI (claude、fzf、less 等) は、stdin が非 tty の時に /dev/tty を開いてキーボードを読む慣習。hyoui の子の制御端末は PTY なので /dev/tty = PTY になり、attach や `hyoui input` はそのまま届く見込み
- stdin を読む普通のプログラム (`cat` 等) は pipe を直接読み、pipe の EOF で自然に終わる。EOT の送出 (`--stdin-eof`)、改行で終わらない入力に 0x04 を 2 個送る処理、ICRNL / ^C の解釈、バイナリが運べない制約が全部要らなくなる
- attach では、attach client は stdin (= 子に渡した pipe) を読まず、キー入力は呼び出し元の /dev/tty から読む形になる (直接実行で pipe + キーボードのプログラムを使う時と同じ)
- stdin を読むプログラムに pipe を渡した後は、外からの `hyoui input` はそのプログラムの stdin には届かない。これは直接実行で pipe を渡した時と同じ挙動

## PoC の結果 (2026-10-06、worker が実機観測)

hyoui の spawn 構造 (`crates/hyoui/src/sys/raw.rs` の `openpty_fork_anchor_exec`: anchor が setsid + TIOCSCTTY、子が setpgid + tcsetpgrp) を python で再現し、子の fd 0 だけを pipe に、fd 1 / 2 と制御端末を PTY にした。キーは PTY master に直接書いた (attach / `hyoui input` が daemon 経由で master に書くのと同じ経路)。hyoui バイナリを通した提案方式は未実装なので未観測。

| 対象 | 直接実行 (stdin = pipe、tty あり) | PoC (fd 0 = pipe、制御端末 = PTY) |
|---|---|---|
| claude 2.1.290 | pipe の内容が最初のプロンプトとして送信され、その後の打鍵 + CR も送信される | 同じ (env の CLAUDECODE 等を外した版・外さない版とも) |
| claude、現行方式 (bytes を master に流し EOT) | — | 最初のプロンプトが送信されない (kawaz 報告を再現)。後の打鍵は送信される |
| fzf 0.74.1 | pipe を一覧に読み、キーで選べる | 同じ |
| less 668 | pipe を表示し、`G` / `q` が効く | 同じ |
| cat | 改行で終わらない末尾も出して EOF で終わる | 同じ (EOT 不要) |
| python3 -I / bash | stdin が pipe なので非対話 (isatty 0 = 偽) | 同じ |

現行の hyoui 0.11.0 での対照: `printf 'a\003b\000c\n' | hyoui run -- od -c` は 0x03 が ISIG で SIGINT になり rc=130 (直接実行は `a 003 b \0 c \n`)。`printf 'hi\nthere' | hyoui run -- cat` の出力には PTY の echo と EOT の痕跡 (`^D\b\b`) が混ざる。

## 実装の当たり所 (PoC worker の所見、コード読解)

- fd の受け渡しは継承 (SCM_RIGHTS なし)。`--detached` は `crates/hyoui-cli/src/daemonize.rs` で stdin を inherit し、daemon が `take_stdin_for_forward()` で `F_DUPFD_CLOEXEC` に移す。非 detached の run は daemon に stdin を渡さず、exec する attach client が pipe を読む
- 今は fd を `Session::start` の**後**に `set_stdin_forward` で渡している。spawn の**前**に渡し、`openpty_fork_anchor_exec` の子側 `dup2(slave, 0)` を「fd があればその fd」に変える。daemon は spawn 直後に fd を閉じる (書き手に EPIPE が届き、upgrade の self-exec にも漏れない)
- 非 detached の run も daemon に stdin を継承させ、attach client は stdin を読まない
- 消えるもの: `crates/hyoui/src/daemon/stdin_forward.rs`、`stdin_eof.rs` の EOT 処理、`--stdin-eof`、attach の `send_stdin_eof`
- attach の raw 化 / SIGWINCH / 外側端末のサイズは全部 stdin の fd を見ており、`/dev/tty` を開くコードは無い。stdin が非 tty の attach client は `/dev/tty` を開いてそれを入力端末にする必要がある。現行でも、端末から `cmd | hyoui run -- claude` とすると attach client は pipe を読み終えた後 stdin を見ず、キーボードが子に届かない (コード読解、未観測)
- 呼び出し元に `/dev/tty` が無い時 (Claude の Bash ツールでは ENXIO) は、attach client はキー入力なしの出力中継にする (エラーにしない)

## 提案で生じる論点 (STDIN-Q1 で裁定)

- **stdin が `/dev/null` の起動元**: agent の Bash ツールや web gateway の `--login --detached` は stdin が `/dev/null` になる。fd をそのまま渡すと `bash -i` は EOF で即終了する (直接実行と同じ)。今は `--stdin-eof=detach` で子を残しており (DR-0019 §5)、既存 e2e (`web_e2e_api` / `ctrlz_suspend_client`) もそれを使う。提案では `--stdin-eof` が消えるので、「呼び出し元の stdin を渡さず、子の stdin も PTY にする」指定が別途要る
- **単独の `hyoui attach S < file`**: 稼働中の session の子の fd 0 は後から差し替えられない。run と attach で非 tty stdin の意味が分かれる
- stdin を読む子に pipe を渡した後は、外からの `hyoui input` はその子の stdin に届かない (直接実行と同じ)。「pipe で初期入力 + 後から `hyoui input`」の使い方はできなくなる
- record (DR-0016) は pipe 入力をもともと `in` event に載せないので変化なし。upgrade (DR-0028) 中に転送が途切れる問題は daemon が fd を持たなくなるので消える

## 影響

裁定されれば DR-0019 §5 (pipe-through、`--stdin-eof`、v0.11.0 の detached 拡張) を置き換える。v1.0 前なので `--stdin-eof` は消してよい範囲。
