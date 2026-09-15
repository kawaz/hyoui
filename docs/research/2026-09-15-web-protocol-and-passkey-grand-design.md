# web 境界の契約整理と passkey 認証のグランドデザイン

- Date: 2026-09-15
- Status: In Progress <!-- In Progress / Concluded / Archived -->

## 動機

kawaz 依頼 (2026-09-15) の 2 点に対して、DR を起こす前の**全体像**を示す。

1. webui ↔ gateway の protocol に version を設け、不一致時は自動リロードせず「リロードが必要」を画面端に出す
2. ccmsg で実証した passkey 認証を hyoui webui にも入れる。ccmsg と事情が違う所は hyoui の形にする

補足 (同日): 互換性の破壊は構わない、利用者は kawaz だけ。コマンド体系が大きく変わるなら major bump で足りる。

本文書は決めない。「hyoui ではこういう形にする」を選択肢と統括推しの根拠付きで並べ、§7 で kawaz の裁定が要る点だけを切り出す。裁定でない点は本文で決めて書く。事実は `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md` (以下「棚卸し」) に依り、本文書で新たに確認した事実は「本調査で確認」と付す。推測は推測と明記する。

## 調査範囲

- 扱う: browser ↔ gateway の HTTP / WS 契約、その version 機構、gateway の認証・認可、ccmsg webui の iframe 埋め込み経路との関係、移行順序
- 扱わない: daemon ↔ client の CBOR protocol の変更 (DR-0008 の cap 方式は変えない)、canddy 側の設定変更の中身 (依頼先が別リポ)、passkey の暗号仕様の詳細 (reference `auth-patterns` と ccmsg DR-0001 が正本)

## 調査メモ

### 2026-09-15: 現状の全体像 (§1)

境界は 2 つあり、契約の所在・version 機構・認証の有無が非対称になっている。

```text
                    (a) web 境界                     (b) daemon 境界
 browser  ───────────────────────►  gateway  ──────────────────────────►  daemon (PTY session)
 (xterm.js, 素の JS)  HTTP + WS       (hyoui web, axum)   CBOR frame / UDS      (1 session 1 socket)

 契約の所在   DR-0027 §3 (散文) + lib.rs / ws_attach.rs   DR-0008 + protocol/*.rs (serde 型)
 version     無し (assets にも API にも識別子が無い)        cap flag の intersect (固定 version 無し)
 認証        無し (bind 先 + 前段 tailnet が到達制限)        同 UID (socket perm 0600) + lock token
 エラー形    plain text body / WS は ok:false + 文字列      ErrorCode 語彙あり (unsupported-capability 等)
 未知の要素  query 未知 key = 無視、WS 未知 kind = 黙殺      未知 field = 無視、未知 kind = 無視 (cap で交渉)
```

到達経路は 3 本ある (本調査で canddy の `Caddyfile` を確認):

```text
 [1] tailnet の人 ─► https://hyoui.<host>.kawaz.jp ─► canddy ─► 127.0.0.1:43690 (stable)
                                                            └─► 127.0.0.1:43691 (unstable、DR-0034 P5 以降)
 [2] ccmsg webui (https://ccmsg.<host>.kawaz.jp) の Terminal タブ
        └─ <iframe src="https://hyoui.<host>.kawaz.jp/sessions/<id>?embed=1&resize=1"
                   sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
                   allow="clipboard-read; clipboard-write">      ── 認証情報は一切渡らない
 [3] 同一ホストの curl / test / `hyoui service status` ─► http://127.0.0.1:4369x 直結
```

ここで効く事実を 3 つ挙げる。

- **[1] と [2] は gateway から見ると全部 127.0.0.1 発**。canddy が reverse proxy なので、tailnet からの接続も iframe からの接続も、gateway の accept 時点では loopback から来る。「127.0.0.1 直結は認証免除」を採ると canddy 経由も免除になり、認証を入れた意味が消える (§4.4)
- **ccmsg webui と hyoui gateway は別 origin だが同一 site** (`ccmsg.<host>.kawaz.jp` と `hyoui.<host>.kawaz.jp`、registrable domain は `kawaz.jp`)。同一 site なら `SameSite=Strict` の cookie でも iframe 内のリクエストに乗る (仕様上の帰結。実機ブラウザでの確認は未実施 = 推測)。これが iframe 認証経路の推し (§4.3) を支える
- **stable / unstable の 2 port は canddy の下では同一 origin** (`hyoui.<host>.kawaz.jp` 1 つ、`lb_policy first` で振り分け)。「2 port が同一 origin でない問題」は 127.0.0.1 直結 [3] にしか存在せず、[3] は §4.4 のとおり passkey の対象外にするので、問題自体が消える

### 2026-09-15: web 境界の契約の整理案 (§2)

**1 つの契約文書 (将来の DR、仮称 DR-0035 web contract) に HTTP routes / WS frame / JSON kinds / エラー形 / version を集約する。** 現在は DR-0027 §3 の散文と実装 (`lib.rs` / `ws_attach.rs` / `session.js`) が契約を分担しており、DR-0033 で `leader.request` が足された時も DR-0027 §3 に追記する形だった。棚卸しの Part 1-A/1-B の表がそのまま初版の中身になる。

**契約の正本はコードに置き、文書は表で写す。** Rust 側に `crates/hyoui-web/src/contract.rs` を作り、WS text frame の全 kind を serde 型 (`#[serde(tag = "kind")]` の enum) で、HTTP の JSON body を struct で持つ。現在 `ws_attach.rs` 内で `serde_json::json!` を手書きしている箇所 (`attach.info` / `resize.result` / `leader.result`) をこの型に寄せる。JS 側は bundler 無し (DR-0027 §4) なので型は共有できない。**JSON Schema は持たない**: 検証に使う tooling が無い素の JS 環境で schema を置いても、実装と乖離した時に誰も気付かない dead doc になる。代わりに Rust 側の golden test で各 kind の JSON 例を固定し、契約文書の表はその例から写す。

命名は既存の `noun.verb` (`leader.request` / `leader.result` / `attach.info`) を維持し、新規 kind もこれに揃える。応答は `<noun>.result`、通知は `<noun>.info` か `<noun>.notify` とする。名前空間 (`hyoui.` prefix 等) は付けない: この WS は hyoui 専用で、他の protocol と混ざる経路が無い。

**エラー形は JSON に統一する。** HTTP の `/api/*` は現在 plain text body (棚卸し Part 1-A) で、code の語彙が無い。`{"error": {"code": "<kebab-case>", "message": "<人向け>"}}` に揃え、code は daemon の `ErrorCode` (`unsupported-capability` 等) をそのまま通す。WS の `ok:false` + `error` 文字列も `error: {code, message}` に揃える。**未知 `kind` の黙殺をやめ、`{"kind":"error","code":"unknown-kind","requestId":N?}` を返す**。黙殺は「新しい browser + 古い gateway」の検出を不可能にしている (棚卸し Part 1-B 考察) ので、version 機構と同時に直す。

query パラメータ (DR-0027 §5) は契約文書に含めるが、未知 key を無視する前方互換方針は変えない。表示設定は閲覧者ごとの値であって protocol ではない。

### 2026-09-15: version 機構 (§3)

#### 何を比べるのか

web 境界の「不一致」は、**gateway が配った assets (= browser 上の JS) と、今応答している gateway が同じ契約を話しているか**である。assets は gateway binary に埋め込まれる (DR-0027 §4) ので、平常時は必ず一致する。ずれるのは次の 2 場面だけ:

1. browser がページを開いたまま gateway が入れ替わった (`hyoui service restart`、brew upgrade、unstable の再ビルド)
2. canddy の fallback で、同じ origin の裏で unstable (43691) から stable (43690) に切り替わった、またはその逆 (DR-0034 §7)。**2 unit は常に別版**なので、ここは平常運用で起きる

場面 2 が hyoui 固有の事情で、ccmsg には無い。つまり不一致は「たまの事故」ではなく「fallback のたびに起きうる状態」で、検出と誘導が無いと WS 再接続で黙って壊れる。

#### 比べる値の候補

| 値 | 変わる契機 | 不一致が意味すること |
|---|---|---|
| (i) 契約の世代 `WEB_PROTOCOL_VERSION` (整数 1 つ、Rust と JS の両方に定数) | kind / field の削除・意味変更をした時だけ (追加では上げない) | この JS はこの gateway と**話せない** |
| (ii) `build_id` (DR-0034 §5、git short hash + dirty) | ビルドごと | この JS はこの gateway と**同じビルドではない** (話せるかは不明) |
| (iii) crate version | tag ごと | ほぼ (ii) の粗い版。stable と unstable が同じ値を答える (DR-0034 §5) ので単独では役に立たない |

#### 3 方式の比較

| 方式 | 中身 | 長所 | 短所 |
|---|---|---|---|
| (a) 世代番号 (ccmsg 型) | (i) を hello と応答ヘッダで伝え、不一致 = リロード誘導。互換経路なし | 実装が最小。「話せない」時だけ誘導が出る。fallback で版が動いても reload で必ず収束する (reload 先が今応答している gateway の assets だから) | 追加だけの変更 (新 kind) は世代を上げないので、古い JS が新機能を使えないのは黙って起きる |
| (b) cap 方式 (daemon 境界と同型) | hello で cap 集合を交換し、機能単位に落とす | 追加的進化と部分的な互換 | **browser の JS は gateway 自身が配る**ので、交渉相手は常に同じビルドの自分。交渉する意味が「stale なページ」1 ケースにしか無く、そのケースは reload で解決する。cap 集合を JS 側にも二重に持つコストだけが残る |
| (c) 併用 | (a) で契約の世代、(b) で gateway ↔ daemon の cap を browser に見せる | — | 「web 境界の version」と「daemon 境界の cap を browser に透過する」は**別の問題**で、併用ではなく別々に必要かを問うべき (下記) |

**統括推し: (a) 世代番号。** 理由は表の (b) の短所そのもので、web 境界は「配る側と受ける側が同じビルド」という daemon 境界に無い性質を持つ。cap 交渉が守る「古い相手でも動く」は、reload すれば新しい相手になる境界では価値が無い。

ただし (c) が挙げている **gateway ↔ daemon の cap 不一致を browser に見せる**必要は別途ある。DR-0034 §7 レビューの「intersect に無い cap を要する操作だけ 501」を browser 側で扱うためで、これは web 境界の version ではなく**daemon 境界の状態の透過**。形は:

- WS の hello (下記) に、その session の daemon と intersect した cap 集合を `caps: ["data","lock",...]` として載せる。browser は `leader-request-v1` が無ければ「leader になる」を灰色にする、程度の表示に使う
- HTTP は要求時に判定し、`501 {"error":{"code":"unsupported-capability","message":"daemon does not support <cap>"}}` を返す (JSON エラー形は §2)。現在 `leader-request-v1` だけが手作りでやっていること (`ws_attach.rs:422-432`) を全 cap に一般化する

これは (a) に cap を「併用」するのではなく、(a) の hello frame に daemon 側の事実を 1 field 足すだけなので、ハイブリッドではない (両者の長所は直交している: (a) は browser ↔ gateway の同版性、cap 透過は gateway ↔ daemon の機能有無)。

#### 伝え方と検出点

| 経路 | 伝え方 | 検出 |
|---|---|---|
| WS `/api/sessions/{id}/attach` | 確立直後、`attach.info` の前に gateway が `{"kind":"hello","protocol":N,"version":"0.9.x","build_id":"abc123","caps":[...]}` を送る | browser が自分の定数と `protocol` を比べる。再接続のたびに比べるので、fallback で裏が変わっても拾える |
| HTTP `/api/*` の全応答 | ヘッダ `X-Hyoui-Web-Protocol: N` | index ページ (WS を持たず polling のみ) は `/api/sessions` 応答のヘッダを見る。session ページの `screen` / `input` 応答も同じ |
| `GET /version` (DR-0034 §5) | body に `protocol: N` を足す | 人と `hyoui service status` 向け |

`build_id` は hello と `/version` に載せるが、**不一致の判定には使わない**。unstable の再ビルドで hash が動くたびに誘導を出すと、契約が同じでも「リロードしろ」が出続けて警告が形骸化する。情報タブ (DR-0027 §3 の「情報」タブ) に表示するだけにする。

#### 不一致時の UI

- 画面端 (session ページは上端 1 行、index ページも同じ) に帯を出す。文言は「この画面は契約世代 N、gateway は M です。再読み込みしてください」+ 再読み込みボタン。**自動リロードはしない** (kawaz 指示)
- 帯が出ている間も**既存の WS は切らない**。切るのは gateway 側ではなく browser 側の判断で、入力中の内容 (xterm.js の未送信バッファ、FAB パネルの入力欄) を守る。ただし世代不一致の状態で新しい要求を送ると誤動作しうるので、帯が出た時点で `resize` / `leader.request` 等の**制御 frame の送信は止め**、raw bytes (キー入力) は通す。**raw bytes を通し続けてよいかは未検証**: binary frame は契約の世代に依存しない (PTY bytes の 1:1 転写) ので通せると読んでいるが、実機で「古いページから新 gateway に入力を送って崩れないか」を確認してから決める
- 再読み込みボタンは `location.reload()` のみ。query (表示設定) はそのまま残る
- gateway 側は世代不一致の接続を**拒否しない**。ccmsg は `bad_request` で拒否するが、hyoui では browser が hello を読んで自分で判断すれば足り、拒否すると帯を出す前に切れて「なぜ切れたか」が画面に残らない

### 2026-09-15: 認証のグランドデザイン (§4)

#### 4.1 何を守るか、何を守らないか

- 守る: `/api/*` と WS attach への到達。つまり **session の画面内容と入力**
- 守らない: `/healthz` `/version` (DR-0034 §5 の「認証境界を変えない」を維持、可用性監視の入口)、`/assets/*` (ログイン画面自体がこれを使う)、`GET /` と `GET /sessions/{id}` の HTML (静的 shell であり session の内容を含まない。server が `id` を見ない DR-0027 §5 の性質もそのまま残る)
- 認証は **gateway 自身が判定**する (前段 forward auth にしない、reference `passkey-registration-local-first` の不採用表と同じ理由)。canddy は今後も透過で、`Caddyfile` のコメント「gateway 側は認証省略で OK」は移行完了時に書き換えを依頼する

HTML を無認証で配る帰結として、ログイン状態の判定は JS が `/api/*` の 401 を受けて行い、ページ内に overlay でログイン UI を出す。ログイン専用ページ (`/login`) への redirect はしない: iframe 内で redirect が起きると、親 (ccmsg) の Terminal タブがログインページに化けて、何が起きたか読めなくなる。

#### 4.2 bootstrap (初回をどう信頼するか)

| 案 | 中身 | 評価 |
|---|---|---|
| (A) CLI 発行の招待 URL (ccmsg と同型) | `hyoui web passkey add [--label <名前>]` が `<public_url>/#register=<jwt>` と 6 桁コードを出す。jwt は登録 1 本ごとの乱数 secret の HMAC、10 分、fragment で運ぶ | 登録の起点がホスト上の CLI に閉じる。reference と ccmsg で実証済み。gateway が 2 unit (stable / unstable) ある場合、URL を発行した unit と登録 POST を受ける unit が canddy の振り分けで違いうる (下記 4.6) |
| (B) localhost 限定の無認証登録ページ | `http://127.0.0.1:4369x/register` を開けば登録できる | **不成立**。canddy 経由も 127.0.0.1 発なので「localhost 限定」が判定できない (§1)。`X-Forwarded-For` を信じる形にすると、前段が付けない構成で穴になる |
| (C) 初回は無認証、1 つ登録されたら閉じる (TOFU) | 最初の登録だけ誰でもできる | tailnet 前提の今は実害が薄いが、gateway を再インストールするたびに窓が開く。(A) より簡単でもない (CLI を 1 本足すだけの差) |

**(A) で確定** (reference `passkey-registration-local-first` の規定そのもので、裁定は要らない)。6 桁コードの第 2 経路も ccmsg と同じく持つ (URL が漏れただけでは登録にならない)。reference の任意ゲート (b) (ホスト PC の生体認証承認) は hyoui でも後続とし、初版に入れない。

#### 4.3 iframe 問題 (ccmsg webui に埋め込まれた hyoui をどう認証するか)

ccmsg の検証コードは `topOrigin` が存在するだけで拒否し (棚卸し、本調査で `webauthn.ts:108-118` を再確認)、iframe 内で WebAuthn を走らせる前例が無い。hyoui は ccmsg webui の iframe に置かれる (経路 [2]) ので、ここが構造的な衝突点になる。

| 案 | 中身 | 長所 | 短所 |
|---|---|---|---|
| (P) **top-level で一度 hyoui に login し、cookie を持たせる** | iframe 内の hyoui ページは 401 を受けると「hyoui にログイン」ボタンを出す。押すと `window.open("<hyoui origin>/?login=1")` で hyoui origin の top-level window を開き、そこで passkey 認証 → session cookie が hyoui origin に置かれる。popup は同一 origin の `BroadcastChannel` で iframe に「ログインした」を伝え、iframe は API を叩き直す。**同一 site なので cookie は iframe 内のリクエストにも乗る** (§1、推測含む) | ccmsg 側に変更が要らない (sandbox に `allow-popups` は既にある、棚卸し Part 1-C)。iframe 内で WebAuthn を走らせないので ccmsg と同じ判断 (`topOrigin` 拒否) を hyoui でも採れる。hyoui 単体で開く経路 [1] と全く同じ認証 UI になる | ログインのたびに popup が 1 つ開く。1 度 cookie が乗れば以後は出ない (寿命は 4.5)。Safari 等の ITP が同一 site の iframe cookie をどう扱うかは**未検証** |
| (Q) 親 (ccmsg) が短命 token を発行し、iframe URL か postMessage で渡す | ccmsg daemon が hyoui 用の token を mint、hyoui が検証 | ログイン 1 回で両方に入れる | hyoui が ccmsg を IdP として信頼することになり、ccmsg-protocol / ccmsg daemon / ccmsg-webui の 3 リポに hyoui 専用の契約 (token の形、鍵の共有か公開鍵の配布) が増える。hyoui 単体の経路 [1] には別途 passkey が要るので、認証経路が 2 本になる。「hyoui の認証は hyoui が判定する」から外れる |
| (R) iframe 内で WebAuthn を走らせる | 親が `allow="publickey-credentials-get"` を付け、hyoui の RP が `topOrigin` を許容 | popup が要らない | ccmsg-webui の変更 + hyoui 側で「どの topOrigin を許すか」の allowlist 設計が要る。ccmsg が明示的に拒否した判断の逆を張る根拠が「popup が煩わしい」だけ。`create()` (登録) の iframe 対応はブラウザ差が大きい (推測、未検証) |

**統括推し: (R)** (初版は (P) を推したが、kawaz の指摘「ccmsg の `topOrigin` 拒否は ccmsg 自身が iframe に入らない判断で hyoui の RP には無関係」を受けて改めた)。ccmsg-webui 側の変更は iframe の `allow="publickey-credentials-get"` 1 属性で、登録は CLI の招待 URL を top-level で開く経路なので `create` を iframe で走らせない。hyoui 側は `[web].frame_ancestors` に ccmsg の origin を置いて `clientDataJSON.topOrigin` を照合し、同じ値で CSP `frame-ancestors` を出す (無条件許可にすると他所のサイトに埋め込まれて passkey を求められる形が開く)。(P) は popup が要る分 UX が落ちるだけで、(R) が崩れた時の退路として残す。(Q) は ccmsg と hyoui を結合する判断で、kawaz の「ccmsg の認証と hyoui の認証は別途」と合わないので採らない。

(P) が成立しない場合 (= 同一 site の iframe cookie をブラウザが落とす場合) の退路は (R) で、その時は hyoui 側の `topOrigin` allowlist を config `[web].frame_ancestors` に置き、同じ値で CSP `frame-ancestors` も出す形になる。**(P) の成立は実装前に実機で確認する** (Chrome / Safari / iOS Safari の 3 category、`empirical-verification` の 3 サンプル原則)。

#### 4.4 RP ID / origin と 127.0.0.1 直結

- **RP ID = config `[web].public_url` (例 `https://hyoui.<host>.kawaz.jp`) の hostname に固定。** ccmsg と同じく Host ヘッダから決めない (棚卸し「RP ID / origin」)。`public_url` は新しい config 項目で、無い場合は passkey の CLI が「`[web].public_url` を設定してください」で止まる。`clientDataJSON.origin` は `public_url` の origin と完全一致
- **`/auth/*` の POST と WS upgrade は `Origin` ヘッダを `public_url` の origin と照合**する (無い POST は 403)。cookie が `SameSite=Strict` でも、WS の cross-site hijack と CSRF に対する 2 層目
- **127.0.0.1 直結 (経路 [3]) は passkey の対象外。** WebAuthn の RP ID は domain であり IP アドレスは使えない (仕様上。`localhost` は可)。かつ §1 のとおり loopback 発を免除にはできない。したがって経路 [3] は「認証を切った gateway」でしか使えない: config `[web].auth = "none" | "passkey"` を持ち、test と手元の dev は `none` で動かす。stable / unstable の常駐 unit は `passkey`
- 非 browser client (curl / script) 向けの bearer token (`hyoui web token add`) は**初版で持たない**。hyoui の自動操作 CLI (`hyoui input` / `wait` / `tail`) は daemon の UDS を直接叩き、gateway を経由しない (DR-0005)。gateway の `/api/*` を script から叩く需要が出た時に足す。`hyoui service status` が叩く `/version` は無認証なので影響しない

#### 4.5 認証セッション (cookie / token / 寿命)

| 案 | 中身 | 評価 |
|---|---|---|
| (S1) **httpOnly cookie 1 本** | opaque 乱数 32 byte、`__Secure-hyoui-<sha256(public_url origin) 先頭 16 hex>`、`HttpOnly; Secure; SameSite=Strict; Path=/`。gateway は lookup で検証。寿命 30 日の sliding (使うたび `Max-Age` を伸ばす、値は据え置き)。WS upgrade も cookie で認証 (upgrade request には cookie が乗る) | 素の JS で完結。複数タブの refresh 調停 (reference `multi-tab-token-refresh`) が丸ごと不要。fallback で unit が変わっても、record が file 共有 (4.6) なら session が続く |
| (S2) ccmsg 型 (access = メモリ + WS subprotocol、refresh = cookie、rotate + 再利用検知) | 棚卸しのとおり | refresh の再利用検知で cookie 盗難を検出できる。代償は tab-share (Web Locks + BroadcastChannel) と rotate の実装で、ccmsg でも tab-share のテストは未整備 (棚卸し) |

**統括推し: (S2) reference どおり** (初版は (S1) を推したが、reference `passkey-registration-local-first` が規定する形から乖離する理由が「実装量」で、パターン統一 (kawaz 2026-09-15) より弱い。tab-share は `multi-tab-token-refresh` を素の JS で書く)。(S1) を推していた理由は 2 つで、乖離の代償として残す。hyoui の assets は bundler 無しの素の JS で、ccmsg webui (Preact + signal) の tab-share を移植する土台が無い。もう 1 つは利用者が kawaz 1 人で、再利用検知が守る「盗まれた refresh を誰かが使い回す」場面より、fallback や複数タブでの安定性の方が日常の価値が大きい。cookie が `HttpOnly` なので XSS で読めない点は (S2) と同じ。失効は CLI (`hyoui web passkey remove <sub>` で credential と session を両方消す) と、`hyoui web session list|remove` で個別に落とせる形にする。

`Path=/` にするのは、hyoui は ccmsg と違って同一 host に複数 endpoint を置く構成 (`/` と `/personal/`) を持たないため。ccmsg-protocol の fixture には `https://mba.example.ts.net/hyoui` の path prefix 構成が現れるが (棚卸し Part 1-C)、実機の canddy は host で分けている。**path prefix 配置は初版で非対応**とし、必要になったら `public_url` の path を Path に写す。

#### 4.6 認可の軸と record の置き場

- **認可は credential 単位の `access = "rw" | "ro"`**。passkey を登録する時に `hyoui web passkey add --ro` で決め、gateway は `ro` の session からの `input` / `resize` / `leader.request` / WS の binary 上りを 403 で落とし、WS attach を daemon に `Ro` mode で張る。既定は `rw`。ccmsg の role (`user` 1 種) では表せない「観測だけ許す端末」(例: スマホ) を、hyoui が daemon に既に持つ mode に写して実現する。`rw-no-leader` は web の認可軸には出さない (leader は取り合いの結果であって権限ではない)
- **sub は `<label>` (人が付けた名前)、user handle は sub ごとに 1 度だけ決めた 16 byte 乱数** (reference)。利用者 1 人前提でも、端末ごとに credential が分かれる (macOS / iPhone / 別 PC) ので `sub` は「端末を持つ人」ではなく「登録 1 本」に近い運用になる。ccmsg の `sub = <unit>-<連番>` と同じ
- **record は `$XDG_STATE_HOME/hyoui/web/auth.json`** (mode 0600、tmp + rename、書き込みは `flock`)。credential と session の両方を持つ。**stable / unstable の 2 unit がこの 1 file を共有する**: 同じ origin の裏で振り分けられるので、どちらで登録・ログインしても他方で通る必要がある。DR-0034 の「gateway 間の状態共有をしない」(やらないこと表) は「gateway 自身が状態を持たない」の意味で、file を正本にして各 unit が読むだけの形はこれに反しない (DR-0006 §1 の socket dir が正本と同じ形)。読み込みは要求ごと (mtime で cache)。ccmsg の instance 間複製 (`auth.records` topic、LWW + tombstone) は不要
- 登録 jwt の HMAC secret と challenge の在庫は **発行した unit のメモリにだけ**ある。`hyoui web passkey add` は 1 つの unit に対して発行するので、canddy の振り分けで別 unit に登録 POST が届くと検証できない。ccmsg は「発行者へ転送」で解いているが (棚卸し)、hyoui では **secret と challenge も `auth.json` の隣の `pending.json` に書く** (10 分で消す)。「CLI が前段の今選ぶ unit を当てて発行する」形は fallback の瞬間に崩れるので採らない。file 共有で 2 unit の区別を消す方が、転送 protocol を持つより hyoui の形に合う

#### 4.7 `X-Frame-Options` / CSP との関係

現行は `X-Frame-Options` も `frame-ancestors` も付けず、test で固定している (棚卸し Part 1-C)。**これは変えない**: 埋め込みが hyoui の用途そのもので、(P) 経路は iframe 内の cookie 送信が前提。clickjacking の面は、cookie が `SameSite=Strict` である限り同一 site (`kawaz.jp` 配下) の親にしか乗らないので、外部サイトの iframe からは 401 になる。`[web].frame_ancestors` を足すのは (R) に倒れた時だけ。

#### 4.8 WebAuthn の実装をどう持つか

| 案 | 評価 |
|---|---|
| (L) `webauthn-rs` crate (Rust の主要実装) を hyoui-web の依存に足す | 検証手順 (§7.1 / §7.2) と COSE / CBOR の解釈を crate に委ね、hyoui 側は challenge 管理と record だけを書く。依存は hyoui-web に閉じる (core の Cargo.toml は不変、DR-0027 §1 の線を守る)。attestation `none` / `residentKey preferred` / `userVerification required` を crate の設定で表せるかは**未確認** |
| (M) ccmsg の自前実装 (TS 550 行) を Rust に移植 | workspace に `ciborium` があるので CBOR は書けるが、署名検証 (ES256 / Ed25519 / RS256) の crate が別途要る。ccmsg で「ライブラリに劣らないテスト」が未達のまま (棚卸し) で、その負債を hyoui が引き継ぐ |

**統括推し: (L)。** reference は「library は要らない」と書くが、その根拠は「attestation の固定や challenge の転送のような制御が効かなくなる」で、hyoui は challenge の転送 (instance 間) をしない (4.6) ので当てはまらない。crate の設定で `none` / `required` が表せなければ (M) に倒す。判断は §7 に出す。

### 2026-09-15: ccmsg から借りるもの / 借りないもの (§5)

| 項目 | 借りる | 借りない | 理由 |
|---|---|---|---|
| 登録の起点を CLI に閉じる、fragment jwt、6 桁コード、試行 5 回で焼く | ○ | | 安全性の根。reference と一致 |
| RP ID = endpoint の host 固定、origin 完全一致、`topOrigin` 拒否 | ○ | | hyoui でも iframe 内 WebAuthn を走らせない (4.3 の (P)) |
| 検証順序 (検証を通してから jti / challenge を消費) | ○ | | 一時失敗で URL が焼けない |
| 登録完了 = 即サインイン | ○ | | |
| access (メモリ + subprotocol) / refresh (cookie) の分離、rotate、再利用検知、tab-share | | ○ | 4.5 (S1)。素の JS に移植する土台が無く、利用者 1 人では価値より実装量が勝る |
| 世代 version、不一致でリロード誘導、自動リロードしない、互換経路なし | ○ | | §3 (a)。ただし gateway は拒否せず browser が判断する点が違う |
| 未知 ErrorCode / 未知 frame を世代不一致の検出に使う | | ○ | hello の `protocol` が 1 次検出で足りる。未知 kind は `error` を返す (§2) が、それを不一致の推定には使わない |
| role だけの認可 | | ○ | 4.6。credential 単位 `rw` / `ro` を daemon mode に写す |
| instance 間の record 複製 (`auth.records` topic) | | ○ | 2 unit は同一ホストで file 共有できる |
| 自前 WebAuthn 実装 | | ○ (条件付き) | 4.8 (L)。crate で表せなければ移植 |
| `Origin` 必須、rate limit 30 req/s、body 上限 64 KiB、失敗理由を URL / コードで分けない | ○ | | 実装判断として妥当、コストが小さい |
| 保守メタ情報 (`device_label` / `registered_at,_ip,_user_agent` / `last_used_*`) | ○ | | `hyoui web passkey list` で見せる。認可には使わない |
| webui からの passkey 一覧 / 削除 | | ○ | ccmsg でも未実装。hyoui は CLI 一本 |

### 2026-09-15: 移行の段取り (§6)

kawaz の運用 (tailnet からの閲覧、ccmsg 経由の Terminal タブ) を止めない順序。DR-0034 の P1〜P5 との前後関係を明示する。

```text
 DR-0034  P1 (/healthz /version) ─┬─► P2 (add/remove/list) ─► P3 (start/stop/status) ─► P4 (旧 CLI 撤去) ─► P5 (canddy 2 upstream)
                                  │
 本設計   W1 契約整理 + 世代 version ◄┘ (P1 の /version に protocol を載せるので P1 の後)
            │
          W2 認証実装 ([web].auth 既定 none、CLI passkey add/list/remove、auth.json)
            │
          W3 kawaz が stable に passkey を登録、config で auth = "passkey" に切替、restart
            │  (P5 完了後なら unstable も同じ auth.json を読むので追加作業なし)
            │
          W4 auth の既定を "passkey" に変更 (= major bump)、canddy の Caddyfile コメント修正を issue 起票
```

- **W1 は認証と独立**で先に出せる。契約の破壊 (エラー形の JSON 化、未知 kind への `error` 応答、hello の追加) を含むので、この時点で `WEB_PROTOCOL_VERSION = 1` を置き、以降の破壊で上げる
- **W2 は既定 `none` で出す**ので、入れた時点では何も変わらない。test は `none` で走る
- **W3 は kawaz の手作業** (`hyoui web passkey add` → URL を iPhone / Mac で開いて登録 → `config.toml` に `auth = "passkey"` と `public_url` → `hyoui web service restart`)。P5 より前に W3 をやる場合、対象は stable 1 unit だけ。P5 の後なら 2 unit が同じ file を読む
- **W4 で既定を変える**のは W3 で実運用を通してから。`none` を残すのは test と dev のためで、常駐 unit で `none` を選ぶには config に明示が要る形にする
- (P) 経路の実機確認 (同一 site iframe の cookie、popup → BroadcastChannel) は **W2 の実装前**に、現行 gateway に仮の cookie を返す test 用ハンドラを立てて確認する。ここが崩れると 4.3 の推しが (R) に変わり、ccmsg-webui の変更が要るようになる

### 2026-09-15: kawaz に決めてほしい点 (§7)

裁定が要るのは以下。それ以外は本文の記述で決めたものとして DR 化に進む。

| # | 論点 | 統括推し | 対案 | 推しの根拠 (要約) |
|---|---|---|---|---|
| Q1 | web 境界の version 方式 | (a) 世代番号 1 つ、build_id は表示のみ | (b) cap 方式 / (c) 併用 | assets を配るのが gateway 自身なので交渉相手は常に同ビルド。stale ページは reload で収束する (§3) |
| Q2 | iframe 内の認証経路 | (R) iframe 内 WebAuthn (`allow="publickey-credentials-get"` + hyoui 側 topOrigin allowlist) | (P) top-level ログイン + cookie / (Q) ccmsg 発行 token | ccmsg 側は属性 1 つ、popup 不要。cookie が iframe 内で乗るかの実機確認は (P) と共通 (§4.3) |
| Q4 | 認証セッション | (S2) reference どおり access + refresh cookie、rotate + 再利用検知 | (S1) httpOnly cookie 1 本 | パターン統一。tab-share は `multi-tab-token-refresh` を素の JS で (§4.5) |
| Q5 | 認可の軸 | credential 単位 `rw` / `ro` を daemon mode に写す (既定 rw) | 認可を持たない (全員 rw) | 「観測だけの端末」を hyoui 既存の mode で表せる (§4.6) |
| Q6 | WebAuthn 実装 | (L) `webauthn-rs` | (M) ccmsg 移植 | 依存は hyoui-web に閉じる。crate 設定で `none` / `required` が表せるかは実装前に確認 (§4.8) |
| Q7 | 認証の既定を `passkey` に変える時期 | W3 (kawaz の実運用) を通してから W4 で major bump | W2 の時点で既定 `passkey` | 常駐 unit が config 無しで上がらなくなる事故を避ける (§6) |

裁定でないもの (本文で決めた): 契約の正本を Rust 型に置き JSON Schema を持たない (§2)、エラー形の JSON 統一と未知 kind への `error` 応答 (§2)、gateway は世代不一致を拒否せず browser が判断する (§3)、`/healthz` `/version` `/assets` `HTML` は無認証のまま (§4.1)、127.0.0.1 直結は `auth = "none"` の gateway でだけ使う、bearer token は初版に無い (§4.4)、record は `auth.json` を 2 unit で file 共有し pending も file に置く (§4.6)、`X-Frame-Options` は付けないまま (§4.7)、path prefix 配置は非対応 (§4.5)。

## 暫定的な結論

- web 境界には version が無く、認証も無い。daemon 境界の cap 方式をそのまま持ち込む理由は無く、**世代番号 1 つ + hello frame + 応答ヘッダ**で「stale なページ」を検出し、帯で reload を促す形が hyoui の事情 (stable / unstable の fallback で版が動く) に合う
- 認証は **passkey (ccmsg 型の登録) + cookie 1 本 (ccmsg 型ではない session)** の組合せ。iframe 問題は「hyoui と ccmsg が同一 site」という配置の事実で解け、ccmsg 側を触らずに済む見込み。**この見込みは実機確認が前提**
- 次の一手は §7 の裁定と、(P) 経路の実機確認。裁定後に契約 DR (仮 DR-0035) と認証 DR (仮 DR-0036) を分けて起票する

## 関連

- `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md` — 本文書の事実の根拠
- DR-0027 (web gateway 同居、§3 endpoint / §5 query)、DR-0034 (§5 `/healthz` `/version`、§7 stable / unstable HA)、DR-0008 (§3 cap 方式、§7 認証は同 UID)、DR-0022 (§3 外側 token 継承)、DR-0013 (§4 attach 復元)、DR-0033 (`leader.request`)
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/auth-patterns/` (`passkey-registration-local-first` / `multi-tab-token-refresh`)
- ccmsg: daemon `docs/decisions/DR-0001-passkey-auth-for-people.md`、`src/auth/webauthn.ts:108-118` (topOrigin 拒否)、protocol `docs/decisions/DR-0017-one-generation-no-compatibility-path.md`、webui `src/terminal-url.ts` / `src/ui/TerminalPanel.tsx`
- canddy-app-proxy `Caddyfile` の `@ccmsg` / `@hyoui` ブロック (本調査で origin 構成を確認)
