# DR-0004: CLI サブコマンド設計 (run / completion / 将来枠 send/attach/status)

- Status: Active
- Date: 2026-05-25
- Related: DR-0003 (Rust 一本化), `~/.claude/rules/cli-design-preferences.md`

## Context

poc 段階の CLI は `hyoui -- cmd [args...]` のフラット構造で「PTY 内でコマンドを実行する」
1 機能だけを提供していた。設計 (README) では将来以下のコントロール系機能を予定:

- Unix socket 経由で外部から PTY へ入力注入
- 停止条件 (`--timeout` / `--idle-timeout` / `--until <pattern>`)
- bg/fg 透過制御 (`--child-suspend` / `--parent-suspend` 等)

これらが増えると `hyoui` 直下のフラットなオプション群が肥大化し、コントロール系
(他プロセスとの通信) と実行系 (PTY 起動) の区別がつかなくなる。

## Decision

**サブコマンド方式を採用する**。

初期サブコマンド:
- **`hyoui run -- cmd [args...]`** — PTY 内でコマンドを実行 (poc の `hyoui -- cmd` 相当)
- **`hyoui completion <bash|zsh|fish>`** — 補完スクリプトを stdout に出力

将来枠 (Reserved、現時点では未実装だが --help と completion にプレースホルダ):
- **`hyoui send <socket> <input>`** — 既存 hyoui インスタンスの PTY へ入力注入
- **`hyoui attach <socket>`** — 既存 hyoui の PTY へ対話的に attach
- **`hyoui status <socket>`** — 既存 hyoui の状態問い合わせ

引数なし / サブコマンドなし / 不明サブコマンド時は常に **`--help` を表示**。
`hyoui -- cmd` のようなショートカット (run 省略) は **作らない**。

## Rejected alternatives

### `hyoui -- cmd [args...]` のフラット構造を維持
- 将来の send/attach/status を追加する余地がなくなる (どこで実行コマンドの開始を判定するかで
  曖昧さが生まれる)
- cli-design-preferences ルールの「複数機能はサブコマンドで提供」に反する

### サブコマンド名 `exec`
- シェル組み込み `exec` および `docker exec` の連想と衝突
- 「現プロセスを置き換える」イメージ (シェル exec) を呼び起こすが、hyoui は親プロセスのまま
  子を PTY 内に作る動作のため誤解を招く
- 中立的な動詞 `run` を採用

### `hyoui -- cmd` のショートカット (run 省略)
- `hyoui send` のような既存サブコマンド名と `hyoui send-something-binary` のような
  実行コマンド名が衝突したときの曖昧さの素になる
- 「明示が必要なら `run` を書く」ことで曖昧さを根絶
- 補完設計もシンプル化 (サブコマンド名 → そのサブコマンド固有の補完、で完結)

### サブコマンドなし時にデフォルトで何かを実行 (例: --help でなく run)
- ユーザが意図と違う動作を引き起こすリスク
- 「何もしないか」「help を出すか」の選択肢のうち、help の方が学習可能性が高い

## Consequences

- `crates/hyoui-cli/src/main.rs` に dispatch loop、`completion.rs` に補完スクリプト生成
- `crates/hyoui/src/cli.rs` に CLI 引数パーサ (clap は使わず自前 parse、依存最小化)
- 補完スクリプトは初期サブコマンド + Reserved サブコマンドの全名を列挙 (将来追加時の追従漏れを
  目視しやすくする)
- `hyoui run -- ...` の `--` セパレータで「ここから先は子コマンドの argv」を明示
- 将来 send/attach/status を実装する際、`run` と同列に dispatch を追加するだけで済む
  (フラット構造で後付けする場合の設計負債を回避)
