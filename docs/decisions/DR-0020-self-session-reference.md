# DR-0020: self-session 参照 — `HYOUI_SESSION_ID` 注入と session 引数の省略時解決

- Status: Active
- Date: 2026-06-12
- Related: DR-0018 (env 注入の透過例外の先例 = `HYOUI_NAMESPACE`), DR-0006 (CLI ground rules / `--detach-others` の原典), DR-0004 (`detach` は予約 subcommand), DR-0019 (`hyoui set` — 省略時解決の主要ユースケース)
- Origin: kawaz ドッグフーディング 2 日目 (2026-06-12) の要望群

## Context

`hyoui run -- claude` 配下のプロセス (shell / AI agent) が自セッションを操作したい場面が
ドッグフーディングで頻出した:

- 中から detach したい (= TUI 直起動でも脱出したい)
- 中から `hyoui set on-child-suspend=auto-resume` したい (= 「これから無人になる」宣言)
- 中から `hyoui status` / `wait` / `screen dump` で自分を観測したい
- hyoui in hyoui の事故 (= 自セッションへの attach ループ) を防ぎたい

また同日、端末 B から覗き見 attach した際に「抜け方 (Ctrl-A d) と覗き方 (--mode=ro) が
発見できない」UX 問題も観測された (= 機能は実装済みだが発見性が壊滅)。

## Decision

### 1. 子プロセスへ `HYOUI_SESSION_ID` を常時注入する

daemon が child を spawn する際、`HYOUI_NAMESPACE` (DR-0018) と同様に
`HYOUI_SESSION_ID=<session-id>` を常時注入する。

justify: DR-0018 で確立した透過例外の同枠 (= tmux `$TMUX` / screen `$STY` 慣行)。
セッション自己参照の必然があり、env 1 個の追加は DR-0018 と同じコスト構造。

### 2. session 引数の省略時解決規則 (全 session 系 subcommand 共通)

```
明示引数 > $HYOUI_SESSION_ID (= 中から実行) > 既存 fallback (index 解決 / エラー)
```

- 適用対象: `set` / `status` / `wait` / `screen` / `tail` / `input` / `kill` / `lock` /
  `unlock` / `detach` (§3) 等、session を取る全 subcommand
- 明示引数があれば従来通り (= 外から使う既存挙動は不変)
- `$HYOUI_SESSION_ID` が指す session が存在しない (= stale env) 場合は既存 fallback に
  落とさず明示エラー (= 誤爆防止。「自分を指したつもりが別 session」が最悪)

### 3. `hyoui attach` は self-session default を禁止する (ネスト防止)

`$HYOUI_SESSION_ID` 環境下で attach の対象が自セッションに解決される場合は明示エラー
(= tmux の `$TMUX` ネスト防止と同型)。明示引数で自セッションを指定した場合も同様に
エラー (= 中から自分に attach する入れ子ループに合理的用途が無い。ro 観戦の例外は
将来必要になったら再検討)。`kill` の self default は許容する (= `exit` 相当の意図的用途)。

### 4. `hyoui detach` subcommand の実体化 (予約解除)

DR-0004 で予約していた `detach` を実装する:

```
hyoui detach [session] [--target=others|all|self]
```

- 引数なし + 中から = 自セッションの **all** (= 全 client を detach。TUI 直起動からの
  脱出ユースケース)。外から = target 必須エラーではなく **others** ... ではなく
  **明示 target なしは all** に統一 (= 「この session の attach を全部引き剥がす」が
  最も直感的な動詞の意味。others は attach 中の端末から「自分以外を蹴る」用)
- daemon 側 `Detach{target: Others/All}` の部分実装 (`DetachTargetPartial`) を完成させる
- これにより `attach --detach-others` (DR-0019 §6 で未実装エラー化) も同じ daemon 機構で
  実装可能になる (= docs/issue/2026-06-12-feature-attach-exclusive-detach-others.md と統合)

### 5. 発見性の改善 (UX、透過原則の範囲内)

- attach 成立時に **stderr** へ 1 行ヒントを出す:
  `[hyoui] detach: Ctrl-A d | peek: --mode=ro` (文言は実装時調整)。
  子の出力経路 (PTY) ではなく client の stderr なので透過性を壊さない (screen 慣行)。
  `--quiet` で抑止。非 tty stderr (= pipe 利用) では出さない
- `hyoui status` に client 一覧 (mode / leader / 接続時刻) を表示 — 「どの端末が
  rw/leader か」を外から確認できる

## Rejected alternatives

- **attach の self-ro 観戦許可**: 入れ子 attach の例外として ro なら無害の可能性はあるが、
  画面の無限再帰 (自画面を自画面に映す) の挙動が未検証。需要が出たら検証して再検討
- **Ctrl-Z 等の追加キー intercept** (tmux 流 2 回打ち): DR-0017 案 A4 却下と同根。脱出
  手段は既存の Ctrl-A prefix に集約済みで、これ以上の入力介入は透過原則に反する。
  完全透過が欲しい場合の `HYOUI_DETACH_PREFIX=none` opt-out も既存
- **`hyoui detach` の引数なし = others**: 「中から打ったら自分以外」は shell からは便利
  だが、detach の主ユースケース (TUI 脱出 / 外からの引き剥がし) では all が直感的。
  others は `--target=others` で明示

## Consequences

- env が 1 個増える (`HYOUI_SESSION_ID`)。DR-0018 と同枠の透過例外として記録
- `Detach{Others/All}` の完成により protocol の `DetachTargetPartial` エラーが消える
- 将来の client スコープ操作 (leader 譲渡 / rw⇄ro 降格) は本 DR の射程外 —
  client addressing の設計が必要なため、multi-client 運用の実需が出てから別 DR
- `set` の key 拡張 (`until` / `timeout` / `idle-timeout` / `scrollback-rows` の runtime
  変更) も構想として確認済み (kawaz 2026-06-12)、実装時期は任意 — DR-0019 Update の
  汎用 key=value 構造にそのまま載る

## 関連

- [[DR-0018]] — env 注入の透過例外先例
- [[DR-0019]] — `hyoui set` / 汎用 key=value
- [[DR-0006]] / [[DR-0004]] — detach / exclusive の原典と予約
- docs/issue/2026-06-12-feature-attach-exclusive-detach-others.md — §4 で統合実装
