---
title: session に複数 label を付与可能にする (核心は DR-0041 の tag で実装済み、残りの要否を kawaz 裁定)
status: open
category: request
created: 2026-10-14T09:20:00+09:00
last_read: 2026-10-14T09:20:00+09:00
open_entered: 2026-10-14T09:20:00+09:00
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

# feature: session に複数 label (= 単一文字列の複数貼り) を付与可能にする

## 現状 (2026-10-14)

核心は DR-0041 の tag (`run --tag` 繰り返し、`list --tag`) で実装済み。tag は `key=value` で、単語の集合 (label) は持たない (DR-0041 決定 1、2026-10-07 裁定)。表示列も DR-0041 で入っている (plain の `list` の TAGS 列、`status` の `tags:` 行、jsonl の `tags`)。残りは実行中の label 追加/削除 (DR-0041 の「未決」に「tag を起動後に変える手段 (`hyoui set` 等) を持つか」として残っている)、kill の絞り込み (`--tag` を受けるのは `run` と `list` だけ)、表示列で足りない点があるか → 要否を kawaz 裁定。

以下は 2026-06-16 起票時の記録 (namespace 廃止前の前提で書かれている)。

- Date: 2026-06-16
- Severity: 中 (= namespace の補完機能、無くても動くが多用途運用で役立つ)
- Reporter: kawaz (cmux-msg ↔ hyoui 移行の議論で発生)

## 背景

現状 hyoui は session の分類軸として `--namespace=<NS>` (DR-0018) を持つ。これは:

- 1 session = 1 namespace の **単一軸の隔離**
- `HYOUI_NAMESPACE` env や direnv 連動で session 群を分離

という特徴を持ち、tmux の TMUX / screen の STY と同じ感覚で動作する。

一方、**「同じ namespace 内でさらに用途別マーキングしたい」** ケースが出てきた。例:

- cmux-msg (= 親プロジェクト) が起動する子 claude セッションを `app=ccmsg` でマーキングしたい
- worker 用途は `role=worker`、テスト用途は `purpose=test` でマーキングしたい
- これら複数軸の組み合わせ (= `app=ccmsg AND role=worker` のような絞り込み) を namespace 1 軸で表現するのは無理

namespace は強い隔離 (= 別 namespace は `--all-namespaces` を明示しないと見えない) で適切に動いているので、**namespace を増やすのではなく、軽い「複数 label 付与」を別軸で追加したい**。

## tag vs label のセマンティクス整理

kawaz の感覚 (一般的な開発界隈の用法と整合):

| 用語 | セマンティクス | 例 |
|---|---|---|
| tag | key:value 風の複合 (= `k=v` または `kb` 構造) | Kubernetes label, Docker label-schema, AWS resource tag |
| label | 単一文字列、複数貼れる | GitHub issue label, Trello label |

**本要望は「label」(= 単一文字列、複数貼れる、key:value ではない)** を採用したい。理由:

- key=value にすると、key の正規化 / 名前空間の議論 (`app=ccmsg` vs `application=ccmsg`) が発生して重い
- 単一文字列 (= `ccmsg`, `worker`, `test` 等) なら検索・付与・剥がしがシンプル
- 必要なら `app=ccmsg` のような prefix 付き文字列を **慣習** として使えばよい (= hyoui 側は文字列としてしか扱わない)
- GitHub issue label と同じ感覚で運用できる

## 期待挙動

### 1. session 起動時に複数 label を付与

```bash
hyoui run --detached \
  --label=app=ccmsg \
  --label=role=worker \
  --label=parent=abc123 \
  --session=<UUID> -- claude --session-id <UUID>
```

`--label` は複数指定可能 (= flag を繰り返す)。値は単一文字列 (= `:` を含んでも単なる文字列として扱う、構造化は呼出側の慣習)。

### 2. label を runtime で追加 / 削除

```bash
hyoui label add <session> --label=tag1 --label=tag2
hyoui label remove <session> --label=tag1
hyoui label list <session>
```

(= name 揺れの候補: `hyoui label add/remove/list` or `hyoui set --label=...` の拡張)

### 3. list で絞り込み

```bash
# 単一 label を持つ session を絞り込み
hyoui list --label=app=ccmsg

# 複数 label の AND
hyoui list --label=app=ccmsg --label=role=worker

# 全 namespace 横断 + label 絞り込み
hyoui list --all-namespaces --label=app=ccmsg
```

### 4. kill の絞り込み

```bash
# label で絞った session 群を一括 kill
hyoui kill --all --label=app=ccmsg
```

### 5. status / list に label 列を表示

```
SESSION              STATUS  ...  LABELS
cmsg-child-abc123    live    ...  app=ccmsg, role=worker
```

## 設計上の論点

### A. namespace との関係

label は **namespace の代替ではなく補完**。namespace は依然として強い隔離軸 (デフォルト見えない / 別 namespace との操作分離) を担い、label は同 namespace 内でのソフトなマーキング軸を担う。
namespace を捨てて label に一本化する案は不採用 (= direnv 連動の自動切替や TMUX / STY 互換の感覚を保ちたい)。

### B. label の永続化

- session メタ情報の一部として daemon socket / status file に持つ
- daemon restart 跨ぎで保持される
- 子プロセスへの env injection は **任意** (= `HYOUI_LABELS=<csv>` を入れるか議論余地)

### C. label の文字制約

- 長さ上限 (例: 1 label あたり 64 文字)
- 禁止文字 (= 制御文字、改行)
- ASCII / UTF-8 のどこまで許すか

### D. CLI surface の拡張範囲

`--label` flag の追加対象:

- `run` (起動時)
- `list` (絞り込み)
- `kill` (絞り込み)
- 新規 `label` subcommand (add/remove/list)
- `status` (表示)
- `set` (= 既存の runtime config 変更コマンドに含めるか、別 subcommand にするか)

### E. tag を将来追加するか

key:value の tag が欲しくなったときに、改めて `--tag=k=v` を追加することは妨げない。label と tag は別 primitive として共存可能 (= GitHub の label と milestone のような関係)。

## 影響範囲

- `--label` flag の parser
- session metadata schema (daemon 側)
- list / kill / status / 新 label subcommand
- docs (README / MANUAL / DESIGN)
- DR 起票 (= 本要望が採用されたら DR-00XX として残す)

## 実観測: 2026-06-16 時点の `hyoui list` 混雑状況

本 issue 起票の動機を裏付ける実観測データ (= kawaz の現環境 `hyoui list --all-namespaces`):

```
default     run-1089-47d4ce39    stopped  ...  kawaz/hyoui/main  claude --dangerously-skip-permissions
default     bug-inv-1            live     ...  kawaz/hyoui/main  claude
default     t1781575758-c1       live     ...  kawaz/hyoui/main  claude
default     long-1781575912      live     ...  kawaz/hyoui/main  claude
default     exp1781575967-1      live     ...  kawaz/hyoui/main  claude
default     exp1781575967-2..5   live     ...  kawaz/hyoui/main  claude  (5 セッション)
default     exp21781576124-1..4  live     ...  kawaz/hyoui/main  claude  (4 セッション)
```

`default` namespace の `kawaz/hyoui/main` cwd だけで **10+ session**。命名規則がバラバラ (`bug-inv-` / `t<timestamp>-` / `long-<timestamp>` / `exp<batch>-<n>` / `exp2<batch>-<n>`) で、

- 各 session が「何のための起動か」(= bug 調査 / 長時間ジョブ / 実験バッチ etc.) を label 無しに**区別できない**
- 「実験バッチ exp1781575967」と「実験バッチ exp21781576124」は別物のはずだが、命名でしか区別できず、後から `hyoui list` で「どっちの実験どこまで進んだ」が分からない
- cmux-msg のような外部ツールから「`hyoui/main` cwd で alive な claude session」に msg を送りたい時、宛先候補が複数あって**どれが本物か特定できない**

kawaz 自身もこの状況を見て「namespace で自由命名させると意図が崩れる、label と namespace は分けるべき」と本要望の必要性を再確認している。

## 参考: cmux-msg 側で本要望が出た文脈

cmux-msg (kawaz/claude-cmux-msg) は hyoui era 移行 (DR-0009) で「`hyoui run --detached --namespace=cmuxmsg`」案を一度検討したが、

- 1 軸の namespace で cmux-msg 起動 claude を他用途と隔離する必然性が薄い (= 同じ kawaz の作業内で隔離する意味が小さい)
- でも `hyoui list` で「これは cmux-msg 起動の claude」と一目で分かる印は欲しい

という観点から **「namespace ではなく label でマーキング」** に方針転換 (= cmux-msg DR-0009)。本要望はその受け皿。

## TODO

- [ ] hyoui コミュニティ / kawaz の他 plugin リポでも同様の需要があるか確認
- [ ] DR で採用する場合、namespace と label の責務分離を docs/decisions で明文化
- [ ] CLI surface の確定 (= `--label` の名前、`label` subcommand の必要性、表示列)
- [ ] daemon 側の metadata schema 拡張 + persistent 保存方式の決定
- [ ] テスト追加 (= label 付き session の attach / list / kill の挙動)
