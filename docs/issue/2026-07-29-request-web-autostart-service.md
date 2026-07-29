---
title: hyoui web の自動起動を製品側で提供する (brew services or service サブコマンド)
status: open
category: request
created: 2026-07-29T13:10:00+09:00
last_read: 2026-07-29T13:10:00+09:00
open_entered: 2026-07-29T13:10:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz ccmsg 依頼「hyoui web を PC 再起動時に自動起動するようにしておきたい」(2026-07-29)
---

# hyoui web の自動起動を製品側で提供する

## 背景

kawaz の依頼「hyoui web を PC 再起動時に自動起動したい」に対し、マシンローカルの
LaunchAgent (`~/Library/LaunchAgents/com.github.kawaz.hyoui-web.plist`、authsock-warden と
同形式: RunAtLoad + KeepAlive、log は `~/Library/Logs/hyoui-web/output.log`) で即時対応済み
(2026-07-29、kill → 自動復帰 + API 200 を実機確認)。

ただしこれは手書き plist で、hyoui 製品としての提供手段が無い。他マシン展開・再セットアップの
再現性のために製品側で持ちたい。

## 選択肢

- **a: formula に `service do` block を追加** (release.yml の formula heredoc に追記)。
  `brew services start kawaz/tap/hyoui` で plist 生成・管理を brew に委譲。
  利点: 実装が formula 数行、brew upgrade との整合も brew services が面倒を見る。
  欠点: 次リリースまで反映されない、Linux (systemd) は brew services の対象外気味。
- **b: `hyoui web service register` 系サブコマンド** (authsock-warden DR-013 の先例)。
  利点: brew 非依存で Linux (systemd user unit) にも展開可能、argv[0] から実パス解決。
  欠点: 実装コストが a より大きい、CLI surface が増える (CLI 棚卸し直後)。
- a を先に入れて b は需要が出たら、の段階導入も可。

## 裁定・実装

- **AS-Q1=b 裁定 (2026-07-29)**: `hyoui web service register|unregister|status` を採用
- macOS LaunchAgent + Linux `systemd --user` を同一 CLI で管理する実装を追加
- DR: [DR-0031](../decisions/DR-0031-web-service-subcommand.md)
- `stable-which` で安定 binary path を解決し、macOS の同 label 手書き plist は register 時の
  bootout + 置換経路で移行する
- **実装済み**。issue status は統括での確認・close まで open のまま維持

## メモ

- `hyoui web` の listen は config `[web].listen` or default `127.0.0.1:43690` なので
  plist 側に引数不要 (現行の手書き plist も引数なし)。
- 手書き plist は a/b どちらが入っても移行時に `launchctl bootout` + rm で撤去する。
