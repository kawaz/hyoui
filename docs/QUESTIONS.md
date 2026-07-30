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

### 👺DR32-Q1: DR-0032 (子 suspend 動作の統合 enum + action menu) のレビュー

- [ ] a: [DR-0032](decisions/DR-0032-child-suspend-unified-enum-and-action-menu.md) を承認 (Draft → Active 化して実装へ)
- [ ] b: 修正指示あり (チャットで)

kawaz 骨子 (r92 m18-24) を全部織り込み済み。起草 worker の判断による追加 4 点だけ注意して見てほしい:
旧 bool 2 key は silent 無視でなく**起動エラー + migration hint** / 終了系 SIGINT・SIGHUP に
**SIGCONT を併送** (stopped には pending になるだけで silent no-op になるため) / グループ名は
「継続系・終了系」/ 統合 enum は config 層に閉じ CLI・protocol 不変。
統括追補: `ctrlz_x1_action` 命名、select_on_demand のプロンプト状態 (timeout なし・他キーで
キャンセルは提案値)。

## 確認待ち

### 👺WP-C1: 文字幅パラメータ (v0.9.30) の確認 — 本命の要望分

- [ ] a: `?ambw=full` で ambiguous 文字 (① ★ ⚠ 等) が全角幅になることを確認 (既定 half = 現状)
- [ ] b: `?unicode=6` で旧 (v0.9.25 以前) の幅挙動に戻ることを確認 (既定 11)

補足: 前回誤解して入れた `?fontsize= bg= fg= scrollback= lineheight= fontfamily=` もそのまま使えます。

### 👺RS-C1: embed リサイズ修正 (v0.9.30) の実機確認

- [ ] a: ccmsg webui 等の iframe 縮小で表示が追従し、折り返しが出ない (追従前は横スクロール) ことを確認

真因は WS が leader を持つ間 resize POST が拒否されつつ 204 偽成功を返していたこと。
resize を WS 経由に変更 + 偽成功根絶 + PTY 成功後にのみ再レイアウト。

### 👺SV-C1: 自動起動の再起動実機確認 (kawaz にしか不可)

- [ ] a: 次回 PC 再起動後に web (https://hyoui.kawaz-mbp16-20211217.kawaz.jp) が自動で生きていることを確認

`hyoui web service register` (v0.9.29、DR-0031) で brew バイナリを launchd 登録済み。
KeepAlive の kill→復帰と API 200 は AI 実機確認済み、RunAtLoad の実再起動だけが未検証。

