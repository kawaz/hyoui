# PoC: 親 frame のソフトキーで子 iframe の focus とソフトキーボードが維持されるか

web UI 作り直し (最上位 window → タブ → pane の iframe 木、ソフトキーパネルは最上位 frame に 1 つ、postMessage でフォーカス中 pane へ送る) の前提検証。親 frame のボタンをタップした瞬間に子 iframe 内の textarea (xterm の helper textarea) から focus が外れ、iOS のソフトキーボードが閉じるかどうかを、ボタン実装方式ごとに比べる。

## ファイル

| ファイル | 役割 |
|---|---|
| `index.html` / `index.js` | 親 frame。上部に pane A / pane B の iframe、下部に固定のソフトキーパネル (方式 (a)〜(e) × キー 5 個) |
| `pane.html` | 子 frame (素の textarea 版) |
| `pane-xterm.html` | 子 frame (xterm.js 版、`crates/hyoui-web/assets/vendor/xterm.js` を相対参照) |
| `pane-common.js` | 子 frame 共通処理 (focus/blur 通知、キー受信、状態問い合わせへの応答、ログ) |
| `manifest.webmanifest` | iPad の「ホーム画面に追加」で PWA (standalone) として開くための最小 manifest |

pane A は常に textarea 版。pane B は既定で xterm.js 版、`?b=textarea` で textarea 版に切り替わる (画面上部のリンクからも切替可)。

## ボタン方式

| 方式 | 実装 |
|---|---|
| (a) | 素の `<button>` + `click` で送信 |
| (b) | `<button>` + `pointerdown` で `preventDefault()` して送信 |
| (c) | `<button>` + `touchstart` (passive: false) / `mousedown` で `preventDefault()` して送信 |
| (d) | `<button tabindex="-1">` + (b) と同じ |
| (e) | 参考: `<div role="button">` + `click` (focus 不可能な要素なら preventDefault 無しでも focus が残るかの比較) |

## 画面の見方

- 上部ステータス: `focus frame` (今 focus を持つ pane、親側の要素に focus があれば `parent:BUTTON(a)` 等)、`最後の pane` (キーの送信先)、`キーボード(推定)`
- キーボード推定: `visualViewport.height` の向きごとの最大値を「キーボード非表示時の高さ」とみなし、それより 120px 以上縮んでいれば「表示」。初回はキーボードを出す前に一度画面を開いた状態で基準が取られる。回転したら向きごとに基準を取り直す
- パネル右端の `OK` / `NG`: その方式で直近にタップしたキーの判定。子の入力要素が active かつ `document.hasFocus()` が、キー受信時 (recv)・指を離した時 (up)・click 時・2 フレーム後 (settled) の全時点で真なら OK。子から blur 通知が来たら NG
- 下のログ: `#<seq> <方式> <キー> -> <pane>: OK/NG [時点ごとの o/x]`。子 pane 側のログには focus / blur / 受信キーが出る
- iOS ではソフトキーボード表示中も layout viewport が縮まないため、パネルは `visualViewport` に追従させてキーボードの直上に持ち上げている

使っている API: `postMessage`、`visualViewport`、Pointer Events、Touch Events。いずれも secure context 不要なので HTTP のままでよい。

## 起動

リポ root で静的サーバを起動する (xterm.js を `crates/` から相対参照するため、root はリポ root)。

```sh
python3 -m http.server 18765 --bind 0.0.0.0
```

開く URL (Mac の IP は環境で変わる。`ipconfig getifaddr en0` / `tailscale ip -4` で確認):

- 同一 LAN: `http://<Mac の LAN IP>:18765/docs/research/poc/2026-10-04-softkey-iframe-focus/`
- tailnet: `http://<Mac の tailnet IP>:18765/docs/research/poc/2026-10-04-softkey-iframe-focus/`

## iPhone / iPad での確認手順

1. 上の URL を Safari で開く (キーボードを出す前に一度開いた状態にして、キーボード推定の基準を取る)
2. pane A の textarea をタップしてソフトキーボードを出す。上部が `focus frame: A` / `キーボード(推定): 表示` になることを確認
3. 方式 (a) の行のキー (例: `→`) をタップし、次を見る
   - ソフトキーボードが閉じたか (目視、上部の推定表示とも突き合わせ)
   - パネル右端が OK / NG のどちらか
   - pane A の textarea 末尾に `[→]` が追記されたか (= キーが届いたか)
4. キーボードが閉じた場合は 2 からやり直し、方式 (b)〜(e) で同じことを繰り返す
5. pane B (xterm.js 版) をタップして 3〜4 を繰り返す (helper textarea で同じ挙動になるか)
6. 余力があれば、同じ方式のキーを素早く連打したとき・長押ししたときにキーボードが閉じないか、ボタンが長押しメニューや拡大鏡を出さないかも見る
7. iPad では Safari に加えて「共有 → ホーム画面に追加」で PWA として開いた状態でも 2〜5 を行う

結果は下の表の `キーボード維持` 欄に ○ / × を、気づいた点 (二重送信、キーが届かない、パネルがキーボードに隠れる等) を備考に書く。

## 結果表

### デスクトップ (headless、playwright 経由で実測)

Mac 上の Playwright 同梱 Chromium / WebKit を headless で起動し、pane をクリック (touch モードは tap) して focus させた後、各方式のボタンをクリック / tap して判定欄を読んだ。WebKit は Safari と同じエンジンだが Safari.app そのものではなく、またソフトキーボードは存在しないので focus 維持のみの観測。

| 方式 | Chromium mouse | Chromium touch (hasTouch) | WebKit mouse | WebKit touch (hasTouch) |
|---|---|---|---|---|
| (a) button + click | NG | NG | NG | NG |
| (b) pointerdown pD | OK | OK | OK | OK |
| (c) mousedown/touchstart pD | OK | OK | OK | OK |
| (d) tabindex=-1 + pointerdown pD | OK | OK | OK | OK |
| (e) div role=button + click | NG | NG | NG | NG |

pane A (textarea) / pane B (xterm.js helper textarea) で全セル同じ結果。NG の行は recv の時点で既に子が `hasFocus=false active=BODY` (= mousedown / pointerdown の既定動作で focus が親 document へ移った後に click が来る)。(e) の div も focus 不可能だが、クリックで focus が親 document 側 (body) へ移るため子は blur する。

### iOS (kawaz 確認待ち)

| 方式 | iPhone Safari | iPad Safari | iPad PWA | 備考 |
|---|---|---|---|---|
| (a) button + click | | キーボード閉じる | キーボード閉じる (pane B xterm でも同じ。キー自体は届く) | kawaz 実機 2026-10-04 |
| (b) pointerdown pD | | キーボード維持 | キーボード維持 (pane B xterm でも同じ) | kawaz 実機 2026-10-04 |
| (c) mousedown/touchstart pD | | キーボード維持 | キーボード維持 (pane B xterm でも同じ) | kawaz 実機 2026-10-04 |
| (d) tabindex=-1 + pointerdown pD | | キーボード維持 | キーボード維持 (pane B xterm でも同じ) | kawaz 実機 2026-10-04 |
| (e) div role=button + click | | キーボード閉じる | キーボード閉じる (キー自体は届く) | kawaz 実機 2026-10-04 |

pane B (xterm.js) で結果が A と違った場合は備考に書く。
