---
title: 非 tty の stdin は子の stdin にそのまま渡す (PTY に流し込まない) 方が直接実行と同じになる — DR-0019 §5 の見直し
status: open
category: design
created: 2026-10-05T13:30:00+09:00
last_read: 2026-10-05T13:30:00+09:00
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

## 未検証 (実装前に確かめる)

- hyoui の子として pipe を stdin に受けた claude が、制御端末 (PTY) の /dev/tty からキーを読み、attach と `hyoui input` が届くか
- fd を子に渡す経路 (daemon が受け取った fd を子の fd 0 に dup2 してから exec、daemon 側は閉じる) と、v0.11.0 の reader thread + 内部 pipe の置き換え
- 呼び出し元の /dev/tty が無い (Claude の Bash ツール等) 時の attach の振る舞い

## 影響

裁定されれば DR-0019 §5 (pipe-through、`--stdin-eof`、v0.11.0 の detached 拡張) を置き換える。v1.0 前なので `--stdin-eof` は消してよい範囲。
