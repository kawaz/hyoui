---
title: "feature: `hyoui attach` で session を index で指定したい (= ID コピペ省略)"
status: resolved
category: request
created: 2026-05-30T00:00:00+09:00
last_read: 2026-06-22T11:30:00+09:00
open_entered: 2026-05-30T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-06-22T19:02:53+09:00
discard_reason:
pending_reason:
close_reason: ["implemented","done:--index=N (1始まり、負値で末尾から逆順) を attach/kill/status/tail/wait/screen/lock/input の全 session-targeted subcommand に展開済 (commit 997b0a2b/f569ddd7/a21bf67a/0deeac56/f2778c65)。位置引数の整数→index 解釈は撤回、--all は kill 専用維持"]
blocked_by:
origin: kawaz 発言 (2026-05-30)
---

# feature: `hyoui attach` で session を index で指定したい (= ID コピペ省略)

- Priority: 中 (= UX 改善、複数 session 運用時に効果大)

案 B (`--index=N` 専用) 採用で実装完了。全 session-targeted subcommand (attach / kill / status / tail / wait / screen / lock / input) に共通展開済。実装 commit: `997b0a2b` (attach --index 初版) / `f569ddd7` (kill --index + --all) / `a21bf67a` (位置引数の整数→index 解釈を撤回、案 A 不採用) / `0deeac56` (status/tail/wait/screen/lock に共通化) / `f2778c65` (usage 8 個 + input family)。`--all` は kill 専用維持 (kawaz「ケースバイケース」)

## 背景

kawaz の発言:

> hyoui attach で list 見たり選んだり面倒なので、適当に古いほうから選んで attach するみたいなオプションが欲しい。
> 複数から選びたいけど個別 ID をコピペは面倒。1 番古いやつからの index 指定で選べる程度で、対象がなければエラーで OK。
> `hyoui attach -1` や `1` `2` で新しいセッション/古いセッションからのインデックス、みたいな。
> さすがに何もオプションないとあれなので、そういう指定用のオプションを用意するのもアリ。

## 採用案

案 B: `--index=N` 専用オプション。曖昧さなし、ロングオプション基本の CLI 規約に合致。

```bash
hyoui attach --index=1     # 1 番古い
hyoui attach --index=-1    # 1 番新しい
hyoui attach my-app-1      # 既存通り
```

## 関連

- `crates/hyoui/src/cli.rs::parse_attach`
- `crates/hyoui-cli/src/main.rs::attach_command`
