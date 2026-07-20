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

### 4. 静的アセット

xterm.js + 素の HTML/JS (bundler なし、vendored コピー)。リリースビルドは
crate に埋め込み (include_dir 系)、開発中は `--web-assets-dir` でローカルファイルを
返す二段構え (kawaz 方針 r40m22)。

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
