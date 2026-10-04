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

先行例の llm-gateway (DR-0028 / DR-0013) にそのまま倣う:

- unit = 任意のパスの config ファイル 1 つ。`hyoui web daemon add [--name <name>] <config-path>` で登録し、name 省略時は config の basename (拡張子なし)。置き場所は利用者が決め、登録簿はパスを参照するだけ (llm-gateway の実例: `~/.config/llm-gateway/config-11302-stable.toml`)
- 登録簿 `~/.local/state/hyoui/web/units/<name>.toml` は `{config, binary_path, enabled}` だけ。`listen` / `assets_dir` は config 側の `[web]`
- binary は config 側の `binary_path` を正とし、無ければ登録時点の自分自身の絶対パスを焼き込む (llm-gateway と同じ)
- web の config の既定の置き場は `~/.config/hyoui/web/` (合意 2026-10-04。CLI に `web` サブコマンドが挟まる hyoui の構造に合わせ、config にも `web/` を付ける)。例: `web/stable.toml`、`web/unstable.toml`。`daemon add <path>` は任意パスを受けるので既定の置き場であって強制ではない
- 共通部分は `extends` で土台を共有し、unit ごとには `[web] listen` 等だけを上書きする (llm-gateway の stable / unstable は listen だけが違う)。土台も `web/` の中に置く (例: `web/base.toml`)。gateway は PTY session の設定 (DR-0024 の `[session]`) を使わないので `~/.config/hyoui/config.toml` を土台にせず、web の設定は web の中で閉じる (推し)。そのために hyoui の config 読み込みに `extends` (llm-gateway DR-0013: 表は鍵ごとに潜り他は置き換え、相対パスは書いたファイルの隣から、循環はその場で止める) を入れる
- 稼働中の gateway への問い合わせ先は、登録簿の unit から config を引いて listen で組み立てる (llm-gateway DR-0028 決定 6)
- **XDG 系のディレクトリ名は全部 `hyoui/` (リポ名) にし、web はその下の `web/` にする (合意 2026-10-04)**。`hyoui-web` のような別名ディレクトリは作らない。理由: CLI の階層 (`hyoui web ...`) と揃う / auto mode classifier の環境説明は「リポで作業中のセッションから `$XDG_*_HOME/<リポ名>/` へのアクセスを許可」という形で書かれており、`hyoui-web` だと拒否されるリスクが高く、classifier 側に例外を持ち込むより置き場をその単純な形に合わせる
  - 対象 (現状 `hyoui-web` を使っている箇所): 状態 `~/.local/state/hyoui-web/` (登録簿 `crates/hyoui-cli/src/web_daemon/registry.rs` 221 付近、passkey の `auth.json` / `pending.json` `crates/hyoui-web/src/auth/store.rs` 97 付近、logs、supervisor.sock) → `~/.local/state/hyoui/web/`。service のログ `~/Library/Logs/hyoui-web/` (`crates/hyoui-cli/src/web_service.rs` 111 付近) → 状態の中 `~/.local/state/hyoui/web/logs/` (先行の llm-gateway / ccmsg も状態の中)。`--help` の記述 (`crates/hyoui/src/cli.rs` 2840 / 2982 / 3117 付近) と test の期待値も
  - 移行手順が要るが v1.0 前なので互換は残さず同じ是正で動かす (passkey の `auth.json` は移さないと登録済みの passkey が全部失効するので、移行は必須)
- service の env 固定 (パターンからの逸脱): reference `cli-daemon-subcommands` は「`service register` は場所の導出に効く env (`XDG_STATE_HOME` / `XDG_CONFIG_HOME` / `HOME` 等) を register 時の値で unit に固定し、re-register で値が違えば差分を示して止まる (`--force` で上書き)。client 側にも検知を持つ」とする。現 plist (`jp.kawaz.hyoui-web.supervise`) の `EnvironmentVariables` は `PATH` だけ (`crates/hyoui-cli/src/web_service.rs` 54 付近) で、固定も差分検知も無い。今ずれていないのは kawaz の shell の XDG が既定値と同じだからにすぎない。config と状態の置き場を env から導く今回の是正と同じ中で入れる。固定する変数の一覧は path 導出コードが読む変数を正とし、unit 生成側に別のリストを持たない。先行例 (2026-10-04 確認): llm-gateway は XDG 2 つを固定、ccmsg は `PATH` / `HOME` / `CCMSG_*_DIR` / XDG を固定。どちらも re-register の差分で止まる処理と client 側の検知は未実装 (ccmsg は既存 issue `2026-09-30-service-pins-path-env-and-refuses-drift`、llm-gateway は `2026-10-04-service-reregister-env-drift-not-detected` を起票)。ccmsg は PATH が re-register ごとに重複して膨れている (同 issue に記載済み)。hyoui は先行例でなく reference の形をそのまま入れる
- 既存の stable / unstable 2 unit の移行 (v1.0 前なので互換を残さず置き換えてよい範囲)
- DR-0034 の該当部分を新しい DR で置き換える

## 同じ根: `hyoui web --listen` が 2 本目の起動経路として残っている

`hyoui web --listen=<addr>` が unit も登録簿も通さず gateway を foreground で起動する。名前を省いた `hyoui web daemon run` (config の listen で起動) と同じことをする 2 本目の経路。パターンでは foreground 起動は `daemon run` 1 本で、`hyoui web` は `daemon` / `service` / `passkey` / `session` を束ねる名前空間でしかない。DR-0031 の 1 unit 固定時代に plist が `hyoui web` の 2 語で直接起動していた頃の入口で、DR-0034 で監督者経由に移った後も消していない (現 plist は supervise)。

消す (`hyoui web` は名前空間だけ、引数なしは help のまま、foreground 起動は `daemon run` に一本化)。直す範囲: e2e test (`crates/hyoui-cli/tests/web_e2e_api.rs`、`auth_store_concurrency.rs`) の起動を `daemon run` 系に置き換え、parse test (`crates/hyoui/src/cli.rs` 11684 付近、`crates/hyoui-cli/src/main.rs` 4666 付近)、`--help` の 1 行目、MANUAL (ja / en) の起動例
