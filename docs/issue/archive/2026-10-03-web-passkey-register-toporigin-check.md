---
title: passkey 登録経路が topOrigin を見ず crossOrigin だけで埋め込みを判定している
status: resolved
category: bug
created: 2026-10-03T17:10:00+09:00
last_read: 2026-10-03T17:10:00+09:00
open_entered: 2026-10-03T17:10:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-04T00:00:00+09:00
discard_reason:
pending_reason:
close_reason: 登録経路で topOrigin present を crossOrigin の値によらず拒むよう修正し、4 値 + crossOrigin:false の test で固定した
blocked_by:
---

# passkey 登録経路が topOrigin を見ず crossOrigin だけで埋め込みを判定している

kawaz/passkey の設計レビュー (2026-10-03) で相互比較して判明。

## 現象

`crates/hyoui-web/src/auth/webauthn.rs` の `client_data_says_cross_origin` は `clientDataJSON` の `crossOrigin` だけを読み、`true` のときだけ登録を拒む。`topOrigin` は見ない。reference の `auth-patterns/passkey-registration-local-first` は「`topOrigin` と `crossOrigin` を両方独立に検査する。`topOrigin` だけ付いて `crossOrigin` が欠ける値は client の不整合で、片方の検査に頼るとこれを通してしまう」としている。現実装はその値 (`topOrigin` あり ∧ `crossOrigin` 欠落) を通す。kawaz/passkey は `webauthn.ts` の 244-266 付近で両方を含意検査している。

webauthn-rs が `topOrigin` を読まないことは DR-0036 gate 4 の表に記載済み。本件はそれを受けた hyoui 側の自前検査の抜け。

## 対処の方向

登録経路で `topOrigin` が present なら (`crossOrigin` の値によらず) 拒む。認証経路は DR-0036 決定 6 で任意の `topOrigin` を許すので対象外。test で `topOrigin` のみ / `crossOrigin` のみ / 両方 / 両方無し の 4 値を固定する。

## 決着 (2026-10-04)

`crates/hyoui-web/src/auth/webauthn.rs` の `client_data_says_cross_origin` を `client_data_says_embedded` に改め、`topOrigin` が present (値・型によらず) または `crossOrigin: true` なら真を返すようにした。`finish_registration` はこれを呼ぶので、`topOrigin` あり ∧ `crossOrigin` 欠落の値も拒否される。拒否は従来どおり `WebauthnFailure::CrossOriginRegistration` を経由して routes.rs で `AuthFailure::denied` に翻訳され、攻撃者入力が 500 にならない経路は変えていない。認証経路 (`finish_authentication`) は変更なし (DR-0036 決定 6)。unit test `embedded_client_data_is_detected_by_top_origin_or_cross_origin_true` で `topOrigin` のみ / `crossOrigin: true` のみ / 両方 / 両方無し / `crossOrigin: false` 明示 / `topOrigin` + `crossOrigin: false` / 不正 JSON を固定した。
