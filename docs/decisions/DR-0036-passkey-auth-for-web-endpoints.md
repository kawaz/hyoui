# DR-0036: web endpoint を passkey で守る。gateway は自分の endpoint を知らない

- Status: 🟢 実装済み (2026-09-16)。**W2-1 〜 W2-6 が入り、登録 → 認証 → refresh → 失効の通しを実ブラウザで観測済み** (Chrome + CDP 仮想 authenticator)。残るのは **kawaz が本番 3 endpoint に登録する運用手順** (runbook あり) と **Safari / iOS の gate 3 確認**の 2 点。W3 (ccmsg-webui への `allow` 依頼) / W4 (canddy のコメント修正依頼) は別リポの責務
- Date: 2026-09-16
- Related: DR-0035 (web 契約と世代 version。決定 6 の endpoint 基点相対 URL が本 DR の前提), DR-0027 (認証は当面なし・tailnet 前提という現行前提を本 DR が置き換える), DR-0034 (`/healthz` `/version` は認証境界を変えない、stable / unstable 2 unit と HA endpoint), DR-0013 (attach 復元。`ro` 相当の mode の出どころ), DR-0022 (`POST /input` の auto-lock と `HYOUI_LOCK_TOKEN`。lock token は HTTP 認証ではない), DR-0008 §7 (daemon 境界の認証は同 UID + socket perm。本 DR は触らない)
- Origin: `docs/research/2026-09-15-web-protocol-and-passkey-grand-design.md` (§4 / §5 / §6)、事実は `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md`
- 裁定 (kawaz 2026-09-16): Q2 = (R) iframe 内 WebAuthn (`allow="publickey-credentials-get"`、CSP / frame_ancestors の細部は保留)、Q3 = (A) CLI 発行の招待 URL、Q4 = (S2) access + refresh cookie。**front assets は hyoui の endpoint 自身が配る** (ccmsg にある分離形は採らない)、Q5 = `rw` / `ro` の claim を**定義だけ**して当面 `rw` 固定・session ごとの区別なし、Q6 = (L) `webauthn-rs`、Q7 = **W2 の時点で既定 `passkey`。`auth = "none"` は設けない** (`none` があること自体が事故の元)

## Context

### 現状

hyoui web には認証が一切無い。router に auth middleware も token 検査も無く、到達制限は bind 先 (既定 `127.0.0.1:43690`) と前段 canddy + tailnet が担う設計になっている (DR-0027 は「認証 / HTTPS は scope 外」と明記)。`HYOUI_LOCK_TOKEN` は daemon の lock token を env から継承するだけで HTTP 認証ではない (DR-0022)。

到達経路は 3 本ある (DR-0034 決定 8 の 3 endpoint 構成を前提とする):

```text
 [1] tailnet の人
       https://hyoui-stable.<host>.kawaz.jp   ─► canddy ─► 127.0.0.1:43690 (stable)     … 個別 endpoint
       https://hyoui-unstable.<host>.kawaz.jp ─► canddy ─► 127.0.0.1:43691 (unstable)   … 個別 endpoint
       https://hyoui.<host>.kawaz.jp          ─► canddy ─► 43691 → 43690 (lb_policy first) … HA endpoint

 [2] ccmsg webui (https://ccmsg.<host>.kawaz.jp) の Terminal タブ
       └─ <iframe src="<terminal_gateway>/sessions/<id>?embed=1&resize=1">   ← 認証情報は一切渡らない

 [3] 同一ホストの curl / test / `hyoui web daemon status` ─► http://127.0.0.1:4369x 直結
```

ここで効く事実が 3 つある。

- **[1] と [2] は gateway から見ると全部 127.0.0.1 発である。** canddy が reverse proxy なので、tailnet からの接続も iframe からの接続も accept 時点では loopback から来る。「loopback は認証免除」を採ると canddy 経由も免除になり、認証を入れた意味が消える
- **1 つの gateway プロセスは複数 endpoint から到達される。** stable の unit は `hyoui-stable.<host>` と `hyoui.<host>` (HA が stable に落ちた時) の 2 つから、unstable も同様に 2 つから。さらに path 付き (`https://example.jp/hyoui`) の endpoint を前段で切ってもよい
- **ccmsg webui と hyoui の各 endpoint は別 origin だが同一 site** (どれも `kawaz.jp` 配下)

### 目的

**hyoui web の endpoint に到達した相手が「登録済みの端末を持つ本人」であることを、gateway 自身が判定できるようにする。** 前段の proxy にも外部 IdP にも認証を寄せない。守る対象は session の画面内容と入力である。

**同時に、gateway に endpoint の知識を持たせない。** どんな endpoint を切るか (host / path の構成、HA の優先順) は前段 (canddy) と利用者の責務であり、gateway は port を listen するだけである。endpoint URL を知るのは **CLI (`--endpoint` で受け取る) とブラウザ (`location` から計算する)** の 2 者だけで、gateway はその値を受け取って record を引く。

ここで増やしたくないもの (目的と同格):

- **gateway が持つ設定**。endpoint リストも RP ID の指定口も origin の allowlist も config に置かない。config に endpoint を書く形にすると、前段の構成を変えるたびに gateway の config も直す運用になり、「gateway はマウント先を知らない」が崩れる
- **認証の経路の数**。passkey 1 本にする。無認証 mode も script 向け bearer token も持たない (決定 8 / 決定 9)
- **gateway 間で同期する状態**。2 unit は同一ホストなので file を正本にして各 unit が読むだけにする。instance 間の複製 protocol と「発行者への転送」は持たない (決定 4)
- **認可の軸**。`rw` / `ro` の claim を定義するが実装は当面持たない (決定 7)

### 前提条件と、満たさない場合

| 前提 | 満たさない場合 |
|---|---|
| DR-0035 決定 6 が済んでおり、ブラウザが `location` から自分の endpoint を計算できる | endpoint を要求に載せられず、gateway が record を引けない。endpoint を config に持つ形に倒す判断が必要になる (= 目的の後段に反する) |
| endpoint が https である (前段が TLS 終端する) | refresh cookie に `Secure` が付くので保存されない。リロードごとに passkey を求められる。**実測済** (2026-09-16、gate 3 (d)): `http://<LAN IP>:<port>` は `isSecureContext: false` で `Secure` cookie が保存されず、**cookie 以前に WebAuthn 自体が走らない**。`http://localhost` は secure context 扱いなので `Secure` cookie が保存される (= test と手元の確認はこれで足りる) |
| stable / unstable の 2 unit が同一ホストで同じ state dir を読める (DR-0034 決定 2) | HA endpoint の credential と session が unit 間で引き継げず、fallback のたびに再認証になる |
| 前段が `Sec-WebSocket-Protocol` を透過する (DR-0027) | WS への access token 提示経路が無くなる |
| 利用者が 1 人 (kawaz) で、端末ごとに credential が分かれる | `sub` の採番規則と「認可を持たない」判断 (決定 7) の前提が変わる |

## 介入判断 self-check (CLAUDE.md / DR-0014)

- **PTY / child / signal / daemon protocol への介入は無い。** 足すのは gateway の HTTP 層の middleware と `/auth/*` route で、daemon 境界 (CBOR / cap flags / socket perm 0600 / lock token) は 1 文字も変わらない
- **透過原則は変わらない。** 認証を通った後の attach は現行と同じ覗き窓 (DR-0029) であり、bytes の転写に手を入れない
- **新 cap flag も新 daemon message も足さない**
- **kernel / OS 機能の再発明ではない。** WebAuthn の検証は `webauthn-rs` に委ね (決定 8)、writer の直列化は file の `flock` に委ねる (決定 4)。challenge の在庫と record の管理だけを自分で書く
- **既存 DR の実装漏れを先に見た。** DR-0027 が「token auth は将来 DR」と書いた枠がまさに本 DR である。DR-0034 §5 の `/healthz` `/version` は実装済で、その「認証境界を変えない」を本 DR が守る (決定 1)
- **最小介入か。** 認証を足す以外の介入 (前段への forward auth、iframe への postMessage 経路、ccmsg を IdP にする結合) はいずれも不採用にした (Alternatives)

## Decision

### 1. 守る範囲と、守らない範囲

**守る**: `/api/*` と WS attach への到達。つまり session の画面内容と入力である。

**守らない**:

| 対象 | 理由 |
|---|---|
| `GET /healthz`、`GET /version` | DR-0034 §5 の「認証境界を変えない」を維持する。可用性監視と `hyoui web daemon status` の入口 |
| `GET /assets/*` | ログイン UI 自体がこれを使う。front assets は **hyoui の endpoint 自身が配る** (裁定 Q4) |
| `GET /` と `GET /sessions/{id}` の HTML | 静的 shell であり session の内容を含まない。server が `id` を見ない性質 (DR-0027 §5) もそのまま残る |
| `/auth/*` | 認証そのものの経路 |

**認証は gateway 自身が判定する。** 前段 forward auth にしない (Alternatives)。canddy は今後も透過で、`Caddyfile` のコメント「gateway 側は認証省略で OK」は移行完了時に書き換えを依頼する (決定 10)。

HTML を無認証で配る帰結として、**ログイン状態の判定は JS が `/api/*` の 401 を受けて行い、ページ内に overlay でログイン UI を出す。** ログイン専用ページへの redirect はしない — iframe 内で redirect が起きると、親 (ccmsg) の Terminal タブがログインページに化けて、何が起きたか読めなくなる。

### 2. 登録は CLI 発行の招待 URL に閉じる

```text
hyoui web passkey add --endpoint <url> [--label <名前>]
hyoui web passkey list
hyoui web passkey remove <sub>
hyoui web session list
hyoui web session remove <id>
```

`add` が出すのは **招待 URL `<endpoint>#register=<jwt>` と 6 桁コード**の 2 つ (`<endpoint>` は正規形なので末尾 `/` を含む、決定 3)。reference `passkey-registration-local-first` の規定そのままで、hyoui 固有の差分は決定 4 (file 共有) だけである。

| 項目 | 形 |
|---|---|
| jwt の claims | `{sub, endpoint (正規形), rp_id (= endpoint の hostname), user_id, access, exp (10 分), jti}`。reference の `iss` (発行 instance の id) は**持たない** — 発行するのは CLI で、検証するのはどの unit でもよいので、指す対象が無い |
| jwt の署名 | **登録 1 本ごとの乱数 32 byte secret による HMAC (HS256)**。永続鍵を持たない。secret は CLI が `pending.json` に書き、gateway は読むだけ (決定 4) |
| jwt の運び方 | **fragment (`#`)**。server にも proxy log にも Referer にも乗らない |
| 6 桁コード | URL には含めず CLI にだけ表示する。登録要求の必須引数。**5 回の誤入力でその URL (jti) を焼く** |
| ブラウザ側 `create()` | `residentKey: "required"`、`userVerification: "required"`、`attestation: "none"`、`user.id` = claims の `user_id`、`rp.id` = claims の `rp_id`。**`required` を選ぶのは gate 4 の実測による** — `webauthn-rs` は `Required` / `Discouraged` しか emit できず `preferred` の口が無い。実ブラウザでは preferred ≡ required (crate が proto の doc で明言) なので狙いは変わらず、対象端末 (Mac / iPhone) はどちらも resident key 対応で拒まれない |
| 検証順序 | client data → WebAuthn 登録検証 → 公開鍵の import 可否 → **通ってから jti と challenge を消費**。逆順だと一時的な失敗 1 回で URL が焼ける。6 桁コードだけは**外した時点で試行回数を加算する** (= それが総当たりの上限そのもの) ので、lock 下で数えてから WebAuthn 検証に進む |
| `/auth/challenge` の形 | `{endpoint, purpose: "assert" \| "register", jwt?}`。**登録用 challenge には jwt が要る** — `create()` の options は `user.id` と `rp.id` を claims から採るので、challenge を組む時点で claims が必要になる。ここでは 6 桁コードを見ず、**jti も challenge も消費しない** (消費は `/auth/register` が検証を通してから)。発行した登録用 challenge は jti に束縛し、別の登録 URL 向けの challenge を使い回させない |
| crate の検証途中 state | `RegistrationState` / `AuthenticationState` を `pending.json` の challenge に載せる。**プロセスのメモリには置けない** — challenge を出した unit と消費する unit が違いうる (決定 4) |
| `userHandle` の扱い | `residentKey: "required"` なので assertion に handle が**常に載る**。**record の `user_id` と毎回照合する** (reference `passkey-registration-local-first` §7.2 の 3 をそのまま適用。条件分岐を持たない)。**`webauthn-rs` は非 discoverable (allowCredentials) 経路で userHandle を検証しない**ことを gate 4 で実測したので、照合は hyoui 側で実装する |
| 登録完了時 | そのまま session を mint して返す (登録が即サインイン) |

**attestation は `none` で足りる。** 「この credential を作ってよい人か」は jwt と 6 桁コードが既に担保しており、authenticator の出自証明は要件に無い。要求すると証明書チェーンの検証と信頼リストという管理対象が増える。

**リモートからの登録経路も復旧経路も持たない。** 登録がホスト上の CLI に閉じることがこの設計の安全性の根である。

**`passkey add` は gateway に要求を送らない。CLI が `pending.json` に直接書き、gateway は読むだけである。** 書くのは jwt の HMAC secret と、その jti / endpoint / claims / 6 桁コードのハッシュ / 試行回数 / `exp`。CLI は同じホスト上で同じ uid で走り、state dir に書ける (= 到達が権限である。ccmsg が UDS の管理フレームで表していることを、hyoui では file の所有権で表す)。

この形にする理由が 2 つある。**gateway に管理用の経路を足さずに済む** — 登録 URL の発行に gateway の生存が要らず、`hyoui web daemon` が全て停止していても `passkey add` が打てる。もう 1 つは **secret がプロセスのメモリに無いので、どの unit が POST を受けても検証できる** (決定 4 の HA 対応がこれで成立する)。ccmsg は secret を発行 instance のメモリに置き「発行者へ転送」で解いているが、hyoui は file を正本にすることでその転送を要らなくした。

**したがって「発行した unit の再起動で登録が失敗する」制約は hyoui には無い。** 登録 URL が失効するのは `exp` (10 分) と 6 桁コードの 5 回失敗だけで、その時の文言は「登録 URL を再発行してください」にする。

`sub` の既定は `<endpoint の host>-<連番>` とし、**unit に依存させない** (連番は `auth.json` の当該 endpoint の record から採る。tombstone 済みの名前はスキップする)。credential は endpoint に束縛される (決定 3) ので、`sub` の名前空間も endpoint 側に置くのが揃う。ccmsg は `<unit>-<連番>` だが、hyoui の unit (stable / unstable) は endpoint と 1:1 でないので、unit 名を入れると HA endpoint の登録が「どちらの unit で発行したか」に依存して見える。

**reference の任意ゲート (b) (ホスト PC の生体認証による承認) は本 DR に入れない。** 6 桁コードで「URL が漏れただけでは登録にならない」は満たしており、離席中の第三者を防ぐ層は独立に積める (reference がそう設計している)。必要になった時に足す。

### 3. gateway は endpoint を知らない。RP は record の endpoint から決まる

**passkey は endpoint ごとに個別登録する。** §Context の 3 endpoint はそれぞれ別の RP ID を持ち、credential は登録した endpoint でだけ使える。同じ host の `/` と `/hyoui/` も別 endpoint = 別登録である。

**endpoint は DR-0035 決定 6 の正規形 (`scheme://host[:port]/<path>/`、末尾 `/` 必須、query / fragment なし) で扱う。** record の key、jwt の claims、要求の body、challenge に埋める値のすべてが正規形の文字列であり、**比較は正規化した文字列の完全一致で行う** (URL として解釈し直して比較する経路を持たない)。`hyoui web passkey add --endpoint` は受け取った値を正規化してから jwt と `pending.json` に書き、正規化できない値は拒否する。ブラウザ側の 2 つの計算は正規形をそのまま返す。

正規形を固定しないと、**`--endpoint https://hyoui.<host>` (スラッシュ無し) で登録した record を、ブラウザが送る `https://hyoui.<host>/` では引けない**。key が 1 文字違うだけで登録済みの credential が使えなくなり、原因は認証失敗としてしか見えない。

- **ブラウザが endpoint URL を計算して要求に載せる。** DR-0035 決定 6 と同じ計算 (index は `new URL(".", location.href)`、session ページは `location.href` から `sessions/<id>` と query を落としたもの) で、`/auth/challenge` `/auth/assert` `/auth/refresh` の body に `endpoint` として送る
- **認証の検証**: 要求の `endpoint` で record 集合を絞り、assertion の `rawId` で record を引く。record の `endpoint` から `rp_id` (= hostname) と origin を取り、`authData.rpIdHash` == `sha256(rp_id)` と `clientDataJSON.origin` == record の endpoint の origin (完全一致) を照合する
- **ブラウザが名乗った `endpoint` を信じているわけではない。** 本体の検証は `clientDataJSON.origin` と `rpIdHash` で、これはブラウザが WebAuthn の仕様として書く値である。要求の `endpoint` は record を引く索引にすぎない (違えば引けないだけ)
- **例外は、同一 origin の `/` と `/hyoui/` を同じ gateway が serve する場合である。** `clientDataJSON.origin` に path は含まれないので、この 2 つを区別するのはブラウザ申告の `endpoint` と cookie の `Path` だけになり、**同一 origin 内の path 分離は認可境界にならない** (決定 5 の `Path` と同じ理由)。別の gateway が serve するなら `auth.json` が別 file なので実害は無い。同一 origin に信頼の異なるものを並べるなら、分けるべきは path ではなく host である
- **challenge にも endpoint を埋める。** `/auth/challenge` の応答に `endpoint` を載せ、assert / register の検証で record の endpoint と一致することを見る。これで「challenge を取った endpoint」と「使う endpoint」のすり替えが効かない
- **HTTP の `Host` / `Origin` ヘッダで RP を選ばない。** 前段が何を書くかは gateway の知識の外で、origin の真正性は `clientDataJSON` が担保する。`/auth/*` に `Origin` ヘッダを要求する ccmsg の形も採らない (front assets が endpoint と同一 origin から配られるので CORS 自体が要らない)
- **registrable suffix を許さない。** suffix を名乗れると、その配下の全ホストでその credential が使えることになる
- **HA endpoint に登録した credential は、裏の unit がどちらでも通る。** record は unit を跨いで同じ file から引け (決定 4)、検証に使うのは record の endpoint だけで、どの unit が受けたかは関係しない。個別 endpoint の credential は HA endpoint では使えない (origin が違う)。**HA endpoint の登録 1 本があれば日常の閲覧は足り、個別 endpoint の登録は unstable を狙って開く時 (ドッグフーディング) にだけ要る**

`sub` は人が付けたラベル (`--label`、既定は `<unit 名>-<連番>`)、**user handle は sub ごとに 1 度だけ決めた 16 byte 乱数**を使い回す (同じ人に 2 つの handle を配ると端末上で 2 つのアカウントに見える)。同じ sub への追加登録では既存の handle を再利用する。

保守メタ情報 (`issued_label` / `device_label` / `registered_at,_ip,_user_agent` / `last_used_*` / BE / BS) は record に持ち `hyoui web passkey list` で見せるが、**認証・認可の判定には使わない**。これは記憶の手がかりであって認証の材料ではない (reference)。BE / BS は登録時と認証時の両方で記録する。

### 4. record と pending は endpoint を key にした file。2 unit が `flock` で共有する

置き場は **`$XDG_STATE_HOME/hyoui-web/auth.json`** (mode 0600、tmp + rename、書き込みは `flock`) と、その隣の **`pending.json`**。

root を `hyoui-web/` にするのは DR-0034 決定 2 と同じ理由で、`$XDG_STATE_HOME/hyoui/` は session discovery の走査 base だからである。DR-0034 が作った `hyoui-web/` (`units/` / `logs/` / `supervisor.sock`) にこの 2 file を並べる。

| file | 中身 | key |
|---|---|---|
| `auth.json` | credential record、token family | `credential/<endpoint>/<sub>/<credential_id>`、`family/<endpoint>/<id>` |
| `pending.json` | 登録 jwt の HMAC secret、challenge の在庫 | `<endpoint>` と `jti` (10 分で消す) |

**endpoint を「引く単位」そのものにする** (ccmsg は record の属性として持つが、hyoui では key の 1 段にする)。stable の unit に unstable 個別 endpoint の credential が提示されることは前段の構成上起きないが、起きても record の endpoint との照合 (決定 3) で落ちるだけで、unit 側に「自分の endpoint」の知識は要らない。

**`pending.json` に secret と challenge を書くのは、HA endpoint 宛の登録で発行 unit と受信 unit が違いうるため。** CLI が発行した URL の POST を受けるのは canddy の振り分け次第で、fallback の瞬間にどちらに落ちるかは決められない。ccmsg は「発行者へ転送」で解いているが、hyoui では file 共有で unit の区別を消す。個別 endpoint 宛なら受ける unit は 1 つに決まるが、経路を分けずに同じ file に書く。

#### lock は別 file に取る

**`flock` は `auth.json.lock` / `pending.json.lock` に取る。data file 自身には取らない。** data file は tmp + rename で差し替えるので、rename の瞬間に inode が入れ替わる。data file に lock を取る形だと、後から来た writer が**既に置き換えられた古い inode** の lock を掴んで「自分だけが書いている」と信じ、read-modify-write が衝突して lost update になる (rename は lock の状態を引き継がない)。lock 対象を差し替えられない別 file に固定すれば、lock の identity が writer 全員で一致する。

lock の下で行う read-modify-write は次のもので、いずれも「lock → 読む → 変更 → tmp に書く → rename → unlock」を 1 単位にする:

| 操作 | 対象 file |
|---|---|
| token family の rotate、失効 (tombstone)、credential の登録 / 削除 | `auth.json` |
| **challenge の消費** (発行した challenge を使用済みにする) | `pending.json` |
| **6 桁コードの試行回数の加算**と 5 回到達での jti の焼き切り | `pending.json` |
| 登録 URL の発行 (CLI が secret と claims を書く) | `pending.json` |

challenge の消費と試行回数の加算を lock の外で行うと、**2 unit に同時に来た要求が同じ challenge を 2 回消費でき、試行回数も数え落とす** (= 総当たりの回数上限が unit の数だけ緩む)。回数は 1 箇所で数えるのが要件なので (reference)、file が唯一の計数場所になる。

credential は mtime で cache してよいが、**family の検証は cache を使わず毎回 file を読む** — 書き換わる頻度が高く、rotate の直後に他 unit が古い cache で判定すると再利用検知が誤発火する。

**instance 間の複製 protocol (ccmsg の `auth.records` topic) は持たない。** 2 unit は同一ホストで同じ file を読める。DR-0034 の「gateway 間の状態共有をしない」は「gateway 自身が状態を持たない」の意味で、file を正本にして各 unit が読むだけの形はこれに反しない (DR-0006 §1 の socket dir が正本なのと同じ形)。

#### 失効はいつ効くか

失効は tombstone で表す (削除ではなく)。`passkey remove <sub>` はその sub の credential 全部と family 全部を、`session remove <id>` は family 1 本を tombstone にする。

**CLI は gateway に通知しない** (決定 2 と同じく、CLI が書いて gateway が読む)。したがって失効が確立済みの WS に効く時点は、**その接続の refresh 延長 (`auth.extend`) で unit が family を読み直し、tombstone を見て切る**時である。

**最長の猶予は access token の TTL (4 時間) になる。** 延長は残り寿命の 90% 時点で走るので実際にはそれより早いが、上限としてはこれが正しい値である。「今すぐ全部切る」が要る場面は `hyoui web daemon restart` が答える (WS は unit の再起動で必ず切れる)。

**猶予を無くすために gateway へ通知経路を足さない。** 足すと決定 2 で消した「CLI → unit の管理経路」が戻り、CLI が gateway の生存に依存する。失効の目的は「盗まれた credential で以後入れないこと」で、それは次の認証と次の refresh で満たされる。確立済みの 1 接続が最長 4 時間生き延びることを許容できない要件は今は無く、必要になったら `restart` で足りる。

### 5. 認証セッションは access + refresh の 2 段。reference どおり

| 値 | 置き場 | 属性 / 寿命 | 提示方法 |
|---|---|---|---|
| access token | **ブラウザのメモリのみ** (署名しない opaque 乱数、**base64url で表す**)。localStorage には置かない | 4 時間 | HTTP は `Authorization: Bearer`、WS は subprotocol `hyoui.token.<値>` (server は選んだ subprotocol を echo) |
| refresh token | **httpOnly cookie** | `HttpOnly; Secure; SameSite=Strict`、名前 `__Secure-hyoui-<sha256(endpoint) 先頭 16 hex>`、**`Path` = 正規形 endpoint の path から末尾 `/` を落とした値** (root の endpoint は `/`、`https://example.jp/hyoui/` なら `Path=/hyoui`)、7 日 | cookie のみ (body に token を載せない) |

**access を base64url で表すのは、WS subprotocol に載せるためである。** `Sec-WebSocket-Protocol` の値は RFC 6455 の token (RFC 7230 の `token` 文字) でなければならず、`+` `/` `=` を含む素の base64 は使えない。

token は署名せず、**token family の record を lookup して検証する**。`/auth/refresh` は cookie の値で family を引き、**その family の `endpoint` が要求 body の `endpoint` と一致することを確認する** (決定 3 の照合と同型。cookie が endpoint ごとに分かれていても、確認は値の側で行う)。署名鍵を持つと保管・rotate・配布という管理対象が増えるが、record を引く形なら鍵なしで同じことが済む。

**cookie 名に `sub` を混ぜない。** reference と ccmsg は `sha256(発行者 id + "\n" + sub)` を使うが、`/auth/refresh` を受けた時点で server は **まだ誰の要求か知らない** (refresh token 自体が身元を答える値である)。ccmsg はこれを「`__Secure-ccmsg-` prefix の全 cookie を試す」ことで解いているが、hyoui は名前を **endpoint のハッシュだけ**にして 1 つに決める。endpoint ごとに別 cookie になる要件 (下記) はこれで満たされ、試行の必要が消える。

代償は **同一 endpoint に複数の `sub` が同時にログインできない**ことで、cookie が後の登録で上書きされる。利用者が 1 人 (前提条件表) で `sub` が端末ごとに分かれる運用では、同じブラウザに 2 つの `sub` が並ぶ場面が無い。必要になった時は名前に `sub` を戻し、prefix の全 cookie を試す形に変える (record の形は変わらない)。

**`__Host-` ではなく `__Secure-` + `Path` を選ぶ。** 同一 host の `/` と `/hyoui/` を別 endpoint (別登録) として扱うには cookie を `Path` で分ける必要があり、`__Host-` は `Path=/` を強制する。

**`Path` は認可境界ではない。** 同一 origin の JS は任意の path に fetch でき、cookie の `Path` は「ブラウザが自発的に付けて送る範囲」しか決めない。したがって `https://example.jp/` の endpoint と `https://example.jp/hyoui/` の endpoint を**別の信頼境界として扱うことはできない** — 前者のページで走るスクリプトは後者の `/auth/refresh` を叩けるし、その時 cookie も送られる。この分離は帯域と露出面の絞り込みに留まる。同一 host に信頼の異なるものを並べるなら、分けるべきは path ではなく host (eTLD+1) である。

**rotate と再利用検知**: refresh は使うたび rotate する。family は退役した値のダイジェストを本来の exp まで保持し、**どの世代の値でも再提示を見たら family ごと失効**させ、その sub の WS を切る。直前 1 世代だけは 60 秒の再送猶予として前回の答えを返す (rotate しない)。

**access は据え置く**: 残り寿命が TTL の半分を切るまで同じ値を返す。これが複数タブで 1 本の access を共有する土台になる (reference `multi-tab-token-refresh` のサーバ側手順)。

**長命 WS は切らずに延ばす。** `hello` frame の `auth_expires_at` (DR-0035 決定 1 の表に収録済み) に access の期限を載せ、ブラウザは残り寿命の 90% 時点で `/auth/refresh` を打ち、得た access を **同一接続上の `auth.extend` で提示して期限を延ばす** (応答は `auth.extend.result`)。`exp` で必ず切ると画面が周期的に瞬く。切るのは延長を怠った接続だけである。

この `auth.extend` の処理が、失効を確立済み接続に反映する唯一の点でもある (決定 4 の「失効はいつ効くか」)。unit は family を file から読み直し、tombstone を見たら `ok:false` を返して接続を切る。

**tab-share は `multi-tab-token-refresh` を素の JS で書く。** `navigator.locks.request("hyoui.auth.refresh:<endpoint>:<sub>")` の中でだけ refresh し、得た access を `BroadcastChannel("hyoui.auth:<endpoint>:<sub>")` でメモリからメモリへ配る。ロックを取った側は先に `{kind:"ask"}` を投げて 50ms 待ち、誰かが期限内の access を持っていれば refresh しない。sub が分かる前は endpoint だけの key で待ち、確定後に張り替える。Web Locks が無い環境では各タブが自分で refresh する (収束はサーバ側の据え置きが担う)。reference が固定を要求する 7 性質をそのまま test にする。

**front assets は hyoui の endpoint 自身が配る** (裁定 Q4)。ccmsg にある「webui を別サブドメインに置く」分離形は採らない。endpoint と同一 origin から配られるので CORS が要らず、`clientDataJSON.origin` の期待値が endpoint の origin 1 つに決まる。

ccmsg から借りる実装判断 (コストが小さく、根拠が実装事実として確認できているもの): `/auth/*` 4 経路で共有する rate limit 30 req/s、body 上限 64 KiB、challenge / token / 6 桁コードの**タイミング安全比較**、credential id の**バイト比較**での lookup (base64url が正規形でない)、公開鍵を**登録時に import して検証**する (使えない鍵の record を残さない)、期限切れの掃除を timer でなく**読み取り時**に行う、攻撃者入力由来の例外を一律「認証失敗」に翻訳して 500 にしない、**失敗理由を URL とコードで分けない**。

### 6. iframe 内では WebAuthn をそのまま走らせる

ccmsg の検証コードは `clientDataJSON.topOrigin` が存在するだけで拒否するが (findings)、それは **ccmsg 自身が iframe に入らない判断であって、埋め込まれる側の hyoui の RP には当てはまらない**。

- **ccmsg-webui 側の変更は iframe の `allow="publickey-credentials-get"` 1 属性**。これは ccmsg-webui リポへ issue で依頼する範囲で、本 DR は依頼することだけを決める (実装は別リポの責務)
- **登録 (`create()`) は iframe で走らせない。** 招待 URL を top-level で開く経路 (決定 2) なので、iframe に要るのは `get` だけである。`create` の iframe 対応はブラウザ差が大きい (未検証) が、その差を踏む必要が無い
- **hyoui 側は `topOrigin` が present でも拒否しない。** 検証するのは `rpIdHash` と `clientDataJSON.origin` (= record の endpoint の origin) で、そこが一致していれば「この endpoint のページで get が走った」ことは満たされている
- **`crossOrigin: true` の拒否は登録経路 (top-level の `create()`) だけに適用し、認証経路では見ない。** gate 3 の実測で、**cross-origin iframe の `get()` では Chrome が `crossOrigin: true` を送る**ことが分かった (`topOrigin` に親 origin が入るのと対で来る)。認証経路でこれを拒否すると、本決定が通そうとしている経路 [2] が必ず落ちる。reference が「`true` のときだけ拒否」と書くのは **iframe 内の認証を一切許さない設計** (`topOrigin` present で拒否) の文脈であって、`topOrigin` の規定を覆した本 DR では認証経路に持ち込めない。登録側で拒否する意味は残る (= iframe からの登録を構造的に封じる層)。なお `present` であることを要求してはならない点は変わらない — Chrome 系は最上位フレームでも常に `false` を送る (reference)
- **topOrigin の allowlist と CSP `frame-ancestors` は保留** (裁定 Q2: 「CSP 云々は置いておく」)。現行は `X-Frame-Options` も `frame-ancestors` も付けず、test で固定している (findings Part 1-C)。本 DR はその状態を変えない。埋め込み元を絞る必要が出た時に別 DR で決める

**iframe 内の RP は、ccmsg が埋め込む URL の endpoint で決まる。** ccmsg daemon config の `terminal_gateway` が `https://hyoui.<host>` (HA) なら HA endpoint の credential が、`https://hyoui-unstable.<host>` なら unstable 個別 endpoint の credential が iframe 内で使われる。ccmsg 側は URL を差し替えるだけで、hyoui 側の設定は変わらない (決定 3 の帰結)。

**この経路の成立は実装前に実機で確認する** (Implementation phases の gate 3)。崩れた場合の退路は「top-level で一度ログインして cookie を持たせる」形 (Alternatives の (P)) で、ccmsg の sandbox に `allow-popups` は既にある。

### 7. 認可は `rw` / `ro` の claim を定義するだけ。当面 `rw` 固定

credential の claim に `access = "rw" | "ro"` を**定義する**。record と登録 jwt に field として持ち、`passkey list` に表示する。

**当面は全ての登録が `rw` で、gateway は `access` を読んで分岐しない。session ごとの区別も持たない** (裁定 Q5)。`--ro` のような CLI option も本 DR では出さない。

**拡張点**: `ro` を実装する時は、gateway が `ro` の session からの `input` / `resize` / `leader.request` / WS の binary 上りを落とし、WS attach を daemon に `Ro` mode で張る (hyoui は daemon 側に既に `ro` / `rw` / `rw-no-leader` の mode を持つ)。その設計は必要になった時に行う。

`rw-no-leader` は web の認可軸に出さない — leader は取り合いの結果であって権限ではない。

**claim を今定義するのは、後から足すと record の形が変わるため。** field を定義しておけば、既存 record を書き換えずに実装だけを足せる。逆に「今は無いので後で足す」にすると、既に登録された credential に `access` が無い状態を扱う分岐が要る。

### 8. WebAuthn の検証は `webauthn-rs` に委ねる

依存は `crates/hyoui-web/Cargo.toml` に閉じる (core の `Cargo.toml` は不変、DR-0027 §1 の線を守る)。

**版は `webauthn-rs = "=0.6.1-dev"` の exact pin。** 0.5 系 (安定版) を採らないのは、**`webauthn-rs-core` 0.5.5 が `openssl` / `openssl-sys` に依存し、このリポの `cargo clippy --target x86_64-unknown-linux-gnu` が落ちる**ためである (実測 2026-09-16: `openssl-sys` の build script が `Could not find openssl via pkg-config` で失敗)。cross の check は「macOS だけで通る実装」を防ぐために置いてあり、認証はそれを外してよい範囲ではない。0.6 系は暗号を `crypto-glue` (pure Rust、`p256`) に移しており、native / linux target / `cargo +1.98.0` の 3 系統すべてが通る。

`-dev` は prerelease だが、**crates.io 上のその版は不変で `Cargo.lock` が固定する**ので「勝手に変わる」性質は無い。残る代償は「その版に patch が来ない」「0.6.2 で API が変わりうる」の 2 点で、v1.0 未満の本リポが breaking change を許容する方針と釣り合う。

**この依存は RUSTSEC-2023-0071 (rsa 0.9 の Marvin attack) を連れてくる。** 経路は `webauthn-rs-core` → `crypto-glue` → `rsa` で、**`crypto-glue` は `rsa` を無条件に依存する** (その `Cargo.toml` に `optional` 指定は 1 つも無く、`crypto-glue` / `webauthn-rs-core` のどちらにも RSA を落とす feature が無い、実測 2026-09-16)。したがって「feature で外す」は選べない。

**踏まない理由は、hyoui が RSA 秘密鍵を持たないことである。** advisory の脆弱な経路は秘密鍵演算の timing sidechannel であり、hyoui が RSA を使うのは authenticator が RS256 で作った credential の**署名検証** (= 公開鍵演算) だけである。登録時に許す algorithm から RS256 を外しても依存は消えない (コンパイル時に入る) ので、対処として意味を持たない — 外すと RS256 しか作れない authenticator を拒むだけで、安全性は変わらない。

そこで **advisory を ignore する。** 置き場は `deny.toml` の `[advisories] ignore` (cargo-deny) と `.cargo/audit.toml` (cargo-audit) の 2 つで、どちらにも上の理由を書いた。**workflow の `ignore` 入力は使わない** — `rustsec/audit-check` は repo root で `cargo audit` を呼ぶだけで config を無効化しないので (action が渡すのは `--json` と `--file` だけ、実測)、`.cargo/audit.toml` が local と CI の両方に効く。workflow にも id を書くと一覧が 2 つになり、片方だけ直す事故が生まれる。上流が `rsa` を差し替えるか 0.10 が出たら ignore を外す。

`passkey-auth` (pure Rust、`residentKey: preferred` を含め決定 2 の 3 設定すべてを表せる) も評価したが採らない。**2,536 行に test 27 個という比率は、本 DR が ccmsg の自前実装を却下した理由と同じ状態**であり、library を選ぶ動機 (= 検証手順を実績のあるコードに委ねる) を満たさない。`webauthn-rs` は 9,270 行に test 50 個で、実機 authenticator の fixture を持つ。

reference は「library は要らない」と書くが、その根拠は「attestation の固定や challenge の転送のような制御が効かなくなる」で、**hyoui は challenge の転送 (instance 間) をしない** (決定 4 が file 共有で解く) ので当てはまらない。

**gate 4 の実測 (2026-09-16)。** 裁定 Q6 の「仕様上都合が悪い部分が出たら自作を検討」に対して、**自作には倒さない**と判断した根拠がこの表である。

| 決定 2 の要求 | 実測 | 扱い |
|---|---|---|
| `attestation: "none"` | 表せる (`AttestationConveyancePreference::None`) | そのまま使う |
| `userVerification: "required"` | 表せる (`UserVerificationPolicy::Required`、認証側も同じ) | そのまま使う |
| `residentKey: "preferred"` | **表せない。** challenge を組む builder が受けるのは `require_resident_key: bool` だけで、出るのは `Required` / `Discouraged` の 2 値 | **`required` に変更** (決定 2)。実ブラウザでは preferred ≡ required |
| `crossOrigin` の検査 | **登録経路にだけある** (`register_credential_internal`)。認証経路 (`verify_credential_internal`) には無い。`allow_cross_origin` は `false` 固定で setter が無い | 決定 6 の「登録のみ拒否」とそのまま一致するので、hyoui 側で足すものは無い |
| `userHandle` の検証 | **しない** (非 discoverable 経路に検証コードが無い) | hyoui 側で毎回照合する (決定 2) |
| 仮想 authenticator の往復 | softtoken (`webauthn-authenticator-rs`) で登録 → 認証が通る。**ただし resident key は非対応** (`SoftToken` / `SoftPasskey` のどちらも `if resident_key { return Err(NotSupported) }`) | **検証の分担がここで決まる**: crate の配線 (challenge 生成 → 署名 → `register_credential` → 認証 challenge → `authenticate_credential` → `userHandle` 照合) は Rust の test が `residentKey` だけ下げて通し、**`residentKey: required` そのものは実ブラウザ (Chrome の CDP 仮想 authenticator) で見る**。非 resident の credential は assertion に `userHandle` を載せないので、その事実自体も test で固定する (= 決定 2 で `required` が要る理由) |

表せなかったのは `residentKey` 1 点で、しかも実挙動が代替値と同じなので、library を捨てる理由には足りない。`webauthn-rs` に委ねるのは検証手順と COSE / CBOR の解釈で、hyoui 側が書くのは challenge の在庫管理・record の lookup・endpoint との照合 (決定 3)・userHandle の照合である。

`webauthn-rs` に委ねるのは検証手順 (WebAuthn L2 §7.1 / §7.2) と COSE / CBOR の解釈で、hyoui 側が書くのは challenge の在庫管理・record の lookup・endpoint との照合 (決定 3) である。

### 9. 既定は `passkey`。無認証 mode を設けない

**`[web].auth` のような config 項目を持たない。** 認証は常に有効である (裁定 Q7: 「`none` があること自体が事故の元」)。

- **test は登録 fixture で通す。** record を `auth.json` に直接置き、family も直接書いて access token を提示する形で `/api/*` の test を回す。WebAuthn の署名経路自体の test は仮想 authenticator で別に持つ
- **test は必ず `XDG_STATE_HOME` を隔離する。** 現行の e2e (`crates/hyoui-cli/tests/web_e2e_api.rs`、6 test) は gateway を `env_remove("XDG_STATE_HOME")` で起動しており (`:106`、session daemon 側も `:62`)、そのままだと認証を足した瞬間に **実利用の `~/.local/state/hyoui-web/auth.json` を読み、kawaz の本番 credential に対して test が走る**。読むだけでも fixture を足す過程で書く経路が生まれ、`passkey remove` の test が本番 record を消しうる。`env_remove` を **tempdir を指す `env("XDG_STATE_HOME", …)` に変える**のが要件で、これは認証を足す変更と同じ commit で行う (後回しにすると、その間の test 実行が本番 state を触る)

この隔離は「無認証 mode を持たない」判断 (本決定) の直接の帰結である。`auth = "none"` があれば test はそれを選べたが、無いので **fixture と state dir の隔離が test の前提**になる。
- **127.0.0.1 直結 (経路 [3]) で使えるのは無認証の口だけ** = `/healthz` と `/version` (決定 1)。`hyoui web daemon status` / `restart` の `/healthz` 待ち (DR-0034 決定 5 / 7) はこれで足りる
- **loopback を認証免除にはしない。** canddy 経由も loopback 発なので判定できない (§Context)。`X-Forwarded-For` を信じる形は、前段がそれを付けない構成で穴になる
- **WebAuthn の RP ID は domain であり IP アドレスは使えない** ので、`http://127.0.0.1:43690/` を endpoint として登録する経路も無い。loopback から `/api/*` を叩く需要が出たら、その時に手段を設計する (今は無い)
- **非 browser client 向けの bearer token (`hyoui web token add`) は持たない。** hyoui の自動操作 CLI (`hyoui input` / `wait` / `tail`) は daemon の UDS を直接叩き、gateway を経由しない (DR-0005)。gateway の `/api/*` を script から叩く需要が出た時に足す

### 10. 移行の段取り

```text
 W1 (DR-0035: 契約整理 + 世代 version + 相対パス化)
   │  gate 1 / gate 2 を通す
   ▼
 W2 認証実装 (本 DR。既定 passkey で出す)
   │  出す前に、kawaz が stable / unstable / HA の各 endpoint に登録する手順を runbook に書く
   ▼
 W3 ccmsg-webui の iframe に `allow="publickey-credentials-get"` を足す issue を依頼
   ▼
 W4 canddy の `Caddyfile` のコメント (「gateway 側は認証省略で OK」) 修正を依頼
```

- **W2 を出した瞬間から認証が有効になる** ので、出す前に runbook (`docs/` 配下、W2 の成果物) が必要である。中身は「canddy に 3 endpoint が立っていることを確認 → `hyoui web passkey add --endpoint https://hyoui.<host>` → 出た URL を Mac / iPhone で開いて 6 桁コードを入れる → `passkey list` で登録を確認 → 個別 endpoint も必要なら同じ手順」。unit の定義 (`hyoui web daemon add`) は endpoint を持たないので触らない
- **canddy の 3 endpoint が先に要る** (DR-0034 の P6 / issue `2026-09-15-request-hyoui-three-endpoints-and-ha`)。endpoint が引けないと登録 URL を開けない
- **W3 までの間、iframe 内は 401 の overlay に「別タブで開く」リンクを出す。** `allow` 属性が無い iframe では `get` が走らないので、top-level で開いてもらう
- **canddy 側の設定とコメントの正本は canddy が持つ** ので、hyoui から書き換えない (DR-0034 決定 8 と同じ)

## Implementation phases

| Phase | 内容 | gate |
|---|---|---|
| W2-0 | (実装前) iframe 経路の実機確認 | **gate 3**: (a) `allow="publickey-credentials-get"` 付き iframe 内の `navigator.credentials.get()` が通る、(b) `clientDataJSON.topOrigin` に親 origin が入る、(c) 同一 site iframe 内のリクエストに `SameSite=Strict` の cookie が乗る、(d) 平文 http の endpoint で `Secure` cookie が保存されないこと。(a) か (c) が崩れたら決定 6 を Alternatives の (P) に差し替える。<br>✅ **Chrome 分は通過** (2026-09-16、同一 site・別 origin の 2 port + CDP の仮想 authenticator): (a) assertion が返る、(b) `topOrigin` に親 origin が入る、(c) server が `__Secure-` cookie を受け取る、(d) `http://<LAN IP>` は `isSecureContext: false` で `Secure` cookie が保存されず WebAuthn 自体が走らない。**併せて `crossOrigin: true` が来ることが分かり、決定 6 を「拒否は登録経路のみ」に改めた**。<br>⬜ **Safari / iOS Safari は kawaz 確認待ち。** 検証ページと手順は runbook (W2-6) に置く。最も重要なのは (c) — **iOS Safari の ITP が同一 site iframe の `SameSite=Strict` cookie を落とすと、iframe 内で refresh が効かず Alternatives (P) の popup に倒す判断が要る** |
| W2-1 | `webauthn-rs` を足し、登録 / 認証の検証経路を作る (決定 8) | **gate 4**: 決定 2 の 3 設定が crate の設定で表せる。表せなければ自作の範囲を決めてから進む。仮想 authenticator で登録 → 認証が通る。<br>✅ **実測して確定** (2026-09-16、決定 8 の表): `attestation: none` と `userVerification: required` は表せ、`residentKey: preferred` だけ表せない (→ `required` に変更)。**0.5 系は openssl 依存で linux target gate が落ちるため `=0.6.1-dev` を pin**。仮想 authenticator (softtoken) の往復は通る。crate は `crossOrigin` を登録経路でだけ検査し、`userHandle` は検証しない |
| W2-2 | `auth.json` / `pending.json` と別 file への `flock` (決定 4) | 2 プロセスから同時に rotate しても family が壊れない (並行 test)。**tmp + rename を挟んでも lost update が起きない** (lock file 方式の検証。data file に lock を取る実装だと落ちる test を書く)。**同じ challenge が 2 回消費できない**、**6 桁コードの試行回数が 2 プロセス合計で数えられる**。HA endpoint の credential を片方の unit で登録し、**もう片方の unit で認証が通る** |
| W2-3 | `/auth/*` 4 経路と middleware (決定 1 / 3 / 5) | `/api/*` と WS が 401 を返し、`/healthz` `/version` `/assets` `/` `/sessions/{id}` は通る (決定 1 の表を test で固定する)。endpoint をすり替えた challenge / assert が落ちる。**既存 e2e `crates/hyoui-cli/tests/web_e2e_api.rs` の 6 test が、tempdir の `XDG_STATE_HOME` + 登録 fixture (Bearer / WS subprotocol 付き、endpoint は `http://127.0.0.1:<port>/`) で全通過する** (決定 9)。正規形の endpoint が record の key と一致することがこの test で同時に固定される。<br>✅ **通過** (`crates/hyoui-web/tests/auth_routes.rs` が決定 1 の表 / challenge のすり替え / 6 桁コードの試行上限 / refresh の rotate と再利用検知 / tombstone の反映を、e2e 6 test が Bearer + WS subprotocol の echo + `hello.auth_expires_at` + `auth.extend` の往復を固定)。**署名を伴う登録 → 認証の通しは実ブラウザに残る** (gate 4 の仮想 authenticator の制約、決定 8 の表) |
| W2-4 | `hyoui web passkey` / `session` の CLI (決定 2) | 登録 → 認証 → `passkey list` → `remove` が一続きで通る。**`remove` 後は (a) 新規認証が落ち、(b) `/auth/refresh` が落ち、(c) 確立済み WS は次の `auth.extend` で切れる** (決定 4 の「失効はいつ効くか」)。gateway が停止していても `passkey add` が URL を発行できる (決定 2) |
| | | ✅ **CLI は通過** (`hyoui web passkey add \| list \| remove`、`hyoui web session list \| remove`。parse / help / completion 3 shell を test で同期)。`crates/hyoui-cli/tests/auth_store_concurrency.rs` が **本物の 2 プロセス**で「並行 `passkey add` が登録を落とさない」「6 桁コードの試行回数が 2 unit 合計で数えられる」「片 unit で置いた session が両 unit で通り、tombstone が両方に即効く」を固定。**登録 → 認証の通し (= 署名を伴う一続き) は W2-5 の front が入ってから実機で見る** |
| W2-5 | front の overlay ログイン UI と tab-share (決定 5) | reference `multi-tab-token-refresh` の 7 性質を test で固定する。**2 タブの access が同時に切れても refresh が 1 回だけ**走る |
| | | ✅ **通過。** 7 性質は `crates/hyoui-web/tests/js/auth-share.test.js` (node の test runner、`just test-js` と CI の js job) が `assets/auth-share.js` を本物のまま読んで固定する。**実ブラウザでは 2 つの bug が出て直した**: (1) channel を sub で張り替えると**開いたばかりのタブ (sub 未知) が sub を知っているタブに届かない** → 待ち合わせ場所を endpoint 単位にし、誰の値かは message の `sub` で判定する (決定 5 の「配るメッセージには sub を載せる」がこのため)、(2) 予定した延長を `ensure` (= 期限内なら何もしない) で呼んでいて**実際には走っていなかった** → 先回りの入口 (`refreshAhead`) を分けた。どちらも test に落としてある |
| W2-6 | runbook を書き、kawaz が各 endpoint に登録 (決定 10) | HA endpoint に登録した 1 本で、**fallback を起こしても再認証を求められない** (実機で unstable を落として stable に回す) |
| | | ✅ **runbook は `docs/runbooks/2026-09-16-web-passkey-registration.md`**。gate 3 の検証ページを `docs/runbooks/assets/dr36-gate3/` に置いた (Safari / iOS の確認手順込み)。⬜ **kawaz の登録そのものは W2-5 待ち** — 招待 URL を開いた先の登録ページが front の成果物なので、W2-5 が入るまで手順は実行できない |

**残る未検証事項は Safari / iOS Safari の gate 3 だけである** (2026-09-16 時点)。**`residentKey: required` のままの登録 → 認証は実ブラウザで通した** (Chrome + CDP 仮想 authenticator、2026-09-16): 招待 URL → 6 桁コード → `create()` → 登録が即サインイン → cookie を消して `get()` で再サインイン → record の `last_used_at` と `sign_count` が動く → `passkey remove` で次の要求から落ちる、までを観測した。同じ経路で `auth.extend` → `auth.extend.result (ok:true)` の往復も CDP の WS frame で確認した。gate 4 と前提条件表の「平文 http では refresh cookie が保存されない」は実測で埋まり、実測の結果 3 件を裁定して決定 2 / 決定 6 / 決定 8 に反映した。**推測のまま実装に進まない。**

## Alternatives Considered

| 案 | 中身 | 不採用理由 |
|---|---|---|
| 人の認証を前段 (canddy の forward auth / tailnet の identity) に寄せる | proxy が認証し、gateway は透過で受ける | 前段の構成が endpoint ごとに違い、gateway が「誰か」を知る形が揃わない。passkey なら gateway 自身が判定でき、前段は透過でよい |
| localhost 限定の無認証登録ページ | `http://127.0.0.1:4369x/register` を開けば登録できる | **不成立**。canddy 経由も loopback 発なので「localhost 限定」を判定できない。`X-Forwarded-For` を信じる形は前段が付けない構成で穴になる |
| 初回だけ無認証で登録し、1 つ登録されたら閉じる (TOFU) | 最初の登録だけ誰でもできる | gateway を再インストールするたびに窓が開く。CLI を 1 本足す (決定 2) より簡単でもない |
| (P) top-level で一度ログインして cookie を持たせる | iframe 内は 401 で「hyoui にログイン」ボタンを出し、popup で hyoui origin の top-level を開いて認証、`BroadcastChannel` で iframe に伝える | ログインのたびに popup が開く。決定 6 が崩れた時の**退路として残す** (ccmsg の sandbox に `allow-popups` は既にある) |
| (Q) 親 (ccmsg) が短命 token を発行して iframe に渡す | ccmsg daemon が hyoui 用の token を mint し、hyoui が検証 | hyoui が ccmsg を IdP として信頼することになり、ccmsg 側 3 リポに hyoui 専用の契約が増える。hyoui 単体の経路 [1] には別途 passkey が要るので認証経路が 2 本になる。「hyoui の認証は hyoui が判定する」から外れる |
| `topOrigin` が present なら拒否する (ccmsg と同じ) | iframe 内の認証を一切許さない | ccmsg 自身が iframe に入らない判断であり、埋め込まれる側の hyoui には当てはまらない。拒否すると経路 [2] (Terminal タブ) が常に別タブ送りになる |
| httpOnly cookie 1 本だけで済ませる | opaque 乱数 1 本を 30 日 sliding で持ち、WS upgrade も cookie で認証 | tab-share と rotate の実装が丸ごと不要になるが、reference が規定する形から乖離する理由が「実装量」になる。パターン統一を優先する (裁定 Q4)。再利用検知で cookie 盗難を検出できる利点も失う |
| endpoint / RP ID / origin allowlist を gateway の config に持つ | `[web].endpoints` に URL の一覧を書く | 前段の構成を変えるたびに gateway の config も直す運用になる。「gateway はマウント先を知らない」(目的) が崩れる。record の endpoint で足りている |
| `[web].auth = "none" \| "passkey"` を持ち、test と dev は `none` で動かす | 無認証 mode を config で選べる | `none` があること自体が事故の元 (裁定 Q7)。常駐 unit が誤って `none` で上がる経路を残さない。test は登録 fixture で通せる (決定 9) |
| credential 単位の `rw` / `ro` を今から実装する | `passkey add --ro` で決め、gateway が `ro` の入力を 403 で落とし daemon に `Ro` mode で張る | 利用者が 1 人で、観測だけ許したい端末が今は無い。claim の定義だけ置いて実装は必要時に行う (裁定 Q5) |
| ccmsg の自前 WebAuthn 実装 (TS 550 行) を Rust に移植 | `ciborium` + 署名検証 crate で自作 | ccmsg 側で「ライブラリに劣らないテスト」が未達のまま残っており、その負債を引き継ぐ。crate で表せない部分が出た時に、その部分だけを書く |
| instance 間の record 複製 (ccmsg の `auth.records` topic) と発行者への転送 | peer 間で record を複製し、HMAC secret / challenge / rotate は発行者に問い合わせる | 2 unit は同一ホストで同じ file を読める。転送 protocol を持つより file + `flock` の方が hyoui の形に合う (決定 4) |
| CLI → gateway の管理経路を持つ (ccmsg の UDS 管理フレーム `passkey_add` 相当) | CLI が走っている gateway に登録要求を送り、secret は gateway のメモリに置く | gateway の生存が `passkey add` の前提になり、全 unit が停止していると登録 URL を発行できない。さらに HA endpoint では「発行した unit」と「POST を受ける unit」が違いうるので、ccmsg と同じ「発行者へ転送」が要る。CLI が `pending.json` に書く形 (決定 2) なら両方が消える |
| cookie 名に `sub` のハッシュを含め、`__Secure-hyoui-*` の全 cookie を試す (ccmsg と同じ) | 認証前は誰の cookie か分からないので、prefix 一致の cookie を順に検証する | 同一 endpoint に複数 `sub` を並べられる利点があるが、利用者 1 人の運用でその場面が無い。試行のループは「どの cookie で失敗したか」を分ける必要も生み、失敗理由を分けない方針 (決定 5) と噛み合わない。endpoint だけのハッシュで 1 つに決める |
| `flock` を `auth.json` / `pending.json` 自身に取る | data file を直接 lock する | tmp + rename で inode が入れ替わるため、rename を跨いだ writer 同士が別の inode の lock を掴み lost update になる。lock を差し替えられない別 file に固定する (決定 4) |
| webui から passkey の一覧 / 削除ができるようにする | `/api/passkeys` を足す | ccmsg でも未実装。CLI 一本で足りており、認証済みの画面から認証情報を消せる経路を増やす理由が無い |

## Consequences

- **gateway が endpoint を知らない構造が、認証の中心になる。** 「どの endpoint が存在するか」を知るのは CLI とブラウザだけで、gateway は record を引くだけ。前段の構成 (host / path / HA の優先順) を変えても gateway 側の変更はゼロである。代償は、ブラウザが endpoint を計算できることへの依存 (DR-0035 決定 6 が前提条件になる)
- **認証を入れた瞬間から、loopback 直結で `/api/*` は使えなくなる。** 使えるのは `/healthz` と `/version` だけ。`hyoui web daemon status` / `restart` はこれで足りるが、`curl http://127.0.0.1:43690/api/sessions` のような手元の確認手段は失われる。代わりに `hyoui list` (daemon の UDS 直結) を使う
- **endpoint ごとに登録が要る。** HA endpoint 1 本で日常は足りるが、unstable を狙って開く時は個別 endpoint の登録が別に要る。endpoint を増やすたびに `passkey add` が 1 回増える
- **2 unit が 1 file を共有する依存が生まれる。** `auth.json` が壊れれば両方の unit の認証が止まる。逆に片方の unit だけを入れ替えても session は続く (fallback で再認証を求められない) のが、この共有の目的である
- **`webauthn-rs` の依存が入る。** hyoui-web に閉じるので core の依存は変わらない。版は `=0.6.1-dev` の exact pin で、prerelease を踏む代償 (patch が来ない / 0.6.2 で API が変わりうる) を受けている (決定 8)。crate の設計に合わなかったのは `residentKey` 1 点で、そこは `required` に倒して吸収した。**併せて RUSTSEC-2023-0071 (rsa) の ignore を 2 つの config に抱える** — feature で外す口が上流に無く、hyoui は RSA 秘密鍵を持たないので踏まない (決定 8)。上流が差し替えたら外す
- **`residentKey: required` にした帰結として、登録には resident key の枠を持つ authenticator が要る。** Mac / iPhone の passkey はどちらも該当するので現運用では拒まれないが、枠を持たない security key を足したくなった時は登録できない (その時は決定 2 を見直す)
- **ccmsg-webui と canddy に依頼が 2 本出る。** どちらも別リポの責務で、hyoui 側から設定を書き換えない。W3 が済むまで iframe 内は別タブ送りになる
- **CSP を保留した帰結として、任意のサイトが hyoui を iframe に埋め込み `allow="publickey-credentials-get"` を付けて passkey のプロンプトを出せる。** 認証が通るのは `clientDataJSON.origin` が record の endpoint と一致する場合だけなので、そのサイトが session の内容や token を得ることはない (取れるのは「利用者が生体認証を求められた」という体験だけ) が、埋め込み元を絞る手段は今の設計には無い。clickjacking の面は `X-Frame-Options` / `frame-ancestors` を付けない現行 (findings Part 1-C) と同等で、認証を足すことで悪化はしない。絞る必要が出た時に別 DR で `frame_ancestors` を決める (決定 6)
- **cookie 名を endpoint だけのハッシュにしたので、同一 endpoint に 2 つの `sub` を同時にログインさせられない** (決定 5)。利用者 1 人の前提が変わったら名前に `sub` を戻す
- **失効が確立済み WS に効くまで最長 4 時間かかる** (決定 4)。即時に切る手段は `hyoui web daemon restart` である
- **test が `XDG_STATE_HOME` の隔離に依存する。** 隔離を外した変更は「本番 state を触る test」を作る。無認証 mode を持たない判断の代償で、決定 9 に明記した
- **`rw` / `ro` の claim が record に定義されるが、当面読まれない。** 使われない field が残るのは、後から足すと既存 record の欠落を扱う分岐が要るためである (決定 7)
- **平文 http の endpoint は成立しない。** `Secure` cookie が保存されず、リロードごとに passkey を求められる。https を前段が終端することが前提条件になる (未検証の項目として残る)

## 関連

- `docs/research/2026-09-15-web-protocol-and-passkey-grand-design.md` — 本 DR の母体 (§4 認証のグランドデザイン / §5 ccmsg から借りるもの / §6 移行)、kawaz 裁定表 (2026-09-16)
- `docs/findings/2026-09-15-web-contract-and-ccmsg-passkey-inventory.md` — ccmsg の passkey 実装事実 (登録 / 認証 / 認可 / RP ID / iframe / ライブラリ)、hyoui 側の現行認証 (Part 1-C / 1-D)、reference ↔ ccmsg 実装の差分
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/auth-patterns/passkey-registration-local-first.md` — 登録 / 検証手順 / token family / RP ID 制約の正本
- 同 `multi-tab-token-refresh.md` — tab-share の手順と、test で固定する 7 性質
- DR-0035 — web 契約と世代 version。決定 6 (endpoint 基点の相対 URL と正規形) が本 DR の前提で、本 DR が使う WS frame (`hello.auth_expires_at` / `auth.extend` / `auth.extend.result`) と WS upgrade の 401 は DR-0035 決定 1 の契約表に収録されている
- `crates/hyoui-cli/tests/web_e2e_api.rs` — 認証を足す時に `XDG_STATE_HOME` の隔離が要る既存 e2e (決定 9 / W2-3)
- DR-0034 決定 2 (`hyoui-web/` state dir)、決定 5 (`/healthz` 待ち)、決定 8 (HA fallback)、P6 の canddy issue
- DR-0027 §1 (crate 構成), §4 (bundler 無し assets), Consequences (認証は scope 外 — 本 DR が置き換える)
