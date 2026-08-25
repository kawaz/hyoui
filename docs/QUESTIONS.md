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

### 👺DR32-C1: DR-0032 実装 (v0.9.32) の実機確認

- [ ] a: `~/.config/hyoui/config.toml` に `[session]` `on_child_suspend = "show_child_action_menu"` を書き、attach 中に ^Z×2 等で子を止めると menu (脱出: d/z、子への操作: c・Esc/i/h/k) が出て各操作が効く。Esc = 起こして戻る、それ以外のキーは無反応
- [ ] b: `[attach]` `ctrlz_x1_action = "select_on_demand"` で、^Z 単発 → 1 行プロンプト → ^Z/^C/Esc の 3 択が効く (他キーは無反応)
- [ ] c (v0.9.39): **unattended 中に子が止まった後で attach** しても menu キーが効く
  (= 子を止めた状態で detach → 再 attach、または attach していない間に子が止まる)

m41-43 の裁定 (閉じる廃止 / Esc=resume / UX 視点の 2 群) は v0.9.32 で反映済み。

**確認は v0.9.39 以降で** (`brew upgrade hyoui`)。v0.9.38 以前には「handshake 時点で子が
停止していると、menu が画面に出ているのに menu キーが効かず子への入力になる」bug があった
(= 初回 attach redraw を client が「子が resume した証拠」と誤認して menu の focus を
閉じていた)。項目 c はその経路の確認。項目 a の「attach 中に ^Z×2 で止める」順序は
別経路 (STOP_NOTIFY) なので v0.9.38 以前でも動いていた。

### 👺LINK-C1: ターミナル内リンク (v0.9.40) の実機確認

**前提** (どちらか欠けるとリンクは開けない。2026-08-25 に統括が実施済み):
1. `brew upgrade kawaz/tap/hyoui` で hyoui 本体を v0.9.40 以降にする。**web の assets は
   バイナリに埋め込まれている**ため、古いバイナリのままだと古い session.js が配信される
   (実際に v0.9.35 のままで `linkHandler` が無く、xterm 既定の `confirm()` が呼ばれて
   `Ignored call to 'confirm()'. The document is sandboxed` になった)
2. web gateway を再起動する (launchd 管理なので pid を kill すれば KeepAlive が復帰させる。
   `hyoui web service status` で新 pid を確認)
3. ブラウザをリロードする (ccmsg 経由なら iframe の `allow-popups` を読み込むためにも必要。
   ccmsg v0.112.1 以降)

検証コマンド: `curl -s http://127.0.0.1:43690/assets/session.js | grep -c 'linkHandler'`
が 1 以上なら新しい assets が配信されている。

- [ ] a: デスクトップで Claude Code の応答内 markdown リンクをクリック → 新規タブで開く
  (確認ダイアログは出ない。開いた先が正常に表示・動作する)
- [ ] b: 素の URL テキスト (`https://...` と書かれただけの文字列) もクリックで開く
- [ ] c: **iPad**: リンクを tap → 開く。その後ソフトウェアキーボードが閉じる
- [ ] d: **iPad**: nvim 等 (mouse 有効な TUI) を開いた状態で focus 済み tap →
  **カーソルがタップ位置へジャンプしない** (= 従来どおり閉じ操作だけ)
- [ ] e: **iPad**: LT-C1 b/c の回帰確認 — focus 済み tap でキーボードが閉じる /
  パネル open 中の tap は常に close のみ
- [ ] f: popup がブロックされる環境 (iOS Safari のポップアップブロック on 等) で
  リンクを開くと、URL とコピーボタンのパネルが出る (Esc / × で閉じられる)

**今回開けるようにならないもの** (仕様、確認不要):
- `file://` / `vscode://` (status line に出るもの) — xterm.js の公開 API が
  「http/https のみ」か「`javascript:` 含む全 scheme」の二択しかなく、後者は危険なため
  http/https に限定した。要望があれば別途対応する
- 再接続前から画面にあったリンク — daemon が OSC 8 を保持しないため
  ([docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md](issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md))

### 👺SV-C1: 自動起動の再起動実機確認 (kawaz にしか不可)

- [ ] a: 次回 PC 再起動後に web (https://hyoui.kawaz-mbp16-20211217.kawaz.jp) が自動で生きていることを確認

`hyoui web service register` (v0.9.29、DR-0031) で brew バイナリを launchd 登録済み。
KeepAlive の kill→復帰と API 200 は AI 実機確認済み、RunAtLoad の実再起動だけが未検証。


