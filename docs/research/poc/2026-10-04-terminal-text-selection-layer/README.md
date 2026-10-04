# PoC: xterm.js 6.0.0 の上に重ねたテキスト層で、選択とコピーをブラウザ標準の選択に任せる

web UI 作り直し (`docs/issue/2026-10-04-design-webui-terminal-app-rework.md` の「マウスモードの切り替え」節、タッチ選択の項) の前提検証。xterm の上に「イベントを持たない見た目だけのテキスト層」を常に重ね、選択とコピーをブラウザ標準の HTML 選択で行う案が成り立つかを、2 方式で確かめる。現行 `crates/hyoui-web/assets/session.js` のテキスト層 (`textSelectionOverlay` / `terminalBufferText`) は buffer 全体を `<pre>` に流す静止 snapshot で、セル位置とは揃えていない。本 PoC はセル位置に揃えた層を作る。

## ファイル

| ファイル | 役割 |
|---|---|
| `index.html` / `index.js` | PoC 本体 (ビルドなし、ES module)。xterm は `../2026-10-04-xterm-mouse-mode-toggle/lib/` の 6.0.0 (無改変、出所と integrity はそちらの README) を相対参照する。PTY は無く、テストデータを `term.write` で流す |

## 画面と URL パラメータ

- `?mode=A` (既定) / `B0` / `B1` / `none` (= 何もしない、xterm 自身の選択)
- (A) 用: `?wrap=join|split` (折り返し行をつなぐか)、`?freeze=on|off` (選択中は層を作り直さない)、`?fit=1|0` (文字の送り幅をセル幅に揃える)、`?show=1` (層の文字を半透明の赤で出す。位置合わせの目視用)
- `?focus=collapsed|always`: 層 / 行 DOM で mouseup した時に `term.focus()` を呼ぶ条件。collapsed は何も選ばれていない時だけ
- `?1002+1006` ボタン: アプリのマウス要求を `term.write` する。`出力を 1 行追記`: 選択中の再描画を起こす
- 下のテキストエリアに貼り付けると、改行・空白を JSON で表示する (iOS 確認用)。ログは `onData` / `onBinary` (= PTY に送られるはずのもの)
- 端末は 40 cols × 24 rows、`fontSize: 16`、`fontFamily: Menlo, ui-monospace, monospace`、renderer は xterm 既定の DOM renderer

## 方式

| 方式 | 実装 |
|---|---|
| (A) クローン層 | `.xterm-screen` の子に `div.sel-layer` を `position: absolute; left: 0; top: 0; z-index: 20` で置く。表示中の各行について公開 API (`buffer.active.getLine` / `IBufferLine.getCell` / `isWrapped` / `IBufferCell.getChars` / `getWidth`) だけでセル列を作り、1 セル = 1 個の `span` (`display: inline-block; width: 幅 × セル幅 px; height: セル高; white-space: pre; overflow: hidden`) にする。幅 0 のセル (全角の右半分) は span を作らない。論理行 (`isWrapped` でつながる行) ごとに `div.blk` (幅 = cols × セル幅、`white-space: normal`) に入れ、cols を超えた分は inline-block の行送りで次の行に落ちる (= `wrap=join`)。`wrap=split` は物理行ごとに `div` を分ける。文字色は `transparent`、`::selection` だけ半透明の青。`fit=1` は canvas の `measureText` で測った文字幅との差を `letter-spacing` に入れて送り幅をセル幅に揃える (xterm の DOM renderer と同じ考え方)。`mousedown` / `mouseup` / `click` / `dblclick` / `contextmenu` / `auxclick` は層で `stopPropagation` して xterm に渡さない。`wheel` と key は止めない。`term.onRender` / `onScroll` で作り直し、`freeze=on` の時は層の中に選択がある間は作り直さず、選択が外れた時 (`selectionchange`) に作り直す |
| 未書き込みセルの扱い ((A) の作り方) | `getChars() === ''` のセルは、同じ行で右側に書き込み済みセルがあればスペース、無ければ空の span (幅だけ持ち、文字なし)。xterm の `translateToString(true)` と同じ規則 (下記ソース) |
| (B0) | CSS だけ: `.xterm-rows` とその子孫に `user-select: text` |
| (B1) | B0 + `.xterm-rows` に `pointer-events: auto` を戻し (xterm は `pointer-events: none` を入れている)、host の capture phase で `.xterm-screen` 内の `mousedown` を `stopPropagation` して xterm の選択開始を止める (`preventDefault` はしない) |

(A) (B1) とも、`mouseup` で選択が空なら `term.focus()` を呼ぶ (`focus=collapsed`)。

## テストデータ (行番号は viewport 先頭からの 0 始まり)

| id | 行 | 書き込み |
|---|---|---|
| 1-wrap | 0-2 | `The quick brown fox jumps over the lazy dog. 日本語の全角文字を混ぜて折り返す長い一行です。END` (40 cols で 3 行に折り返す) |
| 2a-trail-sp | 3 | `trail-sp` + スペース 5 個 |
| 2b-trail-none | 4 | `trail-sp` (2a と同じ見た目) |
| 3a-box-sp | 5-7 | `┌──────────┐` / `│abc       │` (内側の右はスペースを書く) / `└──────────┘` |
| 3b-box-cuf | 8-10 | 3a と同じ見た目で、内側の右は `ESC[7C` で飛ばす |
| 4a-bg-sp | 11 | `bg-sp:` + `ESC[44m` + スペース 10 個 + `ESC[0m` |
| 4b-bg-el | 12 | `bg-el:` + `ESC[44m` + `ESC[K` + `ESC[0m` (EL で消したセルは未書き込みで背景色だけ持つ) |
| 4c-none | 13 | `none:` |
| 5-wide | 14 | `全角あいう ○①★ 😀✅ é end` |
| 6-cuf | 15 | `cuf:` + `ESC[10C` + `after` |

## 観測方法

Playwright (playwright-core 1.63.0-alpha) の headless Chromium 152.0.7977.8 と headless WebKit 26.5 (いずれも macOS 上、`navigator.platform` は `MacIntel`)。計測スクリプトはリポ外に置き commit していない。kawaz の画面は使っていない。

- コピー: 各データの全行を「先頭行 0 桁の左端 + 1px」から「最終行 39 桁の右端 − 1px」まで 8 step でドラッグ → `Meta+C` → `navigator.clipboard.readText()`。毎回先に clipboard へ目印の文字列を書き、変化しなかった場合を区別した。同時に `document.getSelection().toString()` と `term.getSelection()` も記録
- 部分選択: 行 0 の 30 桁 → 行 1 の 5 桁 (折り返しをまたぐ)、行 14 の 0〜9 桁、行 14 の 11〜23 桁、行 0 の 0 桁 → 行 2 の 20 桁
- 位置: 各セルについて期待位置 (`.xterm-screen` の左端 + x × セル幅、セル幅 = screen の幅 / cols) と、xterm の行 DOM の文字 (`Range.getBoundingClientRect`)、層の span の箱、層の文字の左端を比較。行の上端も同様
- キー / IME / wheel / マウス要求 / 選択中の再描画 / ダブル・トリプルクリック: 下の表のとおり

## 結果: コピー結果 (clipboard の text/plain)

`none` 列は xterm 自身の選択 (= 比較の基準)。他の列は `none` と同じなら「=」。Chromium / WebKit で同じだったものは 1 セルにまとめ、違うものは分けて書いた。`(A)` は `fit=1`・`freeze=on`。

| データ | none (xterm 自身) | (A) join | (A) split | (B0) | (B1) |
|---|---|---|---|---|---|
| 1-wrap 全体 | Chromium: 1 行につながる `"The quick … 長い一行です。END"`。WebKit: `"The quick … 折り返す長"` で 3 行目が欠ける (終点を行 2 の 20 桁にすると 3 行とも入る) | Chromium =、WebKit は 3 行目まで 1 行につながって入る | 物理行ごとに改行 `"…the lazy \ndog. …返す長\nい一行です。END"` (1 行目末の空白 1 個も入る) | = (Chromium・WebKit とも none と同じ) | (A) split と同じ (行 DOM が物理行ごとの div) |
| 2a 行末にスペースを書いた行 | `"trail-sp     "` (書いたスペース 5 個が入る) | = | = | = | = |
| 2b 行末に何も書いていない行 | `"trail-sp"` | = | = | = | = |
| 3a 罫線 (内側の右はスペース) | `"┌──────────┐\n│abc       │\n└──────────┘"` | = | = | = | = |
| 3b 罫線 (内側の右は CUF) | 3a と同じ (行の途中の未書き込みセルはスペース) | = | = | = | = |
| 4a 背景色 + スペース | `"bg-sp:          "` (スペース 10 個) | = | = | = | = |
| 4b 背景色 + EL | `"bg-el:"` | = | = | = | `"bg-el:"` + スペース 34 個 (行末まで) |
| 4c 何も書いていない | `"none:"` | = | = | = | = |
| 5 全角・曖昧幅・絵文字 | `"全角あいう ○①★ 😀✅ é end"` | = | = | = | = |
| 6 CUF で飛ばしたセル | `"cuf:          after"` (スペース 10 個) | = | = | = | = |
| 部分: 行 0 の 30 桁 → 行 1 の 5 桁 | `" the lazy dog. 日"` | `" the lazy dog. "` (`日` が入らない) | `" the lazy \ndog. "` | = | `" the lazy \ndog. "` |
| 部分: 行 14 の 0〜9 桁 | `"全角あいう"` | = | = | = | = |
| 部分: 行 14 の 11〜23 桁 | `"○①★ 😀✅ é end"` | = | = | = | = |

- どの方式でも、コピー操作で `onData` / `onBinary` に出たものは 0
- (B0) は CSS を当てても xterm 自身の選択のままで、DOM の選択は常に空 (`getSelection().toString() === ""`)。コピー結果は xterm の copy handler が書いたもの
- (A) `fit=0` (送り幅を揃えない) は、Chromium では全項目 none と同じ、WebKit では 1-wrap 全体だけ none と違い (A) join と同じ。どちらも部分選択の `日` は入った (`" the lazy dog. 日"`)。全角のグリフがセルより狭く、終点の位置がグリフの中点より右になるため
- 「部分: 行 1 の 5 桁で終わる」で (A) (B1) から `日` (5〜6 桁の全角) が落ちるのは、終点がちょうど `日` の中点 (6 桁目の左端 − 1px) にあり、ブラウザ標準の選択は「文字の中点を越えたら含める」、xterm は「触れたセルを含める」で規則が違うため。位置ずれではない

## 結果: その他の観点

| 観点 | none (xterm 自身) | (A) | (B1) |
|---|---|---|---|
| 位置: 行 DOM の文字 x と期待位置の差 (最大) | Chromium 0.30px、WebKit 1.0px (行 14 で全角 1 文字ごとに約 0.25px 左へずれていく) | 層の箱・文字の左端とも 0.000px (Chromium・WebKit) | (none と同じ行 DOM) |
| 位置: 行の上端 | 差 0 | 差 0 | 差 0 |
| 位置: 文字の幅とセル幅 | — | `fit=1` で全セル一致。`fit=0` は全角のグリフが Chromium 16px / WebKit 14.77px で 2 セル (19.25px) より狭く、選択ハイライトの右端が 9.66 セル (Chromium) / 9.53 セル (WebKit) で止まった (`fit=1` は 8.00 で全角の境界に一致) | — |
| 目視 (`show=1` のスクリーンショット) | — | 層の赤い文字が xterm の文字の上にほぼ重なる。xterm も層も `😀` `①` (Chromium) をセル幅 1 で扱い、グリフはセルからはみ出す (xterm 既定の Unicode 6 の幅表) | — |
| click 後のキー `x` | 出る | 出る (mouseup で `term.focus()`) | 出る |
| ドラッグ選択の直後のキー `y` | 出る (focus は textarea のまま) | 出ない (focus が `BODY` に移る。選択は残る) | Chromium: 出る (選択はできていない、下記)。WebKit: 出ない (focus が `BODY` に移る。選択は残る) |
| `focus=always` (選択後も `term.focus()`) | — | 選択が消え、コピーは clipboard 変化なし (Chromium・WebKit) | 同じく選択が消える |
| IME (Chromium の CDP `imeSetComposition` → `insertText`) | `onData` に `日本` | `日本` (click 後) | `日本` |
| wheel (層の上、`deltaY = -300`) | Chromium 3 行・WebKit 20 行上へ | none と同じ量。層の先頭行も buffer の viewport 先頭行と一致 | none と同じ |
| アプリのマウス要求 (1002+1006) 中の click とドラッグ | 報告 10 個 (press / release / drag) | 報告 0 個 (層が mousedown を止める)。ドラッグで DOM 選択ができる (WebKit `fill 63`、Chromium は click 直後のため空、下記) | 報告 0 個 |
| ダブルクリック / トリプルクリック | `quick` / 論理行全体 (3 物理行がつながる) | join: `quick` / 論理行全体。split: `quick` / 物理行 1 行 + `\n` | 何も選ばれない |
| 選択中に別の行へ出力 (`tick\r\n`) | 選択は残る | `freeze=on`: 残る。`freeze=off`: 消える (層の作り直しで node が置き換わる) | 残る |
| 選択中の行を書き換え (`ESC 7 ESC[5;1H XX ESC 8`) | 選択範囲は残り、中身は `XXail-sp` になる | `freeze=on`: 選択も中身も `trail-sp` のまま (層が古い)。選択を外すと層は `XXail-sp` に追いつく。`freeze=off`: 消える | 消える |
| xterm に focus がある状態 (直前に端末を click) から 0.5 秒以内に始めたドラッグ | 選択できる | Chromium: 選択されない (5 回中 2〜3 回)。0.6 秒以上空けると毎回選択できる。WebKit: 毎回選択できる | Chromium: 待ち時間によらず毎回選択されない。WebKit: 毎回選択できる |

## ソースでの根拠 (xterm.js tag 6.0.0、commit f447274f430fd22513f6adbf9862d19524471c04)

- 未書き込みセルと書き込み済みスペースの区別: 未書き込みは codepoint 0 (`NULL_CELL_CODE`) で `HAS_CONTENT` ビットが立たない。`getTrimmedLength` は `HAS_CONTENT` のある最後のセルまで: [src/common/buffer/BufferLine.ts#L461-L468](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/buffer/BufferLine.ts#L461-L468)。`translateToString` は codepoint 0 のセルを `WHITESPACE_CELL_CHAR` (スペース) にし、`trimRight` で `getTrimmedLength` 以降を落とす: [#L524-L545](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/buffer/BufferLine.ts#L524-L545)。定数: [src/common/buffer/Constants.ts#L20-L31](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/buffer/Constants.ts#L20-L31)
- 公開 API での区別: `IBufferCell.getChars()` は codepoint 0 のセルで `''` を返す: [src/common/buffer/CellData.ts#L36-L45](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/buffer/CellData.ts#L36-L45)。型は [typings/xterm.d.ts#L1596-L1661](https://github.com/xtermjs/xterm.js/blob/6.0.0/typings/xterm.d.ts#L1596-L1661) (`isWrapped` / `getCell` / `translateToString` / `getWidth` / `getChars` / `getCode`)。PoC の層はこの `''` 判定だけで `translateToString(true)` と同じ結果を作った
- EL / ED で消したセルは背景色付きの null cell (未書き込み扱い): [src/common/InputHandler.ts#L1147-L1157](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/InputHandler.ts#L1147-L1157)、[src/common/buffer/Buffer.ts#L61-L72](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/buffer/Buffer.ts#L61-L72)。このため 4b は none / (A) で `"bg-el:"` になる
- xterm 自身の選択の文字列: 行ごとに `translateBufferLineToString(i, true, …)` (= 右端の未書き込みを落とす)、`isWrapped` の行は直前に連結: [src/browser/services/SelectionService.ts#L203-L253](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L203-L253)
- xterm の DOM renderer は行を `getNoBgTrimmedLength()` (背景色を持つ未書き込みセルも含める) まで描き、未書き込みセルはスペースにする: [src/browser/renderer/dom/DomRendererRowFactory.ts#L79-L82](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/renderer/dom/DomRendererRowFactory.ts#L79-L82)、[#L163](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/renderer/dom/DomRendererRowFactory.ts#L163)、`getNoBgTrimmedLength`: [BufferLine.ts#L470-L477](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/buffer/BufferLine.ts#L470-L477)。(B1) の 4b で行末までスペースが入るのはこれ。送り幅は `letter-spacing` でセル幅に揃える: [DomRendererRowFactory.ts#L168-L169](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/renderer/dom/DomRendererRowFactory.ts#L168-L169)、[#L463-L466](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/renderer/dom/DomRendererRowFactory.ts#L463-L466)
- 行 DOM は `pointer-events: none`: [src/browser/renderer/dom/DomRenderer.ts#L164-L168](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/renderer/dom/DomRenderer.ts#L164-L168)。(B0) で CSS の `user-select` を変えても、ポインタは行 DOM に当たらず `.xterm-screen` に当たる (`elementFromPoint` で確認)
- `.xterm` 全体が `user-select: none`: [css/xterm.css#L38-L44](https://github.com/xtermjs/xterm.js/blob/6.0.0/css/xterm.css#L38-L44)
- xterm の mousedown は選択開始時に `preventDefault` (ブラウザ標準の選択を始めさせない): [src/browser/services/SelectionService.ts#L449-L474](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L449-L474)。結線は `.xterm` 要素: [src/browser/CoreBrowserTerminal.ts#L544](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/CoreBrowserTerminal.ts#L544)
- copy は `.xterm` 要素で受け、xterm 自身の選択がある時だけ clipboard を上書きする: [CoreBrowserTerminal.ts#L334-L341](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/CoreBrowserTerminal.ts#L334-L341)、[src/browser/Clipboard.ts#L32-L38](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/Clipboard.ts#L32-L38)。(A) (B1) では xterm 自身の選択が無いので干渉しなかった (観測でも clipboard は DOM の選択どおり)
- blur で全行、focus でカーソル行を描き直す (行の `replaceChildren`): [DomRenderer.ts#L329-L337](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/renderer/dom/DomRenderer.ts#L329-L337)。(B1) で行 DOM 上の選択が focus の移動や出力で消えるのはこの描き直しで text node が置き換わるため (Chromium で xterm に focus がある時に選択できないのもこれと見られるが、どの描き直しが効いているかまでは切り分けていない)

## この案が成り立つ条件

(A) クローン層について、headless Chromium / WebKit で観測した範囲:

- コピー結果は xterm 自身の選択と全データで一致した (行末の書いたスペースは入る、書いていない行末は入らない、行の途中の未書き込みセルはスペース、背景色だけのセルは入らない、罫線の内側の空白はスペースで入る、全角・曖昧幅・絵文字・結合文字はそのまま)。条件は、未書き込みセルの扱いを `getChars() === ''` で `translateToString(true)` と同じにすること、論理行ごとに 1 つのブロックに入れること (`wrap=join`)。WebKit では xterm 自身の選択より正しく折り返しの 3 行目まで入った
- 層の位置はセル格子に一致した (箱・文字の左端とも差 0、行の上端も差 0)。条件は、セルごとに幅固定の inline-block にすること、送り幅を `letter-spacing` でセル幅に揃えること (`fit=1`。揃えないと全角で選択ハイライトの右端と当たり判定がセルとずれる)
- 層を重ねたまま、click 後のキー入力・IME 合成・wheel スクロールは xterm に届いた。条件は、層で止めるのは mousedown / mouseup / click 系だけにして wheel と key は止めないこと、何も選ばれていない click の mouseup で `term.focus()` を呼ぶこと
- 選択中の出力で選択を失わない条件は、選択がある間は層を作り直さないこと (`freeze=on`)

## 成り立たない点 (事実)

- **アプリのマウス要求と両立しない**: 層が mousedown を止めるので、1002+1006 中でも TUI にマウス報告が 1 個も出ない。転送オン (= アプリ要求ありで転送中) の間は層を `pointer-events: none` にするなど、マウスモードの切り替えと連動させる必要がある (本 PoC では未実装・未検証)
- **選択を始めると focus が端末から外れる**: ドラッグ選択の直後のキー入力は xterm に届かない (focus が `BODY`)。選択後に `term.focus()` を呼ぶと選択そのものが消え、コピーもできない (`focus=always` で観測)。選択中にキー入力したい場合の扱いは別に設計が要る
- **選択中の層は古くなる**: `freeze=on` では選択している間の出力が層に反映されない (選択している行が書き換わっても、コピーは書き換え前の文字)。`freeze=off` にすると出力のたびに選択が消える
- **選択できるのは表示中の行だけ**: 層は viewport の行しか作らない。ドラッグで画面外へ選択を広げる (xterm 自身のドラッグスクロール) は無い。選択したままスクロールした時の振る舞いは未検証
- **選択の境界の規則がブラウザ標準になる**: 終点が全角文字の中点より左なら、その文字は入らない (xterm は触れたセルを入れる)。上の「部分: 行 1 の 5 桁で終わる」
- **Chromium: click の直後に始めたドラッグで選択できないことがある**: xterm に focus がある状態で端末を click してから 0.5 秒以内に始めたドラッグは、5 回中 2〜3 回選択されなかった (0.6 秒以上空けると毎回選択できた、WebKit は毎回選択できた)。原因は切り分けていない
- **幅の扱いは xterm の幅表に従う**: `😀` `✅` などは xterm 既定 (Unicode 6) でセル幅 1。層も同じ幅で作るので xterm とは揃うが、グリフはセルからはみ出し、選択ハイライトはセル幅 1 で出る

(B) xterm の行 DOM を選択可能にする案:

- (B0) CSS だけでは何も変わらない。行 DOM は `pointer-events: none` で、xterm の mousedown が `preventDefault` して自前の選択を始める
- (B1) xterm の mousedown を止めれば行 DOM 上でブラウザ標準の選択はできるが、(1) 行 DOM が物理行ごとの div なので折り返しは必ず改行になる、(2) xterm の描画が背景色だけのセルまでスペースで出すので、EL で消した行末が空白としてコピーされる、(3) 選択中の行の再描画 (出力・focus・blur) で選択が消える、(4) Chromium では xterm に focus がある状態から選択できない、(5) ダブル / トリプルクリックで何も選ばれない。(1)(2)(3) は xterm の DOM renderer の作り (行の `replaceChildren`、`getNoBgTrimmedLength`) で決まり、公開 option では変えられない
- canvas / webgl renderer では行 DOM 自体が無いので、(B) は DOM renderer に縛られる

## 未検証

- iOS Safari / iPadOS の長押しで標準の選択が始まるか、選択ハンドル・ルーペ・コピーメニュー (下の手順で kawaz が確認)
- 実ポインタ・実トラックパッド・実 IME (headless では合成イベント。IME は Chromium の CDP 合成のみ、WebKit は未計測)
- 右クリックのコンテキストメニューからのコピー、Linux / Windows の実 OS
- 選択したままのスクロール、resize・フォント変更時の層の作り直し、alt screen
- DOM renderer 以外 (webgl 等) と重ねた時の位置 (層はセル格子に合わせて作るので、格子どおりに描く renderer なら揃う見込みだが観測していない)
- 1 万行級の scrollback や高頻度出力での層の作り直しのコスト

## iOS 実機での確認手順

1. Mac で `docs/research/poc/` を配信する (`../2026-10-04-xterm-mouse-mode-toggle/lib/` を相対参照するので、本ディレクトリではなく 1 つ上で起動する)。例: `docs/research/poc/` で `python3 -m http.server 8765 --bind 0.0.0.0`
2. 同じ LAN の iPhone / iPad の Safari で `http://<Mac の LAN IP>:8765/2026-10-04-terminal-text-selection-layer/index.html?mode=A` を開く。`&show=1` を付けると層の文字が赤く見える
3. 端末の文字を長押し → 標準の選択 (ハンドル・ルーペ) が始まるか
4. ハンドルを動かして範囲を調整 → 青いハイライトがセルと揃って動くか (全角・曖昧幅・絵文字の行 14 も)
5. コピーメニューでコピー → 下のテキストエリアに貼り付け、JSON 表示を上の「コピー結果」表の期待と比べる (特に 1-wrap が 1 行につながるか、2a/2b、3b、4b、6)
6. 何も選ばずに端末を tap → ソフトキーボードが出て、打った文字がログに `data` で出るか
7. `?mode=B1` でも 3〜5 を繰り返す。`?mode=none` (xterm 自身) は長押しで何が起きるかだけ見る
8. 結果を下の表に書く

| 確認 | (A) iPhone Safari | (A) iPad Safari | (B1) iPhone Safari | (B1) iPad Safari | none iPhone Safari |
|---|---|---|---|---|---|
| 長押しで標準の選択が始まるか | | | | | |
| ハンドルで範囲を調整できるか・ハイライトがセルと揃うか | | | | | |
| コピー結果 (1-wrap / 2a / 2b / 3b / 4b / 6) | | | | | |
| 選ばずに tap でキーボードが出て入力が届くか | | | | | |
