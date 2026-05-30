# feature: `hyoui attach` で session を index で指定したい (= ID コピペ省略)

- Date: 2026-05-30
- Priority: 中 (= UX 改善、複数 session 運用時に効果大)
- Status: Open (= 案 A vs 案 B の設計選択を kawaz と要相談)
- 報告者: kawaz 発言 (2026-05-30)

## 背景

kawaz の発言:

> hyoui attach で list 見たり選んだり面倒なので、適当に古いほうから選んで attach するみたいなオプションが欲しい。
> 複数から選びたいけど個別 ID をコピペは面倒。1 番古いやつからの index 指定で選べる程度で、対象がなければエラーで OK。
> `hyoui attach -1` や `1` `2` で新しいセッション/古いセッションからのインデックス、みたいな。
> さすがに何もオプションないとあれなので、そういう指定用のオプションを用意するのもアリ。

## 現状

`hyoui attach` (= `crates/hyoui/src/cli.rs::parse_attach` line 1325-1411) は以下のみ受理:

- 位置引数: session-id (= 1 個、複数はエラー)
- `--socket PATH`: 明示 socket path
- `--mode rw|ro|rw-no-leader`, `--exclusive`, `--detach-others`, `--debug-dump-client`

session 一覧 (= `main.rs::list_command_with_dirs` 643-700) は socket file path しか持たず、**時系列情報 (mtime) を取得していない**。

## 設計案

### 案 A: 位置引数を数字なら index 解釈

```bash
hyoui attach 1     # 1 番古い session に attach
hyoui attach -1    # 1 番新しい session に attach
hyoui attach 2     # 古い方から 2 番目
hyoui attach -2    # 新しい方から 2 番目
hyoui attach my-app-1   # 既存通り、session-id で attach
```

- **pros**: kawaz 例示そのまま、最短コマンド (= タイプ数最小)
- **cons**: session-id が数字始まりの場合の曖昧さ (= `-1` も session-id として valid な文字列)。`--` セパレータか「数字のみは index」ルールで解消可能。
- **実装規模**: 50-80 lines (= cli.rs parse_attach の位置引数解釈拡張 + main.rs の session sort + index 照合)

### 案 B: 新オプション `--index=N`

```bash
hyoui attach --index=1     # 1 番古い
hyoui attach --index=-1    # 1 番新しい
hyoui attach my-app-1      # 既存通り
```

- **pros**: 曖昧さなし、`~/.claude-personal/rules/cli-design-preferences.md` の「ロングオプション基本」に合致
- **cons**: kawaz 例示 (`attach 1`) より冗長、タイプ数増
- **実装規模**: 60-100 lines

### 案 C: 外部 shell で組む

```bash
hyoui attach "$(hyoui list | awk '$2=="live" {print $1; exit}')"
```

- **pros**: 実装ゼロ
- **cons**: kawaz の「面倒」を解決していない、shell wrapper 前提

## 推奨判断 (= 要 kawaz 相談)

- kawaz 例示は **案 A**寄り (= `attach 1`, `-1`, `2`)
- CLI design preferences は **案 B**寄り
- 両方を実装することも可能 (= 案 A + 案 B、位置引数の数字を `--index` の syntactic sugar として扱う)

→ **推奨**: 両方実装 (= 位置引数で `1` / `-1` を受け、内部的には `--index` 経路に集約)。kawaz 例示も CLI 規約も満たす。

## 前提となる変更

### 1. session 一覧の時系列ソート

- `list_command_with_dirs` の socket scan 時に file mtime を取得
- 出力にも mtime (= human-readable 相対時刻) を追加し、古い順に表示
- 既存 script 互換のため `--no-mtime` または `--format=plain` で旧表示 (要 CLI 議論)

### 2. attach 引数 parser 拡張

- `AttachConfig` に `index: Option<i32>` 追加
- session-id と `--index` の同時指定はエラー (= 不整合検出)
- 位置引数が `^-?\d+$` にマッチかつ session 一覧に同名 session が存在しない場合は index 解釈 (= 曖昧解消ルール)

### 3. 実装場所

- `crates/hyoui/src/cli.rs::parse_attach` (= 15-20 lines)
- `crates/hyoui-cli/src/main.rs::attach_command` (= 50-60 lines)
- `crates/hyoui/src/cli.rs::usage_attach` help 更新 (= 5 lines)
- tests 追加 (= 20-30 lines)
- **総計**: 80-115 lines

## 進行中議論との衝突

`docs/issue/2026-05-28-feature-cli-restructure-discussion.md` の CLI 再編議論は **attach の引数体系を扱っていない** (= screen view 改名 / dump 独立化 / format 整理 等が対象)。本機能は独立 feature として進めて OK。

## kawaz への確認ポイント

1. 案 A / 案 B / 両方実装 (案 A+B 推奨) のどれを採用するか
2. `hyoui list` 出力に mtime + 古い順ソートを追加する (= 出力フォーマット変更) を承認するか
3. session-id と数字の曖昧解消ルール: 「数字のみで session-id に該当なしの場合は index 解釈」で良いか

## 関連

- `~/.claude-personal/rules/cli-design-preferences.md` — CLI 設計の好み
- `crates/hyoui/src/cli.rs::parse_attach` (= 1325-1411 行)
- `crates/hyoui-cli/src/main.rs::list_command_with_dirs` (= 643-700 行)
- `docs/issue/2026-05-28-feature-cli-restructure-discussion.md` (= 衝突なし、独立 feature 可)
