---
title: "feature: `hyoui list` の表示形式改善 (= 固定長 + cwd / argv 表示 + `--format=jsonl`)"
status: resolved
category: request
created: 2026-05-30T00:00:00+09:00
last_read: 2026-06-22T18:49:58+09:00
open_entered: 2026-05-30T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-06-22T19:00:00+09:00
discard_reason:
pending_reason: ["実装完了済み、journal/DR への昇華待ち"]
close_reason: ["implemented","done:cwd/argv/clients 表示 + plain 6 列固定長 + --format=jsonl + stale 格下げ警告 + cwd 短縮表記。DaemonConfig.cwd 追加 + StatusResponse 拡張 + list 並列 status.query で完成"]
blocked_by:
origin: kawaz 発言 (2026-05-30)
---

# feature: `hyoui list` の表示形式改善 (= 固定長 + cwd / argv 表示 + `--format=jsonl`)

- Priority: 中 (= UX 改善、多 session 運用時に効果大)

cwd / argv / clients 表示が完成。実装サマリ:
- `DaemonConfig.cwd` を追加、`run_daemon_child` の `chdir("/")` 直前で `std::env::current_dir()` を capture (失敗時は `/` で fallback)
- `StatusResponse.cwd: String` / `argv: Vec<String>` は **必須 field** (= v1.0 breaking OK 方針)
- `hyoui list` が live socket に対し並列で status.query を投げて cwd / argv / clients を取得
- plain format は `SESSION STATUS DUR CLIENTS CWD ARGV` の 6 列、SOCKET 列は jsonl 側のみに残す
- cwd shorten: `<...>/repos/<host>/<owner>/<repo>/<sub>` → `<owner>/<repo>/<sub>`、それ以外は `$HOME` 前カット (`~/...`) のみ適用
- probe で live と判定 → status.query で failure なら **stale 格下げ + stderr warning**

## 背景

kawaz の発言:

> list が見にくい。固定長フィールドを左にしつつ、cwd や実行コマンドなどが見られると何のプロセスかわかる。
> ソケット名とかだけ出されても分からん。起動日時 / DUR / ステータス / 接続数 / cwd (repos/github.com/ なら前カット) / コマンド引数 を 1 行で、
> cwd 前あたりまでは固定長で出して欲しい。`--format=jsonl` option も。

## 関連

- `crates/hyoui-cli/src/main.rs::list_command_with_dirs`
- `crates/hyoui/src/daemon/messages/` (= StatusResponse protocol)
