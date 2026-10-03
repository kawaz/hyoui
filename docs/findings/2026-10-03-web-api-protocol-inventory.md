# ブラウザから見える web API / プロトコルの棚卸し (2026-10-03)

対象: v0.9.66 (202f99e0) 以降の working copy。`crates/hyoui-web/src/` と `crates/hyoui/src` の該当範囲は 202f99e0 から変更なし (`jj diff --stat -r '202f99e0..@'` が 0 files)。Web UI 強化の前段として、gateway が browser に晒している HTTP / WS / 認証の境界と、それが背後の daemon protocol のどこに写るかを棚卸しする。設計判断・提案の採否は書かない。

起点: `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md` (以下「09-15 棚卸し」)。これは仮定として扱い、現行実装で裏取りした差分を §8 に書く。DOM / UI 構造は `docs/findings/2026-10-03-web-component-tree.md` の範囲なので本書では扱わない。

## 凡例

- **根拠種別**: `observed(実機)` = gateway を実際に起動して curl / WS client で応答を観測、`observed` = コードを読んで確認、`claimed` = コメントや DR の主張で、コード / 実機で未確認
- 行番号は 202f99e0 時点。`lib.rs` は `crates/hyoui-web/src/lib.rs`、`routes.rs` は `crates/hyoui-web/src/auth/routes.rs`、JS は `crates/hyoui-web/assets/` 配下
- 実機 probe の環境: `target/debug/hyoui` (0.9.66 / 202f99e0)、隔離した `XDG_STATE_HOME` (tempdir)、`auth.json` に family 1 本を直接置いた fixture (DR-0036 決定 9 の test と同じ形)、`hyoui run --detached --session probe -- bash --norc --noprofile`、gateway は `127.0.0.1:43799`、WS client は node 26 の組み込み `WebSocket`。終了後に tempdir ごと削除した

## 判明した事実

- **route は WS を含めて 16 本** (HTML 2 / assets 1 / 無認証の `/healthz` `/version` 2 / `/auth/*` 4 / `/api/*` HTTP 6 / WS attach 1)。`/api/*` と WS attach だけが `require_auth` middleware で守られ、他は無認証 (`lib.rs:112-133`)。`/auth/register` `/auth/assert` の成功経路 (実 authenticator が要る) 以外は observed(実機) で status を確認した
- **front が呼ばない route は `/healthz` だけ**。`layer=visible` / `layer=scrollback` は front 未使用 (session.js は常に `layer=both`)、`POST /resize` は WS 未接続時の fallback としてだけ使う
- **WS は binary = PTY bytes の 1:1 転写、text = `kind` タグ付き JSON** の 2 系統。daemon の CBOR control message はほぼ全て gateway で捨てられ、browser に届くのは `leader.notify` / `mode.change` から合成した `attach.info` だけ (`ws_attach.rs:376-387`)。`session.exit.notify` / `session.child.stopped.notify` は届かない
- **WS 接続の寿命に access token の期限が効いていない** (observed)。`hello.auth_expires_at` は表示用に送るだけで、期限切れで接続を切る処理も、`auth.extend` で延長された期限を接続側に記録する処理も無い。DR-0036 決定 5 の「切るのは延長を怠った接続だけ」は実装されていない (§7)
- **DR-0035 の契約表と実装で `auth.extend.result` の field 名が食い違う**: 表は `auth_expires_at`、実装 (`contract.rs:581`) と e2e (`web_e2e_api.rs:909`) は `expires_at`。observed(実機) でも `{"kind":"auth.extend.result","requestId":3,"ok":true,"expires_at":"..."}` を受けた
- **`/api/sessions` 系の session オブジェクトは契約型を持たない**。`session_entry_to_json` が `serde_json::json!` で手書きしており (`lib.rs:207-245`)、`contract.rs` 冒頭の「`lib.rs` は `json!` の手書きリテラルを持たない」(DR-0035 決定 1) に反する。golden test も無い
- **エラー形の統一 (DR-0035 決定 2) には 3 つの穴がある** (observed(実機)): 未定義 route の 404 と method 違いの 405 は空 body、WS upgrade の前提ヘッダ不足は axum 既定の `text/plain` 400 (`Connection header did not include 'upgrade'`)
- **契約は手書きの 2 写し** (Rust `contract.rs` / JS 各ファイルのリテラル)。自動生成は無く、Rust ↔ JS の一致を機械で固定しているのは `WEB_PROTOCOL_VERSION` の値 1 つだけ (`lib.rs:1084`)。kind 名・field 名・error code 名は JS 側にリテラルで散在する
- **認証は passkey → access (メモリ) + refresh (httpOnly cookie) の 2 段**で、cookie 名は `__Secure-hyoui-<sha256(endpoint) 先頭 16 hex>`、`Max-Age=604800` (7 日)、`Path` = endpoint の path から末尾 `/` を落とした値、access は 4 時間 (`record.rs:360-365`)。`code::UNSUPPORTED` は定義だけあって使われていない

## 検証の詳細

### 1. HTTP endpoint 一覧

認証列: 「要」= `require_auth` (`routes.rs:184-221`) を通る。token は `Authorization: Bearer <access>` か WS subprotocol `hyoui.token.<access>` (`routes.rs:224-253`)。未提示・不明・期限切れ・tombstone はすべて `401 {"error":{"code":"auth-required",...}}` で区別しない (observed(実機))。

エラー body は特記なき限り `{"error":{"code":"<code>","message":"<人向け>"}}` (`contract.rs:259`)。

| method | path | 認証 | request | 成功応答 | 失敗応答 (status / code) | 実装 | front の呼び出し |
|---|---|---|---|---|---|---|---|
| GET | `/` | 不要 | — | 200 `index.html` (`text/html`) | 404 `asset-not-found` (assets dir 開発モードで欠落時) | `lib.rs:861` | ブラウザ遷移 (`session.html:20` の `../`) |
| GET | `/sessions/{id}` | 不要 | query は表示設定 (DR-0027 §5)。server は `id` も query も見ない | 200 `session.html` | 同上 | `lib.rs:869` | `index.js:139` (一覧のリンク) |
| GET | `/assets/{*path}` | 不要 | — | 200 ファイル本体 (`content_type_for`) | 404 `invalid-asset-path` (`..` / 空 component)、404 `asset-not-found` | `lib.rs:877`, `:890` | `index.html` / `session.html` の `<link>` `<script>` |
| GET | `/healthz` | 不要 | — | 200 `ok` (`text/plain`) | — | `lib.rs:162` | **front 未使用** (`hyoui web daemon status` 用、DR-0034) |
| GET | `/version` | 不要 | — | 200 `VersionResponse` `{"version","build_id","protocol"}` | — | `lib.rs:166`, `contract.rs:276` | `index.js:101` (一覧 polling と同周期、3 秒)。session ページは呼ばない (hello で代替) |
| POST | `/auth/challenge` | 不要 (rate limit) | `ChallengeRequest` `{endpoint, purpose?: "assert"\|"register", jwt?}` | 200 `ChallengeResponse` `{challenge_id, endpoint, options}` | 400 `invalid-request` (body 不正。endpoint の正規化失敗もここ)、401 `auth-failed`、429 `rate-limited`、500 `internal-error` | `routes.rs:259` | `auth.js:116` (呼出 `:208` assert / `:249` register) |
| POST | `/auth/register` | 不要 (rate limit) | `RegisterRequest` `{endpoint, challenge_id, jwt, code, credential, device_label?}` | 200 `SessionResponse` `{access_token, expires_at, sub}` + `Set-Cookie` (refresh) | 400 / 401 `auth-failed` / 429 / 500 | `routes.rs:370` | `auth.js:122` (呼出 `:254`) |
| POST | `/auth/assert` | 不要 (rate limit) | `AssertRequest` `{endpoint, challenge_id, credential}` | 200 `SessionResponse` + `Set-Cookie` | 400 / 401 / 429 / 500 | `routes.rs:540` | `auth.js:119` (呼出 `:213`) |
| POST | `/auth/refresh` | cookie | `RefreshRequest` `{endpoint}` + refresh cookie | 200 `SessionResponse` + `Set-Cookie` (rotate) | 401 `auth-failed` (失効時は `Max-Age=0` の `Set-Cookie` を同送)、400 / 429 / 500 | `routes.rs:611` | `auth.js:125` (呼出 `:140`、`auth-share.js` 経由) |
| GET | `/api/sessions` | 要 | — | 200 session オブジェクトの配列 | 401、500 `internal-error` | `lib.rs:177` | `index.js:114` |
| GET | `/api/sessions/{id}` | 要 | — | 200 session オブジェクト 1 件 (stale / error / no-response も 200 + `status`) | 401、404 `session-not-found`、500 | `lib.rs:199` | `session.js:1042` (5 秒周期 `:2006`) |
| GET | `/api/sessions/{id}/screen` | 要 | `layer=visible\|scrollback\|both` (既定 visible) | 200 ANSI bytes (`text/plain; charset=utf-8`) | 400 `invalid-request` (layer 不正)、401、404 `session-not-found` / `session-error` / `session-no-response`、501 `unsupported-capability`、500 | `lib.rs:257` | `session.js:1102` (常に `layer=both`) |
| POST | `/api/sessions/{id}/input` | 要 | `InputRequest` `{"specs":[...]}` (`text:` / `hex:` / `key:` / `paste:` のみ) | 200 `InputResponse` `{sent_bytes, specs}` | 400 `invalid-input-spec` / `invalid-request`、415 / 422 `invalid-request` (axum rejection の status を尊重)、401、404、409 `lock-contention`、501、503 `lock-unavailable`、500 | `lib.rs:395` | `session.js:1130` |
| POST | `/api/sessions/{id}/resume` | 要 | 空 | 204 | 401、404、501、500 | `lib.rs:558` | `session.js:1077` |
| POST | `/api/sessions/{id}/resize` | 要 | `ResizeRequest` `{cols, rows}` (> 0) | 204 | 400 / 415 / 422 `invalid-request`、401、404、409 `mode.not-leader` (daemon の code をそのまま)、500 | `lib.rs:603` | `session.js:952` (WS 未接続時の fallback のみ、`:960-968`) |
| GET | `/api/sessions/{id}/attach` | 要 (subprotocol 可) | WS upgrade | 101 (選んだ subprotocol を echo) | 400 **`text/plain`** (axum `WebSocketUpgrade` rejection)、401、404 | `lib.rs:720` | `session.js:1836-1850` |
| * | 上記以外 | — | — | — | 404 / 405 **空 body** | (axum 既定) | — |

session オブジェクトの形 (`lib.rs:207-245`、observed(実機) で live を確認):

| `status` | field |
|---|---|
| `live` / `stopped` | `session_id`, `namespace`, `socket_path`, `started_unix_ms`, `status`, `cwd`, `argv`, `clients`, `child_pid`, `child_pgid`, `child_stopped`, `on_child_suspend` (`notify`\|`auto-resume`\|null), `daemon_version` (空なら null) |
| `stale` / `error` | `session_id`, `namespace`, `socket_path`, `status`, `reason` |
| `no-response` | `daemon_pid`, `session_id`, `namespace`, `socket_path`, `started_unix_ms`, `status`, `reason` |

補足 (observed):

- `/auth/*` の 4 本は body 上限 64 KiB と、**gateway プロセス全体で 1 本の** 30 req/s 固定窓 rate limit を共有する (`routes.rs:49-53`, `:105-134`, `:152-160`)。client ごとの窓ではない。`/api/*` の body 上限は axum 既定 (claimed: 2 MB、未確認)
- `/auth/*` の失敗は `Denied` を一律 `auth-failed` に翻訳して理由を出さない (`routes.rs:787-795`)。ただし `AuthFailure::Bad` (endpoint が RP にならない) だけは理由を 400 の message に載せる (`routes.rs:782-786`)
- 204 応答にも `content-type: text/plain; charset=utf-8` が付く (observed(実機)。`(StatusCode::NO_CONTENT, "")` の帰結、`lib.rs:565`, `:628`)
- `/version` の `content-type` は `application/json` (charset なし)、`/api/*` の JSON も同じ (observed(実機))
- session id の解決は `discovery::find_session` を `SingleFlight` で session_id ごとに束ね、一覧は全走査を 1 本に束ねる (`lib.rs:177-190`, `:825-836`)。namespace を path / query で指定する口は無い

### 2. 静的 asset 配信

| 項目 | 事実 | 根拠 |
|---|---|---|
| 埋め込み | `include_dir!("$CARGO_MANIFEST_DIR/assets")` で `assets/` 全体 (vendor/ 含む) を binary に埋め込む | `lib.rs:57` |
| 開発モード | `--web-assets-dir` / config `[web].assets_dir` 指定時は `tokio::fs::read` で都度読む | `lib.rs:890-900` |
| 経路 | `/` → `index.html`、`/sessions/{id}` → `session.html`、`/assets/<path>` → `<path>`。**HTML も `/assets/index.html` で取れる** (同じ `serve_asset`) | `lib.rs:861-886` |
| content-type | 拡張子による固定表 (html / css / js / json / svg / png / ico / woff2 / woff / ttf / webmanifest、他は `application/octet-stream`) | `lib.rs:915-930` |
| cache | `Cache-Control` / `ETag` / `Last-Modified` を**付けない**。応答ヘッダは `content-type` / `content-length` / `date` だけ | observed(実機) (`curl -I /assets/contract.js`) |
| 世代の名乗り | ファイル名にも query にも hash / version を付けない。応答ヘッダにも protocol を名乗るものが無い (test `no_response_header_announces_the_protocol`、`lib.rs:1116`) | observed |
| HTML の参照 | `index.html` は `assets/...`、`session.html` は `../assets/...` の相対参照。JS は plain `<script>` で読み込み、global (`window.hyouiContract` / `hyouiAuthShare` / `hyouiAuth` / `HyouiAmbiguousWidth`) で受け渡す。読み込み順は contract → auth-share → auth → (unicode-ambiguous) → index / session | `index.html:48-51`, `session.html:178-186` |
| 絶対パス禁止の固定 | test `assets_contain_no_absolute_path_references` が `"/assets/` `'/api/` `"/sessions/` `'/version'` 等を禁止 (vendor/ は対象外)。`'/auth/` と `/healthz` は禁止リストに無い | `lib.rs:1490-1524` |
| iframe | `X-Frame-Options` / CSP `frame-ancestors` を意図的に付けない (test で固定) | `lib.rs:139-144`, `:1568` |
| PWA | `manifest.webmanifest` の `start_url` / `scope` は `../`、icon は `icon.svg` | observed |

### 3. WebSocket

| 項目 | 事実 | 根拠 |
|---|---|---|
| URL | `<endpoint>api/sessions/<id>/attach` を `ws:` / `wss:` に置換 (`resolveWs`) | `contract.js:58-62`, `session.js:1836` |
| 認証 | subprotocol `hyoui.token.<access>` を 1 本提案。gateway は middleware で検証後、同じ文字列を echo | `session.js:1850`, `lib.rs:731-735` / observed(実機) (`ws.protocol` が echo 値) |
| upgrade 前の判定 | 認証 → session 解決 (live 以外は 404) → upgrade。upgrade 後の失敗は close のみで理由 frame は出ない | `lib.rs:727-740`, `ws_attach.rs:609-620` |
| framing | **binary frame = PTY bytes** (双方向、契約の世代に依存しない)、**text frame = UTF-8 JSON 1 個**で `kind` タグ。browser は文字列入力も `TextEncoder` で binary 化して送る | `ws_attach.rs:116-117`, `:194-195`, `session.js:1738-1758` |
| 世代の伝達 | 接続直後に `hello` (`protocol` = `WEB_PROTOCOL_VERSION` = 1) を 1 回、`attach.info` より前に送る。session.js は `reportGatewayProtocol` で比較し、不一致なら帯を出して制御 frame の送信を止める (binary は通す) | `ws_attach.rs:326-339`, `session.js:1883-1896`, `contract.js:131-147` |
| close | gateway 起因の close は常に `Message::Close(None)` (= close code 1005、reason 無し) | `ws_attach.rs:202` / observed(実機) (`auth.extend` 失敗後も、子 exit 後も `code=1005`) |
| 再接続 | session.js は 1 秒から倍々、上限 30 秒の backoff。onclose で cap を不明に戻し、fallback の screen polling を再開 | `session.js:1945-1979` |

browser → gateway (text):

| kind | 型 (`contract.rs`) | field | 応答 | 送信箇所 | 受信処理 |
|---|---|---|---|---|---|
| `resize` | `ClientFrame::Resize` (`:442`) | `requestId: u64`, `cols: u16`, `rows: u16` | `resize.result` | `session.js:1655` | `ws_attach.rs:119-127` → bridge |
| `leader.request` | `ClientFrame::LeaderRequest` (`:453`) | `requestId` | `leader.result` | `session.js:1699` | `ws_attach.rs:128-130` → bridge |
| `auth.extend` | `ClientFrame::AuthExtend` (`:465`) | `requestId`, `accessToken` | `auth.extend.result` | `session.js:1825` (access 差し替えのたび) | `ws_attach.rs:137-158` (reader task 内で完結、daemon へは行かない) |

gateway → browser (text):

| kind | 型 | field | 送信契機 / 箇所 | 受信箇所 |
|---|---|---|---|---|
| `hello` | `ServerFrame::Hello` (`:521`) | `protocol`, `version`, `build_id\|null`, `caps[]` (daemon と intersect 済み), `auth_expires_at` (ISO 8601、常に非 null) | 接続直後 1 回 (`ws_attach.rs:326`) | `session.js:1883` |
| `attach.info` | `ServerFrame::AttachInfo` (`:536`) | `mode`: `rw`\|`ro`\|`rw-no-leader`\|`unknown`, `leader: bool` | hello 直後、`leader.notify` / `mode.change` 受信時、`leader.request` 処理中 (`ws_attach.rs:339`, `:376-386`, `:515-524`) | `session.js:1907` |
| `resize.result` | `ServerFrame::ResizeResult` (`:544`) | `requestId`, `ok`, `error?` | `ws_attach.rs:458` | `session.js:1911` |
| `leader.result` | `ServerFrame::LeaderResult` (`:556`) | `requestId`, `ok`, `error?` | `ws_attach.rs:466` | `session.js:1919` |
| `auth.extend.result` | `ServerFrame::AuthExtendResult` (`:573`) | `requestId`, `ok`, **`expires_at?`**, `error?` | `ws_attach.rs:143`。`ok:false` の後に接続を閉じる | `session.js:1897` (`ok` だけ見る。`expires_at` は読まない) |
| `error` | `ServerFrame::Error` (`:592`) | `requestId: null` 固定, `error` | 未知 kind / 不正 JSON (`ws_attach.rs:159-174`) | `session.js:1927` |

実機で観測した frame 列 (observed(実機)、bash session に接続):

```text
open protocol= hyoui.token.<access>
text   {"kind":"hello","protocol":1,"version":"0.9.66","build_id":"202f99e0","caps":[11 個],"auth_expires_at":"..."}
text   {"kind":"attach.info","mode":"rw","leader":true}
binary 71 (接続時の復元 bytes)
text   {"kind":"attach.info","mode":"rw","leader":true}      ← 接続直後に 2 回目が来る (leader.notify 由来と推定、未確認)
binary ... (送った binary "echo ..." の echo と出力)
text   {"kind":"resize.result","requestId":1,"ok":true}
text   {"kind":"error","requestId":null,"error":{"code":"unknown-kind","message":"unrecognized WS text frame: unknown variant `nope`, ..."}}
text   {"kind":"error","requestId":null,"error":{"code":"unknown-kind","message":"unrecognized WS text frame: expected ident ..."}}
text   {"kind":"attach.info","mode":"rw","leader":true}
text   {"kind":"leader.result","requestId":2,"ok":true}
text   {"kind":"auth.extend.result","requestId":3,"ok":true,"expires_at":"..."}
text   {"kind":"auth.extend.result","requestId":4,"ok":false,"error":{"code":"auth-failed",...}}
close  code=1005 reason=""
```

- 未知 kind の `error` は、元 frame に `requestId: 9` が読めても `null` で返る (相関しない設計、`ws_attach.rs:160-162`)
- 子が `exit 3` で終了した場合: 最後の出力 binary の後、理由 frame 無しで `code=1005` の close。gateway の log は `WS attach ended: recv_frame: invalid argument: frame decode failed` (observed(実機))。exit status は browser に届かない

### 4. ws_attach と daemon protocol の対応

1 WS = 1 `ClientConnection` (mode `Rw`、caps = `MVP_CAPS` 全 11 個を要求、token = gateway プロセスの `HYOUI_LOCK_TOKEN`、exclusive / detach_others は false) (`ws_attach.rs:314-322`)。auto-lock は取らない。

| daemon → gateway | 扱い | 根拠 |
|---|---|---|
| frame `TYPE_RAW_DATA` (0x00) | **透過**: body を binary frame に 1:1 | `ws_attach.rs:369-373` |
| `handshake.response` | **変換**: `caps` → `hello.caps`、`mode` / `leader` → `attach.info` | `ws_attach.rs:326-339` |
| `leader.notify` | **変換**: 自 client_id との一致で `leader` を更新して `attach.info` | `ws_attach.rs:376-380` |
| `mode.change` | **変換**: `client_mode` があれば `attach.info` | `ws_attach.rs:381-386` |
| `status.response` | **消費**: resize の FIFO barrier としてだけ使う | `ws_attach.rs:595` |
| `error` (resize / leader.request 中) | **変換**: daemon の code / message をそのまま `*.result.error` へ | `ws_attach.rs:529-531`, `:597-599` |
| `session.exit.notify` / `session.child.stopped.notify` / `tail.*` / `record.*` / `set.ack` / `upgrade.ack` 等その他全部 | **捨てる** (`Ok(_) => {}`) | `ws_attach.rs:387` |
| frame `TYPE_RAW_ACK` (0x02) | `send_raw_bytes` 内で同期消費 | `ws_attach.rs:393` |

| browser → daemon | 写し先 | 根拠 |
|---|---|---|
| binary | wake ごとに queue を結合して `send_raw_bytes` (DR-0021 raw_ack 同期)。`Error::Remote` (ro-rejected / lock-not-held) は **log だけで browser に返さない** | `ws_attach.rs:435-446` |
| `resize` | leader なら `resize` + `status.query`、非 leader なら daemon に送らず `mode.not-leader` を返す (grid は `latest_grid` に保持) | `ws_attach.rs:553-607` |
| `leader.request` | cap `leader-request-v1` を確認して `leader.request` を送り、`leader.notify` まで待つ。成功後 `latest_grid` で resize | `ws_attach.rs:488-549` |
| `auth.extend` | daemon へ行かない (gateway の `auth.json` を読むだけ) | `ws_attach.rs:131-158` |
| WS close | 明示 `detach` は送らず socket 切断に任せる | `ws_attach.rs:22-24` |

HTTP 経路が使う daemon message (`lib.rs`、すべて短命 connection):

| 経路 | mode | message | 要求 cap |
|---|---|---|---|
| `/api/sessions`, `/api/sessions/{id}` (と全 `/api/sessions/{id}/*` の解決) | Ro | `status.query` / `status.response` (`discovery::query_status`) | — |
| `GET .../screen` | Ro | `screen.dump.request` (format=ansi, rect なし) / `.response` | `screen-dump-v1` |
| `POST .../input` | Rw | `lock.acquire` / `lock.release` (auto-lock 5 秒) + raw_data / raw_ack | `data` |
| `POST .../resume` | Rw | `session.child.resume.request` | `child-state-v1` |
| `POST .../resize` | Rw | `resize` + `status.query` (barrier)。handshake で leader が取れなければ 409 | (cap 確認なし) |

**ブラウザから到達できる daemon 機能**: PTY 入出力 (WS binary / `POST input`)、resize、leader 奪取、screen dump (ANSI、viewport + scrollback)、session 状態 (`status.query` 相当)、stopped child の resume。

**到達できない daemon 機能** (daemon message はあるが gateway に写す経路が無い): `tail.*` (出力履歴)、`screen.snapshot` (cells / cursor / mode)、`record.*`、`set.*`、`signal`、`kill`、`detach` (others / all)、明示 `lock.*` (WS 側)、`upgrade.*`、`session.exit.notify` / `session.child.stopped.notify` の push 受信 (stopped は `GET /api/sessions/{id}` の 5 秒 polling で代替、`session.js:2006`)、`screen.dump` の `rect` / ANSI 以外の format、`wait` / `wait-idle` / `file:` の input spec (`lib.rs:442-458` で 400)、namespace 指定。

### 5. 認証フロー

```text
host CLI: hyoui web passkey add --endpoint <url>
  └─ pending.json に {jti, sub, user_id, access, hmac_secret, code_hash, exp(10 分)} を書く (gateway 不要)
  └─ 招待 URL <endpoint>#register=<jwt(HS256)> と 6 桁コードを表示
browser (top-level のみ): auth.js が fragment から jwt を読み、history.replaceState で URL から消す (auth.js:316-324)
  1. POST auth/challenge {endpoint, purpose:"register", jwt}   → options (create 用)
  2. navigator.credentials.create()
  3. POST auth/register {endpoint, challenge_id, jwt, code, credential, device_label}
       → 200 {access_token, expires_at, sub} + Set-Cookie refresh   (登録 = 即サインイン)
以後:
  /api/* は Authorization: Bearer <access>、WS は subprotocol hyoui.token.<access>
  401 を受けたら: 他タブへ ask → Web Lock 下で POST auth/refresh → それでも駄目なら overlay で passkey ログイン
       (POST auth/challenge {purpose:"assert"} → credentials.get() → POST auth/assert)
  access 残り 10% で refreshAhead → 新 access を同一 WS に auth.extend で提示
```

| 項目 | 事実 | 根拠 |
|---|---|---|
| access token | 32 byte 乱数の base64url、署名なし。寿命 4 時間。ブラウザのメモリのみ (storage に書かない) | `token.rs:22-35`, `record.rs:365`, `auth.js:11-13` |
| access の据え置き | refresh rotate 時、残り寿命が TTL の半分以上なら access を差し替えない | `record.rs:434-439` |
| refresh token | 32 byte 乱数、7 日。使うたび rotate、退役世代は digest を保持。直前 1 世代は 60 秒の再送猶予 (Replay)、それ以外の旧世代の再提示は family を tombstone (Reused) | `record.rs:357-440`, `routes.rs:647-688` |
| cookie | 名前 `__Secure-hyoui-<sha256(endpoint) 先頭 16 hex>`、属性 `Max-Age=604800; Path=<endpoint path から末尾 / を落とした値>; HttpOnly; Secure; SameSite=Strict`。失効時は `Max-Age=0` | `token.rs:77-104`, `routes.rs:715-731`, `:633-641` |
| `/api/*` の検証 | 毎回 `auth.json` を読み、全 endpoint の family から access 値で引く (**提示先の endpoint とは突き合わせない** — gateway は endpoint を知らない設計) | `routes.rs:196-214`, `record.rs:191-203` |
| 失効 | CLI (`passkey remove` / `session remove`) が tombstone を書く。HTTP は次の要求で落ちる。確立済み WS は次の `auth.extend` でだけ落ちる | `mod.rs:11-12`, `ws_attach.rs:131-158` |
| WS の期限 | `hello.auth_expires_at` は接続時の access 期限。**接続を期限で切る処理は無い**。`auth.extend` は提示 token が「どれかの live family の access」なら ok を返し、接続元 family との一致は見ない | `ws_attach.rs:208`, `routes.rs:856-869` |
| tab 協調 | lock 名 `hyoui.auth.refresh:<endpoint>[:<sub>]`、BroadcastChannel 名 `hyoui.auth:<endpoint>` (sub を含めない、Design rationale あり)。ask 待ち 50 ms | `auth-share.js:29`, `:56-76` |
| 招待 URL の取り込み | `createAuth` 時に同期で fragment を読む。iframe 内なら登録せず「別タブで開く」を出す | `auth.js:156`, `:241-247` |
| iframe 内ログイン | overlay に「別タブで開く」リンクを併記 (`allow` 属性が無い親向け) | `auth.js:379-388` |

### 6. `contract.rs` と JS の対応

**生成ではなく手書き**。`contract.js` 冒頭が「kind 一覧や cap 集合の写しは持たない」と宣言しており (`contract.js:1-5`)、JS 側の契約知識は各ファイルのリテラルに分散している。

| Rust (`contract.rs`) | JS 側の対応 | 一致の担保 |
|---|---|---|
| `WEB_PROTOCOL_VERSION = 1` (`:178`) | `contract.js:12` | test `rust_and_js_protocol_version_agree` (`lib.rs:1084`) |
| `Endpoint::parse` / `cookie_path` (`:67-134`) | `indexEndpoint` / `sessionEndpoint` (`contract.js:30-44`) は URL API で計算するだけ。正規化ロジックは別実装 | e2e (`web_e2e_api.rs`) が fixture の endpoint で通ること。JS の計算そのものを Rust 側で比べる test は無い |
| `ErrorEnvelope` / `ErrorInfo` | `httpError` / `frameErrorText` (`contract.js:69-92`) が `{code, message}` を読む | なし (golden は Rust 側だけ) |
| `code::*` | `session.js:1046` が `'session-not-found'` をリテラル比較。他の code は表示に流すだけ | なし |
| `ClientFrame` 3 kind | `session.js:1655` / `:1699` / `:1825` のリテラル | Rust golden (`contract.rs:685-736`) のみ |
| `ServerFrame` 6 kind | `session.js:1883-1939` のリテラル分岐 | Rust golden は hello / attach.info / resize.result / leader.result / error の 5 kind。**`auth.extend.result` の golden は無い** (e2e が `expires_at` を見る、`web_e2e_api.rs:909`) |
| `ChallengeRequest` 等 `/auth/*` 型 | `auth.js:98-128` が body を組む。`purpose` は `'assert'` / `'register'` のリテラル | `tests/auth_routes.rs` (Rust 側から JSON を投げる) |
| `SessionResponse` | `auth.js:142-145`, `:220`, `:263` が `access_token` / `expires_at` / `sub` を読む | `tests/auth_routes.rs:473-478` |
| `InputRequest` / `InputResponse` / `ResizeRequest` / `VersionResponse` | `session.js:955` / `:1133` / `:1137`、`index.js:104` | golden (`contract.rs:849-877`) |
| (型なし) session オブジェクト | `index.js:59-170`、`session.js:1056-1061` が field 名をリテラルで読む | `sessions_endpoint_returns_array` (`lib.rs:1320`) は配列であることだけ |

ずれの確認結果 (observed):

- field 名・kind 名のずれは **見つからなかった** (JS の読み書きする名前はすべて Rust 型の serialize 結果と一致、実機 frame でも確認)
- 不一致は DR-0035 の表と実装の間 (`auth_expires_at` vs `expires_at`、§7)
- `fetchScreen` の非 404 失敗は `httpError` を通さず `'HTTP ' + status` だけを出す (`session.js:1107`)。他の呼び出しは `httpError` で `{code,message}` を表示する

### 7. DR との乖離 (B 方向: DR → 実装)

| DR | 主張 | 実装 | 種別 |
|---|---|---|---|
| DR-0035 決定 1 表 | `auth.extend.result` の期限 field は `auth_expires_at` | `expires_at` (`contract.rs:581`)。JS は読まない | DR の誤記 (実装・e2e は `expires_at` で一貫) |
| DR-0035 決定 1 | `lib.rs` は `json!` 手書きリテラルを持たず契約型を serialize | `session_entry_to_json` が `json!` 手書き (`lib.rs:207-245`) | 実装漏れ |
| DR-0035 決定 2 | plain text body の経路を残さない | 未定義 route 404 / 405 は空 body、WS upgrade rejection は `text/plain` (observed(実機)) | 実装漏れ (test `every_api_error_path_uses_the_contract_error_shape` の対象外) |
| DR-0035 決定 1 / §認証無効時 | 認証が無効な間は `auth_expires_at: null`、`auth.extend` は `error` (`code: "unsupported"`)。WS / `/api/*` の 401 は「認証が有効な時だけ」 | DR-0036 決定 9 で認証は常に有効。`code::UNSUPPORTED` は未使用 (`command grep -rn UNSUPPORTED crates/` で定義 1 件のみ)、`auth_expires_at` は常に非 null | DR の記述が古い (dead な語彙が残る) |
| DR-0035 決定 1 表 | routes 表に `/auth/*` が無い | 4 route 実装済。形は DR-0036 の散文と `contract.rs` にだけある | 表の欠落 (DR-0035 が「`/auth/*` の形は DR-0036 が決める」と委ねている) |
| DR-0036 決定 5 | 「切るのは延長を怠った接続だけ」 | WS を access 期限で切る処理が無い。延長しなくても接続は生き続ける (`ws_attach.rs` に期限判定なし) | 実装漏れ |
| DR-0036 決定 5 | Reused 検知で「family ごと失効させ、その sub の WS を切る」 | family を tombstone するだけ。WS は次の `auth.extend` まで残り、同じ sub の他 family には触れない (`routes.rs:676-679`) | 実装漏れ (DR 決定 4 の「最長 4 時間の猶予」と同じ扱いに倒れている) |
| DR-0036 決定 5 | BroadcastChannel 名 `hyoui.auth:<endpoint>:<sub>`、sub 確定後に張り替え | `hyoui.auth:<endpoint>` 固定 (`auth-share.js:62-76`、Design rationale あり)。DR の W2-5 行は変更理由を書いているが決定 5 本文は旧記述のまま | DR 本文の追従漏れ |
| DR-0036 決定 3 | 保守メタ `registered_at,_ip,_user_agent` | `registered_ip` は無い (`record.rs:70-108`)。gateway からは全接続が loopback に見えるので取れない (DR-0036 §Context) | DR の記述過剰 |
| DR-0036 決定 2 / 決定 3 | `sub` の既定: 決定 2 は `<endpoint の host>-<連番>`、決定 3 は `<unit 名>-<連番>` | `next_sub` は host 版 (`record.rs:240-244`) | DR 内の自己矛盾 |
| DR-0027 §3 | WS は「protocol frame (CBOR) を WS binary message に 1:1 転写」 | binary は raw_data の body (PTY bytes) だけで、CBOR frame は転写しない。同節後段 (`:59`) と DR-0035 は PTY bytes と書いており正しい | DR-0027 内の旧記述 |
| DR-0027 §3 第一弾 | `GET /sessions/:id` / `/api/sessions` 等 5 本 | 現行は WS を含めて 16 本。DR-0035 表が正本を引き継いでいる | (正本移動済み、乖離ではない) |

A 方向 (実装 → DR) で DR に無いもの:

- `GET /api/sessions/{id}` は DR-0035 表にあるが、`no-response` (Hung) status と `daemon_pid` field は DR に無い (`lib.rs:235-243`)。`stale` / `error` の field 構成 (`started_unix_ms` を持たない) も DR に無い
- `/auth/*` の rate limit がプロセス全体 1 窓であること (DR-0036 決定 5 は「4 経路で共有する 30 req/s」とだけ書く)
- 204 応答の `content-type: text/plain`、JSON の `content-type` に charset が無いこと
- `hello` 直後に `attach.info` が 2 回届くこと (observed(実機))

09-15 棚卸しからの変化 (仮定として読んだ箇所の裏取り結果):

| 09-15 の記述 | 現行 |
|---|---|
| 認証なし、全 route 無認証 | `/api/*` と WS は passkey 必須 (DR-0036) |
| エラー body は plain text | JSON `{error:{code,message}}` (例外は §7 の 3 経路) |
| 未知 kind は `eprintln!` して黙殺 | `error` frame (`unknown-kind`) を返す |
| WS の `error` は文字列 | `{code, message}` |
| `/healthz` `/version` 未実装 | 実装済、`/version` は `protocol` を含む |
| session.js が `/api/sessions` (一覧) を叩く | 一覧は叩かず `/api/sessions/{id}` を使う (test `session_asset_polls_only_its_own_session`、`lib.rs:1244`) |
| assets は root 直下前提の絶対パス | 相対 + endpoint 基点 (DR-0035 決定 6) |
| `stale` の field に `started_unix_ms` あり | `stale` / `error` は `started_unix_ms` を持たない。`no-response` が増えた |
| cap 確認は `leader-request-v1` だけ | screen / resume / input / leader.request が共通の `require_cap` を通る (resize は確認なし) |

## 強化に向けた論点

採否は書かない。気づいた事実の列挙。

- **WS の寿命と認証期限が切り離されている**。延長を怠った接続も切られず、`auth.extend` は接続元と別 family の token でも ok を返す (§5 / §7)
- **子の終了・停止が WS に乗らない**。exit 時は理由 frame 無しの close 1005 で、browser は再接続 backoff に入り 404 を受け続ける。stopped は 5 秒 polling 頼み。daemon には `session.exit.notify` / `session.child.stopped.notify` が既にあり gateway が捨てている (§4)
- **WS 上りの意味論的失敗 (ro-rejected / lock-not-held) が browser に届かない** (`ws_attach.rs:438-443`、コメントに「将来 WS text message で client に error 表示する余地あり」)
- **close code が常に 1005**。失効・子 exit・daemon 切断・encode 失敗を browser が区別できない
- **エラー形の穴 3 つ** (未定義 route / 405 / WS upgrade rejection) と、それを固定する test の対象外
- **session オブジェクトに契約型と golden が無い**。index / session ページが field 名をリテラルで読み、status 値 (`live` / `stopped` / `stale` / `error` / `no-response`) の語彙も JS 側に散在する
- **`auth.extend.result` の golden test が無く、DR 表とずれている**
- **DR-0035 / DR-0036 に「認証が無効な間」の記述と `code::UNSUPPORTED` が残っている** (決定 9 と矛盾)
- **front 未使用 / 部分使用の API**: `/healthz` (front 不要)、`layer=visible` / `scrollback`、`hyouiAuth` の `hasInvitation` (定義のみ、`auth.js:279`)
- **ブラウザから触れない daemon 機能** (§4 の到達不能リスト)。tail / snapshot / signal / kill / detach / record / set / namespace 指定などを web に出すかは未決
- **命名揺れ**: JSON の field は WS 制御 frame が camelCase (`requestId` / `accessToken`)、HTTP body と hello は snake_case (`build_id` / `auth_expires_at` / `access_token` / `challenge_id` / `sent_bytes`)。期限は `auth_expires_at` (hello) と `expires_at` (extend.result / SessionResponse) の 2 名
- **`/version` 以外で世代を確かめない HTTP 経路**: session ページの fallback (WS 未接続) 中は hello が来ないので、帯が出るのは WS 接続時だけ (DR-0035 決定 3 が受け入れている制約)
- **`/auth/*` の rate limit がプロセス全体 1 窓**。認証済み利用者の refresh も他の要求と同じ 30 req/s を取り合う
- **resize は HTTP / WS とも cap 確認を通らない** (他の操作は `require_cap` を通る。daemon の `resize` message が cap 外であることに由来するかは未確認)
- **endpoint 正規化が Rust (`Endpoint::parse`) と JS (`URL` API) の 2 実装**で、両者が同じ文字列に到達することは e2e の fixture 1 形でしか固定されていない (prefix 付き endpoint は DR-0035 gate 2 の実機確認のみ)
- **assets に cache ヘッダが無い**ことが DR-0035 の前提 (reload で必ず新 assets が取れる) になっている。PWA / offline / 帯域の強化を考える場合はこの前提との関係が論点になる
- **JS は plain script + global 受け渡し**で、契約リテラルを集約する場所が `contract.js` 以外に無い (`contract.js` は意図的に kind 一覧を持たない)
