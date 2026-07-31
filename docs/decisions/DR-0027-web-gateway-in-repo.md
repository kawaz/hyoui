# DR-0027: Web UI gateway — 同 repo `crates/hyoui-web` + axum、DR-0010 §2 を supersede

- Status: Active
- Date: 2026-07-20
- Related: DR-0005 (思想 — gateway も透過原則に従い既存 attach を bridge する), DR-0008 (protocol — transport 非依存 CBOR framing を WS に転写), DR-0010 §2 (serve gateway 別 repo 方針 — **本 DR が supersede**), DR-0013 (screen state 正本 — screenshot は screen.dump を転写), DR-0024 (config.toml — `[web]` セクション相乗り)
- Origin: kawaz 依頼 2026-07-20 (ccmsg r40m17)、裁定 WEB-Q1=a / WEB-Q2=a / WEB-Q3=ok (r40m31)

## Context

kawaz ドッグフーディングで「ブラウザから hyoui セッションを見る・触る」需要が確定した。
tailnet 内の他マシン / モバイルから、走行中の Claude セッション等を観測・操作したい。

- 前段インフラは整備済み: `https://hyoui.<host>.kawaz.jp` → `127.0.0.1:43690` の
  Caddy reverse proxy (ACME 証明書 / WebSocket upgrade 透過、canddy-app-proxy 管轄)
- 認証は当面なし (tailnet 経由のみ、network 露出時の token auth は将来 DR)

## Decision

### 1. 置き場: 同 repo `crates/hyoui-web` (DR-0010 §2 を supersede)

DR-0010 §2 の「serve gateway は別 repo `kawaz/hyoui-serve`」を覆し、同 workspace の
新 crate `crates/hyoui-web` として実装する。

**Why 覆すか** (DR-0010 の別 repo 論拠への反証):

| DR-0010 の論拠 | 現状での再評価 |
|---|---|
| core の dependency footprint 維持 | workspace 別 crate なら core (`crates/hyoui`) の Cargo.toml は不変。tokio 系は hyoui-web に閉じる |
| 独立 release cycle | v1.0 未満で protocol が毎リリース breaking する現状、別 repo は「共進化の足かせ」でしかない (crates.io publish 経由の参照は都度 publish が必要) |
| security audit boundary | 「認証なし・tailnet 前提」の現运用では audit boundary 分離は過剰。network 露出を正式サポートする段階で再評価 (その時の分離コストは今より小さくならないが、共進化の利益が上回る) |

### 2. スタック: axum + tokio + tokio-tungstenite (hyoui-web に閉じる)

- `crates/hyoui-web`: axum (HTTP + WS)、tokio runtime。core との接続は
  `hyoui::client` の blocking `ClientConnection` を `spawn_blocking` / 専用 thread で
  bridge する (core の blocking 設計は不変)
- CLI 露出: `hyoui web` subcommand (hyoui-cli から hyoui-web を呼ぶ)。
  default bind `127.0.0.1:43690` (= 0xAAAA、kawaz 裁定 r40m20)
- config.toml `[web]` セクション (DR-0024 相乗り): `listen = "127.0.0.1:43690"` 等。
  CLI flag は最小 (`--listen` のみ、DR-0024 の flag 最小化方針踏襲)

### 3. endpoint 構成 (第一弾 / 第二弾)

第一弾:

- `GET /` — セッション一覧ページ (socket dir 走査 + status.query、`hyoui list` 相当)
- `GET /sessions/:id` — セッション情報ページ (xterm.js で screen 表示 + input 欄)
- `GET /api/sessions` — 一覧 JSON
- `GET /api/sessions/:id/screen` — ANSI bytes (`screen.dump.request` 転写)。
  plain テキスト変換はしない (kawaz 裁定 r40m18「最初から ANSI」)
- `POST /api/sessions/:id/input` — input 送信 (text / key spec、`hyoui input` 相当)

第二弾:

- `WS /api/sessions/:id/attach` — フルターミナル attach。protocol frame
  (transport 非依存 CBOR) を WS binary message に 1:1 転写、client 側は xterm.js。
  detach = WS close。1 WS 接続 = 1 `ClientConnection`

WS の binary frame は PTY 入出力 bytes、text frame は browser↔gateway の制御 JSON とする。
resize は `{"kind":"resize","requestId":N,"cols":W,"rows":H}` を送り、gateway が同じ
WS bridge の leader `ClientConnection` から既存 daemon `Resize` message を発行する。応答は
`{"kind":"resize.result","requestId":N,"ok":true}` (失敗時は `ok:false` + `error`)。
daemon protocol / capability は増やさない。browser は成功応答後だけ xterm.js grid を変更し、
応答前・失敗時・`resize` off では旧 grid を維持する。極小 viewport では vt100 に 0 行/列を
渡さないため、提案寸法を最低 `2x2` に clamp してから送る。

`POST /api/sessions/:id/resize` は WS 未接続時の fallback として残す。短命接続が leader を
取得できない場合は daemon の `mode.not-leader` を握りつぶさず HTTP 409 で返す。

既存キーボード FAB のフローティングパネルは「入力」「情報」の 2 タブを持つ。情報タブは
attach の実効 mode / leader、URL query から決まる表示設定と出自、`/api/sessions` で取得できる
session id / child pid / child state / attach client 数を read-only 表示する。gateway は WS 確立時と
leader / mode の変化時に次の text frame を browser へ送る。daemon protocol は変更しない。

```json
{"kind":"attach.info","mode":"rw","leader":true}
```

leader でない browser は情報タブの「leader になる」から takeover を要求できる。WS 制御
JSON は `{"kind":"leader.request","requestId":N}`、応答は
`{"kind":"leader.result","requestId":N,"ok":true}` (失敗時は `error` 付き)。gateway は
`leader-request-v1` を確認して daemon の `leader.request` を送り、成功後は browser の現在
viewport に対する grid 提案で resize する。`leader.notify` による `attach.info` 再送で、新
leader は `yes`、旧 leader は `no` に更新される ([[DR-0033]])。

表示設定の出自は `URL 指定` / `default` / `embed 中に変更` の 3 区分とする。情報タブ上で
unicode / ambw / fontsize / lineheight / scrollback / fontfamily / bg / fg を変更でき、変更した項目は
第三の出自へ切り替える。font / lineheight は xterm.js option 更新後に既存 fit/resize 経路を通し、
unicode / ambw は provider を再選択して daemon screen dump を再描画する。変更値は URL や storage
へ保存せず、外側の reload で URL 指定または default にリセットされる。

### 4. 静的アセット

xterm.js + 素の HTML/JS (bundler なし、vendored コピー)。リリースビルドは
crate に埋め込み (include_dir 系)、開発中は `--web-assets-dir` でローカルファイルを
返す二段構え (kawaz 方針 r40m22)。

### 5. セッションページの URL query パラメータ

`GET /sessions/:id` の見た目・挙動はすべて URL query で制御する。閲覧クライアント
(iframe 埋め込み側 / ブックマーク) ごとに違う値を選べる必要があり、gateway 側の
config (`[web]`) に持たせると全閲覧者で共有されてしまうため。優先は「query → 既定」
の 1 段だけで、config との合成はしない。

| param | 型 / 範囲 | 既定 | 効果 |
|---|---|---|---|
| `embed` | `1` | off | header / debug panel を隠して viewport にフィット |
| `resize` | `1` | off | auto-resize (PTY) を強制 ON |
| `fontsize` | 整数 6-40 (px) | `13` | xterm.js `fontSize` |
| `lineheight` | 数値 1.0-2.0 | `1.0` | xterm.js `lineHeight` (xterm.js は 1 未満を拒否する) |
| `scrollback` | 整数 0-100000 (行) | `2000` | xterm.js `scrollback` |
| `fontfamily` | font family 名のカンマ区切り | (なし) | 既定チェーンの**先頭に挿入**。指定フォントが持たないグリフは HackGen Console NF → host monospace へ落ちる |
| `bg` / `fg` | hex 3/4/6/8 桁、`#` 省略可 | `#111` / `#e0e0e0` | terminal の背景 / 前景 |
| `unicode` | `6` \| `11` | `11` | 文字幅計算に使う Unicode 版 (`term.unicode.activeVersion`) |
| `ambw` | `half` \| `full` | `half` | East Asian Ambiguous を幅 2 として扱うか |

不正値・範囲外は既定に落として `console.warn` を出す (= ページ内 debug panel にも
残る)。`#` を省略できるのは、URL に `%23` を書かずに済ませるため。`fontfamily` は
font family 名に現れうる文字だけを通し、`;` `{` `(` 等を含む値は拒否する
(= 値は xterm.js が inline style に直接入れるため)。

#### 文字幅パラメータ (`unicode` / `ambw`)

`unicode=6` は xterm.js 内蔵の UnicodeV6 テーブルに戻す。Unicode 6 当時に存在
しなかった絵文字が幅 1 になるので、絵文字を半角で組む端末に合わせたい場合に使う。

`ambw=full` は Ambiguous (EastAsianWidth の `A`) を幅 2 にする。CJK 文脈で全角、
それ以外で半角に組まれてきた文字群で、どちらで数えるかは端末の設定に委ねられている
(tmux の `utf8-ambiguous-width`、iTerm2 の "Treat ambiguous-width characters as
double-width" と同じ選択)。

**`ambw=full` は opt-in であり既定にしない。** daemon の vt100 と TUI アプリ本体は
Ambiguous を幅 1 として数える (実測で三者一致を確認済み)。web だけ 2 にすると、
screen dump が渡す行を xterm.js が自前の幅で組み直す際に対象文字より後ろが 1 文字
あたり 1 桁ずつ右へずれる (行頭で再同期するのでずれは行内に閉じる)。screen state を
正本とする設計 (DR-0013 / DR-0005) からの意図的な乖離になるため、明示指定した
閲覧者だけが踏む経路にしている。

影響は装飾記号に留まらない。**box drawing (U+2500-254B) と block elements
(U+2580-258F) も Ambiguous** なので、`full` では罫線が 2 セル幅になり枠線を引く
TUI の表示は大きく崩れる。日本語混在の文書を読む用途で幅を揃えたいときに使い、
TUI を操作する用途では `half` のままにする想定。

結合文字 (U+0300-036F)、variation selector、Nerd Font の PUA も Ambiguous に
含まれるが、これらは基底幅が 0 なので `full` でも 0 のまま通す (2 に上げると
カーソル位置と Powerline 記号が壊れる)。

## Rejected alternatives

- **別 repo `hyoui-serve`** (DR-0010 §2): 上表の通り。breaking 期の共進化コストが利益を上回る
- **websocketd + `hyoui attach`**: 実装ゼロだが第一弾要件 (一覧ページ / screenshot / input POST) を満たせない
- **blocking HTTP (tiny-http 等) 手組み**: WS 多重化を自作することになり第二弾で作り直しになる

## Consequences

- core (`crates/hyoui`) の依存は不変。tokio / axum は hyoui-web のみ
- DR-0010 §2 は Superseded (INDEX に反映)。§1 (CLI nested family) / §3 以降は不変
- 認証 / HTTPS は scope 外: HTTPS は前段 Caddy (ACME) が担い、認証は network
  露出を正式サポートする将来 DR で扱う
- 検証要件 (DR-0014): 一覧 / screen / input の各 endpoint を実機 3 category
  (TUI alt screen / line-oriented / REPL) のセッションに対して確認。WS attach は
  ラウンドトリップ (attach → 入力 → 画面反映 → detach → 再 attach) を確認

## Implementation

- Phase 1: `crates/hyoui-web` 新設 (axum skeleton + `GET /api/sessions` +
  `GET /api/sessions/:id/screen` + `POST /api/sessions/:id/input`)、
  `hyoui web` subcommand、config `[web]`
- Phase 2: HTML UI (一覧 / セッションページ、xterm.js 表示 + input 欄、定期 screen 更新)
- Phase 3: `WS /api/sessions/:id/attach` (frame ↔ WS binary bridge、resize / detach)
- Phase 4: 実機検証マトリクス + ドッグフーディング (tailnet 経由)
