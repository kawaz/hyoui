# PoC: xterm.js 6.0.0 でアプリのマウス要求を保ったまま UI 側で転送だけを止める

web UI 作り直し (`docs/issue/2026-10-04-design-webui-terminal-app-rework.md` の「マウスモードの切り替え」節) の前提検証。アプリのマウス追跡要求 (DECSET 1000 / 1002 / 1003、encoding 1006) に触れずに、UI のトグルで PTY へのマウス報告の転送だけを止め、その間は通常のドラッグ選択・コピー・縦スクロール (wheel / touch)・カルーセルの横フリックに戻せるかを確かめる。調査側の位置付けは `docs/research/2026-10-04-web-terminal-renderer-survey.md` の「深掘り B」と「判断の軸と暫定推奨」最終段落の gate。

## ファイル

| ファイル | 役割 |
|---|---|
| `index.html` / `index.js` | PoC 本体 (ビルドなし、ES module)。PTY は無く、ページ内の疑似アプリが `term.write` で DECSET/DECRST を流す |
| `lib/xterm.mjs` / `lib/xterm.css` / `lib/LICENSE` | xterm.js 6.0.0。npm `@xterm/xterm@6.0.0` の tarball (`https://registry.npmjs.org/@xterm/xterm/-/xterm-6.0.0.tgz`、integrity `sha512-TQwDdQGtwwDt+2cgKDLn0IRaSxYu1tSUjgKarSDkUM0ZNiSRXFpjxEsvc/Zgc5kq5omJ+V0a8/kIM2WD3sMOYg==`、tarball の sha256 `908e66e04af6c8dc6b00dd3b54de088e2e81e5ed866284fd6c2fb3c2d1c7a3f6`) の `lib/xterm.mjs`・`css/xterm.css`・`LICENSE` を無改変で配置。source map は同梱しない |

## 画面

- 疑似アプリ: `?1000` `?1002` `?1003` `?1006` `?1049 alt` `?1 DECCKM` の各ボタンが `\x1b[?Nh` / `\x1b[?Nl` を交互に `term.write` する。`行を 200 行書く` で scrollback を作る
- UI マウスモード: `転送: オン / オフ`。表示は「要求なし」「要求あり・転送中」「要求あり・遮断中」の 3 状態 + `mouseTrackingMode` (= アプリ要求、xterm の公開 `term.modes.mouseTrackingMode`)・buffer 種別・DECCKM
- オフ時の方式: (1)〜(4) を選ぶ (累積、下表)
- ログ: `onData` / `onBinary` に来たもの (= PTY に送られるはずのもの)。マウス報告の形 (`ESC [ M` + 3 byte、`ESC [ <` … `M`/`m`) と `onBinary` は `MOUSE` と表示
- 端末の下に 2 枚目の slide を置いたカルーセル (`scroll-snap-type: x mandatory`)。横フリックでの移動はブラウザに任せる
- `?platform=Linux%20x86_64` で `navigator.platform` を差し替える (xterm は `navigator.platform` で Mac 判定するので、非 Mac 経路の検証用)

## 方式 (オフ = 遮断時の実装。いずれも xterm の要素より外側の host 要素で capture phase に listener を置く)

| 方式 | 実装 |
|---|---|
| (1) | `mousedown`・ボタンを押していない `mousemove`・`wheel` を止める (`stopPropagation` + `preventDefault`) だけ |
| (2) | (1) + 止めた左ボタンの `mousedown` を修飾キー付きの合成イベントとして元の target に dispatch し直す (Mac は `altKey: true` + `macOptionClickForcesSelection: true`、それ以外は `shiftKey: true`)。xterm の `SelectionService.shouldForceSelection` 経路で選択させる。Mac で本物の Option を押していた時は、続く `mouseup` を document の capture で止め `altKey: false` にして渡し直す (後述の穴 1) |
| (3) | (2) + `wheel` を自前で変換。normal buffer は `term.scrollLines(n)`、alt buffer は矢印キー列 (`ESC [ A/B`、DECCKM 中は `ESC O A/B`) を `term.input(seq, false)` で `onData` 経路に流す。delta は `deltaMode` を px に直してセル高で割り、端数を持ち越す |
| (4) | (3) + touch の縦スワイプを (3) と同じ変換で自前処理。横は `touch-action: pan-x` でブラウザ (scroll-snap) に任せる。xterm 6.0.0 は touch で scroll しないため、(4) は「要求なし」の時も縦スワイプを扱う。「要求あり・転送中」の時だけ xterm に任せる |

どの方式もアプリへ DECSET を書かず xterm の内部状態にも触れない。xterm に対して使うのは公開 API (`modes`、`scrollLines`、`input`、`clearSelection`、`macOptionClickForcesSelection`) と DOM イベントだけ。オフ→オン切り替え時は `term.clearSelection()` を呼ぶ。

## ソースでの根拠 (xterm.js tag 6.0.0、commit f447274f430fd22513f6adbf9862d19524471c04)

- DECSET 1000 系が入ると `onProtocolChange` で selection service を `disable()`、要素に `mousemove` / `wheel` を結線、`mouseup` / drag 用 `mousemove` は mousedown 時に document へ結線: [src/browser/CoreBrowserTerminal.ts#L727-L774](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/CoreBrowserTerminal.ts#L727-L774)
- 常時結線の `mousedown` は `!areMouseEventsActive || shouldForceSelection(ev)` なら報告しない: [#L780-L803](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/CoreBrowserTerminal.ts#L779-L804)。選択側の `mousedown` 結線は [#L544](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/CoreBrowserTerminal.ts#L544)
- 要求なし (かつ wheel 要求なし) で scrollback が無い buffer では wheel を矢印キー列に変換して `triggerDataEvent`: [#L806-L840](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/CoreBrowserTerminal.ts#L806-L841)。Viewport は wheel 要求中 `handleMouseWheel: false`: [src/browser/Viewport.ts#L65-L70](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/Viewport.ts#L65-L70)
- `shouldForceSelection`: Mac は `altKey && macOptionClickForcesSelection`、それ以外は `shiftKey`: [src/browser/services/SelectionService.ts#L437-L443](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L437-L443)。disable 中でも強制選択なら `stopPropagation` して選択開始: [#L449-L494](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L449-L494)。ドラッグ中の document `mousemove` は `stopImmediatePropagation`: [#L600-L604](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L600-L604)
- Mac の Option は `macOptionClickForcesSelection` の時 column select にならない: [#L591-L593](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L591-L593)
- `mouseup` で `event.altKey && altClickMovesCursor` (既定 true) かつ選択 1 文字以下・500ms 以内なら cursor 移動の矢印キー列を `triggerDataEvent`: [#L703-L720](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/browser/services/SelectionService.ts#L703-L720)
- 報告の出口: 従来 encoding は `triggerBinaryEvent` (= `onBinary`)、SGR 等は `triggerDataEvent` (= `onData`): [src/common/services/CoreMouseService.ts#L325-L332](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/services/CoreMouseService.ts#L325-L332)
- Mac 判定は `navigator.platform`: [src/common/Platform.ts#L38](https://github.com/xtermjs/xterm.js/blob/6.0.0/src/common/Platform.ts#L38)
- 公開型: `mouseTrackingMode` [typings/xterm.d.ts#L1932](https://github.com/xtermjs/xterm.js/blob/6.0.0/typings/xterm.d.ts#L1932)、`macOptionClickForcesSelection` [#L195](https://github.com/xtermjs/xterm.js/blob/6.0.0/typings/xterm.d.ts#L195)、`altClickMovesCursor` [#L46](https://github.com/xtermjs/xterm.js/blob/6.0.0/typings/xterm.d.ts#L46)、`input` [#L1025](https://github.com/xtermjs/xterm.js/blob/6.0.0/typings/xterm.d.ts#L1025)
- touch: xterm 6.0.0 の browser 層に touch を扱う結線は無い (vs の `Gesture` は同梱されるが scrollable element から使われていない)。下の観測でも、要求なしで縦スワイプしても viewport は動かなかった

## 観測方法

Playwright (playwright-core 1.63.0-alpha) の headless Chromium 152.0.7977.8 と headless WebKit 26.5 (いずれも macOS 上)。`navigator.platform` は実 Mac 値 (`MacIntel`) と `?platform=Linux%20x86_64` の 2 通り。計測スクリプトはリポ外に置き commit していない。

各セルは毎回ページを読み直して、200 行書く → (alt なら `?1049h` + 18 行) → DECSET を立てる → 方式を選び転送オフ、の後に:

- ドラッグ: 表示行 3 の 4〜20 桁を 6 step でドラッグし `term.getSelection()` を期待文字列 (同じ範囲の行テキスト) と比較、その間の `onData` / `onBinary` を記録
- hover: ボタンなしで 3 点へ移動
- wheel: normal は `deltaY = -300` (scrollback を見る方向)、alt は `+300`。`buffer.active.viewportY` の変化と出力
- キー: `x` を押して `onData` に `x` が出るか (キー入力は遮断しないこと)
- `modes.mouseTrackingMode` がトグル前後で同じか
- オフで別行を選択した状態から転送オンに切り替え、選択が消えるか、その直後の click で報告が通常どおり出るか

方式 × DECSET 7 通り (なし / 1000 / 1002 / 1003 / 各 +1006) × normal/alt × 2 エンジン × platform 2 通り = 224 ケース、転送オンの基準 56 ケース。加えて click 単体・ダブルクリック・右/中クリック・本物の修飾キー付き click/drag・alt screen の wheel 出力 (DECCKM 有無)・コピー・touch (Chromium の CDP `Input.dispatchTouchEvent` と Playwright の `touchscreen.tap`) を個別に観測した。

## 結果

凡例: 選択 ○ = `getSelection()` が期待文字列と一致、× = 空。報告 = ドラッグ・hover・wheel の間に出たマウス報告の数。「エンジン 4 列」は Chromium/Mac・Chromium/Linux 偽装・WebKit/Mac・WebKit/Linux 偽装で、4 列とも同じ結果だったものは 1 セルにまとめた。

### 転送オフ (遮断)

| 方式 | DECSET | screen | 選択 | 報告 | wheel | キー `x` | mode 保持 | エンジン 4 列 | iOS Safari 実機 | iPad PWA 実機 |
|---|---|---|---|---|---|---|---|---|---|---|
| (1) | なし | normal | ○ | 0 | viewport 移動 (xterm 本来) | ○ | ○ (`none`) | 同じ | | |
| (1) | なし | alt | ○ | 0 | `ESC[B` 1 個 (xterm 本来) | ○ | ○ | 同じ | | |
| (1) | 1000/1002/1003 (±1006) | normal | × | 0 | 動かない | ○ | ○ | 同じ | | |
| (1) | 1000/1002/1003 (±1006) | alt | × | 0 | 動かない・出力なし | ○ | ○ | 同じ | | |
| (2) | なし | normal/alt | ○ | 0 | (1) と同じ | ○ | ○ | 同じ | | |
| (2) | 1000/1002/1003 (±1006) | normal | ○ | 0 | 動かない | ○ | ○ | 同じ | | |
| (2) | 1000/1002/1003 (±1006) | alt | ○ | 0 | 動かない・出力なし | ○ | ○ | 同じ | | |
| (3)(4) | なし | normal/alt | ○ | 0 | (1) と同じ | ○ | ○ | 同じ | | |
| (3)(4) | 1000/1002/1003 (±1006) | normal | ○ | 0 | viewport 移動 (`scrollLines`) | ○ | ○ | 同じ (移動量のみ差、下記) | | |
| (3)(4) | 1000/1002/1003 (±1006) | alt | ○ | 0 | 矢印キー列のみ出力 | ○ | ○ | 同じ | | |

- mode 保持: 全 224 ケースでトグル前後の `mouseTrackingMode` は `none` / `vt200` / `drag` / `any` のまま変わらなかった
- オフ→オン: 全ケースで切り替え直後に選択は消え (`hasSelection()` false)、直後の click は報告が通常どおり出た (1000/1002 は press/release の 2 個、1003 は直前の move を含め 3 個)。選択の残りに起因する余分な報告・その他の byte は出なかった
- wheel の移動量 (`deltaY = -300` 1 回): xterm 本来 (要求なし) は Chromium 3 行・WebKit 22 行、方式 (3) は Chromium 18 行・WebKit 17 行。方式 (3) は px ÷ セル高で、xterm 本来の感度計算 (`consumeWheelEvent` / SmoothScrollableElement) とは揃えていない
- alt の wheel 出力 (`deltaY` を +100, +100, -100): xterm 本来 (要求なし) は 1 event につき 1 個 (`ESC[B` ×2, `ESC[A` ×1)、方式 (3) は量に比例 (Chromium `ESC[B`×6 ×2・`ESC[A`×5、WebKit も同等)。DECCKM 中はどちらも `ESC O B` / `ESC O A` になった
- 方式 (2)〜(4)、DECSET 1000 と 1003+1006、4 列すべて: click 単体は出力 0・選択なし、ダブルクリックは出力 0 で単語 `abcdefghijklmnopqrstuvwxyz` を選択、右クリック・中クリックは出力 0
- コピー (方式 (4)、1002+1006、Mac の Cmd+C、Chromium/WebKit): ドラッグで選択した文字列が `copy` イベントの `text/plain` に入り、`onData` への出力は 0。Linux 偽装の Ctrl+C は xterm の通常の動作どおり `\x03` を送った (実 OS が Mac のためブラウザの copy ショートカットにならない。Linux 実機は未検証)

### 本物の修飾キーを押しながらの click (方式 (2) 以降、転送オフ)

| 操作 | 修正前 | 修正後 (現 `index.js`) |
|---|---|---|
| Mac で Option を押したまま click | `onData` に `ESC[D` が約 3500 個 (= `altClickMovesCursor` による cursor 移動列、10727 byte)、Chromium/WebKit とも | 出力 0 |
| Mac で Option を押したままドラッグ | (未計測) | 出力 0、選択 `bcdefghijklm` |
| Linux 偽装で Shift を押したまま click | 出力 0 | 出力 0 |

### touch (Chromium の CDP touch 合成、方式 (4)、上下 200px・左右 500px スワイプ)

| 状態 | 縦スワイプ normal | 縦スワイプ alt | 横スワイプ | tap | iOS Safari 実機 | iPad PWA 実機 |
|---|---|---|---|---|---|---|
| 要求なし | 下スワイプで viewport 181→169 (方式 (4) の自前処理。xterm だけでは 181→181 で動かない) | `ESC[A` / `ESC[B` を 10 個 | カルーセル scrollLeft 0→760 | (未計測) | | |
| 要求あり (1002+1006 / 1003)・遮断中 | 181→169 | `ESC[A` / `ESC[B` を 10 個 | 0→760 | 出力 0 (Chromium/WebKit) | | |
| 要求あり・転送中 | 動かない、報告 0 | 動かない、報告 0 | 0→760 | 報告が出る (1000: `ESC[M` 2 個、1003+1006: `ESC[<35;..M` `ESC[<0;..M` `ESC[<0;..m`、Chromium/WebKit) | | |

スワイプは 1002+1006 と 1003、tap は 1000 と 1003+1006 で観測した。上方向のスワイプは viewport が最下端 (181) のため動かないのが正しい。WebKit の touch は Playwright の `tap` だけ (CDP touch が無い) なのでスワイプは Chromium のみ。

## 成立したこと

- 方式 (2) 以降で、アプリの要求 (`mouseTrackingMode`) を保ったまま、遮断中は 1000 / 1002 / 1003 × 1006 有無 × normal / alt の全組み合わせで、ドラッグ・click・ダブルクリック・hover・wheel・右/中クリックのいずれでもマウス報告が 1 byte も出ず、ドラッグ・ダブルクリックで xterm 自身の選択が働き、Cmd+C で選択文字列がコピーされた (headless Chromium / WebKit、Mac と Linux 偽装)
- 方式 (3) で遮断中の wheel が normal は scrollback 移動、alt は矢印キー列になった。方式 (4) で縦スワイプも同様、横スワイプはカルーセル移動になった
- 転送オンへ戻すと報告は基準 (転送オンのみのケース) と同じ形で出た。キー入力はどの状態でも遮断されなかった
- xterm に対して使ったのは公開 option / API と DOM イベントの合成だけで、私的 API・内部状態の書き換え・アプリへの DECSET 送出は無い

## 残る穴・未検証

1. **Mac の Option + click の cursor 移動**: xterm の `altClickMovesCursor` (既定 true) は合成した mousedown ではなく本物の mouseup の `altKey` を見る。対処しないと遮断中に `ESC[D` 等が PTY に流れる (上表で観測)。PoC は合成 mousedown に続く mouseup だけ `altKey` を外して渡し直した。代案は option `altClickMovesCursor: false` (遮断中以外の Option+click の cursor 移動も失う)
2. **転送中の touch は tap しか TUI に届かない**: xterm 6.0.0 は touch を扱わないので、要求あり・転送中の縦スワイプは何も起きない (報告もスクロールも 0)。tap はブラウザの互換 mouse イベント経由で報告になる。「オン時はタッチを TUI に渡す」をスワイプ (drag / wheel 相当) まで含めるなら、touch → mouse 報告の変換を自前で持つ必要がある。その場合も DECSET 保持と公開 API の範囲で書けるかは別途検証
3. **touch でのドラッグ選択**: xterm 6.0.0 は touch で選択しない。方式 (4) は縦スワイプを scroll に割り当てているので、touch の選択 (長押し後のドラッグ等) は別の操作設計が要る。iOS Safari の長押しによるネイティブ選択が xterm の DOM でどう振る舞うかも未確認
4. **wheel の量の感度**: 方式 (3) は xterm 本来の感度と揃えていない (Chromium で 3 行 vs 18 行)。alt の矢印キー個数も xterm 本来 (1 event 1 個) と違う。実装時は `consumeWheelEvent` 相当の挙動に合わせるか決める
5. **ドラッグ途中のトグル・要求の変化**: ドラッグ中に転送を切り替える、ドラッグ中にアプリが DECSET を変える、は未検証
6. **リンク**: OSC 8 / web-links の hover・click (Linkifier の mouse listener) と遮断の干渉は未検証
7. **headless の限界**: 実ポインタ・実トラックパッド (慣性スクロール、`deltaMode`)・実タッチ・IME は headless では再現しない。Linux / Windows の実 OS (中クリック貼り付け、Ctrl+C の扱い) も未検証
8. **iOS**: 上表の iOS Safari / iPad PWA 列は未記入 (kawaz 実機確認待ち)。iPadOS Safari は既定でデスクトップ表示のため `navigator.platform` が `MacIntel` で Mac 経路 (Option) になる見込みだが未確認

## iOS 実機での確認手順

1. Mac でこのディレクトリを配信する (例: このディレクトリで `python3 -m http.server 8765 --bind 0.0.0.0`)。同じ LAN の iPhone / iPad の Safari で `http://<Mac の LAN IP>:8765/index.html` を開く
2. `行を 200 行書く` を押し、`?1002` と `?1006` を押す (表示が「要求あり・転送中」になる)。方式は (4) のまま
3. 転送オンのまま: 端末を tap → ログに `MOUSE` 行が出るか。縦スワイプ → 何が起きるか (スクロール / ログ)。横スワイプ → 2 枚目に移るか
4. `転送: オン` を押してオフ (「要求あり・遮断中」): 縦スワイプで scrollback が動き、ログに `MOUSE` が出ないか。横スワイプで 2 枚目に移り戻れるか。tap でログが出ないか。長押し・ドラッグで選択できるか、できたらコピーできるか
5. `?1049 alt` を押して alt screen: 遮断中の縦スワイプでログに `ESC[A` / `ESC[B` (`"\u001b[A"` 等) が出るか
6. 各手順でステータス行の `mouseTrackingMode` が `drag` のままか
7. iPad はホーム画面に追加した standalone 表示でも 3〜6 を繰り返す (manifest は置いていないので Safari の「ホーム画面に追加」のブックマーク扱いになる点に注意)
8. 結果を上の結果表の iOS 列に書く
