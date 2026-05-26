# Runbooks Index

hyoui の運用・障害対応 runbook 一覧。`docs-structure.md` ルールに従い
ファイル名は `YYYY-MM-DD-<slug>.md`。各 runbook は以下 5 セクションを
含む:

1. **症状** — どんな現象が起きたら本 runbook を見るか
2. **切り分け** — root cause を絞り込む手順
3. **対処** — 即時対応 / 復旧手順
4. **予防** — 再発防止策
5. **関連** — DR / backlog / コード位置への参照

## Active

- [stale socket 検出と削除](./2026-05-27-stale-socket-detection.md) —
  daemon panic / SIGKILL 後の stale socket 検出と `--prune-stale` 利用
- [backpressure による client 切断](./2026-05-27-backpressure-disconnect.md) —
  `queued_bytes` 観測、buffer cap tuning、`--client-buffer-bytes` の使い方
- [handshake cap 超過による reject](./2026-05-27-handshake-cap-rejection.md) —
  `MAX_CAPS_COUNT` / `MAX_CAP_LEN` / `MAX_TOKEN_LEN` 超過時の対処
- [daemon panic/abort からの復旧](./2026-05-27-daemon-crash-recovery.md) —
  `panic = abort` 維持下での再現手順 (`HYOUI_ALLOW_CORE=1`) と復旧
- [孫プロセスの orphan 化検出](./2026-05-27-child-orphan-detection.md) —
  killpg 化後の意図的 detach 検出と対処
- [v0.x release deployment checklist](./2026-05-27-deployment-checklist.md) —
  brew tap / SHA256SUMS / SLSA attestation 検証手順

## Archived

(なし)

## 追加・更新ルール

- 新規 runbook 追加時は本 INDEX の `## Active` に 1 行追記
- 廃止 (= 該当機能の撤廃、別 runbook への統合等) は `## Archived` に
  移動。理由を簡記
- runbook の `Status:` フロントマター (Active / Archived) も併せて更新
- 関連 backlog 番号 (= R*-H*、R*-SRE-*) は冒頭の `> Related:` 行に明記し、
  本 INDEX の各エントリ説明文にも背景として 1 行入れる

## runbook を立てるタイミング

`docs-knowledge-flow.md` の規定に従う:

- **運用フェーズで再発しうる問題と対処手順**を runbook に残す
- journal で「同じ問題が複数回出てきた」と気づいたら runbook 化を検討する
- journal は時系列の「物語」、runbook は手順の「整理済みレシピ」

## 関連

- [[../decisions/INDEX]] — Decision Record 一覧
- [[../journal]] — 日々の生記録 (時系列)
- [[../findings]] — 単発調査の確定事実
