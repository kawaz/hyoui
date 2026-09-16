---
title: "DR-0036 W2-5: front の overlay ログイン UI と登録ページ、tab-share (= 認証は有効だがブラウザから開けない状態)"
status: resolved
category: task
created: 2026-09-16T21:55:00+09:00
last_read: 2026-09-16T21:55:00+09:00
open_entered: 2026-09-16T21:55:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-09-16T22:15:00+09:00
discard_reason:
pending_reason:
---

# DR-0036 W2-5: front の overlay ログイン UI と登録ページ、tab-share

## 決着 (2026-09-16)

実装して close。**裁定は (A) + (B) の分担**で、7 性質は node の test runner
(`crates/hyoui-web/tests/js/`、`just test-js` と CI の js job)、実ブラウザでの通しは
playwright (Chrome + CDP 仮想 authenticator) が見る。

実機で 2 つの bug が出て直した (どちらも test に落とした): channel を sub で
張り替えると開いたばかりのタブに届かない / 予定した延長が `ensure` 経由で実際には
走っていない。詳細は DR-0036 の W2-5 行。

以下は起票時の記述。

## 今どうなっているか

W2-1 〜 W2-4 と W2-6 (runbook) が入り、**認証は既に有効**である (DR-0036 決定 9 — `auth = "none"` を設けていない)。したがって:

- `/api/*` と WS attach は access token が無ければ 401 (`code: auth-required`)
- `/` と `/sessions/{id}` の HTML、`/assets/*`、`/healthz`、`/version` は通る
- **front 側に 401 を扱う経路が無いので、ブラウザで開くと画面が出たまま何も読めない。** 招待 URL (`<endpoint>#register=<jwt>`) を開いてもコード入力の画面が無いので、runbook の手順 2 で止まる

server 側は揃っている: `/auth/challenge` (`purpose: "assert" | "register"`、登録用は `jwt` 必須) / `/auth/register` / `/auth/assert` / `/auth/refresh`、 `hello.auth_expires_at`、`auth.extend` → `auth.extend.result`。

## やること (DR-0036 決定 5 / 決定 1)

- **401 を受けたらページ内に overlay でログイン UI を出す。redirect しない** — iframe 内で redirect すると親 (ccmsg) の Terminal タブがログインページに化けて何が起きたか読めなくなる (決定 1)
- **登録ページ**: `location.hash` の `#register=<jwt>` を拾い、6 桁コードの入力を受けて `/auth/challenge` (`purpose: "register"`, `jwt`) → `create()` → `/auth/register`。**top-level 前提**で、iframe では出さない (決定 6)
- **access はメモリのみ** (localStorage に置かない)。refresh は cookie で server が持つので JS は触らない
- **WS は subprotocol `hyoui.token.<access>` で token を運ぶ**。`hello` の `auth_expires_at` を見て残り寿命 90% で `/auth/refresh` を打ち、得た access を **同一接続の `auth.extend`** で提示する (`ok:false` が来たら失効なので接続は gateway 側から閉じられる→ ログイン UI に落ちる)
- **tab-share** (reference `multi-tab-token-refresh`): `navigator.locks.request( "hyoui.auth.refresh:<endpoint>:<sub>")` の中でだけ refresh し、 `BroadcastChannel("hyoui.auth:<endpoint>:<sub>")` でメモリからメモリへ配る。ロックを取った側は先に `{kind:"ask"}` を投げて数十 ms 待つ。sub が分かる前は endpoint だけの key で待ち、確定後に張り替える。Web Locks が無い環境では各タブが自分で refresh する
- endpoint の計算は `assets/contract.js` に既にある (`indexEndpoint` / `sessionEndpoint`)。**その値をそのまま `/auth/*` の body に載せる** (決定 3)
- WebAuthn の options は crate が組んだ JSON をそのまま返しているので、 `challenge` / `user.id` / `allowCredentials[].id` の base64url ↔ ArrayBuffer 変換が front 側に要る

## 裁定が要る点: 7 性質をどこで test するか

DR-0036 の W2-5 gate は reference の **7 性質を test で固定する**ことを求めるが、 **このリポには JS の test 基盤が無い** (assets は bundler 無しの素の JS、DR-0027 §4)。現状の JS ↔ Rust の整合は `rust_and_js_protocol_version_agree` のように「Rust の test が asset を読む」形しかなく、これは振る舞いを固定できない。

| 案 | 中身 | 代償 |
|---|---|---|
| (A) `deno test` (or `node --test`) を gate に足す | tab-share の core を依存注入 (locks / channel / clock / refresh fn) の純 module に切り出し、fake で 7 性質を固定 | **CI に JS runtime の setup が 1 段増える** (手元には deno / node 両方ある)。`just push` の deps にも足すか判断が要る |
| (B) playwright-cli で実ブラウザ 2 タブ | 実際の `navigator.locks` / `BroadcastChannel` で見る | 遅く、gateway + 登録済み credential (CDP 仮想 authenticator) の段取りが要る。**ただし `residentKey: required` の実機確認 (gate 4 の残り) はどうせこの経路が要る** |
| (C) Rust 側の静的検査だけ | asset に lock 名の pattern があることを grep | 振る舞いを固定できない (= gate を満たさない) |

**(A) と (B) は排他ではない** — (A) が 7 性質、(B) が「登録→ 認証の通し」と `residentKey: required` を見る、という分担が自然。ただし (A) は CI への追加なので kawaz の裁定が要る。

## 関連

- [DR-0036](../decisions/DR-0036-passkey-auth-for-web-endpoints.md) 決定 1 / 決定 5 / 決定 6、 Implementation phases の W2-5
- [runbook: web endpoint に passkey を登録する](../runbooks/2026-09-16-web-passkey-registration.md) —手順 2 以降がこの issue に依存する
- reference `auth-patterns/multi-tab-token-refresh` — 7 性質の正本
- `crates/hyoui-web/assets/contract.js` — endpoint の計算が既にある
