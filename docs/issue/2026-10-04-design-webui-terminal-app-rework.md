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
- 未決: 転送オン時のタッチ。xterm 6.0.0 は touch を扱わず、TUI に届くのは tap (互換 mouse) だけ。スワイプまで TUI に渡すなら touch → mouse report の変換を自前で持つ。touch でのドラッグ選択の操作 (長押し等) と iOS 実機も未確認

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
- 自前で作り直す候補は daemon 側の仮想スクリーン (vt100 → 自前のセルモデル)。色・属性・OSC 8 をセル単位で持つ (screenshot / snapshot の貧弱さ、web 初期表示のモノクロ、`attach-osc8-hyperlink-metadata-loss`)、rect 指定の切り出しと監視 (`screen-region-watch-api`)、オーバーレイ (`screen-overlay-general-mechanism`)、履歴の保持がまとめて同じ方向を向く。DR-0013 を引き継ぐ大きな別案件で、webui 作り直しとは別 track にする
- オーバーレイは子 PTY に送らない原則。web では DOM の層 (制御 frame で位置と内容を送る) で描き、TTY bytes に混ぜない案。CLI attach は bytes で重ねるしかないので、届け先で方式が分かれる点を DR で決める
- web 初期表示のモノクロは vt100 ではなく hyoui 側の手抜きが原因で、自前セルモデルを待たずに直せる (issue `2026-10-04-screen-scrollback-ansi-drops-color`)。自前セルモデルが本当に要るのは vt100 で持てない OSC 8・オーバーレイ・rect 単位の切り出しと監視

### 旧 API の扱い

- web の HTTP / WS 契約は作り直しで一新する。旧 API (`POST /input` 等) は残さない。旧 UI と並行運用する切り替え期間だけ存在し、切り替え完了で消す

## 未決

- 新規セッション作成で web 境界から shell を起動できるようになることを DR に書く (DR-0027 / DR-0036 の前提の更新)
- (後回し) UI アクション (新規タブ・分割等) のショートカット割り当て: ブラウザ予約キー (Cmd+T / W / N 等) は通常タブで奪えない。prefix 方式か等。アクションとキー割り当ては別層なので後から決めても作りが変わらない
- iframe 間のキー転送 (キーイベントは iframe 境界を越えない) と focus の扱い
- leader が去った時の次の leader と再レイアウトの体験 (実装時に観測して決める)
