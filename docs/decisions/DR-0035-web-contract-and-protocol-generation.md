# DR-0035: web 境界の契約を型で正本化し、世代 version で stale なページを検出する

- Status: ⬜ 未実装 (2026-09-16)
- Date: 2026-09-16
- Related: DR-0027 (web gateway 同居、route / WS frame / query の現行正本), DR-0034 (`/version` と `build_id`、stable / unstable の HA fallback), DR-0008 (daemon 境界は cap flag 一本で固定 version を持たない), DR-0033 (`leader.request` = cap 差を browser に返す唯一の既存実例), DR-0013 (screen state 正本化。`layer=both` 復元が契約に乗る), DR-0022 (`POST /input` の auto-lock), DR-0036 (認証。本 DR の契約の上に載る)
- Origin: `docs/research/2026-09-15-web-protocol-and-passkey-grand-design.md` (§2 / §3)、事実は `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md` Part 1-A〜1-F
- 裁定: version 方式は (a) 世代番号 1 つ、**応答ヘッダは持たない** — CORS preflight を増やさないため (kawaz 2026-09-16、Q1)

## Context

### 現状

browser ↔ gateway の契約には version 識別子が無い。assets にも API にも「どの契約を話しているか」を名乗る値が無く、露出している version は `/api/sessions` の `daemon_version` (= daemon binary の版) だけで、これは web 契約の世代ではない。契約の所在も分散している: DR-0027 §3 の散文と、`crates/hyoui-web/src/lib.rs` の route 定義、`ws_attach.rs` 内の `serde_json::json!` 手書き、`assets/session.js` の読み取り側が分担しており、DR-0033 で `leader.request` を足した時も DR-0027 §3 に追記する形だった。

エラーの形も 2 系統ある。HTTP `/api/*` は plain text body で code の語彙が無く、WS は `ok:false` + `error` 文字列 1 本。未知の `kind` は `eprintln!` して黙って continue するので、browser には何も返らない。

assets は root 直下前提で書かれている。`/assets/...` の絶対参照が計 15 箇所、`fetch('/api/sessions')`、WS URL は `${proto}//${location.host}/api/sessions/<id>/attach`、session id の抽出は `location.pathname.split('/')[1]`。

### 目的

**browser 上で動いている JS が、今応答している gateway と同じ契約を話しているかを、browser 自身が判定できるようにする。** 判定できた時に出すのは「再読み込みしてください」の 1 つだけで、互換経路は持たない。

**同時に、契約の所在を 1 箇所に寄せる。** どの kind がどんな形かを知るために 3 ファイルを突き合わせる状態をやめ、Rust の serde 型を正本にして文書はそこから写す。

**path prefix の下にマウントされても成立させる。** これは DR-0036 の「gateway は自分の endpoint を知らない」設計の前提で、endpoint が `https://example.jp/hyoui` の形を取りうる以上、絶対パス前提が残っていると認証の endpoint 判定より前にページが壊れる。

ここで増やしたくないもの (目的と同格):

- **契約の写しの数**。型・文書・JSON Schema・JS の検証コードと増やすと、乖離した時に誰も気付かない写しが残る。正本は Rust 型 1 つ、写しは文書の表 1 つに限る
- **交渉の機構**。web 境界に cap 交渉を持ち込まない (下記「なぜ cap ではなく世代番号か」)
- **version を名乗る経路**。伝えるのは WS hello frame と `GET /version` の 2 つだけで、応答ヘッダ・query・cookie・assets のファイル名には出さない
- **browser 側が持つ定数**。JS が持つのは世代番号 1 つだけで、kind 一覧や cap 集合の写しを持たない

### なぜ「不一致」が平常運用で起きるのか

assets は gateway binary に埋め込まれる (DR-0027 §4) ので、配る側と受ける側は平常時必ず同じビルドである。ずれるのは 2 場面だけ:

1. browser がページを開いたまま gateway が入れ替わった (`hyoui web daemon restart`、brew upgrade、unstable の再ビルド)
2. HA endpoint (`hyoui.<host>`) の裏で canddy の fallback により unstable (43691) ↔ stable (43690) が入れ替わった (DR-0034 決定 8)

**場面 2 が hyoui 固有の事情である。** stable と unstable は常に別版なので (DR-0034 決定 8 末尾)、不一致は「たまの事故」ではなく「fallback のたびに起きうる状態」になる。検出と誘導が無ければ、WS 再接続で黙って壊れる。

### なぜ cap ではなく世代番号か

daemon 境界は cap flag の intersect 一本で固定 version を持たない (DR-0008)。同じ形を web 境界に持ち込まない理由は、境界の性質が違うことに尽きる。

**web 境界では、JS を配るのが gateway 自身である。** 交渉相手は常に同じビルドの自分で、交渉に意味があるのは「開いたままの stale なページ」1 ケースしかない。そのケースは reload で必ず収束する — reload 先は今応答している gateway が配る assets だからである。cap 集合を JS 側にも二重に持つコストだけが残る。

daemon 境界は逆で、client と daemon の版が独立に動く (古い daemon が走り続けている session に新しい CLI が繋ぐ)。そこでは「古い相手でも動く」が要件になる。

### 前提条件と、満たさない場合

| 前提 | 満たさない場合 |
|---|---|
| assets が gateway binary と同一ビルドで配られる (DR-0027 §4) | 世代の比較対象が「配った gateway」と「応答する gateway」の 2 つに分かれ、世代番号 1 つでは表せなくなる。`--web-assets-dir` でローカル dir を指す dev ではこれが起きるので、その時は帯が出続けるのを受け入れる (dev の便宜であって運用形ではない) |
| reload で新しい assets が取れる (cache ヘッダを持たない、findings Part 1-D) | 帯を出しても reload が効かず、誘導が嘘になる。cache ヘッダを足す判断をする時は、この前提を壊さないことが要件になる |
| 前段が `Sec-WebSocket-Protocol` と WS upgrade を透過する (DR-0027) | hello frame が届かず検出経路がゼロになる |
| 前段が path を strip する場合も、strip しない場合も、相対リンクだけでページが成立する (決定 6) | prefix 付き endpoint でページが壊れ、DR-0036 の endpoint 判定に到達しない |
| prefix 付き endpoint をスラッシュ無しで開いた時 (`https://example.jp/hyoui`) に、前段が `/hyoui/` へ redirect する | ブラウザの相対解決の基点が 1 段上になり、`assets/...` が `/assets/...` に化けてページが壊れる。**スラッシュ無しの URL を正規形へ寄せるのは前段 (canddy) の責務**で、gateway は自分のマウント path を知らないので redirect を書けない (決定 6 の正規形) |

## 介入判断 self-check (CLAUDE.md / DR-0014)

- **PTY / child / signal への介入は無い。** 変えるのは browser ↔ gateway の HTTP / WS 契約だけで、daemon 境界 (CBOR / cap flags) の message も cap も増やさない
- **透過原則には触れない。** binary frame は PTY bytes の 1:1 転写のまま (決定 5 で「世代不一致でも binary は通す」を明示的に扱う)
- **新 cap flag を足さない。** 決定 4 の cap 透過は、既に intersect されている事実を browser に見せるだけで、daemon 側に新しい cap を要求しない
- **kernel / PTY / shell の標準機能の再発明ではない。** version 比較と reload 誘導は web の層の話
- **既存 DR の実装漏れを先に見る。** DR-0034 §5 の `/healthz` `/version` は実装済 (`crates/hyoui-web/src/lib.rs:73-74`、v0.9.48) なので、本 DR は `/version` の body に 1 field を足す形に乗る。DR-0033 の `leader-request-v1` の cap 確認 (`ws_attach.rs:422-432`) は実装済で、決定 4 はこれを全 cap に一般化する

## Decision

### 1. 契約の正本は `contract.rs` の serde 型。文書は表で写す

`crates/hyoui-web/src/contract.rs` を作り、web 境界に乗る全ての形をそこに置く。

- WS text frame は `#[serde(tag = "kind")]` の enum 2 つ (browser → gateway、gateway → browser)
- HTTP の JSON request / response は struct
- エラーは `{"error": {"code": "<kebab-case>", "message": "<人向け>"}}` の 1 型 (決定 2)

`ws_attach.rs` が `serde_json::json!` で手書きしている `attach.info` / `resize.result` / `leader.result` をこの型に寄せ、手書きの JSON リテラルを残さない。

**JSON Schema は持たない。** JS 側は bundler 無し (DR-0027 §4) で schema を検証する tooling が無く、置けば実装と乖離した時に誰も気付かない写しになる。代わりに **Rust 側の golden test が各 kind の JSON 例を固定**し、本 DR の表はその例から写す。表と golden の食い違いは、golden を直した人が表も直す (同じ変更で両方を触る)。

命名は既存の `noun.verb` を維持する。応答は `<noun>.result`、通知は `<noun>.info`。名前空間 prefix (`hyoui.`) は付けない — この WS は hyoui 専用で、他の protocol と混ざる経路が無い。

#### HTTP routes

| method | path | request | 成功応答 | エラー |
|---|---|---|---|---|
| GET | `/` | — | `index.html` | — |
| GET | `/sessions/{id}` | 表示設定 query (DR-0027 §5) | `session.html`。server は `id` も query も見ない | — |
| GET | `/assets/{*path}` | — | 埋め込み or ローカル dir のファイル | 404 |
| GET | `/healthz` | — | `ok` (text) | — |
| GET | `/version` | — | `{"version": "<crate version>", "build_id": "<id>\|null", "protocol": N}` | — |
| GET | `/api/sessions` | — | session オブジェクトの JSON 配列 | 500 |
| GET | `/api/sessions/{id}/screen` | `layer=visible\|scrollback\|both` | ANSI bytes (`text/plain`) | 404 / 500 / 501 |
| POST | `/api/sessions/{id}/input` | `{"specs": ["text:...","key:Enter"]}` | `{"sent_bytes":N,"specs":M}` | 400 / 404 / 409 / 500 / 501 / 503 |
| POST | `/api/sessions/{id}/resume` | 空 | 204 | 404 / 500 / 501 |
| POST | `/api/sessions/{id}/resize` | `{"cols":W,"rows":H}` | 204 | 400 / 404 / 409 / 500 |
| GET | `/api/sessions/{id}/attach` | WS upgrade | WS (下記) | 400 / 401 / 404 |

`/version` に足すのは `protocol` 1 field。`version` / `build_id` は DR-0034 決定 7 のまま意味を変えない。

`/api/*` と WS attach の `401` は認証が有効な時だけ出る (DR-0036 決定 1)。body は決定 2 のエラー形で、`/auth/*` の route 自体の形は DR-0036 が決める。

#### WS text frame (browser → gateway)

| kind | payload |
|---|---|
| `resize` | `{"kind":"resize","requestId":N,"cols":W,"rows":H}` |
| `leader.request` | `{"kind":"leader.request","requestId":N}` |
| `auth.extend` | `{"kind":"auth.extend","requestId":N,"accessToken":"<値>"}` — DR-0036 決定 5 で使う |

#### WS text frame (gateway → browser)

| kind | payload | 送信契機 |
|---|---|---|
| `hello` | `{"kind":"hello","protocol":N,"version":"0.9.x","build_id":"abc123\|null","caps":["data","lock",...],"auth_expires_at":"<ISO 8601>\|null"}` | WS 確立直後、`attach.info` より前 (決定 3)。`auth_expires_at` は DR-0036 決定 5 で使う |
| `attach.info` | `{"kind":"attach.info","mode":"rw"\|"ro"\|"rw-no-leader"\|"unknown","leader":bool}` | `hello` の直後、`leader.notify` / `mode.change` 受信時 |
| `resize.result` | `{"kind":"resize.result","requestId":N,"ok":bool,"error"?:{"code":"...","message":"..."}}` | `resize` への応答 |
| `leader.result` | `{"kind":"leader.result","requestId":N,"ok":bool,"error"?:{"code":"...","message":"..."}}` | `leader.request` への応答 |
| `auth.extend.result` | `{"kind":"auth.extend.result","requestId":N,"ok":bool,"auth_expires_at":"<ISO 8601>"?,"error"?:{"code":"...","message":"..."}}` | `auth.extend` への応答 — DR-0036 決定 5 で使う |
| `error` | `{"kind":"error","requestId":N\|null,"error":{"code":"...","message":"..."}}` | 未知 `kind` / 不正 JSON を受けた時 (決定 2) |

binary frame は双方向とも PTY bytes の 1:1 転写で、契約の世代に依存しない。

**`auth.extend` / `auth.extend.result` / `hello.auth_expires_at` と、WS upgrade の 401 応答は、契約として本 DR の表に先に載せる。** 使うのは DR-0036 だが、契約の正本は 1 箇所 (`contract.rs` と本表) であるべきで、認証を足す時に「どの kind があるか」を別 DR に探しに行かせない。**追加であって削除も意味変更もしないので、DR-0036 で `WEB_PROTOCOL_VERSION` は上げない** (決定 3 の上げる条件)。認証が無効な間は `auth_expires_at` が `null` で、`auth.extend` は `error` (`code: "unsupported"`) を返す。

WS upgrade は認証が要る経路なので、**`101` の前に `401` を返しうる** (DR-0036 決定 1)。この場合 frame は 1 つも流れないので、browser は HTTP 応答として 401 を読む。

表示設定の query パラメータ (DR-0027 §5 が正本) は契約に含めるが、**未知 key を無視する前方互換方針は変えない。** 表示設定は閲覧者ごとの値であって protocol ではないので、世代の対象外である。

### 2. エラー形を JSON に統一し、未知 `kind` には `error` を返す

HTTP `/api/*` のエラー body を `{"error": {"code": "<kebab-case>", "message": "<人向け>"}}` に揃える。`code` は daemon の `ErrorCode` (`unsupported-capability` 等) をそのまま通す。WS の `ok:false` + `error` 文字列も同じ `{code, message}` に揃える。

**未知 `kind` / 不正 JSON の黙殺をやめ、`{"kind":"error","error":{"code":"unknown-kind",...}}` を返す。** 黙殺は「新しい browser + 古い gateway」を検出不能にしており、version 機構を入れるのと同時に直さなければ、帯を出す根拠が片方向しか揃わない。

**ただし `error` frame を世代不一致の推定には使わない。** 検出は決定 3 の `hello.protocol` 1 本で、`error` を受けたら「その要求が通らなかった」だけを扱う。2 経路で推定すると、cap 不足 (決定 4) と世代不一致が同じ帯に化ける。

この 2 つは既存契約の破壊であり、本 DR で `WEB_PROTOCOL_VERSION = 1` を置く契機そのものである。

### 3. `WEB_PROTOCOL_VERSION` — 世代番号 1 つ。伝えるのは hello と `/version` だけ

Rust 側に `pub const WEB_PROTOCOL_VERSION: u32` を `contract.rs` に、JS 側に同じ値を `assets/contract.js` に持つ。初版は `1`。

**上げるのは kind / field の削除と意味変更をした時だけ。** field や kind の追加では上げない。追加だけの変更で古い JS が新機能を使えないのは黙って起きるが、それは reload で解消する状態であって「話せない」ではない。

**伝達経路は 2 つに限る:**

| 経路 | 伝え方 | 検出する側 |
|---|---|---|
| WS `/api/sessions/{id}/attach` | 確立直後の `hello` frame | session ページ。**再接続のたびに比べる**ので、fallback で裏の unit が変わっても拾える |
| `GET /version` | body の `protocol` | index ページ (WS を持たないので、`/api/sessions` の polling と同じ周期で `/version` も引く)。人と `hyoui web daemon status` |

**応答ヘッダ (`X-Hyoui-Web-Protocol` 等) は持たない** (裁定 Q1)。カスタムヘッダは cross-origin で読むために CORS の preflight を要し、endpoint 構成が増えるたびに preflight の設計が付いてくる。hello frame と `/version` で検出点は足りている。

**`build_id` は不一致の判定に使わない。** unstable の再ビルドで値が動くたびに誘導を出すと、契約が同じでも「リロードしろ」が出続けて警告が形骸化する。hello と `/version` に載せるのは表示 (DR-0027 §3 の「情報」タブ) のためだけである。

### 4. daemon と intersect した cap を browser に見せる

これは世代 version とは別の問題である。web 境界の同版性 (決定 3) が browser ↔ gateway の話であるのに対し、こちらは **gateway ↔ daemon の機能有無を browser に透過する** 話で、両者は直交する。

- `hello` frame の `caps` に、**その session の daemon と intersect した cap 集合**を載せる。browser は `leader-request-v1` が無ければ「leader になる」を灰色にする、程度の表示に使う
- HTTP は要求時に判定し、`501 {"error":{"code":"unsupported-capability","message":"daemon does not support <cap>"}}` を返す

現在これを手作りでやっているのは `leader-request-v1` だけ (`ws_attach.rs:422-432`) で、**それを全 cap に一般化する**。DR-0034 決定 8 の「intersect に無い cap を要する操作だけ 501」を browser 側で扱える形にするのが目的である。

**daemon 側に新しい cap も message も足さない。** 既に handshake で決まっている事実を 1 field で見せるだけである。

### 5. 不一致時は帯を出す。自動リロードも切断もしない

- **画面端に帯**を出す (session ページは上端 1 行、index ページも同じ)。文言は「この画面は契約世代 N、gateway は M です。再読み込みしてください」+ 再読み込みボタン
- **自動リロードはしない** (kawaz 指示)。ボタンは `location.reload()` のみで、query (表示設定) はそのまま残る
- **既存の WS は切らない。** 切るのは入力中の内容 (xterm.js の未送信バッファ、FAB パネルの入力欄) を捨てることになる。ただし世代不一致のまま新しい要求を送ると誤動作しうるので、**帯が出た時点で制御 frame (`resize` / `leader.request`) の送信を止め、binary frame (キー入力) は通す**
- **gateway は世代不一致の接続を拒否しない。** ccmsg は `bad_request` で拒否するが、拒否すると帯を出す前に切れて「なぜ切れたか」が画面に残らない。browser が hello を読んで自分で判断すれば足りる

**binary frame を通し続ける根拠は「契約の世代に依存しないこと」だが、これは実機で確認してから確定する** (Implementation phases の gate 1)。古いページから新しい gateway に入力を送って画面が崩れないかを見る。崩れるなら binary も止めて帯だけを残す形に倒す。

### 6. assets / API / WS の URL を endpoint 基点の相対にする

ブラウザは `location` から **自分の endpoint (origin + マウント path) を 1 回決め**、以降の URL をそれ基点で組む。gateway はマウント先を知らず、route は `/{prefix}` 無しのままで、相対リンクだけで成立させる (前段が strip する構成でも、strip しない構成でも動く)。

endpoint の決め方:

- index ページ: `new URL(".", location.href)`
- session ページ: `location.href` から末尾の `sessions/<id>` と query を落とした URL

#### endpoint の正規形

**endpoint は `scheme://host[:port]/<path>/` の形とし、末尾の `/` を必須とする。** query と fragment を含まない。ccmsg の `Endpoint` 型 (`^https?://[^/?#\s]+(/[^?#\s]*)?/$`) と同じ形である。

正規形を仕様で固定するのは、**複数の実装 (ブラウザの JS、`hyoui web passkey add` の CLI、record を引く gateway) が同一の文字列に到達しなければならない**ためである。`new URL(".", location.href)` は必ず末尾 `/` 付きを返すのに対し、人が CLI に渡す `--endpoint https://hyoui.<host>` はスラッシュ無しなので、決めずに放置すると **record の key が食い違って引けない** (DR-0036 決定 3 / 決定 4)。どちらでもよいが 1 つに決める、が正しい箇所である。

- 上の 2 つの計算は正規形をそのまま返す
- CLI は受け取った `--endpoint` を正規化する (末尾 `/` を足し、query / fragment を落とす)。正規化できない値は拒否する
- cookie の `Path` は正規形から**末尾の `/` を落とした値**を使う (root の endpoint は `/` のまま)。`https://example.jp/hyoui/` なら `Path=/hyoui` (DR-0036 決定 5)

直す箇所:

| 箇所 | 現行 | 直し方 |
|---|---|---|
| `index.html` / `session.html` の `<link>` `<script>` (`/assets/...` 計 15 箇所) | 絶対 | `<base href>` は使わず相対に。`index.html` は `assets/...`、`session.html` は `/sessions/<id>` から配られるので `../assets/...` |
| `index.js:92` の `fetch('/api/sessions')`、`index.js:117` の `<a href="/sessions/...">` | 絶対 | `api/sessions`、`sessions/<id>` |
| `session.js:934,1026,1051,1079,1107` の `fetch('/api/sessions/...')` | 絶対 | `../api/sessions/...` |
| `session.js:1769-1770` の WS URL (`${proto}//${location.host}/...`) | host 直下 | `new URL('../api/sessions/<id>/attach', location.href)` で prefix を保ち、scheme だけ `ws(s):` に置換 |
| `session.js:10` の session id 抽出 (`location.pathname.split('/')[1]`) | root 前提 | 末尾 2 要素 (`sessions/<id>`) から取る |
| `lib.rs:915,938,941` の test (`/assets/...` の絶対文字列を find) | 絶対 | 相対に追従 |

**`<base href>` を使わない**のは、base が効く範囲がページ内の全ての相対 URL (将来足すものも含む) に及び、1 箇所の設定で全ての解決が変わる形になるため。endpoint の計算は JS が 1 回行い、URL はその値から組む方が、どこで解決されたかが読める。

### 7. 「持たない」を壊す変更をテストで止める

決定の核が「写しを増やさない」「経路を増やさない」の形をしているので、壊す変更は改善に見える。文章の禁止では止まらないので test で固定する:

| 固定する性質 | 形 |
|---|---|
| Rust と JS の世代番号が一致する | `contract.rs` の定数と `assets/contract.js` の定数を読み比べる test (JS は正規表現で 1 行抽出) |
| 各 kind の JSON 表現が動いていない | golden test (決定 1)。表を写す元でもある |
| `/version` が `protocol` を含む | 既存の `/version` test に 1 assert |
| 応答ヘッダに version を名乗る経路が無い | `/api/*` と `/version` の応答ヘッダに `protocol` を含む名前のヘッダが無いことを見る test (決定 3 の「持たない」を固定) |
| assets に絶対パス参照が無い | `assets/*.html` / `*.js` に `"/assets/` `'/api/` `"/sessions/` のリテラルが無いことを見る test (決定 6) |

## Implementation phases

| Phase | 内容 | gate (= 次に進む条件) |
|---|---|---|
| W1-1 | `contract.rs` 新設。既存の `json!` 手書きを型に寄せ、golden test で JSON 例を固定 | golden の JSON が現行の実際の frame と byte 一致する (= この Phase では契約を変えていない) |
| W1-2 | エラー形の JSON 統一、未知 `kind` への `error` 応答 (決定 2) | `hyoui screen` / `input` / session ページの既存操作が全部通る。エラー時の body が全経路で `{error:{code,message}}` |
| W1-3 | `WEB_PROTOCOL_VERSION = 1`、`hello` frame、`/version` の `protocol`、決定 7 の test (決定 1 / 3) | 世代番号の Rust / JS 一致 test が通る。`hyoui web daemon status` から `/version` の `protocol` が読める |
| W1-4 | 帯の UI と制御 frame の停止 (決定 5) | **実機確認 (gate 1)**: stable / unstable を別の `WEB_PROTOCOL_VERSION` でビルドし、(a) ページを開いたまま `daemon restart` で入れ替える、(b) HA endpoint で fallback を起こす、の 2 経路で帯が出る。**帯が出ている間に古いページから入力を送って画面が崩れないこと**を `hyoui screen dump` で確認する (崩れるなら決定 5 を binary も止める形に直す)。**(a) だけで先に通してよい** — (b) は canddy の 3 endpoint (DR-0034 P6) が立つまで確かめられないが、決定 5 の判断に必要な事実は (a) で揃う。(b) は endpoint が立った時点で確認する |
| W1-5 | cap 透過 (決定 4) | `leader-request-v1` を持たない daemon (旧版) に対して、WS は `caps` から落ち、HTTP は 501 を返す。**3 category で確認** (TUI = vim / line-oriented = cat / REPL = bash の session それぞれに対して) |
| W1-6 | 相対パス化 (決定 6) | **実機確認 (gate 2)**: (a) root 直下 (`http://127.0.0.1:43690/`)、(b) 前段が path を strip する prefix 付き (`https://<host>/hyoui/` → strip)、(c) strip しない prefix 付き、の 3 構成で index / session の両ページが動き、WS が繋がる。加えて **(d) prefix 付きをスラッシュ無し (`https://<host>/hyoui`) で開いた場合**の挙動を観測し、前段の redirect が無いと壊れることを確認して前提条件表の依頼内容 (canddy への redirect 追加) を確定する。ブラウザが計算する endpoint が 3 構成すべてで正規形 (決定 6) になることも見る |

gate 1 と gate 2 は DR-0036 の実装より前に通す。gate 2 が通らないと DR-0036 の endpoint 判定が成立しない。

## Alternatives Considered

| 案 | 中身 | 不採用理由 |
|---|---|---|
| cap 方式 (daemon 境界と同型) | hello で cap 集合を交換し、機能単位に落とす | JS を配るのが gateway 自身なので、交渉相手は常に同じビルドの自分。交渉に意味があるのは stale なページ 1 ケースだけで、それは reload で収束する。cap 集合を JS 側に二重に持つコストだけが残る |
| 世代番号 + cap 方式の併用 | 契約の世代を世代番号で、機能の有無を cap 交渉で | 「web 境界の同版性」と「daemon 境界の cap 透過」は別の問題で、併用ではなく別々に必要かを問うのが正しい。後者は決定 4 として hello に 1 field 足すだけで足り、交渉の機構は要らない |
| 応答ヘッダ (`X-Hyoui-Web-Protocol`) で伝える | 全 `/api/*` 応答にヘッダを載せ、index ページもそれを見る | cross-origin で読むには CORS preflight が要り、endpoint 構成が増えるたびに preflight の設計が付いてくる (裁定 Q1)。index ページは `/version` を polling すれば足りる |
| JSON Schema を契約の正本または副本に持つ | schema ファイルを置き、文書と実装の両方をそこから導く | 素の JS 環境に検証する tooling が無く、乖離しても誰も気付かない写しになる。Rust 型 + golden test で同じ固定が得られる |
| `build_id` の不一致でも誘導を出す | ビルドが違えば帯を出す | unstable の再ビルドごとに帯が出て、契約が同じでも警告が出続ける。警告が形骸化する |
| gateway が世代不一致の接続を拒否する (ccmsg 型) | hello の前に `bad_request` で切る | 帯を出す前に切れて「なぜ切れたか」が画面に残らない。判断は browser が持てば足りる |
| 自動リロード | 不一致を検出したら `location.reload()` | 入力中の内容を予告なく捨てる。kawaz 指示で明示的に不採用 |
| `<base href>` で prefix を吸収する | ページに `<base>` を 1 つ置く | 効く範囲がページ内の全相対 URL に及び、将来足す URL の解決も暗黙に変わる。endpoint を JS が 1 回計算して組む方が、どこで解決されたかが読める |

## Consequences

- **契約の破壊を 1 度だけ行う。** エラー形の JSON 化と未知 `kind` への `error` 応答は既存 browser を壊すが、assets は gateway と同一ビルドなので実害は「開いたままのページ」に限る。まさに本 DR が検出して reload を促す対象である
- **`WEB_PROTOCOL_VERSION` を上げる判断が今後付いて回る。** 上げる条件 (削除と意味変更) を決定 3 に書いたので、追加だけの変更で上げない。上げた時は帯が出るのが正しい挙動である
- **写しが 2 つになる。** Rust の定数と JS の定数。一致は test で固定する (決定 7)。これは「bundler を持たない」(DR-0027 §4) の帰結で、型を共有できない以上避けられない
- **`--web-assets-dir` の dev では帯が出うる。** ローカル dir の assets が binary の世代と食い違う場合で、前提条件表のとおり受け入れる
- **cap 透過で 501 が増える。** 旧版 daemon に繋いだ session では操作が個別に落ちるようになる (現在は 500 か手作りのエラー文字列)。DR-0034 決定 8 が要求した形であり、browser 側で灰色表示に落とせる
- **endpoint の正規形が 3 者 (JS / CLI / gateway) の共通語彙になる。** 末尾 `/` を必須にしたので、スラッシュ無しで開かれた prefix 付き endpoint を正規形へ寄せる redirect が前段に要る。これは canddy への依頼が 1 本増えることを意味する (DR-0034 P6 の issue に相乗りできる)
- **相対パス化が DR-0036 の前提になる。** gate 2 が通らなければ、DR-0036 の「gateway は endpoint を知らない」は成立せず、endpoint を config に持つ形に倒す判断が必要になる
- **index ページの polling が 1 本増える** (`/api/sessions` に加えて `/version`)。同じ周期に乗せるので往復は増えるが、頻度は変わらない

## 関連

- `docs/research/2026-09-15-web-protocol-and-passkey-grand-design.md` — 本 DR の母体 (§2 契約整理 / §3 version 機構)、kawaz 裁定表 (2026-09-16)
- `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md` — 現行契約の事実 (Part 1-A HTTP / 1-B WS / 1-D assets・cache / 1-E cap / 1-F 関連 DR)、ccmsg の世代 version 実装
- DR-0027 §3 (route / WS frame の現行正本、本 DR の決定 1 が引き継ぐ)、§4 (bundler 無し vendored assets)、§5 (query パラメータ)
- DR-0034 決定 7 (`/version` の body と `build_id`)、決定 8 (HA fallback と cap 差の吸収)
- DR-0008 §3 (daemon 境界が固定 version を持たない方針)
- DR-0036 — 認証。本 DR の決定 6 (endpoint 基点の相対 URL) を前提に載る
