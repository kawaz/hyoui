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

### 👺ECO-Q1: `ignored-tests` job の `continue-on-error: true` を外すか

外部レビュー H-1 の提案。実測 (直近 15 run) では macOS は 08-21 以降 8 run 連続 green、ubuntu は毎回違う 1〜2 本 (daemon shutdown 系 / attach 系) が落ちる = 負荷依存の不安定で、レビューが言う「固定 2 本の恒常 fail」は現状と合わない。

- [ ] a: 外さない (統括推し)。外すと ubuntu の負荷依存 fail で main が常時 red になり、それ自体が別のノイズになる。代わりに issue `2026-07-26-bug-ignored-tests-job-permanently-red` の集計を現状 (macOS green / ubuntu は毎回違うテスト) に更新し、ubuntu 側の真因調査を継続する
- [ ] b: 外す。red を見えるようにして、落ちる各テストを `#[ignore = "<理由 + issue>"]` で明示 skip に倒しながら潰す
- [ ] c: 外さないが、ubuntu job だけ retry (`nick-fields/retry` 等) を入れて 1 回の負荷依存 fail を吸収する


### 👺SVC-Q1: service verb 群の階層 (DR-0034 OQ-A)

[DR-0034](decisions/DR-0034-service-multi-unit-and-stable-unstable-ha.md) OQ-A。常駐 unit の kind は現時点で web gateway のみ (record / redaction / screen watch は daemon 内、PTY session は `run` が unit を決める。将来候補は「login 時に決まった session を起こす」だが issue も DR も無い)。

- [ ] a: `hyoui web service <verb>` (統括推し)。`add` の option が `hyoui web` の引数の写しなので、階層が「何の unit を足すか」を名前で示す。kind ごとに option 集合が閉じる
- [ ] b: `hyoui service <verb>`。kind が 1 種類のうちは階層を 1 段減らす。kind が増えたら `--kind` で分岐 (option 集合が flag の値で変わるので help / completion で説明しづらい)
- [ ] c: `hyoui service <kind> add <name>` / `service add <kind> <name>`。kind を階層に出しつつ top-level は `service` 1 つ

### 👺SVC-Q2: reference 体系からの乖離を認めるか (DR-0034 OQ-B)

原指示は「reference `cli-daemon-subcommands` に沿った作り」だが、DR-0034 は **監督者 (`supervise`) を置かない**判断をしている。launchd / systemd が「落ちたら上げる / login で上げる」を担うので、監督者は OS 機能の再発明になるため。帰結として reference の `daemon` / `service` 2 系統が `service` 1 系統に畳まれ、`register` / `unregister` は `add` / `remove` に吸収される。

- [ ] a: 乖離を認める (統括推し)。unit 1 つ = launchd job 1 つ、定義ファイルが正本
- [ ] b: reference どおり llm-gateway 型にする。`daemon supervise` 1 つを OS に載せ、`daemon add` は登録簿に書くだけ、`service register` が監督者を載せる 2 段。監督者用の socket / protocol / backoff / ログ集約が hyoui 側に必要になる

### 👺WEB-Q1: web 境界の version 方式

グランドデザイン [research/2026-09-15-web-protocol-and-passkey-grand-design.md](research/2026-09-15-web-protocol-and-passkey-grand-design.md) §3。assets は gateway 自身が配るので browser と gateway は平常時同ビルド、ずれるのは restart / brew upgrade / canddy fallback で裏の unit が変わった時。

- [ ] a: 世代番号 1 つ (`WEB_PROTOCOL_VERSION`、統括推し)。WS の hello frame + `X-Hyoui-Web-Protocol` ヘッダ + `/version` で伝え、不一致なら画面端に帯 + 再読み込みボタン (自動リロードなし、既存 WS は切らない)。build_id は情報表示のみ
- [ ] b: daemon 境界と同型の cap 方式
- [ ] c: 併用

### 👺WEB-Q2: ccmsg webui の iframe 内での認証経路

同 §4.3。ccmsg の WebAuthn 検証は iframe 内 (`topOrigin` あり) を拒否する。ccmsg と hyoui は別 origin だが同一 site (`*.kawaz.jp`)。

- [ ] a: top-level で hyoui にログイン (popup) → 同一 site cookie が iframe 内にも乗る → popup から BroadcastChannel で通知 (統括推し。ccmsg 側変更ゼロ。**同一 site iframe の cookie 送信は実機未検証**、崩れたら c へ)
- [ ] b: ccmsg が短命 token を発行して iframe に渡す (hyoui が ccmsg を IdP として信頼、3 リポに契約が増える)
- [ ] c: iframe 内で WebAuthn を走らせる (ccmsg-webui に `allow="publickey-credentials-get"`、hyoui に topOrigin allowlist)

### 👺WEB-Q3: passkey 登録の bootstrap

同 §4.2。localhost 限定は canddy 経由も 127.0.0.1 発なので不成立。

- [ ] a: CLI 発行の招待 URL (`#register=<jwt>`、10 分) + 6 桁コード (ccmsg 同型、統括推し)
- [ ] b: 初回だけ無認証で登録できる (TOFU)

### 👺WEB-Q4: 認証セッションの形

同 §4.5。

- [ ] a: httpOnly cookie 1 本 (`Secure; SameSite=Strict`、sliding 30 日、CLI で失効。統括推し: 素の JS に tab-share を持ち込まない)
- [ ] b: ccmsg 型 (access = メモリ + WS subprotocol、refresh = cookie、rotate + 再利用検知)

### 👺WEB-Q5: 認可の軸

同 §4.6。

- [ ] a: credential 単位 `rw` / `ro` (`hyoui web passkey add --ro`) を daemon の attach mode に写す。既定 rw (統括推し。スマホ等「観測だけの端末」を既存 mode で表す)
- [ ] b: 認可を持たない (登録済みは全員 rw)

### 👺WEB-Q6: WebAuthn の実装

同 §4.8。

- [ ] a: `webauthn-rs` crate (依存は hyoui-web に閉じる。統括推し。attestation none / UV required が crate 設定で表せなければ b)
- [ ] b: ccmsg の自前実装 (TS 550 行) を Rust に移植 (ccmsg 側のテスト負債も引き継ぐ)

### 👺WEB-Q7: 認証の既定を `passkey` に切り替える時期

同 §6。W2 (実装) は `[web].auth = "none"` 既定で出し、W3 で kawaz が stable に登録して config で切替。

- [ ] a: W3 の実運用を通してから W4 で既定を `passkey` に (= major bump、統括推し)
- [ ] b: W2 の時点で既定 `passkey`

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
