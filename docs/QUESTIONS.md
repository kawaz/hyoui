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

### 👺RESUME-Q1: `hyoui run` の子が self-stop した時、attach client が起こしてよいか

- [ ] a (推奨): `run` が内部生成する attach では resume を適用しない (明示 `hyoui attach` でのみ起こす)
- [ ] b: test を現状に合わせる (default が実質 auto-resume になったと認める)
- [ ] c: `resume_on_reattach` の default を `false` にする

`run` は「起動」であって「復帰意思の表明」ではない ([DR-0019](docs/decisions/DR-0019-run-option-cleanup-and-suspend-policy-placement.md) §3
の「勝手に起こさない」と [DR-0029](docs/decisions/DR-0029-attach-is-a-viewport-ctrl-z-guard.md) §5 の衝突)。
c は明示 attach の UX まで変わる。

## 確認待ち

(現在なし)
