# handshake cap 超過による reject 時の対処

> Status: Active
> Date: 2026-05-27
> Related: [[DR-0008]] §2.2 (cap protocol)、[[R5-H10]] (backlog)

## 症状

- client が daemon に `connect` 直後、handshake 段階で切断される
- `hyoui status` / `hyoui attach` が `protocol error` / `handshake rejected`
  相当のエラーで終了
- 「攻撃的に巨大な `caps` Vec を送った」「巨大な token を送った」事例の後
- 攻撃ではなく、独自実装 client が proto spec を超えた `caps` を送ってしまった

## 切り分け

1. handshake message size の上限を確認 (`protocol/messages/handshake.rs`):
   | 定数 | 値 | 意味 |
   |------|----|----|
   | `MAX_CAPS_COUNT` | 32 | `caps` Vec の要素数上限 |
   | `MAX_CAP_LEN` | 64 byte | 1 cap string の byte 長上限 |
   | `MAX_TOKEN_LEN` | 256 byte | token string の byte 長上限 |
2. client 側が送信した handshake の dump を確認:
   - 独自実装の場合: `caps.len() <= 32`、各 cap の `.len() <= 64`、
     `token.len() <= 256` を満たすか
   - 公式 CLI 経由なら MVP_CAPS が固定 4 個程度なので通常超過しない
3. daemon 側ログ (R5-SRE-C1 後): `reason=handshake_rejected` で grep
4. 攻撃疑い (= 1 GiB 級 transient peak) なら src IP / UID も確認

## 対処

1. **正規 client の修正**: spec 超過なら client 側で cap 数を絞る
   (= dotted name の細分化を粗くする、無関係 cap を削る)
2. **token 形式の見直し**: 128-bit hex (= 32 byte) が想定。長い token を
   使っているなら hex 形式に揃える (= `MAX_TOKEN_LEN = 256` は十分余裕がある
   が、超過しているなら token 生成側が異常)
3. **攻撃検知時**: src の IP / UID を block、socket file の permission を
   `0600` で再確認 (= 同 UID 攻撃でなければ socket file アクセスで止まる)
4. cap 上限の見直しが必要なら DR を立てて定数を上げる (= 安易に拡大しない、
   handshake は認証前の信頼境界)

## 予防

- 独自 client 実装時は `protocol/messages/handshake.rs` の上限定数を参照
  して同等の preflight check を入れる
- daemon socket は同 UID 限定 (= `0600` + `${XDG_RUNTIME_DIR}` 配下)。
  cross-UID 攻撃面を増やさないため `chmod` で広げない
- 認証前メモリ消費を増やす変更 (= MAX_* 上限拡大、handshake schema 拡張) は
  DR で「攻撃時の transient peak」も明記する
- R5-H10 で導入した cap は protocol 仕様の一部 (= spec 化済)。client/server
  両側でテストが必要

## 関連

- [[DR-0008]] §2.2 — handshake message schema、cap dotted-name 仕様
- [[R5-H10]] — 上限導入の経緯 (= 1 GiB transient peak 抑止)
- `crates/hyoui/src/protocol/messages/handshake.rs:32-39` — 上限定数定義
- `crates/hyoui/src/protocol/messages/mod.rs:47` — re-export
