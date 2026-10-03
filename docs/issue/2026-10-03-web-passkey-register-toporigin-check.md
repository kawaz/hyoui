---
title: passkey 登録経路が topOrigin を見ず crossOrigin だけで埋め込みを判定している
status: open
category: bug
created: 2026-10-03T17:10:00+09:00
last_read: 2026-10-03T17:10:00+09:00
open_entered: 2026-10-03T17:10:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# passkey 登録経路が topOrigin を見ず crossOrigin だけで埋め込みを判定している

kawaz/passkey の設計レビュー (2026-10-03) で相互比較して判明。

## 現象

`crates/hyoui-web/src/auth/webauthn.rs` の `client_data_says_cross_origin` は `clientDataJSON` の `crossOrigin` だけを読み、`true` のときだけ登録を拒む。`topOrigin` は見ない。reference の `auth-patterns/passkey-registration-local-first` は「`topOrigin` と `crossOrigin` を両方独立に検査する。`topOrigin` だけ付いて `crossOrigin` が欠ける値は client の不整合で、片方の検査に頼るとこれを通してしまう」としている。現実装はその値 (`topOrigin` あり ∧ `crossOrigin` 欠落) を通す。kawaz/passkey は `webauthn.ts` の 244-266 付近で両方を含意検査している。

webauthn-rs が `topOrigin` を読まないことは DR-0036 gate 4 の表に記載済み。本件はそれを受けた hyoui 側の自前検査の抜け。

## 対処の方向

登録経路で `topOrigin` が present なら (`crossOrigin` の値によらず) 拒む。認証経路は DR-0036 決定 6 で任意の `topOrigin` を許すので対象外。test で `topOrigin` のみ / `crossOrigin` のみ / 両方 / 両方無し の 4 値を固定する。
