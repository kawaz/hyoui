---
title: web ターミナルの初回 fit が webfont 読み込み前のセル寸法で固定される
status: open
category: bug
created: 2026-07-30T11:17:33+09:00
last_read: 2026-07-30T11:17:33+09:00
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
