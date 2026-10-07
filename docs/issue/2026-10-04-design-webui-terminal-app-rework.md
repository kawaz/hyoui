---
title: web UI をブラウザ上のターミナルアプリとして作り直す (新規セッション作成 / タブ・pane / アクション / ソフトキー)
status: open
category: design
created: 2026-10-04T00:30:00+09:00
last_read: 2026-10-04T00:30:00+09:00
open_entered: 2026-10-04T00:30:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# web UI をブラウザ上のターミナルアプリとして作り直す

議論フェーズの記録。合意したものを「合意」、未決を「未決」に置く。DR 起草はここが固まってから。現状の棚卸しは `docs/findings/2026-10-03-web-api-protocol-inventory.md` と `docs/findings/2026-10-03-web-component-tree.md`。

## 目的

daemon が動いていれば、ブラウザからリモートでターミナルを開いてローカル作業ができる。いまは `hyoui run` の起点が ghostty / iTerm2 等のターミナルアプリしかないが、web から session を作れればターミナルアプリを開かずに作業を始められる。iTerm / Ghostty のようなフルスクリーンのターミナルアプリの使い心地を軸にしつつ、全描画領域を文字セルで敷き詰める必要があるというターミナル特有の制限には囚われない。

## 合意 (2026-10-04)

### 新規セッション作成

- web から新規セッションを作れるようにする。gateway が既存の `hyoui run --detached` を呼んで新しい session の daemon を起動する (1 session = 1 daemon、新 CLI は不要)
- gateway の restart / upgrade で session が道連れにならないこと (`--detached` の fork + setsid で独立するはず、未検証) を実装時の検証項目にする
- 起動するのは普通のターミナルアプリと同じくユーザのログイン shell (passwd から引き、argv[0] を `-zsh` 形式)。env は gateway のものを引き継がず、ログイン時と同じ最小の env から始めて残りは shell の rc に任せる (gateway の env を持ち込まないので DR-0024 の scrub の論点は生じない)。プロファイルやコマンド限定の仕組みは持たない
- 設定は初期ディレクトリだけ: 新規タブ / 新規分割 / 新規タブグループのそれぞれで「元 pane の cwd を引き継ぐ」か「HOME」。既定はタブと分割が引き継ぎ、タブグループが HOME (Ghostty / iTerm と同じ)

### 子の出力から取れるメタデータ (OSC)

- daemon が子の出力から観測できる情報は取れるだけ取って session の属性として持つ (観測のみで介入しないので透過原則と整合)。表示に使うかは UI 側で決める。変化は WS で通知し、セッション一覧 API でも返す
- 候補: OSC 0 / 2 (title)、1 (icon name)、7 (cwd。「cwd を引き継ぐ」の元データ)、133 (shell integration のプロンプト / コマンド境界と終了コード)、9 / 777 (通知)、9;4 (進捗)
- OSC 7 を出さない shell の cwd は OS から子の cwd を読む (macOS は libproc、Linux は `/proc/<pid>/cwd`) で補う案。どちらを正にするかは実装時
- OSC 52 (クリップボード読み書き) は観測ではなく操作を伴うので別の論点。少なくとも読み出しは既定で無効
- vt100 0.16.2 はどの OSC も保持しないが、全部 `Callbacks` で拾える (0 / 1 / 2 は title・icon name の callback、52 は clipboard の callback、それ以外は `unhandled_osc`)。vt100 の前段に自前 parser は要らない。hyoui は現在 callback 未使用で、追加の影響は `daemon/screen/state.rs` に閉じる (`docs/findings/2026-10-04-daemon-osc-coverage.md`)
- `;` を含む title は分割されて `unhandled_osc` に落ちるので結合し直す
- resize は入力ログを新 parser に replay するので callback が再発火する。title / cwd は最後の値で上書きなので無害、通知 (9 / 777) は重複するので replay 中は callback を無視する印が要る
- session 一覧の `cwd` は現状起動時 cwd。現在 cwd を OS から読む処理は無い

### アクション

- 新規タブ・画面分割・leader 昇格などを **web 専用のアクション** として形式化し、後からショートカットを割り当てられるようにする。hyoui の TUI 側にキー割り当ては足さない (TUI の手前の層)
- UI のボタンもショートカットも、アクションを invoke するだけ (1 操作 = 1 アクション)
- **TTY への送信 (キー X を TTY にそのまま送る) もアクション / イベントとして今から形式化する**。ソフトキー (Esc / Tab / ^C / 矢印 等) はそれを invoke するだけ。xterm への直打ちも同じ経路で WS に流れる
- キーの既定の行き先は TTY。割り当ての無いキーは全部 TTY へ送り、UI アクションのショートカットは割り当てたキーだけを横取りする (後からショートカットを足しても作りが変わらない)

### コンポーネントツリー (1 から作り直す)

```
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

- タブコンテナ・特定タブ・特定 pane は単独で別ブラウザタブに開ける。iframe の木にして postMessage で疎結合する。ソフトキーツールは最上位 frame に 1 つ。ボタンは `pointerdown` で `preventDefault()` して postMessage で送る (iPad Safari / PWA、textarea / xterm.js で子 iframe の focus とソフトキーボードが維持されることを実機確認済み: `docs/research/poc/2026-10-04-softkey-iframe-focus/`)

### session list の構造

```
タブグループ (ターミナルアプリのウインドウ相当)
  タブ
    pane (1 hyoui session に紐づく)
未アタッチ (どこにも属さない session)
```

- pane → session は 1 つ。1 session に複数 pane が紐づいてよい
- **pane を閉じる = detach**。session は終了しない。閉じた pane の session は未アタッチ一覧に移る。選ぶとデフォルトのタブグループ等に新規タブとしてアタッチする。session の終了は別のアクション

### 共有する構造と端末ごとの配置

| 層 | 中身 | 置き場所 |
|---|---|---|
| 構造 (共有) | タブグループ → タブ → pane のツリー、pane と session の紐付け、未アタッチ一覧 | gateway |
| 配置 (端末ごと) | 分割の向き・比率、サイドバー開閉、pane の非表示 | ブラウザ (localStorage = 端末単位。必要になればウィンドウ単位を sessionStorage で上書き) |

- pane の別タブ・別タブグループへの移動は構造の変更なので共有。分割比率は配置なので共有しない
- 閉じる (構造から外れ全端末で未アタッチへ) と 非表示 (この端末の配置で見せないだけ) を分ける
- タブ内の配置には分割のほかに **カルーセル** (pane を 1 枚ずつ全面表示し左右フリックで切り替え) を持つ。スマホでタブ内分割が現実的でないため。配置の層なので端末ごと (iPhone はカルーセル、Mac は分割を同じタブで両立)。分割 ⇄ カルーセルの切り替えと前 / 次の pane はアクション。フリックと吸着は CSS `scroll-snap` に任せる。フリックとターミナルのタッチ操作の衝突は、次節のマウスモード切り替えで解く
- 他端末が構造に pane を足したら、手元の配置には既定で末尾に分割して足す (非表示で足すと気づけない)
- 構造の正本は gateway 1 か所、変更は操作単位 + 版番号、他端末へは push で配る。web 契約 (DR-0035) に構造の取得・変更・変更通知が加わる

### マウスモードの切り替え

- マウスモード (TUI へのマウスイベント転送) のオン / オフをアクションにする。手軽な切り替えは必須 (PC でも邪魔なことが多い)
- オフ時のタッチ: 横フリック = カルーセル配置ならカルーセル移動、縦フリック = スクロール、ドラッグ = 選択。オン時はタッチを TUI に渡す
- 切り替えるのは UI が PTY に転送するかどうかだけで、アプリのマウス追跡要求 (DECSET 1000 系) には触れない (透過原則と整合)
- 表示は「アプリが要求しているか」と「UI が転送しているか」の 2 状態 (要求なし / 要求あり・転送中 / 要求あり・遮断中)
- 既定はアプリの要求に従う。ユーザがオフにしたらその pane で明示的に遮断
- 記憶単位は pane × 端末 (配置と同じ層)
- alt screen 中の縦フリックは矢印キーに変換する (xterm 系の慣習)
- xterm.js 6.0.0 で転送だけを止められることを PoC で確認 (`docs/research/poc/2026-10-04-xterm-mouse-mode-toggle/`、headless Chromium / WebKit)。オフ時は capture で mouse / wheel を止め、左 mousedown を修飾キー付き (Mac は Option + `macOptionClickForcesSelection`、他は Shift) で渡し直して xterm 自身の強制選択経路を使う。wheel は normal で `scrollLines`、alt で矢印キー列。touch の縦スワイプは自前、横は `scroll-snap` のカルーセル。DECSET は全 224 ケースで保持、遮断中の report は 0 byte。Mac の `altClickMovesCursor` (本物の mouseup の altKey を見る) は mouseup の altKey を外して渡し直す
- 選択の方式は入力デバイスで分ける (合意): マウスは xterm 自身の選択 (オフ時は修飾キー付きで渡し直す)、タッチは長押しでクローン層 (`docs/research/poc/2026-10-04-terminal-text-selection-layer/`、(A) 方式) の OS 標準の選択。クローン層は選択で focus が外れるのでマウスには使わない
- 解消したい痛み: TUI がマウスモードで自前の選択ハイライトを描くせいで、選択したつもりで ⌘C が何もコピーせず、前のクリップボードが貼られる。マウスモードのオフで大半は防げるが、オン (既定でアプリの要求に従う) のままだと残る。追加案 (合意 2026-10-04、(2) は書き込みが通るかを確かめて採否): (1) copy イベント時に xterm 側の選択が空なら「クリップボードは更新されていません」と短く通知し、マウスモードをオフにする操作を添える (2) TUI が自前の選択を確定して出す OSC 52 の書き込みをブラウザのクリップボードに反映する (ブラウザはユーザ操作中しか書き込みを許さない制約があり、通るかは未確認)
- 本物の選択 (コピーされるもの) と TUI が描く選択風のハイライトを見た目で区別できるようにする。TUI のハイライトはただの反転セルで hyoui からは判定できず、透過原則でそちらの色は変えないので、区別は本物の側で作る (合意 2026-10-04): (a) 本物の選択の見た目を TUI がよく使う反転と紛れない独自の色 + 縁取りにする (xterm の theme `selectionBackground` 等) (b) マウスモードをポインタの形で示す (転送オン = 矢印、オフ = I ビーム。ターミナル領域の CSS `cursor` の切り替えだけ) (c) コピーの結果を成功時も短く知らせる (「N 文字コピー」)。成功と空振りが両方見える
- 転送オン時のタッチ: TUI に渡すのは tap だけ、スワイプはオン時もスクロールのまま (xterm 6.0.0 は touch を扱わず tap は互換 mouse で届く。touch → mouse report の自前変換は持たない)
- タッチの選択: 長押しで選択モード (マウス転送のオン / オフによらず)。現行 `session.js` のテキスト層 (buffer を素のテキストにして `<pre>` で重ね、OS 標準の選択ハンドル・ルーペ・コピーメニューに任せる) を土台に、ターミナルのセルと同じ位置・フォント・行高でその場に重ねる。選択範囲の調整とコピーは OS 標準に任せ、自前は閉じる操作だけ
- 未確認 (iOS 実機 PoC で確かめる): 長押しで層を出してそのまま OS 標準の選択が始まるか (間に合わなければ透明な層を常に重ねる案)、全角・曖昧幅・絵文字でテキスト層がセルとずれないか

### サイズ違い (1 session を大きさの違う複数 pane で見る)

- cols/rows は leader 優先 (DR-0033)
- leader より大きい pane は余白を置く。**余白は余白と分かる見た目にする** (ターミナル背景色と同じにすると cols/rows を誤認させる)
- leader より小さい pane は cols/rows を変えず、その縦横比の矩形を pane に収まるよう丸ごと縮小する (content-fit)。実装は `transform: scale()` だと xterm のマウス座標がずれるので、収まる fontSize を計算して設定するのが第一候補
- 非 leader pane には leader 昇格のアクション (ボタン + ショートカット)。昇格すると cols/rows が変わり TUI アプリが再描画する

### 入力の 2 系統

- xterm への直打ち (1 文字ごとに TUI の反応を受ける、claude の `/` 補完等) と、テキストエリアでまとめて送る (IME 変換位置を TUI に縛られない) の 2 系統を両方持つ。2 系統は UI の違いだけで、プロトコル経路は WS 1 本 (予備経路は持たない)

### WS 制御 frame の拡充

- 新しい契約に次の 3 つを入れる (pane の「プロセスが終了しました」表示等の前提):
  - 子の終了 / 停止 / 再開の通知 (daemon の `session.exit.notify` / `session.child.stopped.notify` を gateway で捨てずに写す。終了は exit code か signal を載せる)
  - 入力が拒否されたことの通知 (ro / lock 未保持 等の理由付き。現状は log に出るだけ)
  - 接続を閉じた理由 (close code + reason。認証切れの 4401 は実装済み、子の終了・daemon 切断も区別できるようにする)

### ブラウザ側の描画と daemon 側の仮想スクリーン

- ブラウザ側は xterm.js 6.0.0 を使う (5.3.0 から上げる)。外に出るのは TTY の raw bytes だけなので、daemon 側の画面モデルを替えてもブラウザの経路は変わらない (`docs/research/2026-10-04-web-terminal-renderer-survey.md`、マウスは `docs/research/poc/2026-10-04-xterm-mouse-mode-toggle/`)
- daemon 側の仮想スクリーンを自前のセルモデルにする件 (層の合成によるオーバーレイ、attach 出力を合成画面から作る) は別 track の issue `2026-10-04-design-daemon-own-cell-model`
- web 初期表示のモノクロは vt100 ではなく hyoui 側の手抜きが原因で、自前セルモデルを待たずに直せる (issue `2026-10-04-screen-scrollback-ansi-drops-color`)

### 旧 API の扱い

- web の HTTP / WS 契約は作り直しで一新する。旧 API (`POST /input` 等) は残さない。旧 UI と並行運用する切り替え期間だけ存在し、切り替え完了で消す

## 未決

- 新規セッション作成で web 境界から shell を起動できるようになることを DR に書く (DR-0027 / DR-0036 の前提の更新)
- (後回し) UI アクション (新規タブ・分割等) のショートカット割り当て: ブラウザ予約キー (Cmd+T / W / N 等) は通常タブで奪えない。prefix 方式か等。アクションとキー割り当ては別層なので後から決めても作りが変わらない
- iframe 間のキー転送 (キーイベントは iframe 境界を越えない) と focus の扱い
- leader が去った時の次の leader と再レイアウトの体験 (実装時に観測して決める)

## 構造の持ち方の見直し (2026-10-07、kawaz との議論、DR-0039 決定 5 は未改訂)

- **server 側が持つのは pane でなく session。** 構造は タブグループ → タブ → session。pane は UI 側 (端末ごと) の概念で、1 pane は 1 session への対応を 1 つ持つ。pane の id を共有しないので、端末間で揃える状態が減る。閉じる = タブから session を外す (共有)、非表示 = その端末で pane を作らない (端末ごと)、未アタッチ = どのタブにも入っていない session。同じ session を 1 つのタブに 2 つ並べる見せ方は、端末ごとの操作になる
- **1 つの session は構造上 1 か所にしか所属しない** (kawaz 裁定)。webui 側で symlink のような参照を発明するのは可
- **構造の正本を session の tag (DR-0041) に置く案** (kawaz「セッションに貴賎はないので webui_group=0_0 みたいなタグを付けるとかで良いのかも」、検討中)。良い点: 構造の file とその lock・版番号・変更通知が要らない。どの gateway も同じ面の session を読めば同じ構造が見える。daemon は tag を解釈しないので web の都合が core に漏れない。`hyoui run --tag webui.tab=work` のように CLI や agent からも構造を作れる。決める必要があること:
  1. tag を起動後に変える手段 (DR-0041 の未決にした `hyoui set --tag` 相当) が必須になる。daemon への tag の更新要求が protocol に加わるので、必然性を DR に書く
  2. 空のタブやタブグループは存在できない。名前・並び順・空のグループを残すなら別の置き場が要る (最後の session が抜けたら消える、と割り切るなら要らない)
  3. 値は番号でなく名前か id にする (番号だと並べ替えで他の session の値がずれる)。グループとタブは別の key (`webui.group` / `webui.tab`)、並び順も別の key (`webui.order`) にするのが統括推し
- **方向 (2026-10-07、kawaz)**: 構造専用の仕組みは持たず、front が session の普通の tag から構造を導く。`webui.tabgroup` で session を group by してタブグループにし、選んだタブグループの session の `webui.tab` を集めてタブにする。値は名前 (index にしない。index だと並べ替え・削除で他の session の値を書き換えることになり、途中で失敗するとタブが黙って合流する)。並び順は front が決める (名前順・起動順、端末ごとの並べ替えは localStorage の配置)。空のタブ・タブグループは存在しない。tag の無い session は未アタッチ。タブの名前の変更はそのタブの session 全部の tag の書き換えになる (途中で失敗してもタブが分かれて見えるだけ)
- 残る判断: session をタブ間で移せるか (= 起動後に tag を変える手段 `hyoui set --tag` と daemon への tag 更新要求を足すか)

## view を呼び出し側が定義する形 (2026-10-07、kawaz 案、検討中)

「タブ」「pane」を固定の構造にしない。tag と selector で session の集合を作り、見せ方はコンポーネント、並べ方は端末ごとのレイアウトにする。daemon は tag を解釈しない (kawaz「daemon 側では何も考えない」)。

- **使い方の例 (ccmsg)**: claude の session に `claude.session_id` / `claude.project_dir` / `claude.config_dir` の tag を付ける (後付けでもよい)。ccmsg の Terminal タブは hyoui web を `?sessions=<selector の JSON>&embed=true` で iframe に埋め込む
- **定義の置き場**: 呼び出す側が URL で渡す (session の scope と構造)。並べ方は localStorage。hyoui web は定義を保存しない
- **selector** (配列は OR): `{ids?, tags?, in?, notIn?}`。例 `{}` = 全部、`{tags:["claude.session_id=UUID","claude.project_dir=PATH"]}`、`{notIn:[{tags:["claude.session_id"]}]}`。統括案: 空のリストは条件なしと同じ。1 つの selector は ids と tags の一致の和集合に `in` を掛け `notIn` を引く。tag の書き方は `list --tag` と同じ (`k` は key があれば、`k=v` は完全一致)。selector は見せ方の絞り込みで権限ではない
- **新規 session** `new={cwd, tags}`: command は hyoui 側の設定 (URL から任意コマンドを起動させない)。統括案: `${key}` は操作を起こした pane の session (無ければ選択中の session) の tag と `hyoui.*` から展開し、値が無ければ作成を断る。作るのは利用者の操作の時だけ
- **`hyoui.*` の予約 key**: `hyoui.pid` / `hyoui.session_id` 等を読み取り専用で template と selector に使う (procfs 的な写像)。利用者の tag では `hyoui.` を予約して拒否する案
- **コンポーネント**: HyouiSessionList (sections ごとに name / session_name の template / selectors)、HyouiLayoutList (localStorage のレイアウト一覧。default の空レイアウト、pane の分割、pane と session の紐付け、新規 shell)、HyouiPanesContainer、HyouiSessionTabs。**タブはレイアウトとして見直す** (タブの見た目である必要はない)
- 後付けの tag には、起動後に tag を変える手段 (汎用の `hyoui set --tag`) が要る (claude の SessionStart hook から `HYOUI_SESSION_ID` で自分の session に付ける等)
- 未確認・未決: レイアウトは端末ごとで共有しない (DR-0039 の「構造は共有」からの転換)。localStorage の名前空間 (scope / view ごとか全体か)。iframe での cookie と storage の partition (同じ登録ドメインの下なら通る見込み、実機未確認)。埋め込める origin の指定 (`frame-ancestors`)
- **裁定 (2026-10-07、kawaz)**:
  - selector の空のリスト `[]` は条件なし (書かないのと同じ)
  - `${key}` は選択中の hyoui session の tag から展開する。選択が無い時は embed の定義に書いた変数のデフォルト値、それも無ければ空文字 (cwd が空なら新規 session の既定 cwd)
  - `hyoui.` は予約 key として利用者に付けさせない (v0.14.0 で実装)
  - レイアウトは端末ごとで他の端末に出ない。export / import は今は考えない
  - localStorage の名前空間は scope ごと。selector を正規化した hash (key を並べ替える、空のリストを落とす、OR のリストを並べ替える) を使う。統括案: URL に任意の `view=<名前>` があればそれを名前空間に使う (selector の形を変えてもレイアウトを引き継げる)
