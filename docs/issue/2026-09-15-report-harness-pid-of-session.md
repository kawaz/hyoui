---
title: hyoui のセッションが起動したハーネス本体 (claude/codex) の pid を answer できるようにする
status: open
category: request
created: 2026-09-15T10:58:19+09:00
last_read:
open_entered: 2026-09-15T10:58:19+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: ccmsg
---

# hyoui のセッションが起動したハーネス本体 (claude/codex) の pid を answer できるようにする

## 概要

hyoui 経由で起動した直後、ハーネスが自分の状態ファイル (`$CLAUDE_CONFIG_DIR/sessions/<pid>.json`) を書く前 (初回ディレクトリの trust 確認で TUI が止まっている等) の段階で、ccmsg が「この hyoui セッション = このハーネスプロセス」を知りたい。webui で「起動したはずなのに来ない」時に、TUI で止まっているのか失敗したのかを見分けて terminal へ誘導するため。

具体的には、hyoui のセッション id から、その pane のシェルの子孫のうちハーネス本体 (`claude` / `codex` の main process) の pid を返す口 (CLI サブコマンドか、既存の status 出力への追加)。無ければ「まだ起動していない / 終了した」が区別できると良い。

## 背景

ccmsg (daemon) issue `launcher-run-before-state-file`、契約 DR-0001 §4 からの依頼。

## 受け入れ条件

- [ ] `hyoui <something> <session-id>` で `{pid, started_at}` 相当が JSON で取れる (無ければ空)
- [ ] ccmsg の launcher は起動後にこれを呼び、契約の `agents` 行 (`sid` 無し、`terminal_id = hyoui:<session-id>`) として載せられる
