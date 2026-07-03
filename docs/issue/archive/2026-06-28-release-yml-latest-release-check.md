---
title: release.yml semver gate に latest-release 並列 check 追加 (DR-0039 canonical 同期)
status: resolved
category: request
created: 2026-06-28T20:04:49+09:00
last_read:
open_entered: 2026-06-28T20:04:49+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-07-03T19:00:00+09:00
discard_reason:
pending_reason:
close_reason: "DR-0039 canonical の latest-release / latest-tag 並列 check を移植 (trigger が paths [Cargo.toml] のため eq=良性 skip / lt=error の適応あり、workflow 内に Design rationale 記載)。あわせて ci.yml workflow_call 化 + release build-release の CI gate 化、test job timeout-minutes: 30 も同時実装。actionlint clean"
blocked_by:
origin: bump-semver dogfood報告
---

# release.yml semver gate に latest-release 並列 check 追加 (DR-0039 canonical 同期)

## 概要

bump-semver canonical (DR-0039) で確立した release.yml の semver gate pattern に合わせ、本リポの release.yml に `latest-release` 並列 check を追加する。

## 背景

bump-semver canonical (DR-0039) で release.yml の semver gate pattern が更新された。本リポは `latest-tag` 単独 + `gh release view` の B 型で、`gh release create` が origin に git tag を push しない仕様で gap がある。

## 現状 (release.yml L51-67 該当)

`vcs:latest-tag()` 入力構文 + `gh release view` のみ、`latest-release` 並列 check 無し。

## 修正方針

`latest-release` 並列 check を追加。canonical pattern は bump-semver の release.yml と DR-0039 参照。

## 参考

- bump-semver の `.github/workflows/release.yml`
- bump-semver の docs/decisions/DR-0039-release-yml-semver-gate-pattern.md
- kawaz/die dogfood 報告: session 911732b3、2026-06-28

## 優先度

中 (= B 型)。bump-semver v0.43.0 release 後に着手推奨。

## 受け入れ条件

- [ ] release.yml の semver gate が DR-0039 canonical pattern と一致する
- [ ] `latest-release` 並列 check が追加されている

## Close 時の記録 (2026-07-03)

- latest-release / latest-tag 並列 check を release.yml `check-version` step に実装
  (canonical: kawaz/bump-semver release.yml / DR-0039)
- 本リポ固有の適応: trigger が `paths: [Cargo.toml]` で非 bump 変更でも発火するため、
  「現 version == 最新」は良性 skip (changed=false, exit 0)、「現 version < 最新」のみ
  error (exit 1)。canonical の一律 error は不採用 (理由は workflow 内コメント)
- スコープ拡張分 (同一 commit): CI red のまま release が出る穴を塞ぐため
  ci.yml に `workflow_call` を追加し、release.yml の `build-release` が
  `needs: [check-version, ci]` で CI 一式 green を必須 gate 化
