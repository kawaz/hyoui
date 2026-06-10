# Issue 運用規約 (hyoui)

`docs/issue/` の運用ルール。hyoui で試験運用中、安定したら `~/.claude-personal/rules/docs-structure.md` (kawaz 個人 global) に「各 dir に README 配置慣習」として展開予定。

## ファイル名形式

```
YYYY-MM-DD-{prefix}-{slug}.md
YYYY-MM-DD-{slug}.md           (prefix なし)
```

例:
- `2026-05-26-feature-recording-and-dump.md`
- `2026-05-26-bug-paste-hang-on-shutdown.md`
- `2026-05-26-task-homebrew-tap-distribution.md`
- `2026-05-21-ci-release-workflow.md` (prefix なし、旧来の起票)

## Prefix 一覧

| prefix | 意味 | 採用判定 |
|---|---|---|
| `bug-` | バグ報告、修正必要 | 即対応 (優先度別途) |
| `feature-` | アイデア、採用未確定 | 判断保留中 |
| `task-` | 採用済み TODO、やる | 採用済み |
| (なし) | 外部からの依頼/受付窓口、または一般 issue | (依存) |

## 状態遷移

```
feature-  ─ 採用 ─→  task- にリネーム or 直接実装
          └ 却下 ─→  削除 (jj/git 履歴で内容追える)

bug-      ─ 修正 ─→  削除 (commit/GitHub Releases で追える)
          └ 不再現/仕様 ─→  削除 + journal/findings に記録

task-     ─ 完了 ─→  削除
          └ 中止 ─→  削除

(なし)    ─ 解決 ─→  削除、必要なら decisions/runbooks/journal/findings に昇格
```

## 削除フロー

解決時は **delete** が基本。jj/git 履歴で旧内容は追える。削除前に内容の性格に応じて記録を残す
(`docs-knowledge-flow.md` ルール参照):

| 解決の性格 | 記録先 |
|---|---|
| 単純なコード修正のみ | 記録不要 (commit/GitHub Releases で足りる) |
| 設計判断を伴う | `docs/decisions/DR-NNNN-...md` |
| 運用上の再発可能性 | `docs/runbooks/<topic>.md` |
| 経緯・試行錯誤・ハマり所 | `docs/journal/YYYY-MM-DD-<slug>.md` |

## prefix 判定迷い時の指針

- **判断保留にしたい** → `feature-`
- **やる確定 + 着手前** → `task-`
- **既に動いてる/正しいはずなのに動かない** → `bug-`
- **外部に発信したい/外部からの受付** → prefix なし

`feature-` と `task-` の境界は「採用判断したか」。アイデアレベル = `feature-`、「この機能を実装する」と決まった瞬間 = `task-`。リネームが筋。

## 関連

- `~/.claude-personal/rules/docs-structure.md` — docs/ ディレクトリの大枠規約 (hyoui で試験中の prefix 詳細はここに反映予定)
- `~/.claude-personal/rules/docs-knowledge-flow.md` — issue 解決時の記録フロー
