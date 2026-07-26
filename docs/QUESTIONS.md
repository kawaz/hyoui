# 裁定・確認待ち一覧 (ユーザ用)

## 運用規約

<details>
<summary>ゼロコンテキストエージェント向け（本セクションは消さない）</summary>

- 裁定/確認待ち項目を 1項目=1ラベル=1セクション で記載
- ラベル形式: XX-Q1（バッチやセッション内で一意な短プレフィクス、Qn単独の使い回し禁止、長期一意性は不要)
- 依頼形式: 「👺XX-Q1 の裁定お願いします」（参照用途ではラベルに👺を付けない。誤陽性がユーザのハイライト/アラームを汚す）
- チャット提示と同一ターンで本ファイルに記録 + path 指定 commit (push はリリース窓に同乗)
- 裁定が下りたら該当セクションを即削除し、内容は正規の記録先 (DR / issue / journal / close_reason) へ反映。本ファイルは常に「現在待ち」だけを持つ
- 参照は[]()で提示（リポ内は相対、リポ外はフルパス）
- 初版質問/依頼は長文で書かない（ユーザが説明を求めらたら本ファイルに説明を追加し、チャットで👺ラベルで再依頼）
- **選択肢・確認項目は `- [ ] a: …` 形式（チェックボックス + ラベル）で書く**。
  Q / C で記法を分けない。回答は「チェックを付ける」でも「XX-Q1a」と言葉で返すでも通る
  （複数まとめてチェックし「チェックしたよ」の一言で済ませる運用を想定）

</details>

## 裁定待ち

### 👺RS-Q1: `hyoui run` の子が self-stop した時、attach client が起こしてよいか

- [ ] a (推奨): `run` が内部生成する attach では resume を適用しない (明示 `hyoui attach` でのみ起こす)
- [ ] b: test を現状に合わせる (default が実質 auto-resume になったと認める)
- [ ] c: `resume_on_reattach` の default を `false` にする

`run` は「起動」であって「復帰意思の表明」ではない ([DR-0019](docs/decisions/DR-0019-run-option-cleanup-and-suspend-policy-placement.md) §3
の「勝手に起こさない」と [DR-0029](docs/decisions/DR-0029-attach-is-a-viewport-ctrl-z-guard.md) §5 の衝突)。
c は明示 attach の UX まで変わる。

### 👺CLI-Q1: `attach --exclusive` / `--detach-others` の扱い

- [ ] a (推奨): parse 段でエラー化し help/completion から消す (需要が出たら再実装)
- [ ] b: daemon 側の占有判定 / 奪取処理を実装完成させる
- [ ] c: 現状維持 (中途半端に通る) + DR-0019 の記述を実態に合わせる

[DR-0019](docs/decisions/DR-0019-run-option-cleanup-and-suspend-policy-placement.md) は parse 段エラー化を決定したが実際は daemon まで到達しており、
この経路の中途半端さが `kill --no-terminate` の全 client 蹴りバグの温床になった。
[DR-0020](docs/decisions/DR-0020-attach-exclusive-and-detach-others.md) §4 の正式機能を消す判断なので裁定要。

### 👺CLI-Q2: 必ず失敗する `screen snapshot --include=style/scrollback` の扱い

- [ ] a (推奨): parse 段で明示エラー化 (未実装の旨を案内)
- [ ] b: 実装されるまで help/completion から隠す (parse は受理)
- [ ] c: 現状維持 (daemon エラーで返る)

`--rect` (無視されるだけで無害、help に注記済み) は現状維持とし、必ず失敗する値のみが対象。

## 確認待ち

(現在なし)
