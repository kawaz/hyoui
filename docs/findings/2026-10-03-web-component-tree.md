# Web front (ブラウザ側) のコンポーネントツリー棚卸し

対象は `crates/hyoui-web/assets/` の素の JS / HTML / CSS (フレームワーク無し)。ここでいう「コンポーネント」は画面上の UI 単位と、それを担う JS 関数・DOM 要素・CSS の束を指す。HTTP / WS / 認証の API 境界 (frame の型定義) は `docs/findings/2026-10-03-web-api-protocol-inventory.md` の担当で、本書は front 内部の構造に集中する。

記法: 「事実」はコードで確認したもの、「主張」はコメント・DR に書かれているが本調査で裏を取っていないもの、「未確認」は実機で見ていないもの。行番号は調査時点 (jj の作業コピー、2026-10-03) のもの。

## 判明した事実 (要約)

- ページは 2 枚。`GET /` → `index.html` (セッション一覧)、`GET /sessions/{id}` → `session.html` (ターミナル)。どちらも classic script (module ではない) を `</body>` 直前に並べ、モジュール間の受け渡しは全部 `window.*` グローバル経由。読み込み順が唯一の依存解決手段
- `session.js` (2018 行) は 1 つの async IIFE のクロージャで、14 前後の責務 (表示設定・リンク・xterm 生成・IME 補正・touch・情報タブ・テキスト選択・resize・status・screen polling・入力パネル・浮遊位置・WS・認証延長) が同じスコープの変数を直接読み書きしている。外から呼べる単位が無く、JS test は `auth-share.js` にしか無い
- 状態は「モジュール変数」「DOM 属性 (`hidden` / `checked` / `disabled`)」「xterm の `term.options`」「storage (localStorage 1 キー + sessionStorage 1 キー)」に分散し、表示設定は モジュール変数と `term.options` の二重持ち
- 稼働中の gateway 2 本 (port 43690 / 43691、どちらも 0.9.66・protocol 1) が配る `index.html` / `session.html` / `session.js` はリポの作業コピーと byte 一致。`/api/sessions` は 401 で、ブラウザ DOM の実機観測は passkey 認証が要るため行っていない (= DOM 構造は静的読解のみ)
- 見直し論点の主なもの: 重なり順の破綻 (認証 overlay `z-index: 200` の上に FAB `1000` / 入力パネル `1001` / link fallback `1100` が乗る)、浮遊物の位置を JS が `left/top` で自前管理、`<dialog>` / popover 等ネイティブの top-layer 機構を使わず overlay を 4 種自作、`100vh` 固定 (embed)、soft keyboard / safe-area 対応なし、xterm.js 内部 API への依存 4 箇所、DR-0035 決定 3 が「情報タブで表示」とする `version` / `build_id` が未表示、DR-0032 の web 版 child action menu が未実装

## 1. ページ構成

| URL | HTML | 配信 (事実) |
|---|---|---|
| `GET /` | `index.html` | `crates/hyoui-web/src/lib.rs:861` `get_index_page` → `serve_asset("index.html")` |
| `GET /sessions/{id}` | `session.html` | `crates/hyoui-web/src/lib.rs:869` `get_session_page`。server は `id` も query も見ない |
| `GET /assets/{*path}` | 各 asset | `crates/hyoui-web/src/lib.rs:877`。`include_dir!` 埋め込み、`--web-assets-dir` 指定時はローカル dir。cache 系ヘッダは付かない (実測: `content-type` のみ) |

PWA: 両 HTML が `manifest.webmanifest` (`start_url` / `scope` = `../` = endpoint、`display: standalone`) と `icon.svg` を参照。`apple-mobile-web-app-status-bar-style: black-translucent`。

### index.html の読み込み順 (すべて classic script、`defer` / `async` なし)

1. `assets/style.css`
2. `assets/contract.js` → `window.hyouiContract`
3. `assets/auth-share.js` → `window.hyouiAuthShare`
4. `assets/auth.js` → `window.hyouiAuth` (評価時に 2, 3 を分割代入するので順序依存)
5. `assets/index.js` (IIFE、export なし)

### session.html の読み込み順

1. `../assets/vendor/xterm.css`、`../assets/style.css`
2. インライン script (`session.html:145-177`): ページ内 debug log。`window.__hyouiDebug` を定義し、`error` / `unhandledrejection` / `console.error` / `console.warn` を横取りして `#debugLog` に流す
3. `../assets/vendor/xterm.js` → `window.Terminal`
4. `../assets/vendor/addon-unicode11.js` → `window.Unicode11Addon`
5. `../assets/vendor/addon-fit.js` → `window.FitAddon`
6. `../assets/vendor/addon-web-links.js` → `window.WebLinksAddon` (session.js より前であることは `lib.rs:1400` 付近の test が固定)
7. `../assets/contract.js`、`../assets/auth-share.js`、`../assets/auth.js`
8. `../assets/unicode-ambiguous.js` → `window.HyouiAmbiguousWidth`
9. `../assets/session.js` (async IIFE)

### vendor の使い方

| vendor | 版 (vendor/README.md) | session.js での使い方 |
|---|---|---|
| xterm.js / xterm.css | 5.3.0 | `new Terminal({...})` (`session.js:240`)。公開 API に加え内部 API を 4 系統参照: `term._core.unicodeService._providers` (`:280`、ambw の V6 base 取得)、`term._core._charSizeService.measure()` (`:365`、font load 後の再測定)、`term._core._syncTextArea()` / `_compositionHelper.isComposing` (`:414-420`、IME 補正) |
| addon-unicode11 | 0.8.0 | `term.loadAddon` (`:293`) + `HyouiAmbiguousWidth.captureUnicode11()` で provider を横取り (`unicode-ambiguous.js:94`) |
| addon-fit | 0.10.0 | **`proposeDimensions()` だけ**使い、`fit()` は呼ばない (`:1001`, `:1679`)。grid 変更は daemon の resize 受理後に `term.resize` で行う |
| addon-web-links | 0.9.0 | 素の http/https URL の link 化。activate は `openTerminalLink` (OSC 8 の `linkHandler` と共有、`:260`, `:319`) |
| fonts (HackGen Console NF) | 2.10.0 | `style.css:36-101` の `@font-face` 2 本 (`unicode-range` で全角 advance 記号を除外)、`font-display: block`。JS 側は `document.fonts.load` で待ってから `term.open` (`:345-356`) |

## 2. コンポーネントツリー

凡例: [静] = HTML に静的に存在、[動] = JS が動的生成。`→` の後が担当 JS、`css:` が主な CSS の所在。

### 2.1 index ページ

```
body
├─ [動] #protocolBanner 世代不一致の帯           → contract.js:110 showMismatchBanner (body 先頭に insert)   css: style.css:282 .protocol-banner
├─ [静] header
│   ├─ h1 "hyoui sessions"
│   └─ p.meta 操作行
│       ├─ #reload ↻ (location.reload)             → index.js:194
│       ├─ #refresh                                → index.js:191 fetchSessions
│       ├─ #auto checkbox (auto refresh)          → index.js:190 schedule
│       └─ #status.status-text                    → index.js:111,118,120             css: style.css:147
├─ [静] main
│   ├─ table#sessions
│   │   ├─ thead th[data-sort] ×9 (sort)           → index.js:88 syncHeaders, :181 click   css: style.css:189-202
│   │   └─ tbody
│   │       └─ [動] tr.row-<status> ×N            → index.js:124 render (innerHTML で行ごと生成)
│   │           ├─ td: session_id link (live/stopped のみ a)
│   │           ├─ td: status badge (.badge-live / .badge-stopped)  css: style.css:323-339
│   │           ├─ td.uptime / td.suspend / version / td.argv / td.cwd  css: style.css:204-208, 341-348
│   └─ p#empty (0 件時)                            → index.js:129,132                 css: style.css:210
└─ [動] .auth-overlay[role=dialog] ログイン / 登録 / 失効   → auth.js:330 createOverlay (body 末尾に append)   css: style.css:748-817
    └─ .auth-panel (h2 + p.auth-note/.auth-error + input.auth-code/.auth-label + button.auth-primary + a.auth-secondary)
```

### 2.2 session ページ

```
body.session-page (+ .embed if ?embed=1, session.js:31)
├─ [動] #protocolBanner                           → contract.js:110            css: style.css:282
├─ [静] header (embed で display:none)
│   ├─ h1: a "← sessions" (../) / span#sid         → session.js:24
│   └─ p.meta 操作行
│       ├─ #reload ↻                               → session.js:1998
│       ├─ #refresh (WS 中は confirm)               → session.js:1990
│       ├─ #auto checkbox (WS 中 disabled)          → session.js:1986, 1864, 1958
│       ├─ #autoResize checkbox (localStorage)     → session.js:924-943
│       ├─ span#size.meta "80x24"                  → session.js:983,996,1026,1720
│       ├─ span#status.status-text                 → session.js fetchScreen (:1100-1123)
│       └─ [動] span#wsStatus.meta "ws: …"         → session.js:1599-1610 (status の後ろに append、inline style marginLeft)
├─ [静] main (flex column, css: style.css:217。embed は height:100vh, :226)
│   ├─ #stoppedBanner.stopped-banner[hidden] 子 SIGSTOP 帯   → session.js:1057 refreshSessionStatus, :1072 sendResume   css: style.css:299-322
│   │   └─ #resumeBtn
│   ├─ #term (xterm のホスト)                      → session.js:356 term.open(termEl)   css: style.css:245-258, embed :232-242
│   │   ├─ [動] .xterm (xterm.js が生成。helper textarea / viewport / screen)
│   │   └─ [動] .text-selection-overlay[hidden] テキスト選択   → session.js:827-908   css: style.css:610-665
│   │       ├─ .text-selection-head (title / meta / close ×)
│   │       └─ pre.text-selection-content
│   ├─ [静] aside#linkFallback.link-fallback[hidden][role=alertdialog] リンク開けず時   → session.js:148-238   css: style.css:669-741
│   ├─ [静] button#inputFab ⌨ 浮遊ボタン (fixed, drag)   → session.js:1206-1398, 1519   css: style.css:366-396
│   ├─ [静] #inputPanel.input-panel[hidden][role=dialog] 浮遊パネル (fixed, drag)   → session.js:1463-1562   css: style.css:400-513
│   │   ├─ .panel-tabs-row (drag ハンドル兼用)
│   │   │   ├─ .panel-tabs[role=tablist]: #inputTab / #infoTab   → session.js:1463 selectPanelTab
│   │   │   └─ #inputPanelClose ×
│   │   ├─ #inputTabPanel 入力タブ
│   │   │   ├─ textarea#inputText (Cmd/Ctrl+Enter 送信)   → session.js:1153 submitFromTextarea, :1176
│   │   │   ├─ .input-panel-buttons: #sendBtn / #sendEnterAfter / #keypadToggle / #sendKey (prompt)
│   │   │   ├─ #keypad[hidden] button[data-spec] ×9      → session.js:1189-1198
│   │   │   └─ p#sendStatus
│   │   └─ #infoTabPanel 情報タブ (.info-panel)
│   │       ├─ section テキスト: #textSelectionBtn
│   │       ├─ section Attach: dl (#infoAttachMode / #infoAttachLeader) + #infoLeaderAction[hidden] (#requestLeaderBtn / #requestLeaderStatus)   → session.js:1611 updateAttachInfo, :1714
│   │       ├─ section 表示設定: #infoDisplaySettings
│   │       │   └─ [動] .info-setting-row ×8 (label + select|input + .source-badge)   → session.js:765-821
│   │       │   + p#displaySettingStatus + 凡例 .source-badge ×3 [静]
│   │       └─ section Session: dl (#infoSessionId / #infoChildPid / #infoChildState / #infoAttachClients)   → session.js:822, 1059-1066
│   └─ [静] details#debugPanel (embed で非表示): summary(#debugCount) + pre#debugLog   → session.html インライン script
└─ [動] .auth-overlay (index と同じ)                → auth.js:330
```

## 3. 状態の持ち方

### 3.1 所有者と書き換え元

| 状態 | 置き場 | 所有 (定義) | 書き換え元 |
|---|---|---|---|
| 契約世代の不一致 | モジュール変数 `mismatch` | contract.js:103 | `reportGatewayProtocol` (index は `/version` polling、session は WS `hello`) |
| access token / sub | クロージャ `standing` / `sub` (メモリのみ) | auth-share.js:47-49 | `adopt` / channel の `offer` / `refreshNow` |
| ログイン・登録の進行 | `signInPromise` / `registrationPromise` / `invitation` / `refreshTimer` | auth.js:149-157 | `signIn` / `ensureRegistration` / `scheduleRefresh` |
| refresh token | httpOnly cookie | server | server (JS は触れない) |
| 一覧データ・ソート | `sessions` / `sortKey` / `sortAsc` / `timer` | index.js:19-24 | `render` / th click / `schedule` |
| 表示設定 8 項目 | モジュール変数 `fontSize` 等 (`session.js:133-142`) **と** `term.options` の二重 | session.js | URL query (初期) / 情報タブの `apply` (`:655-754`) が両方を書く。`openTextSelection` は `term.options` 側を読む (`:869-874`) |
| 表示設定の出自 | `displaySettingSources` (obj) + `displaySettingSourcesByName` (Map → DOM badge) | session.js:55, 634 | `settingValue` / `setRuntimeSource` |
| 文字幅 provider | `registeredAmbwVersions` / `unicodeWidthAvailable` + `term.unicode.activeVersion` | session.js:268-269 | `applyUnicodeWidth` |
| screen 最終 payload | `lastPayload` | session.js:916 | `fetchScreen`、`redrawForWidthChange` が `''` に戻す |
| polling timer | `timer` | session.js:915 | `schedule` / `ws.onopen` (止める) / `ws.onclose` (`schedule`) |
| auto refresh 可否 | `#auto.checked` / `.disabled` (DOM) | session.html | 利用者 / `ws.onopen` / `ws.onclose` |
| auto-resize 可否 | `#autoResize.checked` (DOM) + localStorage `hyoui.session.autoResize` + `?resize=1` | session.js:924-930 | 利用者 change |
| resize キュー | `resizePending` / `resizeRunning` / `fitTimer` | session.js:936-937, 1014 | `proposeAndQueueResize` / `drainResizeQueue` / `scheduleFit` |
| WS 本体と再接続 | `ws` / `wsReconnectMs` / `wsReconnectTimer` / `wsExplicitClose` | session.js:1578-1581, 1970 | `connectWs` / `scheduleWsReconnect` / `beforeunload` / `auth.extend.result ok:false` |
| WS 未送信入力 | `wsPendingInput` / `wsPendingBytes` (上限 8 KiB) | session.js:1585-1587 | `sendBytesToWs` / `flushPendingToWs` |
| WS 要求相関 | `wsResizePending` / `wsLeaderPending` (Map) + 採番 3 系列 | session.js:1589-1596 | 送信関数 / `onmessage` / `onclose` |
| daemon cap | `daemonCaps` (null = 不明) | session.js:1623 | `hello` / `onclose` (null に戻す) |
| 認証延長の購読 | `authExtendStop` | session.js:1818 | `armAuthExtend` (hello ごと) |
| attach mode / leader | **DOM のテキストのみ** (`#infoAttachMode` / `#infoAttachLeader` / `#infoLeaderAction.hidden`) | session.html | `attach.info` / `onclose` |
| child 停止 | **DOM のみ** (`#stoppedBanner.hidden`) | session.html | `refreshSessionStatus` (5 秒 polling) |
| パネル開閉・タブ・keypad | **DOM のみ** (`hidden` / `aria-selected` / `.active`) | session.html | `openPanel` / `closePanel` / `selectPanelTab` / keypad toggle / touch handler / `openTextSelection` |
| 浮遊位置 | `floatPos` + sessionStorage `hyoui.session.floatPos` (edge 相対) | session.js:1229-1253 | drag / open / close / query (`?fab=` / `?fab-*=`) |
| FAB デザイン | inline style / CSS custom property (`--fab-bg` 等) | session.js:1366 | query のみ (保存しない) |
| テキスト選択表示中 | `textSelectionOpen` | session.js:827 | `openTextSelection` / `closeTextSelection` |
| touch tap 判定 | `terminalTouch` / `terminalCloseOnlyClickPending` | session.js:500-501 | touch / pointerdown / click capture |
| link fallback の focus 戻し先 | `linkFallbackReturnFocus` | session.js:154 | `showLinkFallback` / `closeLinkFallback` |
| debug ログ | インライン script の `lines` (最大 200) | session.html:150 | `window.__hyouiDebug` |

### 3.2 状態遷移 (session ページ)

接続 (`#wsStatus` の表示文字列が遷移の観測点、事実: `session.js:1833-1979`):

| 状態 | 表示 | 入る契機 | polling | 入力経路 |
|---|---|---|---|---|
| 初期 | `ws: init` | ページ読込 | `#auto` が on なら 2 秒間隔 | — |
| 接続試行 | `connecting…` | `connectWs` | 継続 | xterm 打鍵は pending queue |
| 未ログイン | `auth required` | `AUTH.acquire()` が throw (= overlay の操作待ちで pending の間は `connecting…` のまま) | 継続 | 同上 |
| 接続済 | `connected` | `onopen`。`fetchScreen` 1 回で初期画面を復元、pending を flush | 停止 (`#auto` disabled) | xterm → WS binary。パネルは HTTP POST `/input` |
| 世代不一致 | `stale page (protocol N)` + 帯 | `hello.protocol` 不一致 | 停止のまま | binary は通す、制御 frame (resize / leader.request) は送らない |
| 切断 | `disconnected (code=N)` | `onclose`。info を `disconnected` / `—` に、cap を null に、pending 要求を reject | 再開 (`schedule`) | pending queue |
| 再接続待ち | `reconnecting in Ns…` | `scheduleWsReconnect` (1s → 2 倍 → 上限 30s) | 継続 | pending queue |
| 失効 | `auth revoked` + 失効 overlay | `auth.extend.result ok:false`。`wsExplicitClose = true` で再接続しない | `onclose` で再開されない (explicit) | — |

leader: `attach.info.leader` が false の時だけ「leader になる」を表示。`hello.caps` に `leader-request-v1` が無ければ disabled + title で理由表示 (`:1631`)。押下 → 先に resize frame (reject 想定) → `leader.request` → `leader.result ok` で **client が自分で `term.resize`** (`:1719`)。

認証 (auth.js): access 無し → refresh (lock 下、他タブへ ask 50ms) → 失敗なら招待 URL があれば登録 overlay、無ければログイン overlay → 成功で `share.adopt` → 残り 10% で `refreshAhead` → `onAccess` 経由で WS に `auth.extend`。

その他の 2 値状態: パネル開/閉、タブ 入力/情報、keypad 開/閉、テキスト選択 開/閉 (開いている間は送信を全遮断 `:1127`, `:1739`)、child stopped 帯 表示/非表示、link fallback 表示/非表示。

## 4. イベント経路

### 4.1 入力 → 送信

| 入力 | ハンドラ (登録箇所) | 経路 | 送信先 |
|---|---|---|---|
| xterm 上のキー (通常) | xterm 内部 → `term.onData` (`session.js:1801`) | `sendBytesToWs` (string は UTF-8 化) | WS binary frame。未接続なら pending (8 KiB 上限) |
| Shift+Enter | `term.attachCustomKeyEventHandler` (`:1789`) | xterm の CR を止め `\x1b[13;2u` を送る | WS binary |
| mouse report 等 | `term.onBinary` (`:1804`) | Latin-1 string → Uint8Array | WS binary |
| IME 確定 | xterm CompositionHelper → onData。補正: `compositionend` bubble (`:432`, value クリア) と capture (`:477`, 二重送信抑止) | 同上 | WS binary |
| IME 位置追従 | `term.onResize` / `term.onRender` → `imeSync` (`:441-442`) | `_syncTextArea` 呼び直し | — |
| touch tap (terminal) | `#term` の touchstart/move/end/cancel (`:516-579`)、pointerdown capture (`:580`)、click capture (`:586`) | focus トグル / パネルを閉じるだけの tap | — |
| 入力パネル送信 | `#sendBtn` click (`:1172`)、textarea keydown Cmd/Ctrl+Enter (`:1176`) | `submitFromTextarea` → `sendSpecs` | **HTTP POST `api/sessions/{id}/input`** (WS 接続中でも HTTP) |
| keypad | `button[data-spec]` mousedown preventDefault + click (`:1191-1196`) | `sendSpecs([spec])` | HTTP POST `/input` |
| Key… | `#sendKey` click → `prompt()` (`:1200`) | `sendSpecs(['key:'+name])` | HTTP POST `/input` |
| resume | `#resumeBtn` click (`:1088`) | `sendResume` | HTTP POST `/resume`、300ms 後に screen / status 再取得 |
| viewport 変化 | `window` resize (`:1030`) + `ResizeObserver(#term)` (`:1033`) | `scheduleFit` (150ms debounce) → `proposeAndQueueResize` → `drainResizeQueue` → `requestResize` | WS 接続中は text frame `resize`、未接続は HTTP POST `/resize`、接続中 (CONNECTING) は送らない |
| leader になる | `#requestLeaderBtn` click (`:1714`) | `sendLeaderRequestOverWs` | WS text `resize` → `leader.request` |
| 認証延長 | `AUTH.onAccess` (`:1823`) | access 差し替え時 | WS text `auth.extend` |
| 表示設定変更 | 各 control の change (`:800`) | `setting.apply` → `term.options` 更新 / `scheduleFit(true)` / unicode は `fetchScreen` 再投入 | (resize 経由で WS / HTTP) |
| refresh | `#refresh` click (`:1990`) | WS 中は `confirm()` | HTTP GET `/screen?layer=both` + `/api/sessions/{id}` |
| FAB / パネル drag・tap | `attachDrag` の pointer 4 種 + click (`:1404-1461`) | `applyAndSaveFromRect` / `togglePanel` | — |
| Esc | パネル keydown (`:1533`)、link fallback keydown (`:219`)、document keydown capture (テキスト選択中のみ, `:894`) | 閉じる | — |
| index: ソート / refresh / auto | th click (`index.js:181`) / `#refresh` / `#auto` | `render` / `fetchSessions` / `schedule` | HTTP GET `api/sessions` (+ `version`) |

### 4.2 受信 → 描画

| 受信 | ハンドラ | 描画先 |
|---|---|---|
| WS binary | `ws.onmessage` (`session.js:1874`) | `term.write(Uint8Array)` |
| WS text `hello` | `:1883` | `reportGatewayProtocol` (帯) / `daemonCaps` → `syncCapabilityAffordances` / `armAuthExtend`。`version` / `build_id` は**読んでいない** |
| WS text `attach.info` | `:1907` → `updateAttachInfo` | 情報タブ Attach 節 |
| WS text `resize.result` / `leader.result` / `error` | `:1911-1939` | pending Promise を resolve/reject (結果は `#size` / `#requestLeaderStatus` / debug log) |
| WS text `auth.extend.result` | `:1897` | `ok:false` で失効 overlay |
| HTTP `/screen?layer=both` | `fetchScreen` (`:1099`) | 差分があれば `term.reset()` + `term.write(text)`、`#status` |
| HTTP `/api/sessions/{id}` | `refreshSessionStatus` (`:1051`、5 秒 interval) | `#stoppedBanner`、情報タブ Session 節 |
| HTTP 401 (`/api/*`) | `AUTH.fetch` (`auth.js:288`) → `acquire` | ログイン / 登録 overlay |
| HTTP `/version` (index) | `index.js:99` | 帯 |
| `/api/sessions` (index) | `fetchSessions` → `render` | tbody 全再生成 |

## 5. モジュール間依存

すべて `window` グローバル経由 (import は無い)。`auth-share.js` だけが `module.exports` も持ち、node test から require される。

| 利用側 | 参照するもの | 提供元 |
|---|---|---|
| auth.js | `hyouiContract.resolve` / `httpError` (評価時に分割代入) | contract.js |
| auth.js | `hyouiAuthShare.createAuthShare` (評価時) | auth-share.js |
| auth.js | `navigator.locks` / `BroadcastChannel` / `navigator.credentials` / `history.replaceState` | ブラウザ |
| index.js | `hyouiContract.indexEndpoint` / `resolve` / `httpError` / `reportGatewayProtocol` | contract.js |
| index.js | `hyouiAuth.createAuth` → `AUTH.fetch` / `registerFromFragment` | auth.js |
| session.js | `hyouiContract` の 8 関数 (`:6-9`) | contract.js |
| session.js | `hyouiAuth.createAuth` → `AUTH.fetch` / `acquire` / `wsProtocol` / `onAccess` / `overlay.showRevoked` / `registerFromFragment` | auth.js |
| session.js | `Terminal` / `Unicode11Addon` / `FitAddon` / `WebLinksAddon` | vendor |
| session.js | `HyouiAmbiguousWidth.captureUnicode11` / `wrapProvider` | unicode-ambiguous.js |
| session.js | `__hyouiDebug` (存在チェック付き) | session.html インライン script |
| session.js が公開 | `window.__hyouiTerm` / `window.__hyouiTextSelection` (playwright / devtools 用 hook) | — |
| contract.js | `document.body` に帯を直接 insert (DOM 依存) | — |
| auth.js | `document.body` に overlay を直接 append (DOM 依存) | — |

CSS は `style.css` 1 枚を両ページで共有し、セレクタはページ別の名前空間を持たない (`.session-page` は main の flex 化にしか使われていない)。

## 6. session.js の内部構造

| 行範囲 | 区画 | 責務 |
|---|---|---|
| 1-25 | 起動 | contract 取得、endpoint、`AUTH` 生成、sid、title |
| 27-33 | embed | `?embed=1` → `body.embed` |
| 35-146 | 表示設定 query | 8 項目の parse / 検証 / 出自記録、`#term` 背景 |
| 148-238 | link fallback | URL 許可判定、popup 判定付きで開く、失敗時の aside |
| 240-264 | Terminal 生成 | xterm option 一式 |
| 265-326 | 文字幅 + addon | unicode / ambw provider、fit、web-links の load |
| 327-378 | font 待ち + open | `document.fonts.load` race、`term.open`、font ready 後の再測定、`__hyouiTerm` |
| 380-494 | IME 補正 | (A) value クリア / (B) 位置再同期 / キャンセル時二重送信抑止 |
| 496-594 | touch focus | tap 判定、パネル開時の close-only tap、click capture |
| 596-626 | DOM 参照 | getElementById 30 個の集約 |
| 628-822 | 情報タブ: 表示設定 | runtime 変更の定義配列と行の生成 |
| 824-908 | テキスト選択 overlay | buffer → 静止テキスト、`__hyouiTextSelection` |
| 910-916 | 雑多 | stoppedBanner 初期化、`timer` / `lastPayload` 宣言 |
| 918-1035 | resize | autoResize 永続化、POST/WS の選択、キュー、debounce、observer |
| 1037-1088 | session 状態 + resume | 自 1 件取得、停止帯、情報タブ Session 節、resume |
| 1090-1124 | screen polling | `fetchScreen` (reset + 全書き直し) |
| 1126-1204 | 入力パネル送信 | specs 組み立て、textarea、keypad、Key… |
| 1206-1398 | 浮遊位置 | sessionStorage、edge 相対 ⇔ 絶対座標、`?fab` query、FAB デザイン |
| 1400-1562 | 浮遊物 UI | drag 共通、タブ切替、パネル開閉、keypad 開閉、resize 再配置 |
| 1564-1570 | polling 制御 | `schedule` |
| 1572-1637 | WS 状態 | 変数群、`#wsStatus` 生成、attach.info 反映、cap 判定 |
| 1639-1727 | WS 要求 | resize / leader.request の Promise 相関、leader ボタン |
| 1729-1811 | WS 入力 | pending flush、`sendBytesToWs`、Shift+Enter、onData/onBinary |
| 1813-1831 | 認証延長 | `armAuthExtend` |
| 1833-1979 | WS 接続 | `connectWs`、kind 別 dispatch (if 連鎖)、close、再接続 |
| 1981-2016 | bootstrap | unload、refresh / reload ボタン、初回 fetch、interval、招待 URL |

### 責務が混在している箇所 (事実)

- touch 区画 (`:516-594`) が、後方で宣言される `inputPanel` (`:607`)・`closePanel` (`:1499`)・`textSelectionOpen` (`:827`) を参照する。イベント発火時には初期化済みなので動くが、区画の順序に意味が無く、区画単位で切り出せない
- 表示設定の `redrawForWidthChange` (`:649`) が screen polling 区画の `lastPayload` / `fetchScreen` を直接操作する
- テキスト選択 (`openTextSelection`, `:864`) が入力パネルを閉じ FAB を隠す (浮遊物 UI の状態を外から書く)
- `ws.onopen` / `onclose` が polling UI (`#auto` の disabled / title) と情報タブ (`#infoAttach*`) を直接書く
- fit 提案 → clamp の処理が `proposeAndQueueResize` (`:999-1008`) と `sendLeaderRequestOverWs` (`:1675-1685`) に重複
- leader 成功時に client が `term.resize` する (`:1719`) のは resize 区画の「daemon 受理後にだけ grid を変える」(`:920-923`) と別経路
- WS の要求相関 (resize / leader) が同形の Promise + timeout + Map を 2 回書いている (`:1641-1662` と `:1688-1705`)、`error` frame の受け側は両 Map を探す (`:1931`)

## 7. 見直し論点

列挙のみで、採否・改修案は確定しない。kawaz の webui 方針 (モダン CSS / JS を積極採用、古いハックで代替しない、ブラウザが持つ追従・貼り付き・伸縮を JS で自前制御しない) に反していると読める箇所は【方針】を付けた。

### 7.1 構造・責務

1. `session.js` が単一クロージャで、区画が相互の変数を直接読み書きする (6 節)。区画単位の切り出し・単体 test ができない。現状 JS test は `tests/js/auth-share.test.js` のみで、session.js に対する test は Rust 側の文字列 grep 2 本 (`lib.rs:1243` の 1 件 API 使用、`:1400` 付近の script 順) だけ
2. 全モジュールが classic script + `window` グローバルで、依存は HTML の並び順だけが保証する (auth.js は評価時に分割代入するので順序を間違えると即 TypeError)。【方針】ES modules (`<script type="module">` + import) を使っていない。bundler 無し (DR-0027 §4) とは両立する
3. 表示設定がモジュール変数と `term.options` の二重持ち。`openTextSelection` は `term.options`、情報タブの表示値は モジュール変数を読む
4. UI 状態の多くが DOM 属性にしか無い (attach mode / leader、停止帯、パネル開閉)。「今 leader か」を JS から問う手段が DOM テキストの読み戻ししか無い
5. 重複: query parse (`numParam` と `parseRuntimeNumber`)、`new URLSearchParams(location.search)` の 4 回生成 (`:30`, `:54`, `:928`, `:1321`)、等幅フォントチェーンが JS (`:109`) と CSS 5 箇所 (`style.css:205, 350, 551, 563, 718`) に写し、reload ボタン・招待 URL bootstrap が index.js / session.js に同文で存在
6. 古くなったコメント: `session.js:1-3` (「Input form POSTs」「every few seconds」— 現在は WS 主経路と浮遊パネル)、`session.js:1210` (「localStorage は 1 つ」— 実装は sessionStorage)、`style.css:364` (「JS 側で translate3d」— 実装は left/top)
7. dead CSS: `#inputForm` (`style.css:259-264`) は対応 DOM が無い (直前 `:243` のコメントが廃止を述べている)。index の `tr.row-stale` (`style.css:203`) は効くが、`row-error` / `row-no-response` (Hung) に対応する見た目は無い

### 7.2 重なり順・overlay

8. z-index が一元管理されておらず逆転している: `.auth-overlay` 200 < `#inputFab` 1000 < `.input-panel` 1001 < `.link-fallback` 1100 (`style.css:751, 385, 402, 673`)。ログイン overlay 表示中も FAB / 入力パネルが上に出て操作できる (CSS からの帰結、実機未確認)。`.protocol-banner` は 100、`.text-selection-overlay` は 20
9. 【方針】overlay / dialog 相当を 4 種自作している (auth overlay、link fallback aside、入力パネル、テキスト選択)。`<dialog>` (`showModal` の top layer・Esc・focus 管理・`inert`) や popover API を使っていない。auth overlay は `aria-modal="true"` だが focus trap / 初期 focus (登録時の code 以外) / ラベル関連付けが無い
10. ネイティブ `alert()` (`:1082`) / `prompt()` (`:1201`) / `confirm()` (`:1992`) を使用。sandbox 付き iframe (embed) で `allow-modals` が無いと黙って無効になる可能性 (未確認)。iPad では `prompt` の UX も粗い

### 7.3 レイアウト・位置制御

11. 【方針】浮遊物 (FAB / パネル) の位置を JS が `left/top` の絶対座標で書き、viewport resize のたびに `applyEdgePos` で再計算・clamp している (`:1270-1288`, `:1558-1562`)。保存形式は既に edge 相対 (`right`/`bottom` + 距離) なので、CSS の `right` / `bottom` + `clamp()` / `min()` に任せればブラウザが追従する形に置ける。keypad 開閉・タブ切替のたびに rAF で再配置しているのも同根 (`:1471`, `:1550`)
12. 【方針】embed の `main { height: 100vh }` (`style.css:227`)。モバイルで動的ツールバー分ずれる `vh` で、`dvh` / `svh` 系を使っていない。さらに帯 (`#protocolBanner`, sticky) は body 先頭に入るので、embed で帯が出ると `main` の 100vh と合わせて viewport を超える (CSS からの推論、未確認)
13. 【方針】`body.embed #term .xterm, .xterm-viewport, .xterm-screen { width/height: 100% !important }` (`style.css:237-242`) で xterm の寸法を上書き。xterm は自前で canvas 寸法を管理するので、`!important` で外側から伸ばすのは描画寸法との不一致の温床になりうる (未確認)
14. `scheduleFit` が `window` resize と `ResizeObserver(#term)` の両方に登録されている (`:1030-1035`)。後者だけで足りる可能性
15. `.link-fallback` の中央寄せが `left: 50%` + `transform: translateX(-50%)` (`style.css:671, 676`)。`inset-inline` + `margin-inline: auto` 等で書ける
16. `<span id="size" class="meta">` と `#wsStatus.meta` が `p.meta` の中で `.meta` を再利用しており、`display:flex` と `margin: 0.25rem 0 1rem` が入れ子要素にも掛かる (`session.html:30`, `session.js:1601`, `style.css:135-143`)。`#wsStatus` は inline style `marginLeft` も持つ

### 7.4 モバイル (IME / Safari / iOS / soft keyboard)

17. IME 補正は xterm.js 5.3.0 の内部 API (`_syncTextArea` / `_compositionHelper`) に依存。実機 Safari / iOS は未検証 (`docs/issue/2026-07-26-web-ime-safari-ios-unverified.md` が open)
18. soft keyboard 対応が無い: `visualViewport` の利用も viewport meta の `interactive-widget` 指定も無い (grep で 0 件)。iPad で入力パネル (fixed) が keyboard の下に隠れるかは未確認
19. `apple-mobile-web-app-status-bar-style: black-translucent` だが `viewport-fit=cover` も `env(safe-area-inset-*)` も無い (grep で 0 件)。standalone PWA で header / FAB がステータスバーやホームインジケータと重なるかは未確認
20. touch の focus トグルは xterm の Linkifier2 の mouse 互換イベント順に依存し、`setTimeout(() => term.blur(), 0)` を重ねて focus を奪い返している (`:557, 566, 576, 593`)。vendor 更新で崩れる前提がコメントに明記されている (`vendor/README.md` 末尾も実機再検証を要求)
21. ソート可能な `th` (index) はクリックのみで、button / tabindex が無くキーボードから操作できない (`aria-sort` は付いている)

### 7.5 時間待ち・polling

22. `setTimeout(fetchScreen, 300)` (`:1140`) と resume 後の `setTimeout(..., 300)` (`:1080`) は応答到着を時間で待っている。WS 非接続の fallback 経路に限られるが、根拠のない間隔値
23. `refreshSessionStatus` は WS 接続中も 5 秒 interval で回り続ける (`:2006`)。WS 側には child 停止を伝える frame が無いため (API 側の論点、web-api-protocol-inventory に委ねる)
24. index の `setInterval(fetchSessions, 3000)` は前回の完了を待たない (遅い応答で重なりうる)。tbody を毎回 `innerHTML` で全再生成するので、行内のテキスト選択や focus が 3 秒ごとに失われる
25. WS 未接続時の xterm 打鍵を 8 KiB まで溜め、再接続時に一括送信する (`:1585-1587`, `:1729-1736`)。長い切断の後に古い打鍵が (leader が変わっている可能性も含めて) 遅れて届く。コメントは意図的としている

### 7.6 経路の二重化

26. 入力パネル / keypad / Key… は WS 接続中でも HTTP POST `/input` を使い、xterm 直打ちは WS binary を使う。同じ「子への入力」が 2 経路で、`/input` 側には auto-lock (DR-0022、DR-0035 Related の記述による) が掛かる。到着順の保証も経路間には無い
27. `fetchScreen` の失敗処理は `'HTTP ' + status` で contract の `httpError` を使わず (`:1107`)、404 も code を見ない (`:1103`)。`fetchOwnSession` は code を見る (`:1046`)。同ファイル内でエラー読み取りの流儀が 2 つ

### 7.7 DR / issue が要求していて未実装、または食い違う UI

28. DR-0035 決定 3 は `hello` / `/version` の `version` / `build_id` を「表示 (DR-0027 §3 の情報タブ) のためだけ」に載せるとするが、session.js は `hello.version` / `build_id` を読まず、情報タブに版表示は無い (`:1883-1895`)
29. DR-0032 の「後続 issue として残す」に web UI 側の child action menu 相当がある (`DR-0032:379`)。web にあるのは停止帯 + resume ボタンだけ。`on_child_suspend` は index の一覧に表示されるが session ページには出ない
30. 一覧 API は stale / error / no-response の `reason` を返すが (`lib.rs:230-243`)、index は表示しない (status 文字列のみ)
31. 情報タブの `child-state` は `me.status` (= `live` / `stopped` / `stale` …) を出しており、ラベル (child の状態) とセッション状態の語が混ざっている。フォールバックの `'running'` (`:1060`) は `status` が常に返るため到達しない
32. DR-0033 §6 の WS frame 例は `request_id` (snake)、DR-0035 の表と実装は `requestId`。front は `requestId` で動いている (文書の食い違い。契約側の論点として web-api-protocol-inventory に委ねる)
33. `docs/issue/2026-07-21-screen-overlay-general-mechanism.md` (open) は web ターミナルでのダイアログ / 通知表示をこの機構上で実装することを受け入れ条件に含む。現状の web の帯・overlay はすべて DOM 側の独自実装で、screen state の機構とは無関係
34. `docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md` (open): attach 前に出た OSC 8 リンクは web の初期画面 (`fetchScreen` の screen dump 投入) で死ぬ。front からは直せない (screen state 側の課題) が、web のリンク UI の完成度に直結する

### 7.8 xterm.js 内部 API 依存

35. 内部 API 参照が 4 系統 (1 節の表)。いずれも存在チェック付きで「壊れたら補正を諦める」設計 (主張: コメント)。vendor を上げる時の再検証項目が `vendor/README.md` には link tap しか書かれておらず、IME 補正・font 再測定・ambw V6 base は列挙されていない

## 検証の詳細

### 実機観測 (2026-10-03)

| 観測 | コマンド | 結果 |
|---|---|---|
| 稼働 gateway の世代 | `curl http://127.0.0.1:43690/version` / `:43691/version` | 両方 `{"version":"0.9.66","protocol":1}` (build_id は別) |
| `/api/*` の認証 | `curl -o /dev/null -w '%{http_code}' http://127.0.0.1:43690/api/sessions` | `401` |
| 配信 assets と作業コピーの一致 | 両 port の `/`、`/sessions/x`、`/assets/session.js` を `diff -q` | 3 ファイルとも一致 |
| asset の応答ヘッダ | `curl -D - .../assets/session.js` | `content-type` のみ。cache 系ヘッダなし |

### 静的検査

| 項目 | 方法 | 結果 |
|---|---|---|
| `visualViewport` / `dvh` / `safe-area` / `<dialog` / `popover` / `interactive-widget` / `viewport-fit` | `command grep -E` (assets 直下の js/css/html) | 0 件 |
| `#inputForm` の DOM | 同上 | CSS (`style.css:243, 259`) のみ、HTML / JS に無し |
| xterm 内部 API | `command grep -n '_core'` | `session.js:280, 365, 414` (+ `:420, 423, 478` で `imeCore` 経由) |
| storage | `command grep localStorage\|sessionStorage` | localStorage `hyoui.session.autoResize` (読み書き) + 旧 2 キーの削除、sessionStorage `hyoui.session.floatPos` |
| JS test | `ls crates/hyoui-web/tests/js` | `auth-share.test.js` のみ (`just test-js`) |

### 未確認 (ブラウザ DOM を開いていないもの)

- ログイン overlay 表示中に FAB / パネルが上に出ること (7.2-8)、embed で帯が出た時の縦はみ出し (7.3-12)、`!important` 上書きの描画影響 (7.3-13)、iframe sandbox 下の `alert` / `prompt` / `confirm` (7.2-10)、iOS の soft keyboard / safe-area (7.4-18, 19)。いずれも CSS / JS からの推論で、passkey 認証が要るため実ブラウザで確認していない
- DOM ツリーのうち xterm.js が生成する内部構造 (`.xterm` 配下) は vendor の既定であり、本書では列挙していない
