# hyoui web の現行契約と ccmsg passkey 認証の棚卸し

- Date: 2026-09-15

「webui ↔ gateway の protocol に version を設け、不一致時にリロード誘導を出す」「ccmsg で実証した passkey 認証を hyoui webui に取り入れる」の 2 要望に向けた**事実の棚卸し**。設計判断は本 findings では行わない。推測は「推測」と明記する。

## 判明した事実

- **hyoui web の HTTP/WS 契約に version 識別子は存在しない。** gateway と assets の間に version を名乗る仕組みがなく、assets 配信は `Content-Type` だけで cache ヘッダも ETag も持たない (`crates/hyoui-web/src/lib.rs:712-734`)。露出している version 情報は `/api/sessions` の各 session の `daemon_version` 1 つだけで、これは daemon binary の version であって web 契約の世代ではない (`crates/hyoui-web/src/lib.rs:141-145`, `crates/hyoui-web/assets/index.js:128-129`)。
- **daemon ↔ client 境界 (CBOR) には version 機構がある**が、それは固定 version 番号ではなく **cap flag の intersect 一本**で、「固定 `PROTOCOL_VERSION` は持たない」と明示されている (`crates/hyoui/src/protocol/mod.rs:9`, `crates/hyoui/src/protocol/caps.rs:1-52`)。つまり hyoui には「世代番号で弾いてリロードを促す」前例が無い。
- **ccmsg には両方の前例がある。** wire 世代は `PROTOCOL_VERSION = 4` の単一定数で、全 greeting の必須フィールドとして相手に伝わり、サーバは不一致を `bad_request` で拒否、クライアントは `generationMismatch` → 帯 + 切断画面の 2 箇所にリロード誘導 UI を出す (自動リロードはしない)。互換経路は設計として持たない。
- **hyoui web には認証が一切無い。** router に auth middleware も token 検査も無く (`crates/hyoui-web/src/lib.rs:66-82`)、`HYOUI_LOCK_TOKEN` は daemon の lock token を env から継承するだけで HTTP 認証ではない (`crates/hyoui-web/src/lib.rs:205`, `:380`)。到達制限は bind 先 (既定 `127.0.0.1:43690`) と前段 Caddy + tailnet が担う設計 (`crates/hyoui/src/config/mod.rs:122-127`, DR-0027:15,163)。CORS ヘッダも CSP も付けない。
- **iframe 埋め込みは意図的に許可**されている。`X-Frame-Options` / `CSP frame-ancestors` を付けないことが方針として書かれ、regression test で固定されている (`crates/hyoui-web/src/lib.rs:88-93`, `:1022-1051`)。`?embed=1` は body に class を付けて header / debug panel を隠すだけのクライアント側処理で、サーバは query を一切見ない (`crates/hyoui-web/assets/session.js:18-20`, `crates/hyoui-web/src/lib.rs:694-696`)。
- **ccmsg 側の実装は iframe 内 WebAuthn を能動的に拒否する。** `clientDataJSON.topOrigin` が存在するだけで拒否、`crossOrigin === true` で拒否。`Permissions-Policy: publickey-credentials-get` の記述は現行 3 リポに存在せず、ccmsg-webui が hyoui を貼る iframe の `allow` は `clipboard-read; clipboard-write` のみ。**hyoui を iframe に置いたまま passkey を入れる前例は ccmsg 側に無い。**
- **ccmsg-webui → hyoui の embed は認証非連携。** URL は `<terminal_gateway>/<...>?embed=1&resize=1` で、token も cookie hint も付かず `postMessage` も無い。gateway 自身の認証は ccmsg の関知外 (契約にも daemon にも規定が無い)。
- **ccmsg の passkey 認証は「登録起点を CLI にだけ置く」設計で、ライブラリ非依存の自前実装**。WebAuthn 検証 401 行 + CBOR decoder 150 行を自前で持ち、credential は `<state dir>/auth/records.json` (mode 0600) に永続化する。access token はメモリのみ + WS subprotocol、refresh は `__Secure-` httpOnly cookie で毎回 rotate + 再利用検知。
- **ccmsg の認可粒度は role のみ** (`session` / `user` / `instance`)。`sub` は dispatch に届かず、passkey で入った人は全員同一権限の `user`。人ごとの権限差・read-only 利用者の区別は機構として存在しない。
- **DR-0034 §5 が決めた `/healthz` と `/version` は未実装** (`grep -rn "healthz\|/version" crates/ --include='*.rs'` が 0 件)。設計 → 実装方向の未着手が 1 件ある。

## 実用的な示唆 / ベストプラクティス

- web 契約に version を入れるなら、**hyoui 内に既存の前例は cap flag 方式しか無い**。「世代番号 + 不一致でリロード誘導」は ccmsg-protocol の形を持ち込むことになるので、hyoui の cap 方式 (加算的進化、negotiate で落とす) と併存させるのか置き換えるのかを最初に決める必要がある。両者は互換方針が正反対 (cap = 旧相手でも動く / 世代 = 互換経路を持たない)。
- リロード誘導を出す場所は既にある。session ページは WS text frame で `attach.info` / `resize.result` / `leader.result` を受けており、`kind` を 1 つ足せば画面端バナーを出す経路は増設できる (`crates/hyoui-web/src/ws_attach.rs:47-93`, `crates/hyoui-web/assets/session.js:1806-1820`)。ただし index ページは WS を持たず `/api/sessions` の polling のみなので、そちらは HTTP 応答側に version を載せる別経路が要る (`crates/hyoui-web/assets/index.js:92`)。
- assets に version 識別が無いので、**「gateway を入れ替えたがブラウザが古い assets を掴んでいる」を検出する手段が現状ゼロ**。version 機構の設計では「誰の version を比べるか」(gateway binary / assets の内容 / 契約の世代) を分けて扱う必要がある。cache ヘッダを持たない現状は、逆に言えばリロードで確実に新しい assets が取れる状態でもある (推測: `Cache-Control` 未指定時のブラウザ heuristic 次第なので実機確認が必要)。
- passkey を hyoui に持ち込む際の**最大の構造的衝突は iframe**。ccmsg-webui の Terminal タブの中で hyoui が自前の WebAuthn を走らせる形は、ccmsg 側の検証コードの判断 (topOrigin 拒否) と真逆になる。選択肢は「別タブで認証だけ済ませる」「親から子へ委譲する機構を新設する」「親の `allow="publickey-credentials-get"` と子の topOrigin 許容を両方新規設計する」のいずれかで、どれも ccmsg に前例が無い。
- ccmsg の認可が role だけである事実は、hyoui に「観測のみ許す閲覧者」を作りたい場合に流用できないことを意味する。hyoui は既に daemon 側に `ro` / `rw` / `rw-no-leader` の mode を持つので (`crates/hyoui-web/src/ws_attach.rs:215-222`)、認可の軸は hyoui 側の既存概念に寄せられる可能性がある。
- DR-0034 §5 の `/healthz` / `/version` は未実装のまま。version 機構の設計と同じ面を触るので、**別々に設計せず 1 つの判断にまとめるのが自然** (`/version` が何を答えるかは契約 version の定義そのもの)。

## 検証の詳細

### Part 1-A: HTTP routes

router 定義は `crates/hyoui-web/src/lib.rs:71-82`。**全 route が認証なし**、CORS ヘッダなし、CSP なし。

| method | path | query | request | response | 呼ぶ側 |
|---|---|---|---|---|---|
| GET | `/` | — | — | `index.html` (`text/html`) | ブラウザ |
| GET | `/sessions/{id}` | 表示設定一式 (下表) | — | `session.html` (`text/html`)。**server は `id` も query も見ず、存在確認もしない** (`:694-696`) | ブラウザ |
| GET | `/assets/{*path}` | — | — | 埋め込み or ローカル dir のファイル。`..` / 空 component は 404 (`:702-708`) | ブラウザ |
| GET | `/api/sessions` | — | — | session オブジェクトの JSON 配列 (`:116-156`) | `index.js:92`, `session.js:1026` |
| GET | `/api/sessions/{id}/screen` | `layer=visible\|scrollback\|both` (既定 `visible`) | — | ANSI bytes (`text/plain; charset=utf-8`)。404 = 不在/stale、500 = daemon エラー | `session.js:1079` (常に `layer=both`) |
| POST | `/api/sessions/{id}/input` | — | `{"specs": ["text:...","key:Enter"]}` | `{"sent_bytes":N,"specs":M}`。400 = spec 不正、409 = lock 競合、503 = lock 不能、500 = 送信失敗 | `session.js:1107` |
| POST | `/api/sessions/{id}/resume` | — | 空 | 204。404 / 500 | `session.js:1051` |
| POST | `/api/sessions/{id}/resize` | — | `{"cols":W,"rows":H}` | 204。400 = 0 以下、409 = leader 競合、500 | `session.js:934` (WS 未接続時の fallback) |
| GET | `/api/sessions/{id}/attach` | — | WS upgrade | WS (下表) | `session.js:1770-1773` |

`/api/sessions` の JSON 形 (`:125-156`):

| status | フィールド |
|---|---|
| `live` / `stopped` | `session_id`, `namespace`, `socket_path`, `started_unix_ms`, `status`, `cwd`, `argv`, `clients`, `child_pid`, `child_pgid`, `child_stopped`, `on_child_suspend`, `daemon_version` (空文字なら `null`) |
| `stale` | `session_id`, `namespace`, `socket_path`, `started_unix_ms`, `status`, `reason` |

エラー応答は**すべて plain text の body** (`:754-768`)。JSON エラー形は持たない。

考察: 契約に version を入れる余地としては、`/api/sessions` 応答に gateway 側の version を足す形か、全応答にヘッダを足す形が既存構造に干渉しない。エラー body が plain text なので「version 不一致エラー」を構造化して返したいなら、そこは新形式になる。

### Part 1-B: WS endpoint の契約

実装 `crates/hyoui-web/src/ws_attach.rs`。1 WS 接続 = 1 daemon `ClientConnection` (Rw、auto-lock は取らない、`:30-38`)。

| frame 種別 | 向き | 役割 |
|---|---|---|
| binary | daemon → browser | PTY output bytes を `raw_data` frame body から 1:1 転写 (`:291-295`) |
| binary | browser → daemon | xterm.js のキー入力。wake drain 内で 1 frame に結合して `send_raw_bytes` (`:357-368`) |
| text | 双方向 | 制御 JSON (下表)。`kind` tag で判別 |

browser → gateway の text frame (`:48-63`):

| kind | payload | 応答 |
|---|---|---|
| `resize` | `{"kind":"resize","requestId":N,"cols":W,"rows":H}` | `resize.result` |
| `leader.request` | `{"kind":"leader.request","requestId":N}` | `leader.result` |

未知の `kind` / 不正 JSON は **`eprintln!` して黙って continue** (`:162-166`)。browser には何も返らない。

gateway → browser の text frame:

| kind | payload | 送信契機 |
|---|---|---|
| `attach.info` | `{"kind":"attach.info","mode":"rw"\|"ro"\|"rw-no-leader"\|"unknown","leader":bool}` | WS 確立直後 (`:261`)、`leader.notify` 受信時 (`:298-302`)、`mode.change` 受信時 (`:303-308`) |
| `resize.result` | `{"kind":"resize.result","requestId":N,"ok":bool,"error"?:string}` | `resize` 要求への応答 (`:380-386`) |
| `leader.result` | `{"kind":"leader.result","requestId":N,"ok":bool,"error"?:string}` | `leader.request` への応答 (`:388-397`) |

エラーの形は `ok:false` + `error` の文字列 1 本。error code の語彙は無い (daemon 側 `ErrorCode` の文字列が message に埋め込まれるだけ、`:526-534`)。

考察: **未知 `kind` を黙殺する現在の挙動は、version 不一致の検出に使えない**。ccmsg のクライアントは「自分が知らない ErrorCode」「契約に無い topic frame」を世代不一致の検出経路にしているので、hyoui で同じ形を取るなら黙殺をやめる判断が必要になる。

### Part 1-C: 埋め込み・iframe の前提

| 項目 | 事実 | 出典 |
|---|---|---|
| `X-Frame-Options` | **付けない** (方針として明記 + regression test) | `lib.rs:88-93`, `:1038-1041` |
| `CSP frame-ancestors` | **付けない** (同上) | `lib.rs:1042-1050` |
| `?embed=1` の実装 | クライアント側のみ。`document.body.classList.add('embed')` で header / debug panel を隠す | `session.js:18-20` |
| `?resize=1` | auto-resize (PTY) 強制 ON | `session.js:916` |
| server 側の query 解釈 | **一切しない** (`Path(_id)` で id すら捨てる) | `lib.rs:694-696` |
| `postMessage` | **使っていない** (assets 全体で 0 件) | grep |
| ccmsg 側の iframe 属性 | `sandbox="allow-scripts allow-same-origin allow-forms allow-popups"`, `allow="clipboard-read; clipboard-write"` | ccmsg-webui `src/ui/TerminalPanel.tsx:36-37` |
| ccmsg → hyoui の URL | `<gateway>/<...>?embed=1&resize=1`、認証情報なし | ccmsg-webui `src/terminal-url.ts:26,49-63` |
| ccmsg 契約中の hyoui | `TerminalId` の `hyoui:` スキームだけが `terminal_gateway` の serve 対象 (`HYOUI_TERMINAL_SCHEME = "hyoui"`) | ccmsg-protocol `src/identifiers.ts:84-96` |
| gateway URL の出どころ | ccmsg daemon config `upstream.terminal_gateway` → `hello` 応答の `terminal_gateway` | ccmsg-protocol `src/common/hello.ts:155-166` |
| fixture 値 | `https://mba.example.ts.net/hyoui` (tailnet ドメイン + path prefix) | ccmsg-protocol `src/fixtures/common.ts:96` |

考察: hyoui が **path prefix (`/hyoui`) の下に置かれる**運用が fixture に現れている。認証を入れる場合、cookie の Path 境界や RP ID の決め方はこの prefix 付き構成を前提に考える必要がある (ccmsg 自身も `__Host-` ではなく `__Secure-` + Path を選んだ理由が同型の問題)。

### Part 1-D: bind / 認証 / assets 配信

| 項目 | 事実 | 出典 |
|---|---|---|
| 既定 listen | `127.0.0.1:43690` (= 0xAAAA) | `crates/hyoui/src/config/mod.rs:122,138-140` |
| listen 解決順 | CLI `--listen` > config `[web].listen` > 既定 | `crates/hyoui-cli/src/main.rs:2455` |
| 前段の想定 | Caddy reverse proxy (ACME / WS upgrade 透過)。HTTPS は前段、認証は network が担う | `config/mod.rs:124-125`, DR-0027:14-15,163 |
| HTTP 認証 | **無い** (router に middleware なし) | `lib.rs:66-82` |
| `HYOUI_LOCK_TOKEN` | daemon の lock token を env から継承するだけ。HTTP 認証ではない。screen / input / resume / resize / WS の全経路で `AttachOptions.token` に入る | `lib.rs:205,380,476,546`, `ws_attach.rs:255` |
| `AppState.config` | `#[allow(dead_code)]` で未使用。「将来 `[web]` から rate limit / auth を読む余地」とコメント | `lib.rs:52-54` |
| CORS | ヘッダを付けない | grep (`lib.rs` に CORS 記述なし) |
| assets 配信 | リリースは `include_dir!` で binary に埋め込み、`--web-assets-dir` 指定時はローカル dir を都度読む | `lib.rs:44,712-734` |
| cache ヘッダ | **`Content-Type` 以外を付けない**。`Cache-Control` / `ETag` / `Last-Modified` なし | `lib.rs:717,726-729` |
| assets の version 識別 | **無い**。ファイル名に hash も query も付かない (`/assets/session.js` 等の素のパス) | `session.html` の script 参照 |
| client 側の cache 回避 | API 呼び出しは `{cache: 'no-store'}` を明示 (assets には効かない) | `index.js:92`, `session.js:1026,1079` |
| 露出している version | `/api/sessions` の `daemon_version` のみ (session 一覧の表に表示) | `lib.rs:141-145`, `index.js:128-129` |
| `/healthz` `/version` | **未実装** (DR-0034 §5 で決定済み) | grep 0 件 |

`GET /sessions/:id` の query パラメータ (全てクライアント側で解釈、DR-0027 §5 が正本):

| param | 型 / 範囲 | 既定 |
|---|---|---|
| `embed` | `1` | off |
| `resize` | `1` | off |
| `fontsize` | 整数 6-40 | `13` |
| `lineheight` | 数値 1.0-2.0 | `1.0` |
| `scrollback` | 整数 0-100000 | `2000` |
| `fontfamily` | family 名のカンマ区切り | なし (既定チェーン先頭に挿入) |
| `bg` / `fg` | hex 3/4/6/8 桁、`#` 省略可 | `#111` / `#e0e0e0` |
| `unicode` | `6` \| `11` | `11` |
| `ambw` | `half` \| `full` | `half` |
| `fab` (+ 個別 `left`/`right`/`top`/`bottom`/`size`/`bg`/`fg`) | 位置と装飾 | 右下 16px、size は 32-96 に clamp |

不正値は既定に落として `console.warn` (`session.js:48-50`)。**未知 key は silent skip = 前方互換**と明記 (`session.js:1292`)。

考察: query は前方互換を明示的に選んでいる (未知 key を捨てる) 一方、WS の `kind` は未知を黙殺する。どちらも「新しいクライアント / 新しい gateway」の片方向しか救わない。version 機構を入れるなら、この 2 つの前方互換方針と矛盾しないかを見る必要がある。

### Part 1-E: web が使う daemon message / cap

daemon ↔ client 境界は CBOR + cap flag。**固定 `PROTOCOL_VERSION` を持たない**方針 (`crates/hyoui/src/protocol/mod.rs:9`)。handshake で自分と相手の caps を intersect し、不足 cap での呼び出しには `error` (kind=`unsupported-capability`) を返す (`caps.rs:1-6`)。

web gateway は **`MVP_CAPS` 全部を要求**して connect する (`lib.rs:204,384-387,474,544`, `ws_attach.rs:254`) が、実際に使うのは以下:

| 経路 | mode | 使う message | 必要 cap |
|---|---|---|---|
| `/api/sessions` | (接続しない) | `discovery::list_sessions` のみ | — |
| `GET .../screen` | Ro | `ScreenDumpRequest` / `ScreenDumpResponse` | `screen-dump-v1` |
| `POST .../input` | Rw | `acquire_auto_lock` / `release_auto_lock`, `send_raw_bytes` (raw_ack) | `lock`, `data` |
| `POST .../resume` | Rw | `SessionChildResumeRequest` | `child-state-v1` |
| `POST .../resize` | Rw | `Resize` + `StatusQuery` を FIFO barrier に使う | (Resize は cap 外), `StatusQuery` |
| `WS .../attach` | Rw | raw_data 双方向、`LeaderNotify`, `ModeChange`, `Resize`+`StatusQuery`, `LeaderRequest` | `data`, `lock`, `leader-request-v1` |

`leader-request-v1` だけは **gateway が明示的に negotiate 結果を確認**し、無ければ browser に `ok:false` + 「daemon does not support leader.request」を返す (`ws_attach.rs:422-432`)。これが hyoui 内で唯一の「cap 不足をクライアントに伝える」実例。

`MVP_CAPS` 全 11 個: `data`, `lock`, `tail-v1`, `screen-dump-v1`, `state-snapshot-v1`, `session-exit-v1`, `child-state-v1`, `record-v1`, `set-v1`, `upgrade-v1`, `leader-request-v1` (`caps.rs:38-50`)。

考察: **daemon 境界には negotiate + 明示エラーの機構が既にあり、web 境界には何も無い**、という非対称が現状。`leader-request-v1` の扱い方 (cap を確認してクライアントに理由を返す) は、web 側に version 機構を入れるときの参考になる既存パターン。

### Part 1-F: web 契約に触れる DR

| DR | 節:行 | 内容 |
|---|---|---|
| DR-0027 | `## Context`:11-15 | tailnet 前提、前段 Caddy、**認証は当面なし・token auth は将来 DR** |
| DR-0027 | `### 3. endpoint 構成`:42-96 | route 一覧、WS の binary/text 分担、`resize` / `attach.info` / `leader.request` の JSON 形、`POST /resize` を fallback に残す判断 |
| DR-0027 | `### 4. 静的アセット`:97-102 | bundler なし vendored、埋め込み + `--web-assets-dir` の二段 |
| DR-0027 | `### 5. セッションページの URL query パラメータ`:103-126 | 表示設定は全て query。**config と合成しない** (閲覧者ごとに違う値が要るため) |
| DR-0027 | `#### 文字幅パラメータ`:127-152 | `unicode` / `ambw` の実測根拠 |
| DR-0027 | `## Consequences`:163 | 認証 / HTTPS は scope 外 |
| DR-0027 | `### 2.`:30 | security audit boundary 分離は「認証なし・tailnet 前提」の現運用では過剰、network 露出を正式サポートする段階で再評価 |
| DR-0031 | `### 1. CLI`:27, `### 2. OS 対応`:44 | `hyoui web service register [--listen]`、launchd / systemd の argv |
| DR-0033 | `### 6. web gateway への配線`:117-139 | WS 制御チャネルへの `leader.request` / `leader.result` 追加 |
| DR-0033 | `### 4. cap flag`:93-103 | `leader-request-v1` |
| DR-0034 | `### 5. gateway に /healthz と /version を足す`:193-213 | **未実装**。`/api/` の下に置かない理由 = `/api/*` の仕様変更 (認証追加・スキーマ変更) が可用性監視を壊すため |
| DR-0034 | :200, :337 | `/healthz` `/version` は既存 `/` `/api/*` と同じく認証を持たず、**認証境界を変えない**と明記 |
| DR-0034 | `### 7. stable / unstable の HA`:222-273 | 前段 proxy が 43691 → 43690 の順で upstream を持つ |
| DR-0034 | `### cap 差は gateway 側で吸収する`:258-273 | 前段は cap 差に関与しない |
| DR-0034 | :12, :141, :159 | 実機 plist は `--listen` が焼かれておらず config 依存、`stable`=43690 / `unstable`=43691 |
| DR-0013 | `### 4. attach 復元 protocol`:102-204 | screen state 正本化。web の `layer=both` 復元が依存する |
| DR-0022 | `### 1.`:41-50, `### 3. 外側 token 継承時は auto-acquire skip`:58-72 | `POST /input` の auto-lock 5s と `HYOUI_LOCK_TOKEN` 継承時 skip の根拠 |
| DR-0008 | `### 3 schema evolution`, `### 3.4 cap 命名規約` | cap flag 一本、固定 version を持たない方針の正本 |

## ccmsg の passkey 認証と protocol version (実装事実)

以下は subagent による読み取り調査の結果。出典は略号 + リポ内相対パス。

| 略号 | 絶対パス |
|---|---|
| `[D]` | `/Users/kawaz/.local/share/repos/github.com/kawaz/ccmsg/main` (daemon + CLI、認証の実装本体) |
| `[P]` | `/Users/kawaz/.local/share/repos/github.com/kawaz/ccmsg-protocol/main` (契約) |
| `[W]` | `/Users/kawaz/.local/share/repos/github.com/kawaz/ccmsg-webui/main` (クライアント) |
| `[C]` | `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-ccmsg/main` (旧世代。現行認証の実装は無い) |
| `[R]` | `/Users/kawaz/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/auth-patterns` |

全体像は 3 行:

- 人の認証は daemon 自身が passkey で判定する。前段 proxy / 外部 IdP に寄せない (`[D]/docs/DESIGN-ja.md:735`)。
- WebAuthn 検証は**ライブラリ不使用の自前実装** (`[D]/src/auth/webauthn.ts` 401 行 + `cbor.ts` 150 行、判断は `[D]/docs/decisions/DR-0001-passkey-auth-for-people.md:104-112`)。
- 判断の正本は 3 箇所に分かれる: 手順 = daemon の DR-0001、線上の形 = protocol の DR-0020 / DR-0021 / DR-0022、クライアント作法 = `[W]/docs/DESIGN.md:597-651`。

### 登録フロー

| 項目 | 実装事実 | 出典 |
|---|---|---|
| bootstrap 方法 | **CLI 発行のみ**。`ccmsg daemon passkey add <unit> [endpoint] [--name <ラベル>]` で登録 URL を 1 本発行 | `[D]/src/cli.ts:248-273`, `:1059-1074` |
| 無認証初回登録 | **無い**。localhost 限定でもない。CLI 実行が唯一の起点で、リモート登録経路も復旧経路も持たない | `[D]/docs/decisions/DR-0001-passkey-auth-for-people.md:32` |
| CLI → daemon の経路 | 契約の op ではなく **UDS 専用の管理フレーム** (`admin: "passkey_add" \| "passkey_list" \| "passkey_remove"`)。到達 = 権限。ネットワークからは届かない | `[D]/src/auth/admin.ts:16-44,71-85` |
| 発行される URL | `<endpoint>#register=<jwt>` (fragment。server / proxy log / Referer に乗らない) | `[D]/src/auth/auth.ts:254` |
| jwt の署名 | **登録 1 本ごとの乱数 32 byte secret による HMAC (HS256)**。永続鍵なし、プロセスメモリのみ、再起動で消える | `[D]/src/auth/auth.ts:249,1087-1092` |
| claims | `{ iss, sub, unit, endpoint, rp_id, expires_at, jti, user_id, issued_label? }` | `[P]/src/common/auth.ts:80-111`, `[D]/src/auth/auth.ts:238-248` |
| URL の寿命 | 10 分 (`REGISTER_TTL_MS`) | `[P]/src/common/auth.ts:23` |
| 第 2 経路 (6 桁コード) | `randomInt(0,1e6)` を CLI にだけ表示。URL に含めない。`auth.register` の**必須引数** | `[D]/src/auth/auth.ts:250`, `[P]/src/common/auth.ts:139` |
| コード誤入力上限 | 5 回で URL 自体が焼ける (`CODE_ATTEMPTS`)。判定は**発行者 instance だけ**が行い、受け手は無判定で転送 | `[D]/src/auth/auth.ts:84,566-578,991-994` |
| ブラウザ側 `create()` | `residentKey:"preferred"`, `userVerification:"required"`, `attestation:"none"`, `pubKeyCredParams: [-7,-8,-257]`, `user.id` = claims の `user_id`, `rp.id` = claims の `rp_id` | `[W]/src/auth/client.ts:101-119` |
| endpoint | `POST <endpoint>auth/register` (JSON, `credentials:"include"`) | `[W]/src/auth/client.ts:40-47`, `[W]/src/auth/endpoint.ts:17-19` |
| 検証順序 | client_data → WebAuthn 検証 → **公開鍵 import 可否** → その後で初めて jwt / コード / challenge を消費 (一時失敗で URL が焼けないため) | `[D]/src/auth/auth.ts:452-483` |
| 登録検証の中身 | `type=webauthn.create` / challenge 一致 / origin 完全一致 / `crossOrigin!==true` かつ `topOrigin` 不在 / `rpIdHash==sha256(rp_id)` / UP+UV / `fmt=="none"` かつ `attStmt` 空 / rawId と authData の credentialId 一致 / credential id 重複拒否 | `[D]/src/auth/webauthn.ts:90-118,152-196`, `[D]/src/auth/auth.ts:484-486` |
| 永続化先 | **`<state dir>/auth/records.json`** (JSON 配列、mode 0600、tmp + rename、書き込みは直列化)。DB ではない | `[D]/src/auth/records.ts:17-18,379-401`, `[D]/src/instance/instance.ts:698` |
| state dir | `$CCMSG_STATE_DIR` / `$XDG_STATE_HOME/<key>` (既定 `~/.local/state/…/<instance key>`) | `[D]/src/instance/paths.ts:142-164` |
| record の複製 | 専用 topic `auth.records` (`roles: ["instance"]`、element 粒度、LWW + tombstone)。共有 kv には**載せない** | `[P]/src/common/auth.ts:516-527`, `[P]/docs/decisions/DR-0020-auth-shape-on-the-wire.md` |
| 登録完了時 | そのまま session を mint して返す (登録が即サインイン) | `[D]/src/auth/auth.ts:518` |

### 認証フロー

| # | 役割 | method/path | request | response |
|---|---|---|---|---|
| 1 | challenge 発行 | `POST <endpoint>auth/challenge` | `{}` | `{ challenge (32 byte 乱数 base64url), issuer (instance id), expires_at }` |
| 2 | 登録 | `POST <endpoint>auth/register` | `{ token, code, device_label?, challenge?, credential{id,raw_id,client_data_json,attestation_object} }` | `AuthSession = { sub, access:{value,expires_at} }` + `Set-Cookie` |
| 3 | 認証 | `POST <endpoint>auth/assert` | `{ credential{raw_id,client_data_json,authenticator_data,signature,user_handle?}, challenge }` | 同上 |
| 4 | refresh | `POST <endpoint>auth/refresh` | `{ reason?: "reload"\|"expiring"\|"reconnect" }` (**token は body に無い。cookie のみ**) | 同上 |
| 5 | 接続延命 | WS op `auth.extend` | `{ access_token }` | `{ auth_expires_at }` |

出典: `[D]/src/auth/http.ts:21-31,185-207`, `[P]/src/common/auth.ts:131-261`, `[D]/src/auth/auth.ts:352-358,926-941`。

cookie と token は役割分離:

| 値 | 置き場所 | 属性 / 寿命 | 出典 |
|---|---|---|---|
| access token | **ブラウザのメモリのみ** (signal)。localStorage 等に一切書かない | 4 時間 (`ACCESS_TTL_MS`) | `[W]/src/auth/session.ts:5-13`, `[D]/src/auth/auth.ts:54` |
| access の提示方法 | WS handshake の **subprotocol** `ccmsg.token.<値>`。サーバは選んだ subprotocol を echo | — | `[W]/src/connection.ts:19,193`, `[D]/src/instance/instance.ts:1238-1259` |
| refresh token | **httpOnly cookie**。名前は `__Secure-ccmsg-<sha256(instance id + "\n" + sub) の先頭 16 hex>` | `HttpOnly; Secure; SameSite=Strict; Path=<request の /auth/ までの prefix>; Max-Age=<残り秒>`、7 日 | `[D]/src/auth/http.ts:77-80,249-266`, `[D]/src/auth/auth.ts:55` |
| `__Host-` を選ばない理由 | 同一 host の `/` と `/personal/` を別 endpoint (別登録) として扱うため。**Path は認可境界ではない**と明記 | — | `[D]/src/auth/http.ts:60-69` |
| token の性質 | **署名しない opaque 乱数 32 byte**。検証は record の lookup | — | `[D]/src/auth/auth.ts:1076-1079`, `[D]/src/auth/records.ts:247-273` |

refresh / rotate の規則:

| 規則 | 実装 | 出典 |
|---|---|---|
| refresh は使うたび rotate | `rotate()` が新 refresh を mint | `[D]/src/auth/auth.ts:794-843` |
| access は据え置き | 残り寿命が TTL の半分 (2h) を切るまで同じ値を返す (タブ間で 1 本を共有するため) | `[D]/src/auth/auth.ts:76,813-816` |
| 直前 1 世代の猶予 | 60 秒 (`PREVIOUS_GRACE_MS`) は前回の答えを返す (再送救済、rotate しない) | `[D]/src/auth/auth.ts:63,806-808` |
| 再利用検知 | 退役値の sha256 を本来の exp まで保持。どの世代でも再提示を見たら **family ごと失効 + その sub の WS を切断** | `[D]/src/auth/auth.ts:853-875`, `[P]/src/common/auth.ts:468-478` |
| 失効の表現 | record 削除ではなく **tombstone** (family 7 日、credential 無期限)。分断復帰した peer が生きた写しを戻せない | `[D]/src/auth/records.ts:284-294`, `[P]/src/common/auth.ts:31` |
| 単一 writer | family を書けるのは `iss` だけ。別 instance に来た rotate は `auth.rotate` で `iss` へ転送 | `[D]/src/auth/auth.ts:733-764` |
| 接続の期限 | `hello` の `auth_expires_at`。クライアントは残り寿命の 90% 時点で `/auth/refresh` → `auth.extend`。切断しない | `[W]/src/state.ts:941,952-978`, `[P]/src/common/hello.ts:169-173` |
| rate limit | `/auth/*` 4 経路で共有、30 req/s。超過は 429 | `[D]/src/auth/auth.ts:92-93`, `[D]/src/auth/http.ts:152-154` |
| CORS / Origin | `Origin` **必須** (無い POST は 403)。許可集合は「自分の endpoint origin + credential record の endpoint origin + 未使用登録 URL の endpoint origin」の**完全一致のみ**。rp_id の suffix では許可しない | `[D]/src/auth/http.ts:114-137`, `[D]/src/auth/auth.ts:695-704` |
| 認証検証の中身 | `type=webauthn.get` / challenge / origin 完全一致 / request path == endpoint の path / `crossOrigin!==true` / `topOrigin` 不在 / `rpIdHash` は **record の `rp_id`** と照合 / UP+UV / `userHandle` は提示された場合のみ照合 / 署名 `authData \|\| sha256(clientDataJSON)` | `[D]/src/auth/auth.ts:585-656`, `[D]/src/auth/webauthn.ts:199-232` |
| signCount | record が非 0 なら「提示値 > record」を要求 (提示 0 も退行として拒否)。record が 0 なら提示値を保存 (同期 passkey は常に 0)。await 後に standing record で**再チェック** | `[D]/src/auth/webauthn.ts:214-221`, `[D]/src/auth/auth.ts:639-641,1066-1069` |
| 失敗応答 | `{ok:false,error:{code,msg}}`。400 = 形の誤り、401 = 認証拒否、500 = 内部。**どちらの半分 (URL / コード) が失敗したかは言わない** | `[D]/src/auth/http.ts:278-285`, `[P]/docs/decisions/DR-0021-registration-in-two-halves.md` |

複数タブの調停 (`[W]/src/auth/tab-share.ts`):

| 仕組み | 実装 |
|---|---|
| 排他 | `navigator.locks.request("ccmsg.auth.refresh:<endpoint>[:<sub>]")` (`:112-120,168-171`) |
| 配布 | `BroadcastChannel("ccmsg.auth:<endpoint>[:<sub>]")`、**メモリ間のみ** (`:173-198`) |
| ロック取得後に「持ってる?」と問う | `{kind:"ask"}` を投げ、50 ms (`ASK_MS`) で打ち切り (`:43,125-137`) |
| sub 未確定時 | endpoint だけの scope で待ち、確定後に張り替え (`:17-24,165-171`) |
| Web Locks 無し | 各タブが自前 refresh (サーバ側の据え置きが収束を担う) (`:109-111`) |

### 認可

| 問い | 実装事実 | 出典 |
|---|---|---|
| 認可の単位 | **role のみ** (`session` / `user` / `instance`)。op attribute table の `roles` で判定 | `[P]/src/attributes.ts:17-49`, `[P]/src/identifiers.ts:99-112` |
| `sub` は認可に効くか | **効かない**。dispatch に届く identity は `{state, role, sid?}` だけで `sub` フィールドが無い。`sub` の使途は「期限切れ / 失効時に誰の接続を切るか」だけ | `[D]/src/dispatch/identity.ts:9-22`, `[D]/src/instance/instance.ts:893`, `[D]/src/auth/auth.ts:332-342` |
| 帰結 | **passkey で入った人は全員同一権限の `user`**。人ごとの権限差・read-only 利用者・管理者/一般の区別は無い | `[D]/docs/DESIGN-ja.md:735` |
| 複数 passkey / 複数デバイス | **対応済み**。record key は `credential/<sub>/<credential_id>`。同じ sub への追加登録は既存 `user_handle` を再利用 | `[D]/src/auth/records.ts:38-52`, `[D]/src/auth/auth.ts:275-283` |
| 複数利用者 | `sub` は既定 `<unit>-<連番>`、record から採番 (tombstone 済みの名前はスキップ) | `[D]/src/auth/auth.ts:296-311` |
| 保守用メタ情報 | `issued_label` / `device_label` / `registered_at,_ip,_user_agent` / `last_used_at,_ip,_user_agent` / `backup_eligible` / `backup_state`。**どれも認可判定に使わない** | `[P]/src/common/auth.ts:391-417`, `[P]/docs/decisions/DR-0022-credential-bound-to-an-endpoint.md` |
| 一覧を読む経路 | **CLI のみ** (`passkey list` / `remove <sub>`)。webui から自分の passkey 一覧を見る op は契約に無い (issue 化済み) | `[D]/src/auth/admin.ts:80-85`, `[P]/docs/issue/2026-09-09-passkey-list-for-people.md` |
| credential 単位の削除 | **無い**。削除は `sub` 単位 tombstone で、その sub の credential 全部 + 全 family + 認証済み WS を落とす | `[D]/src/auth/records.ts:187-204`, `[D]/src/auth/auth.ts:321-324` |
| WS 入口の allowlist | `source_ips` のみ。`Origin` は見ない (token が既に答えているため)。token 無しの handshake は匿名で通さず拒否 | `[D]/src/instance/instance.ts:1204-1253` |

### ライブラリ

| 面 | 実装 | version |
|---|---|---|
| サーバ (WebAuthn 検証) | **自前**。`@simplewebauthn/*` 等への依存なし (`[D]/package.json` の dependencies は `@ccmsg/protocol` 1 本のみ) | — |
| CBOR decoder | 自前 150 行 (`[D]/src/auth/cbor.ts`) | — |
| 暗号 | Node 標準 `node:crypto` + WebCrypto `crypto.subtle` (`[D]/src/auth/auth.ts:1`, `[D]/src/auth/webauthn.ts:1,280-333`) | — |
| 対応アルゴリズム | ES256 (-7、DER→raw 変換を自前)、EdDSA/Ed25519 (-8)、RS256 (-257) (`[D]/src/auth/webauthn.ts:236-240,363-385`) | — |
| クライアント | **素の `navigator.credentials`**。ラッパなし (`[W]/src/auth/client.ts:101,146`) | — |
| 契約スキーマ | `@sinclair/typebox` (`[P]/package.json`) | ^0.34.33 |
| 自前判断の条件 | 「テストを既存ライブラリに劣らない水準まで徹底する」「やり切った時点で `@simplewebauthn/server` 等と改めて比較する」が DR の条件。**現状は未達** (受け入れ条件は全項目未チェック) | `[D]/docs/decisions/DR-0001-passkey-auth-for-people.md:104-112`, `[D]/docs/issue/2026-09-12-webauthn-tests-library-grade.md` |
| 現状のテスト | daemon 側 `[D]/test/auth.test.ts` 1039 行 + `[D]/test/authenticator.ts` 229 行 (自前仮想 authenticator)。webui 側 `[W]/test/auth.test.ts` 259 行 + e2e は CDP の `WebAuthn.addVirtualAuthenticator` (`protocol:"ctap2"`, `transport:"internal"`, `hasResidentKey:true`, `hasUserVerification:true`) で実 daemon に本物の署名を通す | `[W]/test/visual/harness.ts:298-312` |

### RP ID / origin

| 項目 | 実装事実 | 出典 |
|---|---|---|
| RP ID の決め方 | **endpoint の hostname に固定** (`new URL(endpoint).hostname`)。CLI にも config にも env にも rp_id の指定口が無い | `[D]/src/auth/auth.ts:232,1136-1138` |
| Host ヘッダから決めるか | **決めない**。決めるのは endpoint (CLI 引数か、起動時 probe で確定した自分の endpoint) | `[D]/src/auth/auth.ts:211-232` |
| registrable suffix | **許さない**。「suffix を名乗れると配下の全ホストでその credential が使える」ため。**契約のコメントだけは suffix の余地を残しており、契約と daemon 実装で記述が食い違っている** | `[D]/docs/decisions/DR-0001-passkey-auth-for-people.md:42` vs `[P]/src/common/auth.ts:91-93` |
| assertion 時の RP ID | 到達した endpoint の host ではなく **record に保存した `rp_id`** と照合。未設定の旧 record は endpoint の host にフォールバック | `[D]/src/auth/auth.ts:678-680`, `[P]/docs/decisions/DR-0022-credential-bound-to-an-endpoint.md` |
| origin の照合 | `clientDataJSON.origin` == record/claims の endpoint の **origin 完全一致** + request 到達 path == endpoint の path (完全一致) | `[D]/src/auth/auth.ts:665-669,1150-1158` |
| 複数 origin 対応 | **config に origin 一覧を持たない**。CORS 許可集合は record / pending / 自 endpoint から**動的に導出**。別名ホストから入るには `passkey add <unit> <その endpoint>` で**登録し直す** | `[D]/src/auth/auth.ts:695-704`, DR-0001:25 |
| tailnet / 127.0.0.1 / localhost | **特別扱いは一切無い**。`Endpoint` は `^https?://[^/?#\s]+(/[^?#\s]*)?/$` を満たす任意の base URL。ただし cookie に `Secure` が常に付くので、**平文 http の endpoint では refresh cookie が保存されない** (`http://localhost` は secure context 扱いで動く。`http://<LAN IP>` は動かない — 推測: コード上の帰結として指摘、実機のブラウザ挙動は未検証) | `[P]/src/identifiers.ts:68-72`, `[D]/src/auth/http.ts:257-264` |
| dev の実運用形 | vite dev server が `/ws`, `/auth`, `/mesh`, `/webhook` を daemon (`CCMSG_DEV_DAEMON`、既定 `http://127.0.0.1:39847`) へ proxy し、reverse proxy の位置に立つ | `[W]/vite.config.ts:8-33`, `[W]/README-ja.md:22-30` |
| webui を別サブドメインに置く構成 | **非対応と明記** (兄弟サブドメインが cookie 付きで `/auth/refresh` を叩いて access token を読む穴を開けないため)。`endpoint` は `location.origin + build の base` から導出 (設定項目ではない) | `[D]/docs/DESIGN-ja.md:166`, `[W]/src/auth/endpoint.ts:36-43`, `[W]/src/state.ts:170` |
| TLS 終端 | proxy 側。daemon の listener は plain のまま | DR-0001:135 |

### iframe 内 WebAuthn (Permissions-Policy)

grep 結果 (`--include='*.ts' --include='*.tsx' --include='*.html' --include='*.md' --include='*.json'`、`node_modules`/`dist`/`test-results` 除外):

| 探した文字列 | 現行 3 リポ (`[D]` `[P]` `[W]`) |
|---|---|
| `Permissions-Policy` | **記述なし** |
| `publickey-credentials-get` / `publickey-credentials-create` | **記述なし** |
| `featurePolicy` | **記述なし** |
| `allow=` 属性 | 1 箇所のみ: `allow="clipboard-read; clipboard-write"` (端末 iframe、`[W]/src/ui/TerminalPanel.tsx:37`)。WebAuthn 系の許可は付いていない |

唯一の言及は旧リポの DR で、しかも**逆方向 (iframe / 別 origin に WebAuthn を届かせない多層防御)** の文脈: 「別 eTLD+1 なので RP ID が届かない。canddy が `Permissions-Policy: publickey-credentials-get=(), publickey-credentials-create=()` で多層防御済み」(`[C]/docs/decisions/DR-0030-sandbox-origin-serving.md:257`)。

さらに実装側は **iframe 内 WebAuthn を能動的に拒否する**側に立っている: `topOrigin` が存在するだけで拒否、`crossOrigin === true` で拒否 (`[D]/src/auth/webauthn.ts:115-117`)。コメントも "this exchange must not be run from an embedded page"。

考察: hyoui が ccmsg-webui の iframe 内で自前の WebAuthn を走らせたいなら、ccmsg 側にモデルは存在しない。親の `allow="publickey-credentials-get *"` 付与と、hyoui 側 RP の `topOrigin` / `crossOrigin` 許容 (= ccmsg とは逆の判断) の両方を新規に設計する必要がある。**ccmsg の検証コードをそのまま流用すると iframe 内認証は必ず落ちる。**

### protocol / API version

| 項目 | 実装事実 | 出典 |
|---|---|---|
| version 値の定義箇所 | `export const PROTOCOL_VERSION = 4` (契約リポの `envelope.ts` に 1 箇所) | `[P]/src/envelope.ts:9` |
| 意味 | 「世代」。世代内では optional フィールドと新 op の追加のみ可。削除・意味変更で世代が上がる。**互換経路を持たない** | `[P]/src/envelope.ts:5-9`, `[P]/docs/decisions/DR-0017-one-generation-no-compatibility-path.md` |
| npm package version との関係 | 別物。`@ccmsg/protocol` は semver `2.1.1`、その中の wire 世代が 4 | `[P]/package.json` |
| 相手への伝え方 | 全 greeting (`hello.user` / `hello.session` / `hello.instance`) の必須フィールド `protocol_version`。応答にも載る | `[P]/src/common/hello.ts:40-46,134` |
| サーバ側の不一致時 | `bad_request` で拒否 (`this instance speaks protocol <N>`)。client 接続も mesh link も同じ扱い | `[D]/src/sessions/registry.ts:407-408` |
| クライアント側の検出 | 3 経路: (a) `hello` 応答の `protocol_version` 不一致、(b) 自分が知らない `ErrorCode` が返った、(c) 契約に無い topic frame が届いた | `[W]/src/connection.ts:250-259,310,324` |
| クライアントの挙動 | `generationMismatch(reason)` → `generationWarning` signal → **リロード誘導 UI あり**。文言「`instance は契約世代 4、この画面は 3 です` — この画面を再読み込みしてください (互換経路はありません)。」を 2 箇所 (Shell の帯 / 切断画面) に出す。degrade はしない | `[W]/src/state.ts:194,589`, `[W]/src/ui/Shell.tsx:30-32`, `[W]/src/ui/Disconnected.tsx:14-17` |
| 自動リロード | **しない**。人に再読み込みを促すだけ | 同上 |
| 副次的な version | `MeshHello.ver` (mesh handshake 形式の世代、wire と独立)、`client_version` (表示専用、何もゲートしない) | `[P]/src/common/hello.ts:12-17,46` |

### ccmsg-webui → hyoui embed の関係

| 項目 | 実装事実 | 出典 |
|---|---|---|
| hyoui は契約に名前で入っている | `TerminalId` は `<scheme>:<id>` 形式で、**`hyoui:` スキームだけが `terminal_gateway` が serve するもの**と契約が定義 | `[P]/src/identifiers.ts:84-96`, `[P]/src/common/hello.ts:177-178` |
| gateway URL の出どころ | `hello` 応答の `terminal_gateway` (optional)。daemon config `upstream.terminal_gateway` | `[P]/src/common/hello.ts:155-166`, `[D]/src/instance/config.ts:845-854` |
| URL の合成 | 契約の `terminalUrl(gateway, terminalId)` が唯一の合成場所。`hyoui:` 以外は `undefined` を返し、クライアントは何も描かない | `[P]/src/common/hello.ts:190-`, `[W]/src/terminal-url.ts:1,44` |
| webui が開く形 | (a) 一覧のリンク (別タブ)、(b) セッションの端末タブの **iframe**。埋め込み URL は `<gateway>/<...>?embed=1&resize=1` | `[W]/src/terminal-url.ts:26,49-63` |
| **認証情報を渡しているか** | **渡していない**。URL に token も cookie hint も付かず、`postMessage` も無い (`src` + sandbox + allow だけ)。gateway が別 origin なら ccmsg の cookie も access token も届かない | `[W]/src/terminal-url.ts:28-47`, `[W]/src/ui/TerminalPanel.tsx:27-38` |
| hyoui 側コードへの参照 | 実装参照は無い。`[D]/test/` に `hyoui` バイナリを PATH に置くテストと `HYOUI_SESSION_ID` / `HYOUI_NAMESPACE` env の読み取りがある | `[D]/test/terminals.test.ts:62-63,119-`, `[D]/test/sessions.test.ts:124-125,1817-1863` |

### reference (`auth-patterns`) ↔ ccmsg 実装の対応

`[R]/_index.md` から辿れる 4 ファイルと実装を突き合わせた。**一致が大多数**なので、以下は相違・未実装・reference に無い実装判断だけを挙げる (一致した 40 項目超は省略)。

`passkey-registration-local-first.md` との相違:

| # | reference の記述 (行) | ccmsg 実装 | 判定 |
|---|---|---|---|
| 1 | webui を別サブドメインに置くなら共通 registrable domain を rp_id に渡す。server は「rp_id が入口 host か registrable suffix」を検証 (:24) | **不採用**。rp_id は endpoint host 固定、suffix 判定なし、別サブドメイン構成は非対応 | **相違 (実装が意図的に狭い)**。理由は DR-0001 §2.3 / DESIGN §3.3 に明記 (兄弟サブドメインが cookie 付きで refresh を叩く穴) |
| 2 | `clientDataJSON.origin` の期待値は webui の origin 集合 (:25) | **単一 endpoint origin の完全一致**。集合は CORS 判定にのみ使う | **相違 (同上の帰結)** |
| 3 | 同じ RP ID の入口が複数あっても credential は 1 つで足りる (:28) | **endpoint (base URL) 単位に束縛**。`https://h/` と `https://h/personal/` は 2 登録が必要 | **相違**。DR-0022 が正面から不採用にしている (「隣の instance への入口になる」) |
| 4 | `userHandle` が record の `user_id` と一致する (:47) | handle が**提示された時だけ**照合 (`residentKey:"preferred"` なので不在もありうる) | **reference に無い実装判断** |
| 5 | BE / BS を登録時・認証時ともに record へ記録 (:50) | **登録時のみ**。`verifyAssertion` は backup flag を返さず、assert 時の更新は `sign_count` / `last_used_*` だけ | **相違 (実装が片側のみ)** |
| 6 | 任意ゲート (b) ホスト PC の生体認証承認 (:111-115) | **未実装**。DR-0001 §2.2 に「ccmsg では後続」と明記、grep でも実装ゼロ | **未実装 (reference が先行)** |
| 7 | challenge に発行者 id を埋める (:88) | `AuthChallenge.issuer` (値の内側ではなく**横に**並べる形) | **一致 (表現が違う)**。契約が理由を明記 (`[P]/src/common/auth.ts:43-50`) |

`multi-tab-token-refresh.md`: クライアント / サーバ両側の 8 項目すべて一致。ただし固定すべき性質 7 項目 (:60-68) のうち **クライアント側 (tab-share) の直接テストは webui に存在しない** (`[W]/test/auth.test.ts` は base64url / endpoint / register-link / refresh reason / device-label / 分岐判定が対象)。「据え置き」は daemon 側 `[D]/test/auth.test.ts:451-484` がカバー。

`peer-auth-url-identity.md` / `self-endpoint-identification.md`: 4 項目一致。1 項目で **reference が旧判断を載せている** — `self` の確定方法は reference の方式 a (probe) だが、ccmsg は DR-0004 §2.4 (設定の endpoint 一覧のうち自分の id を持つ行) に置き換わっており、DR-0001 冒頭に注記がある (`[D]/docs/decisions/DR-0001-passkey-auth-for-people.md:8`)。

reference に無い実装判断 (= 実装が先行):

| 判断 | 出典 |
|---|---|
| 公開鍵を**登録時に import して検証**する (使えない鍵の record を残さない) | `[D]/src/auth/webauthn.ts:249-256`, `[D]/src/auth/auth.ts:465` |
| `/auth/*` で **`Origin` ヘッダ必須** (無い POST は 403)。CORS 許可集合を record から動的に導出 | `[D]/src/auth/http.ts:120-129`, `[D]/src/auth/auth.ts:695-704` |
| body サイズ上限 64 KiB、rate limit 30 req/s を 4 経路で共有 | `[D]/src/auth/http.ts:35`, `[D]/src/auth/auth.ts:92-93` |
| **タイミング安全比較を challenge / token / 6 桁コードすべてに適用** | `[D]/src/auth/webauthn.ts:387-401` |
| credential id を**バイト比較**で引く (base64url が正規形でないため) | `[D]/src/auth/records.ts:230-235` |
| `sub` をパスセグメントとして `encodeURIComponent` でエスケープ (tombstone の prefix 汚染防止) | `[D]/src/auth/records.ts:34-36` |
| 攻撃者入力由来の例外を一律 `auth_invalid` に翻訳 (500 にしない) | `[D]/src/auth/auth.ts:1019-1046` |
| refresh cookie は `__Secure-ccmsg-` prefix の**全 cookie を試す** (認証前は誰の cookie か分からない) | `[D]/src/auth/http.ts:221-242` |
| 期限切れの掃除は timer でなく**読み取り時に行う** | `[D]/src/auth/auth.ts:957-965`, `[D]/src/auth/records.ts:324-335` |
| local write は `updated_at = max(now, held+1)` で必ず前進 (同一 ms の mint→rotate が LWW で消えない) | `[D]/src/auth/records.ts:142-156` |
| peer から届いた「自分が mint した family」の写しを**受理しない** | `[D]/src/auth/records.ts:172` |
| protocol 世代不一致を**クライアント側でもリロード誘導 UI として可視化** | `[W]/src/ui/Shell.tsx:30-32` |

### ccmsg 流用時に引っかかる点 (実装事実として確認できた限界・不整合)

1. **iframe 内 WebAuthn は構造的に不可**。`topOrigin` 存在で拒否、`Permissions-Policy` の付与も無し。hyoui を ccmsg-webui の iframe に置いたまま passkey を入れるのは、ccmsg 側の判断と真逆の設計になる。
2. **認可の粒度が role だけ**。`sub` は dispatch に届かない。「この人は read-only」を hyoui でやるなら、ccmsg には流用できる機構が無い。
3. **credential 単位の削除と webui からの一覧が無い** (`[P]/docs/issue/2026-09-09-passkey-list-for-people.md`)。複数デバイス登録はできるが、本人が UI から保守できない。
4. **契約と daemon で rp_id の規定が食い違う**: 契約コメントは registrable suffix を許す (`[P]/src/common/auth.ts:91-93`)、daemon は endpoint host 固定 (`[D]/src/auth/auth.ts:232`)。契約側の記述を読んで設計すると狭さに気付けない。
5. **`[W]/src/auth/register-link.ts:8` のコメントが現行設計と矛盾**: 「fragment を使う理由は web UI の origin が instance の origin ではないから」と書いてあるが、現行設計は同一 origin 前提 (`[W]/src/auth/endpoint.ts:8`, DR-0001 §2.2)。fragment の正しい理由は reference が書く「server / proxy log / Referer に乗らない」(`[R]/passkey-registration-local-first.md:9`)。
6. **BE / BS が assert 時に更新されない**。保守 UI を作るなら「登録時の値のまま古くなる」前提で読む必要がある。
7. **自前 WebAuthn の品質条件が未達** (`[D]/docs/issue/2026-09-12-webauthn-tests-library-grade.md` の受け入れ条件は全項目未チェック)。hyoui が同じ「自前」判断を踏襲するなら、テスト負債も丸ごと引き継ぐ。ライブラリ比較はまだ行われていない。
8. **平文 http での運用に穴**: cookie に `Secure` が固定で付くため、`http://<LAN IP>/` 形式の endpoint では refresh cookie が保存されず、リロードごとに passkey を求められる。**未検証** (コード上の帰結として指摘、実機のブラウザ挙動は未確認)。
9. **登録は発行者 instance が生きている間だけ成立** (`[P]/docs/decisions/DR-0021-registration-in-two-halves.md` Consequences)。daemon 再起動で HMAC secret が消えるため、URL 発行から 10 分以内かつ無再起動が条件。

## 読んだファイル

hyoui 側 (Part 1、すべて `crates/` / `docs/` 相対):

- `crates/hyoui-web/src/lib.rs` (1164 行、全文)
- `crates/hyoui-web/src/ws_attach.rs` (601 行、全文)
- `crates/hyoui-web/assets/session.js` (grep + 該当箇所)、`index.js` (grep)、`session.html` (grep)
- `crates/hyoui/src/protocol/caps.rs` (1-60 行)、`protocol/mod.rs` (grep)
- `crates/hyoui/src/config/mod.rs` (WebConfig 節)
- `crates/hyoui-cli/src/main.rs` (`hyoui web` の listen 解決部を grep)
- `docs/decisions/DR-0027-web-gateway-in-repo.md` (§3 §4 §5 全文 + grep)
- `docs/decisions/DR-0031` / `DR-0033` / `DR-0034` (節見出し + 該当行 grep)
- `docs/decisions/DR-0013` / `DR-0022` (節見出し)

ccmsg 側 (Part 2): daemon `src/auth/{auth,http,records,webauthn,admin}.ts` 全文、`src/dispatch/identity.ts`、`src/instance/{instance,paths,config}.ts` (該当部)、`src/cli.ts` (passkey 部)、`src/sessions/registry.ts` (version 検査部)、`docs/decisions/DR-0001` 全文、`docs/DESIGN-ja.md` (認証節)、`docs/issue/2026-09-12-webauthn-tests-library-grade.md`、`test/auth.test.ts` / `test/authenticator.ts`。protocol `src/common/auth.ts` 全文、`src/envelope.ts`、`src/common/hello.ts`、`src/identifiers.ts`、`src/attributes.ts`、`docs/decisions/DR-0017` / `DR-0020` / `DR-0021` / `DR-0022`、`docs/issue/2026-09-09-passkey-list-for-people.md`。webui `src/auth/*.ts` 全文 (client / session / endpoint / register-link / device-label / tab-share)、`src/ui/{SignIn,Register,TerminalPanel,Shell,Disconnected}.tsx`、`src/terminal-url.ts`、`src/state.ts`、`src/connection.ts`、`docs/DESIGN.md` (§Authenticating a person)、`vite.config.ts`、`test/auth.test.ts`、`test/visual/harness.ts`。旧リポ `docs/decisions/DR-0030-sandbox-origin-serving.md` (Permissions-Policy 言及部)。reference `auth-patterns/` の `_index.md` + 4 ファイル全文。
