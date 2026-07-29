---
title: web ターミナルの記号グリフ幅対策をクロスプラットフォーム化する (narrow symbol subset webfont の同梱)
status: open
category: request
created: 2026-07-29T14:05:00+09:00
last_read: 2026-07-29T14:05:00+09:00
open_entered: 2026-07-29T14:05:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 申告「web UI で記号が隣の文字と重なる」の調査中に判明した、unicode-range fallback 方式の環境依存の限界 (2026-07-29)
---

# web ターミナルの記号グリフ幅対策をクロスプラットフォーム化する

## 背景

同梱の HackGen Console NF は、xterm.js の unicode11 が幅 1 と数えるコードポイント
1657 個を**全角 advance (1080/1024em) のグリフ**で描く。1 セルしか割り当てられない
のに 2 セル分の幅で描かれるため、隣の文字と重なる (⚠ U+26A0 / ★ U+2605 /
✓ U+2713 / ① U+2460 等)。

幅の意味論は変えない方針を採った。daemon の vt100・TUI アプリ本体・xterm.js の
三者はこれらを幅 1 として一致しており (実測確認済み)、web だけ幅 2 にすると
screen state 正本からの乖離と行内の桁ずれを生むため (DR-0013 / DR-0005)。
原因がグリフの寸法である以上、フォント層で解くのが筋。

対策として `assets/style.css` の `@font-face` に `unicode-range` を入れ、該当
コードポイントを HackGen の適用範囲から外して fallback フォントに描かせている。

## 問題

**この対策は fallback 先が narrow グリフを持つ環境でしか効かない。**

ブラウザ実測 (13px、`a` の advance を 1 とした相対値):

| 文字 | HackGen | Menlo (macOS) | DejaVu Sans Mono (Linux 既定になりがち) |
|---|---|---|---|
| ⚠ U+26A0 | 2.00 | 1.00 | 2.25 |
| ★ U+2605 | 2.00 | 1.00 | 2.25 |
| ✓ U+2713 | 2.00 | 1.00 | 1.70 |

macOS / iOS では fallback 先頭の Menlo が narrow グリフを持つので機能するが、
Linux ブラウザでは DejaVu Sans Mono が全角相当で描くため**対策が無効化され、
重なりが再発する**。tailnet 内で kawaz が macOS / iOS から閲覧する主用途に
合わせた割り切りとして採用した。

## 未解決で残っている文字

`① U+2460` は macOS のどの等幅フォントも narrow グリフを持たない (Menlo /
Monaco / Andale Mono / PT Mono / Courier New すべてで 1.66-1.67 = 同一の
システムフォントに落ちている)。unicode-range で HackGen から外しても改善せず、
現状 2.00 のまま重なる。

`✻ U+273B` は HackGen 未収録で元から fallback が描いており 1.14。軽微。

## 本筋の候補

**narrow symbol subset webfont を同梱する**のが本命と考える。対象 1657
コードポイント (または実用上必要な記号だけに絞った部分集合) を半角 advance で
持つ webfont を用意し、`unicode-range` で HackGen から外した範囲をその同梱
フォントに割り当てる。fallback をシステムフォントに委ねないので環境非依存になり、
① のようにシステム側に narrow グリフが存在しない文字も解決する。

検討事項:

- ライセンス互換の素材選定 (HackGen は SIL OFL 1.1)。既存フォントから subset +
  リメトリクスして再配布できるか要確認
- サブセット化の生成をビルド手順に組み込むか、生成済み woff2 をコミットするか
- 配信サイズ (現状 HackGen 2 ウェイトで約 8.7MB あるので、記号 subset の追加分は
  相対的に小さい見込み)

## 調査済みで不採用にした案の記録

判断のやり直しを防ぐため、調査済みの内容を残す。

**案: xterm.js に custom unicode provider を register して該当文字を幅 2 にする** —
不採用。daemon の screen state 正本から web だけ乖離し、`hyoui screen dump` と
web 表示の桁が恒常的に食い違う。「グリフの重なり」を「正本乖離 + 行内の桁ずれ」に
置き換えるだけで改悪 (DR-0013 / DR-0005)。

ただし実装機構自体は調査済みなので、将来 provider が必要になった場合のために
手順を記録しておく:

- `term.unicode` は xterm.js の **proposed API** 扱いで、Terminal options に
  `allowProposedApi: true` が必要 (無いと getter が throw する)
- `term.unicode` は**アクセスのたびに新しい `UnicodeApi` を返す getter**
  (`get unicode(){ return this._checkProposedApi(), new UnicodeApi(this._core) }`)
  なので、`term.unicode.register` の monkey-patch は原理的に効かない
- unicode11 addon の UMD bundle が公開するのは `Unicode11Addon` クラスだけで、
  `UnicodeV11` provider は内部モジュールに閉じている。`ITerminalAddon.activate(terminal)`
  が「`terminal.unicode.register(provider)` を呼ぶだけ」と規約で決まっているので、
  `register` を捕まえるスタブ terminal を渡せば内部 API に触れずに provider を
  取得できる:

  ```js
  let captured = null;
  new Unicode11Addon().activate({ unicode: { register(p) { captured = p; } } });
  ```

- vendored xterm.js 5.3.0 の `UnicodeService` が provider に要求するのは
  `version` と `wcwidth` のみ (`charProperties` の呼び出し経路は無い)
- 幅を上書きする際、**基底幅 0 (結合文字) は 0 のまま通す**こと。EastAsianWidth
  では U+0300 等の結合文字にも Ambiguous が含まれ、無条件に上げるとカーソル位置が壊れる

**案: East Asian Ambiguous を一律 wide にする** — 不採用。そもそも症状と対応
していない。Unicode 16.0.0 の EastAsianWidth.txt では ⚠ U+26A0 も ✻ U+273B も
`N` (Neutral) で Ambiguous ではなく、逆に § U+00A7 は Ambiguous だが HackGen が
半角グリフで描くため重ならない。重なりの有無を決めているのは EAW の分類ではなく
フォントの advance width である。

## 除外集合の生成手順

フォント差し替え時の再生成用。fonttools で両ウェイトの hmtx から
`advance == 1080` の cmap エントリを集め (Regular / Bold の和集合)、その各
コードポイントを xterm.js の `UnicodeService.wcwidth`(version 11) に通して
1 のものだけを残す。その補集合を `style.css` の `unicode-range` に書く。

```python
from fontTools.ttLib import TTFont
f = TTFont("crates/hyoui-web/assets/vendor/fonts/HackGenConsoleNF-Regular.woff2")
hmtx, cmap = f["hmtx"], f.getBestCmap()
[cp for cp, n in cmap.items() if hmtx[n][0] == 1080]
```

PUA (U+E000-F8FF ほか) は除外集合に入れないこと。Nerd Font のアイコングリフは
HackGen 側で半角 advance (540) なので重ならず、fallback フォントはこれらの
グリフを持たないため除外すると tofu 化する (実測: PUA 10383 グリフすべてが 540)。
