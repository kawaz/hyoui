# feature: kill --wait に timeout / SIGKILL 昇格 (escalation) を入れる

- Date: 2026-06-11
- Status: idea
- Priority: 低 (= default kill は即時応答化済みで影響なし。--wait 利用時のみの残余)

## 背景

kill 即時応答化 (2026-06-11) の調査で判明: `--wait` 経路の `finalize_child` は
`kill_pgrp(SIGTERM)` 後に **timeout なしの blocking waitpid** で子の exit を待つ。
SIGTERM を catch して死なない子 (claude 等) を `--wait` で撃つと永久 block する。
(`Session::drop` の 500ms→SIGKILL は panic/early-return 専用で serve 経路を通らない)

## 対応案

- `--wait` に `--wait-timeout=<DUR>` (または `--wait=<DUR>`) を追加し、超過時の挙動を選択:
  (a) エラーで返る (= 子は生かしたまま) / (b) SIGKILL に昇格して見届ける
- shell の慣行 (= job への TERM に CONT 併送) に倣い、stopped な子への terminate 時は
  SIGCONT 併送も検討 (= stopped のまま TERM が pending になるのを防ぐ)

## 関連

- docs/issue/2026-06-11-kill-should-return-immediately.md (即時化本体、解決済みなら昇華先)
- [[DR-0017]] suspend policy
