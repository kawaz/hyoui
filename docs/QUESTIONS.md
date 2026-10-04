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
- **選択肢・確認項目は `- [ ] a: …` 形式（チェックボックス + ラベル）で書く**。Q / C で記法を分けない。回答は「チェックを付ける」でも「XX-Q1a」と言葉で返すでも通る（複数まとめてチェックし「チェックしたよ」の一言で済ませる運用を想定）

</details>

## 裁定待ち

### 👺WR-Q2: 合成画面から出力する時、セル以外の端末状態をどう届けるか

[DR-0040](decisions/DR-0040-daemon-own-cell-model-and-layer-composition.md) 決定 5 で素通しをやめると、DECSET (マウス / bracketed paste / kitty keyboard / application cursor) と BEL / OSC 52 / title が client に届かなくなる。[DR-0039](decisions/DR-0039-webui-terminal-app-rework.md) のマウス制御は xterm.js に DECSET が届く前提。統括推しは a (tmux と同じ。外から見えるのは今と同じ正しい TTY 出力のまま)。

- [ ] a: crate がモードを追跡して出力に再発行し、BEL / OSC 52 / title はイベントとして転送
- [ ] b: 別案 (自由記述)

### 👺WR-Q3: web から作る session をログイン shell + 最小 env で起動する手段

[DR-0039](decisions/DR-0039-webui-terminal-app-rework.md) 決定 1 は「既存の `hyoui run --detached` を呼ぶだけ、新 CLI 不要」と「argv[0] を `-zsh` 形式、env は最小から」の両方を持つが、`hyoui run` には argv0 指定も env を空にする option も無い (`hyoui run --help` で確認)。統括推しは a (`hyoui run` に option を足すのは CLI の拡張であって新しい起動経路ではない)。

- [ ] a: `hyoui run` に `--login` (argv0 を `-<shell>` に、env を最小に) 相当の option を足す
- [ ] b: gateway が自前で組み立てて daemon を起動する (`hyoui run` を経由しない)

### 👺WR-Q4: タブグループ / タブ / pane の構造の保存先と範囲

[DR-0039](decisions/DR-0039-webui-terminal-app-rework.md) 決定 5 は「構造の正本は gateway 1 か所」までで、stable / unstable の 2 unit で共有するか、gateway の再起動をまたいで残すかが未定。統括推しは a (再起動で消えると「家の Mac のタブ構成を出先で開く」が成り立たない。HA の 2 unit で構造が違うと切り替わった時に別物に見える)。

- [ ] a: file に保存し、2 unit で同じ file を共有する
- [ ] b: unit ごとに file
- [ ] c: メモリだけ (再起動で消える)

### 👺WR-Q5: DR-0005 の「範囲外」(window / pane UI、session グループ) との関係

DR-0005 は window / pane UI と session グループを範囲外にしているが、[DR-0039](decisions/DR-0039-webui-terminal-app-rework.md) はタブグループ / タブ / pane を gateway と browser に持たせる。統括推しは a (DR-0005 が避けたのは daemon / TUI 側を multiplexer にすることで、web の表示層が並べ方を持つのは別物。1 session = 1 daemon と TUI へのキー割り当て無しは保たれる)。

- [ ] a: DR-0005 の範囲外は daemon / TUI の話と明記し直し、web の表示層は対象外と注記する
- [ ] b: DR-0005 を改訂して範囲を広げる

### 👺NB-Q1: DR-0037 (daemon イベントループ非同期化) の runtime

[DR-0037](decisions/DR-0037-daemon-nonblocking-event-loop.md) 「runtime の選択肢」節。統括推しは a (DR-0025 の単一 thread 同期 loop をそのまま nonblocking 化するだけで不変条件を満たせ、依存を増やさない。fd は最大 70 本程度で poll(2) の O(n) は問題にならない)。

- [ ] a: 現行 poll + self-pipe の延長 (全 fd nonblocking)
- [ ] b: mio (kqueue / epoll 抽象)
- [ ] c: tokio

### 👺NB-Q2: client 送信の writer thread

統括推しは a (thread の生存管理と join が無くなり、backpressure が loop 内の純粋 state になる。v0.9.55 の Drop 修正はそれまでの暫定上限)。b は変更範囲が小さいが thread と join 起因の穴が残る。

- [ ] a: writer thread を廃止し loop 内 nonblocking write + POLLOUT
- [ ] b: writer thread を残し、Drop は join せず detach + shutdown

### 👺NB-Q3: 起動後の daemon ログ (fd 2) の出力先

現状は起動元 CLI の stderr (tty / pipe) を継承したままで、読まれない pipe だと eprintln が戻らない (`$(hyoui run --detached ... 2>&1)` が返ってこない実害あり)。統括推しは a (固まった時の原因記録 = watchdog ログの置き場が要る)。

- [ ] a: state dir 配下の session ごとのログファイル
- [ ] b: /dev/null

### 👺NB-Q4: 固まった daemon の検出をどこまで入れるか

DR-0037 「固まった daemon の検出」節の 3 層。統括推しは c (1 は list-prune で実装中、2 は原因究明に必須、3 は有界な占有の可視化で安い)。

- [ ] a: 1 (CLI 側の stale / hung / live 分類) のみ
- [ ] b: 1 + 2 (daemon 内 watchdog がループ停滞をログ)
- [ ] c: 1 + 2 + 3 (status に直近 1 周の最大所要時間と最終周回からの経過も載せる)

### 👺NB-Q5: 段階 1 (client 受信の nonblocking 化) を Q1 の裁定前に着手してよいか

1 client が frame を途中まで送るだけで daemon が止まる経路 (実機再現済み) の修正で、a / b どちらの runtime でも同じ形 (nonblocking fd + 増分 decoder)。統括推しは a。

- [ ] a: 着手してよい
- [ ] b: Q1 裁定まで待つ

### 👺NB-Q6: 「connect はできるが応答が無い」daemon の表示名

DR-0037 「固まった daemon の検出」節。観測できるのは「期限内に応答なし」までで、loop 停止 / backlog 飽和 / handshake 遅延 / 接続直後 crash は区別できない。統括推しは a (観測事実の名前にし、「止まっている可能性が高い」は help に推定として書く)。b は利用者の語彙に近いが原因を断定する名前になる。

- [ ] a: `no-response`
- [ ] b: `hung` (推定であることを help に明記)

## 確認待ち

### 👺DR32-C1: child action menu の実機確認 (v0.9.39 以降)

- [ ] a: `~/.config/hyoui/config.toml` に `[session]` `on_child_suspend = "show_child_action_menu"` を書き、attach 中に ^Z×2 で子を止めると menu が出て各キー (d/z = 脱出、c・Esc/i/h/k = 子への操作、Esc = 起こして戻る) が効く
- [ ] b: `[attach]` `ctrlz_x1_action = "select_on_demand"` で ^Z 単発 → 1 行プロンプト → ^Z/^C/Esc の 3 択が効く
- [ ] c: 子を止めた状態で detach → 再 attach しても menu キーが効く

### 👺LINK-C1: ターミナル内リンク (v0.9.40 以降) の実機確認

- [ ] a: デスクトップで Claude Code 応答内の markdown リンクをクリック → 新規タブで開く (確認ダイアログなし)
- [ ] b: 素の URL テキストもクリックで開く
- [ ] c: iPad: リンク tap → 開き、ソフトウェアキーボードが閉じる
- [ ] d: iPad: nvim 等 (mouse 有効 TUI) で focus 済み tap → カーソルがタップ位置へジャンプしない
- [ ] e: iPad: focus 済み tap でキーボードが閉じる / パネル open 中の tap は close のみ
- [ ] f: popup ブロック環境でリンクを開くと URL + コピーボタンのパネルが出る (Esc / × で閉じる)

### 👺HANG-C1: v0.9.55 で stopped client を抱えた daemon が固まらないことの実機確認

- [ ] a: 新版 (0.9.55 以降) で `hyoui run --detached -- <長く出力するコマンド>` を起動し `hyoui attach` して ^Z で attach client を止め、そのまま子が大量出力しても `hyoui list` で当該セッションが live のまま応答する (stale にならない)
- [ ] b: その状態で子を終了させると daemon が exit し、`ps` に zombie が残らない
- [ ] c: `hyoui web service restart` で gateway が 0.9.55 になっている (statusline / `hyoui web service status` の版で確認)
