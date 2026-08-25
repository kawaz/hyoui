---
title: web gateway の再起動は kill だけでは復帰しないことがある
status: open
category: task
created: 2026-08-25T12:17:50+09:00
last_read:
open_entered: 2026-08-25T12:17:50+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# web gateway の再起動は kill だけでは復帰しないことがある

## 概要

launchd 管理の web gateway (label `com.github.kawaz.hyoui-web`) を更新後に再起動したいとき、`hyoui web service status` で得た pid を kill するだけでは KeepAlive が復帰させないことがある。

## 背景

2026-08-25 に実測: v0.9.40 → v0.9.41 の brew upgrade 直後に kill したところ `running: no` / `pid: -` のまま復帰せず、`hyoui web service register` を実行して初めて起動した (同日の v0.9.39 → v0.9.40 では kill だけで復帰していたので、再現条件は未特定。launchd の再起動 throttle (10s 制限) や、直前の brew upgrade によるバイナリ差し替えとの相互作用が疑わしいが未検証)。

web assets は**バイナリに埋め込まれている**ため、リリース内容を反映するには「brew upgrade → gateway 再起動 → ブラウザリロード」の 3 段が必要。この 2 段目が黙って失敗すると、gateway が落ちたまま気づかない (= web が使えない) か、古い assets が配信され続ける。実際 v0.9.35 が 8/19 から動き続けていて、v0.9.36〜0.9.39 の web 変更が反映されていなかった事例がある。

### 対処 (暫定)

再起動は `hyoui web service register` を使う (help 曰く「Install or replace the definition, enable it, and start now」)。kill + KeepAlive 頼みにしない。

### 検討事項 (未裁定)

- 案 A: `hyoui web service restart` サブコマンドを追加する (現在は register / unregister / status の 3 つ)。「更新を反映したい」は頻出操作なので、専用の入り口がある方が事故が少ない
- 案 B: register が冪等に「停止していれば起動、動いていれば再起動」する現仕様を docs に明記して restart は作らない
- 案 C: `brew upgrade` 後に gateway が古いバイナリで動いていることを検出して警告する仕組み (status に稼働中バイナリの version を出す等)。status の出力に稼働 pid の version が無いため、現状は curl で assets を見るしか確認手段がない

### 検証コマンド (現状の確認手段)

`curl -s http://127.0.0.1:43690/assets/session.js | grep -c '<期待する新機能の文字列>'` で新旧を判別できる (例: v0.9.40 なら `linkHandler`、v0.9.41 なら `layer=both`)。

### 関連

DR-0031 (web service subcommand) / DR-0027 (web gateway、assets の埋め込み方針)

## 受け入れ条件

- [ ] 案 A/B/C のいずれかを裁定し、必要なら実装する
- [ ] 再現条件 (kill だけで復帰する/しない差) を特定するか、無条件に register を使う運用として docs 化する

## TODO

<!-- wip 時のみ -->
