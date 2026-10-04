# DR-0039: web UI をブラウザ上のターミナルアプリとして作り直す

- Status: ⬜ 未実装 (2026-10-04)。決定 1〜13 は裁定済みで着手してよい。「未決」節の項目は本 DR では確定させない
- Date: 2026-10-04
- Related: DR-0005 / DR-0014 (透過原則。本 DR の介入はすべて UI と gateway の層に閉じる), DR-0015 (`hyoui run --detached` = 新規セッション作成が呼ぶ既存経路), DR-0024 (子 PTY の env scrub。新規セッション作成では論点が生じない理由を決定 1 に書く), DR-0027 (web gateway。「認証は scope 外」の前提を DR-0036 が置き換え、本 DR が守る対象をさらに広げる), DR-0029 / DR-0030 (attach は覗き窓。pane を閉じる = detach の根拠), DR-0033 (leader 優先と `leader.request`。サイズ違いと昇格アクションの根拠), DR-0035 (web 契約と世代 version。新契約はこの規則の上に作る), DR-0036 (passkey 認証。守る対象の前提を本 DR が更新する), DR-0013 (screen state 正本。daemon 側の自前セルモデルは別 track)
- Origin: `docs/issue/2026-10-04-design-webui-terminal-app-rework.md` (kawaz との議論で合意 2026-10-04)。事実は `docs/findings/2026-10-03-web-api-protocol-inventory.md`、`docs/findings/2026-10-03-web-component-tree.md`、`docs/findings/2026-10-04-daemon-osc-coverage.md`、`docs/research/2026-10-04-web-terminal-renderer-survey.md`、PoC 3 本 (`docs/research/poc/2026-10-04-softkey-iframe-focus/`、`2026-10-04-xterm-mouse-mode-toggle/`、`2026-10-04-terminal-text-selection-layer/`)

## Context

### 現状

- web から新しい session を作る経路が無い。`hyoui run` の起点は ghostty / iTerm2 等のターミナルアプリだけで、web は既存 session を一覧して覗くことしかできない
- front は 2 ページ (`index.html` 一覧 / `session.html` ターミナル) で、`session.js` (2018 行) は 1 つのクロージャに約 14 の責務を抱え、区画単位で切り出せない。モジュールは classic script と `window` グローバルで受け渡す (findings component-tree §1, §6)
- 子への入力が 2 経路ある。xterm への直打ちは WS binary、入力パネル / keypad は WS 接続中でも HTTP `POST /input` で、経路間の到着順は保証されない (findings component-tree §7.6)
- daemon の control message は gateway でほぼ全部捨てられる。子の終了・停止は WS に乗らず、終了時は理由なしの close (1005) で、browser は再接続 backoff に入る。上りの入力が ro / lock 未保持で拒否されても log に出るだけ (findings api-protocol §3, §4)
- ブラウザ側は xterm.js 5.3.0 を vendor し、内部 API を 4 系統参照している (findings component-tree §1)
- daemon の vt100 0.16.2 はどの OSC も保持せず、hyoui は callback を使っていない。session 一覧の `cwd` は起動時 cwd で、子が `cd` した後の cwd を読む処理は無い (findings osc-coverage §1〜§3)

### 目的

**daemon が動いていれば、ブラウザからリモートでターミナルを開いてローカル作業ができるようにする。** web から session を作れれば、ターミナルアプリを開かずに作業を始められる。

使い心地の軸は iTerm / Ghostty のようなフルスクリーンのターミナルアプリに置く。ただし、全描画領域を文字セルで敷き詰める必要があるというターミナル特有の制限には囚われない (サイドバー、余白、カルーセル、選択用の層を UI の要素として持つ)。

ここで増やしたくないもの (目的と同格):

- **子から見た介入**。TUI 側にキー割り当てを足さず、子の stdin に hyoui 由来の escape を足さず、アプリのマウス追跡要求 (DECSET) に触れない。UI の機能はすべて TUI の手前の層で実現する (DR-0005 / DR-0029)
- **CLI と daemon の数え方**。新規作成のための CLI を作らず、1 session = 1 daemon を崩さない
- **入力の経路**。子への入力は WS 1 本で、予備経路を持たない (決定 9)
- **起動の設定**。プロファイルやコマンド限定の仕組みを持たない。設定は初期ディレクトリだけ (決定 1)
- **自前で持つ変換**。touch → mouse report の変換、vt100 の前段の自前 OSC parser は持たない (決定 7 / 決定 11)

### 用語

| 語 | 意味 |
|---|---|
| タブグループ | ターミナルアプリのウインドウ相当。タブを束ねる |
| タブ | pane を 1 つ以上並べる単位 |
| pane | 1 つの hyoui session を表示する枠。1 pane はちょうど 1 session に紐づき、1 session には複数 pane が紐づいてよい |
| 未アタッチ | どの pane にも紐づいていない session (CLI で起動した session、pane を閉じた session) |
| 構造 | タブグループ → タブ → pane のツリーと、pane と session の紐付け、未アタッチ一覧。全端末で共有する (決定 5) |
| 配置 | 分割の向き・比率、カルーセル、サイドバー開閉、pane の非表示、マウスモード。端末ごとに持つ (決定 5) |
| 端末 | ブラウザを動かしている機器。配置の記憶単位 (localStorage の単位) |
| アクション | web UI が invoke できる操作の単位 (決定 2) |

### 前提条件と、満たさない場合

| 前提 | 満たさない場合 |
|---|---|
| `hyoui run --detached` で起動した daemon は fork + setsid で gateway から独立する (DR-0015) | gateway の restart / upgrade で web から作った session が道連れになる。現時点で未検証なので実装時の検証項目にする (「実装時に確かめること」) |
| web 境界の `/api/*` と WS が DR-0036 の passkey 認証で守られている | 新規セッション作成が無認証の shell 起動口になる (決定 1 の「web 境界の前提の更新」) |
| xterm.js 6.0.0 で、アプリの DECSET を保ったまま UI 側で転送だけを止められる | マウスモードのオフ (決定 7) と、マウスの選択 (決定 8) が成立しない。headless Chromium / WebKit の全 224 ケースで成立を確認済み。iOS 実機は未確認 |
| 最上位 frame のボタンを `pointerdown` で `preventDefault()` すると、子 iframe の textarea / xterm の focus とソフトキーボードが維持される | ソフトキーツールを最上位 frame に 1 つ置く形 (決定 3) が崩れる。iPad Safari / iPad PWA と headless Chromium / WebKit で確認済み。iPhone Safari は未確認 |

## 介入判断 self-check (CLAUDE.md / DR-0014)

- **PTY / child への介入**: 新規セッション作成は既存の `hyoui run --detached` を呼ぶだけで、新しい起動経路を daemon に足さない。起動する shell の env は gateway のものを持ち込まないので、DR-0024 の scrub (= 親の Internal Context env の漏洩防止) の論点は生じない
- **透過原則**: マウスモードの切り替えは UI が PTY に転送するかどうかだけで、DECSET には触れない (決定 7)。OSC は観測するだけで子の出力に手を入れない (決定 11)。本物の選択と TUI の選択風ハイライトの区別は本物の側で作り、TUI の色は変えない (決定 8)。TTY への送信アクションは利用者の操作を bytes として送るだけで、hyoui 由来の escape を足さない
- **DR-0005 との関係**: タブ・pane・タブグループの構造は gateway と browser が持ち、daemon は知らない。1 session = 1 daemon (screen 型) は崩さず、prefix キー体系も足さない
- **kernel / PTY / shell の標準機能の再発明**: 起動するのは利用者のログイン shell で、cwd の引き継ぎは shell が出す OSC 7 か OS が持つ子の cwd を読む。カルーセルの吸着は CSS `scroll-snap`、選択とコピーは xterm 自身の選択か OS 標準の選択に任せる
- **新 protocol message / cap flag**: web 契約 (DR-0035) には構造の取得・変更・変更通知 (決定 5) と WS 制御 frame (決定 10) が加わる。daemon 境界に加わり得るのは OSC から取った session 属性を gateway に渡す field だけで、session 一覧 API は未アタッチの session の属性も返す (= gateway が WS で bytes を受けていない session がある) ため、正本である daemon (DR-0013) から渡す必要がある。形は実装時に決める
- **既存 DR の実装漏れ**: 新契約は DR-0035 の規則 (`contract.rs` の serde 型を正本、golden test、エラー形 `{error:{code,message}}`) の上に作る。現行実装と DR-0035 / DR-0036 の乖離 (session オブジェクトの `json!` 手書き、エラー形の穴 3 つ等) は findings api-protocol §7 に列挙されており、新契約を作る時の確認対象になる

## Decision

### 1. web から新規セッションを作る

**gateway が既存の `hyoui run --detached` を呼んで、新しい session の daemon を起動する。** 1 session = 1 daemon のまま (DR-0005 / DR-0015) で、新規作成のための CLI は作らない。既に daemon 化と socket 常設を担う経路があり、web 側はそれを呼ぶ立場に立てば足りるため。

**起動するのは、普通のターミナルアプリと同じく利用者のログイン shell である。** shell は passwd から引き、argv[0] を `-zsh` の形 (= ログイン shell の慣習) にする。env は gateway のものを引き継がず、ログイン時と同じ最小の env から始めて、残りは shell の rc に任せる。gateway の env を持ち込まないので、DR-0024 の scrub の論点は生じない。

**プロファイルやコマンド限定の仕組みは持たない。** 目的は普通のターミナルアプリと同じ起動で、何を走らせるかは利用者が shell の中で決める。

設定は初期ディレクトリだけで、作り方ごとに「元 pane の cwd を引き継ぐ」か「HOME」を選ぶ。既定は Ghostty / iTerm と同じにする:

| 作り方 | 既定 |
|---|---|
| 新規タブ | 元 pane の cwd を引き継ぐ |
| 新規分割 | 元 pane の cwd を引き継ぐ |
| 新規タブグループ | HOME |

「元 pane の cwd」は決定 11 で daemon が持つ現在 cwd (OSC 7、または OS から読む子の cwd) を使う。session 一覧の現行の `cwd` は起動時 cwd なので、引き継ぎの元データにはならない。

#### web 境界の前提の更新 (DR-0027 / DR-0036)

DR-0027 は「認証 / HTTPS は scope 外」を前提に置き、DR-0036 はそれを置き換えて「守る対象は session の画面内容と入力」とした。本決定で web 境界から利用者のログイン shell を起動できるようになるので、**web 境界に到達できることは、既存 session が 1 つも無くても利用者の uid でプロセスを起動できることを意味するようになる。** 既存 session への入力でも shell があれば任意のコマンドは打てたが、起動の可否が既存 session の有無に依らなくなる点が変わる。

新規セッション作成の経路は DR-0036 が守る範囲 (`/api/*` と WS) の内側に置き、認証を通った要求だけが session を作れる。DR-0036 の「守る対象」と DR-0027 の前提の記述は、この決定に合わせて読み替える。

### 2. アクション

**新規タブ・画面分割・leader 昇格などを web 専用のアクションとして形式化し、後からショートカットを割り当てられるようにする。** UI のボタンもショートカットも、アクションを invoke するだけ (1 操作 = 1 アクション) にする。hyoui の TUI 側にはキー割り当てを足さない — アクションは TUI の手前の層にあり、子の stdin を経由しないため (DR-0005 / DR-0029 の in-band escape ゼロ)。

**TTY への送信 (キー X を TTY にそのまま送る) もアクション / イベントとして今から形式化する。** ソフトキー (Esc / Tab / ^C / 矢印 等) はこのアクションを invoke するだけで、xterm への直打ちも同じ経路で WS に流れる。ボタンと直打ちが別経路だと、到着順と拒否時の扱いが 2 通りになるため (決定 9)。

**キーの既定の行き先は TTY である。** 割り当ての無いキーは全部 TTY へ送り、UI アクションのショートカットは割り当てたキーだけを横取りする。この形なら、後からショートカットを足しても作りが変わらない。ショートカットの割り当て自体は未決 (「未決」節)。

本 DR の各決定に出てくるアクション: 新規タブ / 新規分割 / 新規タブグループ (決定 1)、TTY への送信 (本決定)、pane を閉じる (= detach) / session の終了 / pane の非表示 (決定 4 / 決定 5)、分割 ⇄ カルーセルの切り替え / 前・次の pane (決定 5)、leader 昇格 (決定 6)、マウスモードのオン / オフ (決定 7)。

### 3. コンポーネントツリー (1 から作り直す)

現行の 2 ページと `session.js` の単一クロージャは引き継がず、次のツリーで作り直す。

```text
Window
  menubar
  split コンテナ
    session list (サイドバー、出し入れ可)
    タブコンテナ
      tabbar (ターミナル 1 つなら非表示など)
      ターミナルコンテナ (中で pane 分割)
        pane (複数) — 1 pane = 1 hyoui session
      ソフトキーツール (タッチデバイスで下部、出し入れ可)
```

**タブコンテナ・特定タブ・特定 pane は、それぞれ単独で別のブラウザタブに開ける。** そのため iframe の木にして、frame 間は postMessage で疎結合する。

**ソフトキーツールは最上位 frame に 1 つ置く。** ボタンは `pointerdown` で `preventDefault()` して、postMessage で pane へ送る。PoC (`docs/research/poc/2026-10-04-softkey-iframe-focus/`) で、この方式なら iPad Safari / iPad PWA で子 iframe の textarea / xterm.js の focus とソフトキーボードが維持されることを実機で確認した。素の `click` で送る方式は、キーは届くがソフトキーボードが閉じる (同 PoC、iPad 実機と headless Chromium / WebKit で一致)。

### 4. session list の構造と「閉じる = detach」

session list (サイドバー) は構造をそのまま見せる:

```text
タブグループ
  タブ
    pane (1 hyoui session に紐づく)
未アタッチ (どこにも属さない session)
```

**pane を閉じる = detach である。session は終了しない。** 閉じた pane の session は未アタッチ一覧に移る。未アタッチの session を選ぶと、既定のタブグループ等に新規タブとしてアタッチする。**session の終了は別のアクション**にする。attach は覗き窓であり、client の操作で子の寿命を動かさない (DR-0029 / DR-0030) ので、画面の枠を閉じる操作と子を終わらせる操作は分ける。

### 5. 構造は共有、配置は端末ごと

| 層 | 中身 | 置き場所 |
|---|---|---|
| 構造 (共有) | タブグループ → タブ → pane のツリー、pane と session の紐付け、未アタッチ一覧 | gateway |
| 配置 (端末ごと) | 分割の向き・比率、カルーセル、サイドバー開閉、pane の非表示、マウスモード (決定 7) | ブラウザの localStorage (= 端末単位)。必要になればウィンドウ単位を sessionStorage で上書きする |

- pane を別のタブ・別のタブグループへ移すのは構造の変更なので共有する。分割比率は配置なので共有しない
- **閉じる** (構造から外れ、全端末で未アタッチへ) と **非表示** (この端末の配置で見せないだけ) を分ける
- 他端末が構造に pane を足したら、手元の配置には既定で末尾に分割して足す。非表示で足すと、足されたことに気づけないため

**構造の正本は gateway 1 か所に置く。** 変更は操作単位 + 版番号で行い、他端末へは push で配る。web 契約 (DR-0035) に構造の取得・変更・変更通知が加わる。

#### カルーセル

タブ内の配置には、分割のほかに **カルーセル** (pane を 1 枚ずつ全面表示し、左右フリックで切り替える) を持つ。スマホではタブ内分割が現実的でないため。カルーセルは配置の層なので端末ごとに持ち、iPhone はカルーセル、Mac は分割、を同じタブで両立できる。

- 分割 ⇄ カルーセルの切り替えと、前 / 次の pane はアクション (決定 2)
- フリックと吸着は CSS `scroll-snap` に任せる
- フリックとターミナルのタッチ操作の衝突は、マウスモードの切り替え (決定 7) で解く

### 6. サイズ違い (1 session を大きさの違う複数 pane で見る)

pane はそれぞれ 1 つの attach として daemon に繋がるので、同じ session を見る pane の間で cols/rows が食い違いうる。

- **cols/rows は leader 優先** (DR-0033)
- **leader より大きい pane は余白を置く。余白は余白と分かる見た目にする。** ターミナル背景色と同じにすると、余白まで端末の領域に見えて cols/rows を誤認させるため
- **leader より小さい pane は cols/rows を変えず、その縦横比の矩形を pane に収まるよう丸ごと縮小する (content-fit)。** 実装は、収まる fontSize を計算して設定するのを第一候補にする。`transform: scale()` は xterm のマウス座標がずれるため採らない
- **非 leader の pane には leader 昇格のアクション** (ボタン + ショートカット) を置く。昇格すると cols/rows が変わり、TUI アプリが再描画する (DR-0033 決定 5 の既存挙動)

leader が去った時の次の leader と再レイアウトの体験は未決 (「未決」節)。

### 7. マウスモードの切り替え

**マウスモード (TUI へのマウスイベント転送) のオン / オフをアクションにする。** 手軽に切り替えられることを必須にする。PC でも転送が邪魔なことが多いため。

**切り替えるのは UI が PTY に転送するかどうかだけで、アプリのマウス追跡要求 (DECSET 1000 系) には触れない。** 子から見た状態を変えないので透過原則と整合する (DR-0014)。

- **既定はアプリの要求に従う。** ユーザがオフにしたら、その pane で明示的に遮断する
- **記憶単位は pane × 端末** (配置と同じ層、決定 5)
- **表示は 2 つの状態の組で出す**: 「アプリが要求しているか」と「UI が転送しているか」。見える状態は 要求なし / 要求あり・転送中 / 要求あり・遮断中 の 3 つ
- **ポインタの形でも示す**: 転送オン = 矢印、オフ = I ビーム (ターミナル領域の CSS `cursor` の切り替えだけ)

#### オフ時の振る舞い

実現方式は PoC (`docs/research/poc/2026-10-04-xterm-mouse-mode-toggle/`) の方式 (4) に従う。xterm に対して使うのは公開 option / API と DOM イベントだけで、私的 API と xterm の内部状態の書き換えは使わない。

- xterm の要素の外側で、capture phase で mouse / wheel を止める
- 左 mousedown は修飾キー付きで xterm に渡し直し、xterm 自身の強制選択経路で選択させる (Mac は Option + `macOptionClickForcesSelection`、他は Shift)
- Mac の `altClickMovesCursor` は本物の mouseup の altKey を見るので、合成した mousedown に続く mouseup は altKey を外して渡し直す (対処しないと遮断中に cursor 移動の矢印キー列が PTY に流れる、PoC で観測)
- wheel は normal buffer で `scrollLines`、alt screen で矢印キー列にする
- タッチは、横フリック = カルーセル配置ならカルーセル移動、縦フリック = スクロール。alt screen 中の縦フリックは矢印キーに変換する (xterm 系の慣習)
- 縦スワイプは自前で扱い、横は `scroll-snap` のカルーセルに任せる (xterm 6.0.0 は touch でスクロールしない)

PoC では、方式 (4) で DECSET が全 224 ケースで保持され、遮断中の report は 0 byte、キー入力はどの状態でも遮断されなかった (headless Chromium / WebKit、Mac と Linux 偽装)。

#### オン時のタッチ

**TUI に渡すのは tap だけで、スワイプはオン時もスクロールのまま。** xterm 6.0.0 は touch を扱わず、tap はブラウザの互換 mouse イベント経由で report になる。touch → mouse report の自前変換は持たない。

### 8. 選択

**選択の方式は入力デバイスで分ける。**

| 入力 | 方式 |
|---|---|
| マウス | xterm 自身の選択。マウスモードがオフの時は修飾キー付きで渡し直す (決定 7) |
| タッチ | 長押しで選択モードに入り、セル位置に揃えたクローン層の上で OS 標準の選択を使う。マウス転送のオン / オフによらない |

**クローン層はマウスには使わない。** 選択を始めると focus が端末から外れ、選択後に focus を戻すと選択が消えるため (PoC `docs/research/poc/2026-10-04-terminal-text-selection-layer/` の (A) 方式で観測)。

タッチの選択は、現行 `session.js` のテキスト層 (buffer を素のテキストにして `<pre>` で重ね、OS 標準の選択ハンドル・ルーペ・コピーメニューに任せる) を土台にして、ターミナルのセルと同じ位置・フォント・行高でその場に重ねる。選択範囲の調整とコピーは OS 標準に任せ、自前で持つのは閉じる操作だけにする。PoC の (A) 方式で、コピー結果が xterm 自身の選択と全データで一致し、層の位置がセル格子と一致する条件 (未書き込みセルを `getChars() === ''` で扱う、論理行ごとに 1 ブロック、送り幅を `letter-spacing` でセル幅に揃える、選択中は層を作り直さない) を確認した (headless Chromium / WebKit)。

#### 本物の選択と TUI のハイライトを見た目で区別する

TUI がマウスモードで自前の選択ハイライトを描くと、選択したつもりで ⌘C が何もコピーせず、前のクリップボードが貼られる。TUI のハイライトはただの反転セルで hyoui からは判定できず、透過原則でそちらの色は変えないので、**区別は本物の側で作る**:

- (a) 本物の選択の見た目を、TUI がよく使う反転と紛れない独自の色 + 縁取りにする (xterm の theme `selectionBackground` 等)
- (b) マウスモードをポインタの形で示す (決定 7)
- (c) コピーの結果を成功時も短く知らせる (「N 文字コピー」)。成功と空振りの両方が見えるようにする

#### ⌘C の空振り対策

マウスモードのオフで大半は防げるが、オン (既定でアプリの要求に従う) のままだと残る。

- (1) copy イベント時に xterm 側の選択が空なら「クリップボードは更新されていません」と短く通知し、マウスモードをオフにする操作を添える
- (2) TUI が自前の選択を確定して出す OSC 52 の書き込みを、ブラウザのクリップボードに反映する。ブラウザはユーザ操作中しかクリップボードへの書き込みを許さないので、**書き込みが通るかを確かめてから採否を決める** (決定 11 の OSC 52 の扱いとは別に、ブラウザ側の反映の可否が条件になる)

### 9. 入力経路は WS 1 本

入力 UI は 2 系統を両方持つ:

- **xterm への直打ち**: 1 文字ごとに TUI の反応を受ける (claude の `/` 補完等)
- **テキストエリアでまとめて送る**: IME の変換位置を TUI に縛られない

**2 系統は UI の違いだけで、プロトコル経路は WS 1 本にする。予備経路は持たない。** 経路が 2 本あると、経路間の到着順が保証されず、拒否されたときの扱いも経路ごとに分かれる (findings component-tree §7.6)。テキストエリアとソフトキーも TTY への送信アクション (決定 2) を通って WS に流れる。

### 10. WS 制御 frame の拡充

新しい契約に次の 3 つを入れる。pane の「プロセスが終了しました」表示等の前提になる。

| 通知 | 中身 |
|---|---|
| 子の終了 / 停止 / 再開 | daemon の `session.exit.notify` / `session.child.stopped.notify` を gateway で捨てずに写す。終了は exit code か signal を載せる |
| 入力が拒否されたこと | 理由付き (ro / lock 未保持 等)。現状は gateway の log に出るだけで browser に届かない |
| 接続を閉じた理由 | close code + reason。認証切れの `4401` (DR-0036 決定 5) は実装済み。子の終了・daemon 切断も区別できるようにする |

close code は DR-0035 の WS close 表と同じく RFC 6455 の private use 帯 (4000〜4999) から採り、表に足す。

元データについての事実: daemon の `session.exit.notify` は全 attached client に broadcast されるが、`session.child.stopped.notify` は leader にだけ送られ、再開に当たる daemon の通知は無い (`crates/hyoui/src/protocol/messages/session_lifecycle.rs`)。非 leader の pane への停止の伝え方と、再開の元データは実装時に決める。

### 11. OSC から取れるメタデータ

**daemon が子の出力から観測できる情報は取れるだけ取って、session の属性として持つ。** 観測のみで介入しないので透過原則と整合する。表示に使うかは UI 側で決める。変化は WS で通知し、セッション一覧 API でも返す。

| OSC | 内容 |
|---|---|
| 0 / 2 | title |
| 1 | icon name |
| 7 | cwd。決定 1 の「cwd を引き継ぐ」の元データ |
| 133 | shell integration のプロンプト / コマンド境界と終了コード |
| 9 / 777 | 通知 |
| 9;4 | 進捗 |

- **取り出しは vt100 の `Callbacks` で行い、vt100 の前段に自前 parser は置かない。** 0 / 1 / 2 は title・icon name の callback、52 は clipboard の callback、それ以外は `unhandled_osc` で全部拾える。追加の影響は `daemon/screen/state.rs` に閉じる (findings osc-coverage)
- `;` を含む title は vt100 で分割されて `unhandled_osc` に落ちるので、結合し直す
- resize は入力ログを新しい parser に replay するので callback が再発火する。title / cwd は最後の値で上書きなので無害で、通知 (9 / 777) は重複するので **replay 中は callback を無視する印を持つ**
- OSC 7 を出さない shell の cwd は、OS から子の cwd を読んで補う (macOS は libproc、Linux は `/proc/<pid>/cwd`)。OSC 7 と OS のどちらを正にするかは実装時に決める
- session 一覧の現行の `cwd` は起動時 cwd で、現在 cwd を OS から読む処理は無い (決定 1 の引き継ぎには現在 cwd が要る)

**OSC 52 (クリップボードの読み書き) は観測ではなく操作を伴うので、上の属性とは別の論点にする。少なくとも読み出しは既定で無効にする。**

### 12. ブラウザ側の描画は xterm.js 6.0.0

**ブラウザ側は xterm.js 6.0.0 を使う (5.3.0 から上げる)。** 外に出るのは TTY の raw bytes だけなので、daemon 側の画面モデルを替えてもブラウザの経路は変わらない。6.0.0 は ESM の配布物 (`lib/xterm.mjs`) を持ち、公開の `modes.mouseTrackingMode` と強制選択の option で決定 7 / 決定 8 が成り立つことを PoC で確認した。比較した候補と判断の軸は `docs/research/2026-10-04-web-terminal-renderer-survey.md`。

renderer は xterm 既定の DOM renderer を起点にする。WebGL は pane ごとに context を作り、多数 pane と context の上限・context-loss の報告が衝突するので、全 pane の既定にしない (同 research の深掘り C)。

上げる時の再検証項目 (同 research「xterm.js を続ける場合の移行コストと利得」):

- `.mjs` を JavaScript の MIME で配信する (現行の `content_type_for` は `application/octet-stream` を返す)
- 現行の内部 API 依存 (`_core.unicodeService._providers`、`_charSizeService.measure()`、`_syncTextArea()` / `_compositionHelper.isComposing`) は、削除できる補正は削除し、残すなら 6.0.0 の source と実測で保証する。新たな内部依存を根拠なく増やさない
- IME (変換位置と確定の重複・欠落) を独立した軸として回帰試験する

daemon 側の仮想スクリーンを自前のセルモデルにする件 (層の合成によるオーバーレイ、attach 出力を合成画面から作る) は別 track で、本 DR の範囲外 (`docs/issue/2026-10-04-design-daemon-own-cell-model.md`)。web の初期表示のモノクロは vt100 ではなく hyoui 側の原因で、自前セルモデルを待たずに解消済み (`docs/issue/archive/2026-10-04-screen-scrollback-ansi-drops-color.md`)。

### 13. 旧 API の扱い

**web の HTTP / WS 契約は作り直しで一新する。旧 API (`POST /input` 等) は残さない。** 旧 API が存在するのは、旧 UI と並行運用する切り替え期間だけで、切り替え完了で消す。

旧 route の削除は DR-0035 決定 3 の「kind / field の削除と意味変更」に当たるので、新契約で `WEB_PROTOCOL_VERSION` を上げる。

## 実装時に確かめること

本 DR の決定が依存していて、まだ観測していない事実:

| 項目 | 確かめ方 / 現状 |
|---|---|
| gateway の restart / upgrade で、web から作った session が道連れにならない | `--detached` の fork + setsid で独立するはず (未検証)。gateway を入れ替えて session の daemon が残ることを観測する |
| OSC 7 と OS から読む cwd のどちらを正にするか | 両方を取って突き合わせる。shell が起動した前景ジョブの cwd を取る pid の決め方は未調査 (findings osc-coverage §3) |
| `;` を含む title の落ち方、resize replay での callback 再発火 | どちらも vt100 のソース読解からの推論で、実行では未確認 (findings osc-coverage) |
| タッチ選択: 長押しで層を出して、そのまま OS 標準の選択が始まるか | iOS 実機 PoC で確かめる。間に合わなければ透明な層を常に重ねる案に倒す |
| タッチ選択: 全角・曖昧幅・絵文字でテキスト層がセルとずれないか | iOS 実機 PoC で確かめる (headless では一致を確認済み) |
| マウスモード: iOS Safari / iPad PWA での転送の遮断と選択 | PoC の iOS 列は未記入 |
| ソフトキー: iPhone Safari で focus とソフトキーボードが維持されるか | PoC の iPhone 列は未記入 |
| ⌘C 空振り対策 (2): OSC 52 の書き込みをブラウザのクリップボードに反映できるか | ユーザ操作外の書き込み制約に当たるかを実ブラウザで確かめ、採否を決める (決定 8) |

## 未決

本 DR では確定させない。

- **UI アクション (新規タブ・分割等) のショートカット割り当て** (後回し): ブラウザ予約キー (Cmd+T / W / N 等) は通常のタブでは奪えない。prefix 方式か等。アクションとキー割り当ては別の層なので、後から決めても作りが変わらない (決定 2)
- **iframe 間のキー転送と focus の扱い**: キーイベントは iframe 境界を越えない
- **leader が去った時の次の leader と再レイアウトの体験**: 実装時に観測して決める (決定 6)

## Alternatives Considered

| 案 | 中身 | 不採用理由 |
|---|---|---|
| 新規作成用の CLI / daemon 機能を足す | gateway 専用の起動経路を作る | `hyoui run --detached` が daemon 化と socket 常設を既に担っており、1 session = 1 daemon のまま呼ぶだけで足りる |
| gateway の env を子に引き継ぎ、DR-0024 の scrub に任せる | 起動元の env をそのまま流す | 普通のターミナルアプリはログイン時と同じ最小の env から始める。gateway の env を持ち込まなければ scrub の論点そのものが生じない |
| プロファイル / 起動コマンドの限定 | 起動できるコマンドを設定で決める | 目的は普通のターミナルアプリと同じ起動で、何を走らせるかは shell の中で利用者が決める。設定は初期ディレクトリだけにする |
| ソフトキーのボタンを `click` で送る | 素の `<button>` + `click` | iPad Safari / PWA でソフトキーボードが閉じる (PoC) |
| 入力パネルを HTTP `POST /input` のまま残す | 直打ちは WS、まとめ送りは HTTP | 経路間の到着順が保証されず、拒否時の扱いも 2 通りになる。経路は WS 1 本にする |
| pane を閉じたら session も終了する | 閉じる = kill | attach は覗き窓で、client 操作で子の寿命を動かさない (DR-0029 / DR-0030)。終了は別アクションにする |
| 配置 (分割比率等) も gateway で共有する | 構造と配置を 1 つにまとめる | 画面の大きさが違う端末 (iPhone / Mac) で同じタブを両立できない。配置は端末ごとにする |
| content-fit を `transform: scale()` で行う | pane ごと CSS で縮める | xterm のマウス座標がずれる。収まる fontSize を計算するのを第一候補にする |
| マウスモードのオフでアプリに DECSET を送る / xterm の内部状態を書き換える | 要求そのものを落とす | 子から見た状態を変えるので透過原則に反する (DR-0014)。転送だけを止める |
| 転送オン時に touch → mouse report を自前で変換する | スワイプも TUI に渡す | 変換を自前で持つことになり、DECSET 保持と公開 API の範囲で書けるかも別途の検証が要る。オン時もスワイプはスクロールのままにし、tap だけ渡す |
| マウスの選択にもクローン層を使う | マウス・タッチを同じ選択方式にする | 選択を始めると focus が端末から外れ、focus を戻すと選択が消える (PoC)。マウスは xterm 自身の選択を使う |
| xterm の行 DOM を選択可能にする (PoC の B0 / B1) | xterm の描画をそのまま OS 選択に使う | 折り返しが必ず改行になる、背景色だけのセルが空白でコピーされる、再描画で選択が消える、ダブル / トリプルクリックが効かない (PoC)。DOM renderer の作りで決まり公開 option では変えられない |
| vt100 の前段に OSC 用の自前 parser を置く | bytes を自前で scan する | vt100 の `Callbacks` で全部拾える。前段 parser を持つと chunk 跨ぎの carry まで自前になる |
| ghostty-web 0.4.0 / 自前描画 / hterm | xterm.js 以外の描画器 | ghostty-web は OSC 8 URI が常に null、mouse report 未実装、IME 位置追従が未統合。自前描画は daemon snapshot が色・OSC 8・mouse mode を欠き、WS も差分を配らない。hterm は要件の実証が無い (research) |
| WebGL renderer を全 pane の既定にする | 描画を GPU に寄せる | pane ごとに context を作り、多数 pane と context の上限・context-loss の報告が衝突する (research 深掘り C) |
| 旧 API を新 UI と並存させ続ける | 互換経路として残す | 契約の写しと経路が増える。切り替え期間が終われば消す |

## Consequences

- **web 境界の意味が「覗いて操作する」から「プロセスを起動できる」に広がる。** DR-0036 の認証が守るものが既存 session の画面と入力から、利用者の uid でのプロセス起動に広がる (決定 1)。DR-0036 の「守る対象」と DR-0027 の前提の記述は、本 DR に合わせて読み替える
- **gateway が状態を持つ。** 構造 (タブグループ → タブ → pane、紐付け、未アタッチ) の正本が gateway に置かれ、版番号付きの変更と push 配布が web 契約に加わる (決定 5)。gateway はこれまで session の状態を daemon から引くだけだった
- **web 契約の世代が上がる。** 旧 route の削除と WS 制御 frame・構造 API の追加で新契約になり、`WEB_PROTOCOL_VERSION` を上げる (決定 13)。切り替え期間中は旧 UI 向けの旧 API と新契約が同じ gateway に並ぶ
- **daemon 側の変更は OSC 属性に限られる。** `state.rs` への `Callbacks` 導入と、属性を gateway に渡す field の追加 (決定 11)。daemon にタブ・pane の概念は入らず、1 session = 1 daemon は変わらない
- **入力は WS 1 本になる。** テキストエリアのまとめ送りとソフトキーも WS を通るので、`POST /input` に掛かっていた DR-0022 の auto-lock は web の入力経路から外れる。WS の上り bytes は DR-0021 の PTY drain ack で同期する現行の WS 経路と同じ扱いになる
- **xterm.js 6.0.0 への移行コストを払う。** xterm.js と addon の読み込みは ES module (`.mjs`) の import になり、内部 API 依存の補正は再評価になる (決定 12)
- **iOS 実機での確認が実装の gate に残る。** タッチ選択、マウスモードの遮断、iPhone のソフトキーは headless と iPad の範囲でしか確認していない (「実装時に確かめること」)

## 関連

- `docs/issue/2026-10-04-design-webui-terminal-app-rework.md` — 本 DR の合意と未決の正本 (2026-10-04)
- `docs/findings/2026-10-03-web-api-protocol-inventory.md` — 現行の HTTP / WS / 認証の境界と daemon protocol への写り方、DR との乖離 (§7)
- `docs/findings/2026-10-03-web-component-tree.md` — 現行 front の構造・状態・イベント経路と見直し論点
- `docs/findings/2026-10-04-daemon-osc-coverage.md` — vt100 0.16.2 の OSC の扱いと、取り出しに要るもの
- `docs/research/2026-10-04-web-terminal-renderer-survey.md` — 描画器の比較と xterm.js 6.0.0 を起点にする判断
- `docs/research/poc/2026-10-04-softkey-iframe-focus/` — ソフトキーと子 iframe の focus 維持
- `docs/research/poc/2026-10-04-xterm-mouse-mode-toggle/` — DECSET を保ったまま転送だけを止める
- `docs/research/poc/2026-10-04-terminal-text-selection-layer/` — セル位置に揃えたクローン層での選択とコピー
- `docs/issue/2026-10-04-design-daemon-own-cell-model.md` — daemon 側の自前セルモデル (別 track)
- `docs/issue/archive/2026-10-04-screen-scrollback-ansi-drops-color.md` — web 初期表示のモノクロ (解消済み)
- DR-0005 / DR-0014 / DR-0015 / DR-0024 / DR-0027 / DR-0029 / DR-0030 / DR-0033 / DR-0035 / DR-0036
