# runbook: web endpoint に passkey を登録する

DR-0036 が入った版から、`/api/*` と WebSocket attach は **登録済みの端末でしか開けない**。`auth = "none"` は設けていないので (決定 9)、**登録を 1 本も持たない endpoint はブラウザから開けない**。この手順で各 endpoint に端末を登録する。

登録はホスト上の CLI に閉じている (決定 2)。リモートからの登録経路も復旧経路も無いので、**端末を全部失うと `hyoui web passkey add` を打ち直すところからやり直す** (= ホストに入れれば必ず復旧できる、という形にしてある)。

## 前提

- **endpoint が https である。** `Secure` cookie が保存されない平文 http ではリロードごとに passkey を求められ、そもそも WebAuthn 自体が走らない (実測: DR-0036 前提条件表)。前段 (canddy) が TLS を終端していること
- **canddy に 3 endpoint が立っている** (DR-0034 決定 8): `hyoui.<host>` (HA) / `hyoui-stable.<host>` / `hyoui-unstable.<host>`。立っていないと招待 URL を開けない
- **登録は endpoint ごと** (決定 3)。HA endpoint の 1 本で日常の閲覧は足り、個別 endpoint の登録は unstable を狙って開く時だけ要る
- `hyoui web passkey --help` が出る版であること。**gateway が動いていなくてもよい** (CLI が state file を直に書く、決定 2)
- **招待 URL を開いた先の登録ページ (front) が入っている版であること** (DR-0036 の W2-5)。server 側 (`/auth/*` と CLI) だけ入っている版では、URL を開いてもコード入力の画面が出ない。`hyoui web passkey add` は打てるので、**手順 1 まで進めて手順 2 で止まる**

## 手順

### 1. HA endpoint に 1 本登録する

ホスト上で打つ:

```bash
hyoui web passkey add --endpoint https://hyoui.<host>.kawaz.jp --label "MacBook"
```

出るのは **招待 URL と 6 桁コード**の 2 つ。

```json
{
  "endpoint": "https://hyoui.<host>.kawaz.jp/",
  "sub": "hyoui.<host>.kawaz.jp-1",
  "invite_url": "https://hyoui.<host>.kawaz.jp/#register=eyJ...",
  "code": "414510",
  "expires_at": "2026-09-16T12:30:48Z"
}
```

- URL の token は **fragment (`#`)** にあるので、server にも proxy log にも Referer にも乗らない。それでも他人に見せる経路 (チャット等) に貼らない
- **コードは URL に入っていない。** URL が漏れただけでは登録にならない設計なので、URL とコードを同じ経路で送らない (手元の端末なら画面を見ればよい)
- 有効期限は **10 分**。切れたら `add` を打ち直す (使われなかった登録は次の読み書きで掃除される)

### 2. 登録したい端末で URL を開く

**top-level で開く。** iframe の中では登録が走らない (決定 6 — 招待 URL は必ず新しいタブ / ブラウザで開く)。

1. Mac なら Safari か Chrome、iPhone なら Safari でその URL を開く
2. 登録ページが 6 桁コードの入力を求めるので、CLI に出た数字を入れる
3. 端末の生体認証 (Touch ID / Face ID) が走り、通ると **そのまま閲覧できる状態になる** (= 登録が即サインイン、決定 2)

コードを **5 回間違えるとその URL が焼ける**。焼けたら `add` を打ち直す (= 焼けたことと期限切れは区別せず、どちらも「登録 URL を再発行してください」)。

### 3. 登録を確認する

```bash
hyoui web passkey list
```

`endpoint` / `sub` / `device_label` / `registered_at` が出る。`backup_state` が `true` なら iCloud キーチェーン等で同期される passkey である (= 他の端末でも使える)。**これらは記憶の手がかりで、認証の判定には使っていない** (決定 3)。

```bash
hyoui web session list   # 認証セッション (token family)。PTY の session とは別物
```

### 4. 個別 endpoint も要るなら同じ手順を繰り返す

```bash
hyoui web passkey add --endpoint https://hyoui-unstable.<host>.kawaz.jp --label "MacBook"
hyoui web passkey add --endpoint https://hyoui-stable.<host>.kawaz.jp   --label "MacBook"
```

**HA endpoint の credential は個別 endpoint では使えない** (origin が違う)。逆も同じ。endpoint を増やすたびに登録が 1 回増える。

### 5. HA の fallback を実機で確かめる (W2-6 の gate)

HA endpoint に登録した 1 本で、裏の unit が入れ替わっても再認証を求められないことを見る (決定 4 — record は unit を跨いで同じ file から引ける)。

```bash
# HA endpoint をブラウザで開いたまま、裏の優先 unit を落とす
hyoui web daemon stop unstable
# 画面がもう片方の unit に回る。再認証を求められないこと / WS が張り直せること
hyoui web daemon start unstable
```

## 失効させる

```bash
hyoui web passkey remove <sub>      # 端末 1 台を失効 (credential + その session)
hyoui web session remove <id>       # ブラウザ 1 つの認証セッションだけ失効
```

どちらも **削除ではなく失効の記録** (tombstone) なので、`list` には「失効済み」として残る (= いつ何を消したかが後から読める)。

**効き方に段がある** (決定 4):

| 対象 | いつ効くか |
|---|---|
| 新しい認証 (`/auth/assert`) | 次の要求で即 |
| `/auth/refresh` | 次の要求で即 |
| 既に開いている WebSocket | **次の延長 (`auth.extend`) で切れる。最長 4 時間** |

「今すぐ全部切る」が要る時は unit を入れ替える:

```bash
hyoui web daemon restart --all   # WS は unit の再起動で必ず切れる
```

## 詰まった時の切り分け

| 症状 | 見るところ |
|---|---|
| ページは出るが操作できない / ログイン UI が出る | それは正常な 401 の表示。登録済みの端末で開いているか、`passkey list` にその endpoint の行があるか |
| 生体認証まで行くが「認証に失敗」 | **endpoint の文字列が違う可能性が高い。** `passkey list` の `endpoint` と、ブラウザの URL (`scheme://host[:port]/<path>/` の正規形) が 1 文字も違わないこと。`https://hyoui.<host>` と `https://hyoui.<host>/` は同じに扱われるが、host や path prefix が違えば別 endpoint = 別登録 |
| 生体認証が走らない | endpoint が https か (平文 http では WebAuthn 自体が走らない)。iframe の中なら ccmsg 側に `allow="publickey-credentials-get"` が要る (DR-0036 W3、未了の間は別タブで開く) |
| リロードごとに passkey を求められる | refresh cookie が保存されていない。https であること、`Path` が endpoint の path と合っていること |
| コードが通らない | 5 回で URL が焼ける。`add` を打ち直す |
| `passkey add` が endpoint を拒否する | 正規化できない値 (query / fragment 付き、scheme 無し等)。完全な URL を渡す |

失敗の理由は応答では分けていない (決定 5 — 総当たりに手がかりを与えない)。切り分けは **unit の log** を読む (`hyoui web daemon log <name>`): そこには `authentication failed: <理由>` が出る。

## gate 3: Safari / iOS Safari の確認 (未了)

Chrome では実測済み (DR-0036 の gate 3)。**Safari / iOS Safari は kawaz の確認待ち**で、最も重要なのは **iOS Safari の ITP が同一 site iframe の `SameSite=Strict` cookie を落とさないか**である。落とすなら iframe 内で refresh が効かず、popup 経路 (DR-0036 Alternatives の (P)) に倒す判断が要る。

検証ページは `docs/runbooks/assets/dr36-gate3/` にある (Chrome で使ったものそのまま)。同一 site・別 origin の 2 つを立てて、親が子を iframe で埋める模型である。

```bash
cd docs/runbooks/assets/dr36-gate3
python3 serve.py 8111 parent &   # 親 (= ccmsg webui の位置)
python3 serve.py 8112 child &    # 子 (= hyoui endpoint の位置)
open http://localhost:8111/      # localhost は secure context 扱いなので WebAuthn が走る
```

見るのは 4 点:

| 見るもの | 期待 |
|---|---|
| (a) `allow="publickey-credentials-get"` 付き iframe 内の `navigator.credentials.get()` | assertion が返る |
| (b) `clientDataJSON.topOrigin` | 親の origin が入る (= 入っていても hyoui は拒否しない) |
| (c) 同一 site iframe からの要求に `SameSite=Strict` cookie が乗るか | **server の `/whoami` が `__Secure-` cookie を受け取る** ← 最重要 |
| (d) 平文 http (LAN IP) の endpoint | `Secure` cookie が保存されず、WebAuthn も走らない |

iPhone から見る時は Mac の LAN IP では (d) に落ちるので、**https で到達できる経路 (tailnet 越しの本番 endpoint) で見る**か、iPhone を Mac に繋いで Safari の Web Inspector で `localhost` 転送を使う。

## 関連

- [DR-0036](../decisions/DR-0036-passkey-auth-for-web-endpoints.md) — 認証の設計 (登録 / RP ID / cookie / 失効 / 無認証 mode を持たない判断)
- [DR-0034](../decisions/DR-0034-service-multi-unit-and-stable-unstable-ha.md) — 3 endpoint と HA fallback、unit の管理
- [web gateway を監督者 + 2 unit に移す](./2026-09-15-web-service-migration.md) — endpoint が立っていない時に先に読む手順
