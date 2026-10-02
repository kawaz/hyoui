---
title: CI の単体テストのハングが Test job の timeout を食う + ローカル Linux コンテナと CI で結果が食い違う
status: open
category: bug
created: 2026-10-02T23:55:22+09:00
last_read:
open_entered: 2026-10-02T23:55:22+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# CI の単体テストのハングが Test job の timeout を食う + ローカル Linux コンテナと CI で結果が食い違う

## 概要

CI の単体テスト 1 本のハングが Test job の 30 分 timeout を丸ごと食い、原因が見えにくい。また、ローカル Linux コンテナで CI と結果が食い違うテストがある (調査未完了)。

## 背景

### (1) テストのハングが失敗として表面化しない

- v0.9.59 で `discovery::tests::locked_listener_is_preserved_when_backlog_fills` が Linux の blocking connect で無期限 block した
- ubuntu Test job が `timeout-minutes: 30` で cancelled、Release も cancelled
- ログには "has been running for over 60 seconds" しか出ず、失敗として表面化しなかった
- 真因は v0.9.60 で修正済み
- 対策案:
  - `just ci` の test step に上限時間を付ける
  - cargo-nextest の `slow-timeout` / `terminate-after` を導入し、ハングを数分で fail にする

### (2) ローカル Linux コンテナで serve_tail 系が fail (真因未調査、flaky と片付けない)

- 環境: docker (rust:1.98 image, OrbStack, kernel 7.0.14-orbstack, aarch64, umask 022)
- コマンド: `cargo test --locked -p hyoui --lib -- daemon::session::tests::serve_tail`
- 3 回中 2〜3 回 fail:
  - `serve_tail_request_no_follow_dumps_buffer` (session.rs:3810 `read_until_contains` timeout)
  - `serve_tail_follow_receives_tail_end_when_child_exits_immediately` (session.rs:3796 / 2874)
- v0.9.59 の未変更 baseline でも同様に fail
- GitHub ubuntu-latest runner では ok
- 全件実行では通る回もある

### (3) 同コンテナで web_service_e2e が fail

- 対象: `hyoui-cli/tests/web_service_e2e.rs` の `status_reports_definition_state_in_isolated_home` / `the_old_single_gateway_label_is_not_referenced`
- ubuntu runner では ok
- rust image に systemd / systemctl が無いことが原因と推測 (未確認)

## 受け入れ条件

- [x] CI の test step に上限時間 (または nextest の slow-timeout / terminate-after) を入れ、ハングが数分で fail として表面化する (v0.9.63: `.config/nextest.toml` で 180s 打ち切り、CI は nextest 必須)
- [ ] (2) の真因を、軸 / 再現条件 / 仮説を押さえて特定する (実機マトリクスで確認)
- [ ] (3) の原因 (systemctl 不在か) を確認する
- [ ] ローカル Linux 検証環境を CI と揃えるか、環境前提をテスト側で明示するかを決める
