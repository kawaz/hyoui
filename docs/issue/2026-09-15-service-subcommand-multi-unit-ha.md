---
title: hyoui service を reference の daemon/service 体系に沿った multi-unit 構成にし、stable/unstable 2 インスタンスの HA を組む
status: open
category: task
created: 2026-09-15T15:55:00+09:00
last_read:
open_entered: 2026-09-15T15:55:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 裁定 (2026-09-15、QUESTIONS ECO-Q2 への回答)
---

# hyoui service を reference の daemon/service 体系に沿った multi-unit 構成にし、stable/unstable 2 インスタンスの HA を組む

## 概要

現在の `hyoui web service register|unregister|status` は web gateway 1 unit 固定。kawaz の裁定は「個人 reference `cli-daemon-subcommands` の daemon/service 設計に沿った作り」にすること。unit が複数ある前提の体系 (add / remove / list …) を採り、`add --port` のように差分だけ変えて unit を追加できるようにする。

動機は運用の管理しやすさと、複数 gateway の HA:

- 2 インスタンスを起動し、一方は Homebrew 配布バイナリ (stable)、もう一方は開発中バイナリ (unstable)
- canddy (リポ名は canddy、caddy の typo のまま) の設定で 2 インスタンスに優先順を付け、unstable 優先・死んだら stable に fallback する HA 構成

## 参考

- 個人 reference: `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/cli-daemon-subcommands.md` (1 instance = 1 unit、JSON 出力規約、OS 常駐登録)
- llm-gateway リポが service 体系で先行しているので参考にする
- canddy リポ (kawaz/canddy) の upstream 設定

## 受け入れ条件

- [ ] DR を起草し、`hyoui service` のサブコマンド体系 (unit の識別子、`add --port` 等の差分指定、binary path の指定方法、list / status の JSON 出力) と、既存 `web service register|unregister|status` からの移行を決める
- [ ] 実装 (help / completion / 実装の 3 者同期)
- [ ] stable + unstable の 2 unit を実機で常駐させ、canddy の優先順付き upstream で fallback が効くことを確認する (canddy 側の設定変更は canddy リポの issue として依頼)
