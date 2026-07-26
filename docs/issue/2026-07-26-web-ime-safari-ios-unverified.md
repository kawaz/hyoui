---
title: web ターミナルの IME 追従: 実機 Safari/iOS での未検証範囲
status: open
category: task
created: 2026-07-26T12:27:20+09:00
last_read:
open_entered: 2026-07-26T12:27:20+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 実機フィードバック 2026-07-26 (IME 変換位置が追従しない) の修正時に残った未検証範囲
---

# web ターミナルの IME 追従: 実機 Safari/iOS での未検証範囲

## 概要

kawaz 実機フィードバック「web ターミナルで日本語入力すると IME の変換中テキスト / 変換候補
ウィンドウがカーソル位置に追従しない」の原因を特定し、`crates/hyoui-web/assets/session.js`
で修正済み。ただし検証は Chromium (playwright + CDP `Input.imeSetComposition`) のみで、
実機 Safari / iOS Safari が未検証として残る。

## 背景

### 特定した原因 (2 件、いずれも vendored xterm.js v5.3.0 側の挙動)

OS の IME 候補ウィンドウは「focus 中の editable 要素の caret 座標」に出る。xterm.js は
隠し textarea (`.xterm-helper-textarea`) を 1 セル幅でカーソル位置へ動かすことでこれを
実現しているが、2 経路で崩れる。

**(A) textarea.value が確定文字列を溜め続ける**

- CompositionHelper は compositionend 後も value をクリアしない。クリアするのは
  Enter / Ctrl-C の keydown 経路 (xterm.js `_keyDown`) だけだが、IME 確定の Enter は
  keyCode 229 で弾かれるためそこに到達しない。
- 結果、同じ行で変換を重ねるほど value が伸び、幅 1 セルの textarea 内で content が
  右へ溢れる。実測: 5 語確定で scrollWidth 279px / clientWidth 8px = overflow 271px。
  caret は content 座標の右端に居るので、OS はセル位置ではなく溢れた先に候補を出す。
  これが kawaz の見た「変換位置が画面の別の場所に出る」の主因。

**(B) resize 後に textarea が旧セル座標に取り残される**

- xterm.js は textarea を `onCursorMove` でしか同期しない (`_syncTextArea`)。resize で
  セル幅が変わってもカーソルの行列が変わらなければ move が発火せず、旧 metrics の座標が
  残る。実測: viewport 1280→900px で 5.8px ズレ。
- embed (iframe) はホスト側レイアウトで頻繁にリサイズされるためこの経路を踏みやすい。

### 修正内容

session.js の `term.open()` 直後に、公開イベント (onRender / onResize) + 内部メソッド
呼び出しで外側から補正する層を追加。vendored bundle は minify 済みでパッチ当てが
バージョン更新のたびに再適用必要になるため、外側補正を選択 (rationale はコード内コメント
にも記載済み)。

- (A) compositionend 後の `setTimeout(0)` で textarea.value を空へ戻す (CompositionHelper
  が value を読んで daemon へ送る処理も `setTimeout(0)` 予約なので、先にクリアすると入力が
  消える。順序を実測で確認済み)
- (B) onResize / onRender で `_syncTextArea()` を呼び直す。変換中 (isComposing) は
  CompositionHelper が textarea の幅・位置を変換文字列に合わせて拡張しているため触らない
- 内部 API は存在チェック + try/catch で包み、vendor 差し替え時は追従を諦めて xterm.js
  本来の挙動に戻す (silently 壊れるより安全)

### Chromium での検証結果 (すべて clean context、pageErrors 0)

xterm 自身の cell metrics (`_renderService.dimensions.css.cell`) を ground truth にした実測。

- 6 語連続確定: textarea overflow 最大 0px (修正前は 271px)、composition-view のカーソル
  からのズレ最大 0.02px
- resize (157→124 cols, カーソル移動なし): textarea ズレ 0.00px (修正前 5.8px)
- TUI redraw 中 (変換中に spinner が別行を書き換え、DECSC/DECRC でカーソル退避・復帰):
  ズレ -0.01px
- embed iframe + ホスト resize (160→115 cols): overflow 0px、ズレ -0.01px
- 実 WS 経由の end-to-end: `echo 日本語入力テスト` を IME で入力 → bash が
  `日本語入力テスト` を出力。入力欠落なし
- onRender の負荷: 3000 行バーストで render 2 回 (coalesce される) のため常時 hook の
  コストは無視できる
- iPad user agent + isMobile/hasTouch でも同様に green (ただしエンジンは Chromium のまま
  = 下記の未検証範囲)

### 未検証範囲 (= 本 issue の本体)

検証はすべて Chromium (playwright) + CDP `Input.imeSetComposition` による合成 IME。
以下は原理的に確認できていない:

- 実機 macOS Safari (WebKit) の IME。WebKit は composition event の発火順序が Chromium
  と異なることが知られる
- 実機 iOS Safari (iPhone / iPad)。モバイル Safari は候補バーが要素位置を無視してビュー
  ポート下端に固定される可能性があり、その場合は本修正では解決できない領域になる
  (= xterm.js 側でなくプラットフォーム制約)
- 実機の OS IME (ことえり / Google 日本語入力等)。CDP の合成 composition は実 IME の
  挙動を完全には再現しない
- 上記のうち特に「iOS Safari の候補ウィンドウが要素座標に従うか」は、従わないなら
  「ここまでしか出来ない」と明記して close する判断もありうる

## 受け入れ条件

- [ ] 実機 macOS Safari で、同一行に IME 確定を複数回重ねた後の変換候補位置がカーソルに
  追従するか確認
- [ ] 実機 macOS Safari で、ウィンドウ resize 後 (カーソル移動なし) の変換候補位置を確認
- [ ] 実機 iOS Safari (iPad) の embed (`?embed=1`) で同様に確認
- [ ] 追従しない項目があればプラットフォーム制約か実装不備かを切り分け、制約なら
  「対応範囲外」として明記
- [ ] すべて問題なければ検証結果を記録して close

## TODO

<!-- wip 時のみ -->
