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

a の理由: `run` は「起動」であって「復帰意思の表明」ではない。子の self-stop を尊重する
のが [DR-0019](docs/decisions/DR-0019-run-option-cleanup-and-suspend-policy-placement.md) §3
の意図で、[DR-0029](docs/decisions/DR-0029-attach-is-a-viewport-ctrl-z-guard.md) §5 の意図
(人間が再 attach した時の UX) とも矛盾しない。c は明示 attach の UX まで変わり影響が広い。

#### 背景説明 (基本省略、詳細を求められたら補充)

`notify_default_does_not_resume_self_stopped_child` の CI 失敗 (macOS 7/7、ローカル 6/6 再現)
の真因が DR-0019 と DR-0029 の規定衝突だった。`hyoui run` は [DR-0015](docs/decisions/DR-0015-run-as-fork-plus-attach.md)
で「fork daemon + attach client」の合成なので、run した瞬間に attach 経路が発火して子を起こす。
daemon は notify を守るが同居 client が起こすため、外から見た挙動は auto-resume と区別できない。
DR-0029 は自身を「DR-0019 の配置は不変」と書いているが、`run` 経路では観測可能な挙動が変わっている。

参照: [main.rs:820-823](crates/hyoui-cli/src/main.rs)、[config/mod.rs:190](crates/hyoui/src/config/mod.rs)、
[jobcontrol_auto_resume.rs:77](crates/hyoui-cli/tests/jobcontrol_auto_resume.rs)

## 確認待ち

(現在なし)
