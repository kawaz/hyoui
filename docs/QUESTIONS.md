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

(現在なし)

## 確認待ち

### 👺CZ-C1: Ctrl+Z ガード修正 (v0.9.27) の実機確認

- [ ] a: claude を hyoui 直起動 → attach 中に ^Z **単発** → 子に届かず、500ms 後に detach する (claude は走り続け、再 attach できる)
- [ ] b: ^Z **2 連打** → claude に 1 発届く (suspend メッセージ 1 回 → auto-resume)、detach しない

真因は「端末が Ctrl+Z を CSI-u sequence で送るのにガードが 0x1a しか見ていない」。
kitty CSI-u / modifyOtherKeys / 0x1a の 3 符号化対応 + decode/policy 層分離で修正。
brew v0.9.27 反映済み (現行 v0.9.29 にも収録)。詳細: [issue](issue/2026-07-29-bug-ctrlz-guard-bypassed-by-keyboard-protocol.md)

### 👺WP-C1: web 表示パラメータ (v0.9.29) の使用感確認

- [ ] a: `?fontsize=` `bg=` `fg=` `scrollback=` `lineheight=` `fontfamily=` を試して要望どおりか確認 (embed でも有効。例: `/sessions/<id>?embed=1&fontsize=16&bg=000`)

### 👺SV-C1: 自動起動の再起動実機確認 (kawaz にしか不可)

- [ ] a: 次回 PC 再起動後に web (https://hyoui.kawaz-mbp16-20211217.kawaz.jp) が自動で生きていることを確認

`hyoui web service register` (v0.9.29、DR-0031) で brew バイナリを launchd 登録済み。
KeepAlive の kill→復帰と API 200 は AI 実機確認済み、RunAtLoad の実再起動だけが未検証。

