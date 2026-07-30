---
title: web ターミナルの初回 fit が webfont 読み込み前のセル寸法で固定される
status: open
category: bug
created: 2026-07-30T11:17:33+09:00
last_read: 2026-07-30T11:43:53+09:00
open_entered: 2026-07-30T11:17:33+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 申告「ウィンドウサイズのリサイズが上手く機能しないことが増えた気がする」の実機調査 (2026-07-30)
---

# web ターミナルの初回 fit が webfont 読み込み前のセル寸法で固定される

## 症状

ページ読み込み時に `HackGen Console NF` の webfont が未ロードだと、xterm.js は
fallback フォントのセル寸法で初回 grid を確定する。webfont の読み込み完了だけでは
セル寸法も cols/rows も再計算されず、次に viewport または `#term` の実寸が変わるまで
誤った grid が残る。

`?resize=1` では、この誤った初回 cols/rows が PTY resize API に送信される。
その後のウィンドウ resize では xterm.js 内部のセル寸法が更新され、150 ms debounce 後に
正しい cols/rows が送られるため、初回表示から最初の resize にかけて TUI が不自然に
再レイアウトされる。

## 判明した事実

- Chromium で font request を遅延させると再現率 100%。キャッシュ無効化時も再現した。
- `document.fonts.ready` の完了だけでは xterm.js の内部セル寸法は更新されない。
- font 読み込み後に `FitAddon.fit()`、`term.refresh()`、同値の `fontFamily` option 再設定を
  行っても再測定されない。
- viewport の実寸変更を xterm.js 内部の `ResizeObserver` が検知した時に初めてセル寸法が
  fallback の約 `7.81px` から HackGen の約 `6.84px` へ変わる。その 150 ms 後、hyoui 側の
  `scheduleFit()` が cols/rows を更新する。
- v0.9.25 と現行版の既定表示は同一条件・同一数値になった。v0.9.26 の `unicode-range` や
  v0.9.29 の表示パラメータが race を新規発生させた regression ではない。
- v0.9.29 の `fontsize` / `lineheight` 指定でも同じ race が発生する。表示条件の変更や
  `?resize=1` により誤った初回サイズが見えやすくなった可能性はある。

## 観測マトリクス

Chrome + playwright-cli、gateway は `/opt/homebrew/bin/hyoui` 0.9.29、調査専用
`cat` session を使用。入力は送らず、font cache 無効化または woff2 応答遅延で測定した。

| assets / query | viewport | font 状態 | xterm cell | grid | resize POST |
|---|---:|---|---:|---:|---|
| v0.9.25、既定 | 1280x900 初回 | loading → loaded | 7.815px のまま | 157x24 のまま | off |
| 現行、既定 | 1280x900 初回 | loading → loaded | 7.815px のまま | 157x24 のまま | off |
| 現行、既定 | 900x650、resize 後 50 ms | loaded | 6.841px | 157x24 (fit 前) | off |
| 現行、既定 | 900x650、resize 後 300 ms | loaded | 6.847px | 124x24 | off |
| 現行、`resize=1` | 1200x800 初回 | loaded | 7.810px (古い) | 147x24 | `147x24` |
| 現行、`resize=1` | 800x600、resize 後 300 ms | loaded | 6.845px | 110x24 | `110x24` |
| 現行、`embed=1` | 1200x800 初回 | loaded | 7.813px (古い) | 150x52 | off |
| 現行、`embed=1` | 800x600、resize 後 300 ms | loaded | 6.848px | 112x39 | off |
| 現行、`embed=1&resize=1&fontsize=20&lineheight=1.4` | 1280x900 初回 | loading → loaded | 12.029px のまま | 104x26 | `104x26` |
| 同上 | 900x650、resize 後 350 ms | loaded | 10.561px | 82x21 | `82x21` |
| 同上 | 1440x1000、resize 後 350 ms | loaded | 10.564px | 133x32 | `133x32` |

`embed=1` の rows は viewport 高さに追従し、通常表示の rows は `#term` の内容高により
24 のままなので、この差自体は正常。いずれも最初の実寸変更後は cols/rows が追従した。

## 原因

`crates/hyoui-web/assets/session.js` は `term.open(termEl)` を font load 待ちなしで呼ぶ
(`session.js:174-190`)。初回 fit も layout の 1 tick だけを待つ `setTimeout(..., 0)` で、
使用 font の準備は待たない (`session.js:400-410`)。

resize 時は window event と `#term` の `ResizeObserver` を 150 ms debounce して
`fitAddon.fit()` する (`session.js:385-416`)。この経路は実寸変更後には正常に働くが、
font load 完了は window resize でも element resize でもないため発火しない。

`font-display: block` (`crates/hyoui-web/assets/style.css`) は glyph の表示方針であり、
JavaScript の cell measurement や `term.open()` を block しない。

## 修正案

推奨は **`term.open()` 前に、実際に指定された font shorthand を
`document.fonts.load()` で明示ロードする**こと。

```js
await document.fonts.load(`${fontSize}px ${fontFamily}`);
term.open(termEl);
```

IIFE を async 化してこの順序にした Playwright route PoC では、woff2 を 1.2 秒遅延しても
初回から HackGen のセル幅 `6.844px` を使い、1280x900 で `180x24` に fit した。
font load 後の再 fit だけでは xterm.js の stale な内部セル寸法を使い続けるため不十分。

実装時は `document.fonts.load()` の reject を捕捉し、font load failure 時は fallback
フォントで `term.open()` を続行する。query の `fontfamily` を含む実際の font shorthand を
渡し、既定 HackGen だけを特別扱いしない。

## 修正後の検証条件

- woff2 を遅延した cold load / cache 無効化 /通常 load
- 既定 / `fontsize` + `lineheight` / custom `fontfamily`
- 通常表示 / `embed=1`
- `resize` off / `resize=1`
- 1280x900 → 900x650 → 1440x1000 の複数 resize
- 初回 resize POST が font load 後の cell metrics から算出されていること
- font request failure 時に fallback font で terminal が起動すること

## 追加観測: embed 縮小時の折り返し

同一 origin の親ページに `embed=1` iframe を置き、幅 1200px → 500px に縮小して Playwright で DOM と xterm buffer を観測した。

| 時点 | iframe | xterm grid | row 0 | #term client/scroll width | 表示 |
| 初期 | 1200x700 | 171x45 | 171文字、1170px | 1185/1185px | 1行 |
| 縮小 +50ms | 500x700 | 171x45 | 171文字、1170px | 485/1174px | はみ出し部分が clip |
| 縮小 +450ms | 500x700 | 68x45 | 68文字、465px | 485/485px | xterm buffer が 68列へ reflow、複数行化 |

`.xterm-rows` と各 row div の computed `white-space` は `pre`、`overflow-wrap` と `word-break` は normal。CSS の自然折り返しではない。`FitAddon.fit()` が `term.resize(68,45)` を呼び、xterm.js の normal-buffer reflow が 171文字の行を 68文字単位へ再構成する。

縮小後 fit 前は `#term.scrollWidth=1174px` を既に持つが、`style.css:245-255` の `overflow:hidden` が横スクロールを隠して clip する。`body.embed #term { overflow-x:auto; overflow-y:hidden; }` の動的 PoC で clientWidth 485px / scrollWidth 1174px の横スクロール成立を確認した。`style.css:237-242` の `.xterm* { width:100%!important }` を外す必要はない。

ただし CSS だけでは約150ms後の `fit()` による reflow を止められない。cols 不一致中に折り返さない要件には、`fitAddon.proposeDimensions()` で目標だけ計算し、PTY resize 成功前は `term.resize()` / `fitAddon.fit()` を呼ばず旧 grid を維持する JS 変更が必要。resize off または失敗時は旧 grid + 横スクロール、resize 成功後だけ `term.resize(cols,rows)` する。

## 追加で判明した独立原因: resize endpoint の 204 偽成功

`embed=1&resize=1` の iframe を 1200px → 500px に縮小すると browser grid は171x45→68x45になり、HTTP POST も両方発行された。しかし daemon window size は171x45のまま。後続POSTは204でも効いていなかった。

原因:
- persistent WS bridge は `crates/hyoui-web/src/ws_attach.rs:130-138` で `Mode::Rw` 接続し leader を保持する。
- `post_resize` は毎回短命 `Mode::Rw` connection を作る (`crates/hyoui-web/src/lib.rs:518-535`)。既存 leader がいるため `HandshakeResponse.leader=false`。
- daemon は resize を leader 限定で拒否 (`crates/hyoui/src/daemon/control.rs:645-667`)。
- `resize_blocking` は返ってきた `ControlMessage::Error(mode.not-leader)` を `Ok(_) => continue` で読み飛ばし、後続 StatusResponse を受けて成功扱いする (`lib.rs:536-553`)。そのためHTTP 204になる。
- browser 初回 resize は WS handshake より先に短命 connection が leader を取れる race があり効くことがあるが、WS確立後の resize は恒常的に無効。

WS browser を閉じて leader 不在にした実測では、連続 POST 100x40 → 70x30 が両方 daemon snapshot に反映した。persistent WS leader が直接原因。

修正案:
1. WS bridge を `Mode::RwNoLeader` にする。WS は raw input を送るだけで Resize message を送らないため leader を持つ責務がない。これにより他の leader がいない通常 web 利用では短命 resize connection が leader になれる。
2. `resize_blocking` は接続直後に `conn.response.leader` を確認し、false なら明示的な conflict を返す。また待機 loop で `ControlMessage::Error` を成功扱いせず error にする。別の CLI leader がいる場合に silent 204 を返さない。
3. frontend は `proposeDimensions` → resize POST → 204確認 → `term.resize` の順に変更し、失敗時は旧 grid + 横スクロールを維持する。連続 resize は応答待ち中の最新寸法を coalesce し、古い応答で新しい寸法へ戻さない。

font-load race と resize leader bug は別原因。font race は初回寸法を誤らせ、leader bug は2回目以降のPTY追従を止める。後者により xterm reflow だけが継続して、報告された「ずっと折り返されたまま」になる。
