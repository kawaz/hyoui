---
title: web の unit 登録簿が listen 等の設定値を持っている (daemon / service パターンからの逸脱。unit = config ファイルに直す)
status: open
category: design
created: 2026-10-04T13:00:00+09:00
last_read: 2026-10-04T13:00:00+09:00
open_entered: 2026-10-04T13:00:00+09:00
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

# web の unit 登録簿が listen 等の設定値を持っている

## 現象

`~/.local/state/hyoui-web/units/<name>.toml` (CLI が書く登録簿) に `listen` / `binary` / `enabled` (と `web_assets_dir`) が焼き込まれている。設定ファイル `~/.config/hyoui/config.toml` は存在せず、`~/.config/hyoui` を見てもポートが分からない (2026-10-04 時点: stable = `127.0.0.1:43690`、unstable = `127.0.0.1:43691`)。

## 原因

DR-0034 の比較表「unit の中身」の行が、llm-gateway の形 (unit = 設定ファイル 1 つ、登録簿は config の path + `binary_path` + `enabled`) を「そのまま持ち込む」としながら、「**変更**: 設定ファイルを持たず `listen` / `binary` / `web_assets_dir` / `enabled` を解決して書く (決定 2)。gateway に設定ファイルの概念が無い」とした。

reference の `cli-daemon-subcommands` (kawaz 製 CLI 共通の正本) は unit を「登録の単位で、案件ドメインが決める (dir / config ファイル / id など)」とし、`daemon list` の出力を `{id, unit, enabled, config, binary_path}` としている。登録簿が持つのは config への参照で、設定値そのものではない。また hyoui は既に `~/.config/hyoui/config.toml` を読む仕組み (DR-0024) と `[web].listen` / `[web].assets_dir` を持っており、「設定ファイルの概念が無い」は事実でもない。

## 直し方の方向

- unit を config ファイル 1 つに対応させ、登録簿は `{config, binary_path, enabled}` だけにする。`listen` / `assets_dir` は config 側
- config の置き場所 (未決): (a) unit ごとに `~/.config/hyoui/web/<unit>.toml` (パターンの「unit = config ファイル」に素直) / (b) `config.toml` の中に unit ごとの節 (`[web.units.<name>]`)
- 既存の stable / unstable 2 unit の移行 (v1.0 前なので互換を残さず置き換えてよい範囲)
- DR-0034 の該当部分を新しい DR で置き換える

## 同じ根: `hyoui web --listen` が 2 本目の起動経路として残っている

`hyoui web --listen=<addr>` が unit も登録簿も通さず gateway を foreground で起動する。名前を省いた `hyoui web daemon run` (config の listen で起動) と同じことをする 2 本目の経路。パターンでは foreground 起動は `daemon run` 1 本で、`hyoui web` は `daemon` / `service` / `passkey` / `session` を束ねる名前空間でしかない。DR-0031 の 1 unit 固定時代に plist が `hyoui web` の 2 語で直接起動していた頃の入口で、DR-0034 で監督者経由に移った後も消していない (現 plist は supervise)。

消す (`hyoui web` は名前空間だけ、引数なしは help のまま、foreground 起動は `daemon run` に一本化)。直す範囲: e2e test (`crates/hyoui-cli/tests/web_e2e_api.rs`、`auth_store_concurrency.rs`) の起動を `daemon run` 系に置き換え、parse test (`crates/hyoui/src/cli.rs` 11684 付近、`crates/hyoui-cli/src/main.rs` 4666 付近)、`--help` の 1 行目、MANUAL (ja / en) の起動例
