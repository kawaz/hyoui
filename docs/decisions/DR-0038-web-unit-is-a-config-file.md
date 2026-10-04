# DR-0038: web の unit を config ファイル 1 つにし、置き場を `hyoui/web/` に揃え、service に場所の env を固定する

- Status: 🚧 Active (2026-10-04)。決定 1〜8 は実装済。既存 2 unit の移行は未実施
- Date: 2026-10-04
- Supersedes (部分): DR-0034 決定 1 の `add` の形と `hyoui web` 単体起動 / 決定 2 の unit の中身と置き場 / 決定 6 の環境と log の置き場 / 決定 9 の log の置き場 / 決定 11 の「`hyoui web` 自身は変わらない」、DR-0036 決定 4 の `auth.json` / `pending.json` の置き場
- Related: DR-0034 (2 系統の体系と監督者、本 DR が置き換えない部分はすべて有効), DR-0024 (config ファイル機構), DR-0018 (session namespace と socket dir), DR-0036 (passkey の state file), DR-0014 (介入 self-check)
- Origin: `docs/issue/2026-10-04-web-unit-registry-holds-settings.md` (kawaz と合意 2026-10-04)

## Context

DR-0034 は reference `cli-daemon-subcommands` (kawaz 製 CLI の `daemon` / `service` 体系の正本) と先行例 llm-gateway (DR-0028 / DR-0013) を「そのまま持ち込む」としながら、unit の中身だけを変えていた。登録簿 `units/<name>.toml` に `listen` / `binary` / `web_assets_dir` / `enabled` を解決して書き、設定ファイルを持たない形である。

この形には 3 つの不都合がある。

- **設定値が状態の置き場にある。** ポートを知るには state dir の登録簿を読むしかなく、config の置き場を見ても分からない。reference は unit を「登録の単位で、案件ドメインが決める (dir / config ファイル / id など)」とし、`daemon list` の出力を `{id, unit, enabled, config, binary_path}` とする。登録簿が持つのは config への参照で、設定値そのものではない
- **foreground 起動の口が 2 本ある。** `hyoui web --listen=<addr>` が unit も登録簿も通さず gateway を起動し、名前を省いた `hyoui web daemon run` と同じことをする
- **service が場所の env を固定しない。** plist の `EnvironmentVariables` は `PATH` だけで、launchd から起きた監督者と shell から起きた CLI が別の dir を見ても気づく手段が無い。reference はこれを避けるため env の固定と差分での停止を求めている

加えて置き場の名前が CLI の階層と揃っていない (`hyoui web ...` に対して state は `hyoui-web/`、service のログは `~/Library/Logs/hyoui-web/`)。

## 介入判断 self-check (CLAUDE.md / DR-0014)

- PTY / child / signal / protocol (CBOR / cap flags) への介入は無い。変えるのは web gateway の運用層 (config の読み方、登録簿の形、OS 登録の中身) だけ
- 新しい protocol message は無い。監督者の制御 socket (JSON 1 行) の応答に field が増え、名前が変わる (決定 7)
- 既存 DR の実装漏れの修復を含む: DR-0034 が採ると言った reference / llm-gateway の形 (unit = config ファイル) と、reference の env 固定を入れる

## Decision

### 1. unit = 任意 path の config ファイル 1 つ。設定値は config の `[web]` が正本

```toml
# ~/.config/hyoui/web/base.toml
[web]
listen = "127.0.0.1:43690"

# ~/.config/hyoui/web/unstable.toml
extends = "base.toml"
[web]
listen = "127.0.0.1:43691"
binary_path = "~/src/hyoui/target/release/hyoui"
```

`[web]` の鍵は `listen` (既定 `127.0.0.1:43690`) / `assets_dir` (無ければ埋め込み assets) / `binary_path` (決定 2)。

**web の設定は web の config の中で閉じる。** gateway は PTY session の設定 (`config.toml` の `[scrub_env]` / `[attach]` / `[session]`) を使わないので、`config.toml` を土台にせず、gateway の起動経路は `config.toml` を読まない。`config.toml` から `[web]` を外し、`[web] listen` / `[web] assets_dir` が書かれていれば移し先を案内して起動を断る (DR-0032 §1 の廃止 key と同じ扱い。黙って無視すると、書いた人の意図が既定値に倒れる)。

### 2. 登録簿は `{config, binary_path, enabled, added_at}`。`add` は config の path を取る

```text
hyoui web daemon add [--name <name>] <config-path>
hyoui web daemon run [<name>]
```

- `add` は config を絶対 path にして登録簿に書く。symlink は解かない (= 利用者が symlink の向き先を差し替えれば unit も追従する)。name を省けば config の basename から拡張子を除いたもの (llm-gateway と同じ)。登録の時点で config が読めることを確かめ、読めなければ断る (= 監督者が起こすたびに子が config で落ちる unit を作らない)
- `binary_path` は config の `[web].binary_path` を正とし、無ければ `add` を打った自分自身 (`current_exe`) を焼く。`add` の時点で登録簿に写すので、config の `binary_path` を変えたら `remove` → `add` で入れ直す (llm-gateway DR-0028 決定 2 と同じ)。`resolve_stable_path` を通さない理由は DR-0034 決定 2 のまま
- `listen` / `assets_dir` は登録簿に写さない。子の `daemon run <name>`・監督者・`list` / `status` が読むたびに config から引く。config を書き換えれば次の起動から効き、登録し直す必要が無い
- `add` の listen 衝突の検査 (DR-0034 決定 2) は残す。既存 unit の listen は各 config から引き、読めない config の unit は比べず warning にする。port 0 (= kernel に任せる) はどれとも衝突しない
- 登録簿は `deny_unknown_fields` のままにし、設定値を書いた古い形のファイルは読まない (= 一部の鍵だけを黙って使わない)

**名前を省いた `daemon run` は web の config の既定 path (`$XDG_CONFIG_HOME/hyoui/web/config.toml`) を読み、無ければ組み込みの既定値で起動する。** 登録簿は見ない。reference の「未指定の場合はデフォルト」をここで満たし、DR-0034 決定 1 の「既定の unit は持たない」(= 登録簿に 1 つしか無い時それを選ぶ推測を入れない) も保つ。既定の置き場の `config.toml` は「名前なしで起動した時の設定」で、unit として登録する必要は無い。

### 3. config は `extends` で土台に重ねる

llm-gateway DR-0013 の規則をそのまま採る。`config.toml` と web の config の両方で使える。

- `extends = "<path>"` (top level) で土台を指す。手前 (= `extends` を書いた側) が勝つ
- **表は鍵ごとに潜り、それ以外 (数・文字列・真偽・配列) は丸ごと置き換える。** 配列を要素ごとに混ぜない (並びが意味を持つ値で、土台の要素がどこに割り込むかを予想しながら書く設定にしない)
- **相対パスは、それを書いたファイルの隣から解く。** `extends` の値に加え、web の config の `[web].assets_dir` / `[web].binary_path` も同じ。重ねた後で解くと、土台に書いた相対パスが派生側の位置で解かれてしまうので、ファイルごとに解いてから重ねる。`~` は `$HOME` で開く (= 台をまたぐ dotfiles に置ける)。起動時の cwd は見ない
- 深さは制限しない。**循環はその場で止める。** 辿ったファイルを正規化した実体で覚え (`./a.toml` / `a.toml` / symlink 越しの同じファイルを同一と数える)、戻ってきたら鎖を示して断る
- 指した先が無ければ、**どのファイルがどのパスを指したか**を言う
- 消す手段 (否定) と複数の土台は持たない (DR-0013 の却下理由のまま)

### 4. XDG 系の置き場は `hyoui/` の下、web は `web/`

| 置くもの | 置き場 |
|---|---|
| web の config の既定 | `${XDG_CONFIG_HOME:-~/.config}/hyoui/web/` (例 `base.toml` / `stable.toml` / `unstable.toml`、名前なし `run` は `config.toml`) |
| 登録簿 | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/units/<name>.toml` |
| unit のログ | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/logs/<name>.log` |
| 監督者自身のログ (launchd) | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/logs/<label>.log` |
| passkey の state | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/auth.json` / `pending.json` (+ `.lock`) |
| 監督者の制御 socket | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/run/supervisor.sock` |

`hyoui-web` のような別名の dir は作らない。理由は 2 つ:

- **CLI の階層と揃う。** `hyoui web ...` の state と config が `hyoui/web/` にあれば、どこを見ればよいかを CLI の語から辿れる
- **アクセス許可の書き方と揃う。** auto mode classifier の環境説明は「リポで作業中のセッションから `$XDG_*_HOME/<リポ名>/` へのアクセスを許可」という形で書かれている。`hyoui-web` はこの形から外れて拒否されやすく、classifier 側に例外を持ち込むより置き場をこの単純な形に合わせる

監督者のログを state の中に置くのは先行の llm-gateway / ccmsg と同じで、label の名前にするので unit のログ (unit 名は `.` を含まない) と衝突しない。

**監督者の socket は `web/` 直下に置かず `run/` に 1 段下げる。** `${XDG_STATE_HOME}/hyoui/` は session socket の base (DR-0018) で、discovery (`crates/hyoui/src/discovery.rs`) は直下の dir をすべて namespace とみなし、その中の `*.sock` に hyoui protocol で問い合わせる。`web/supervisor.sock` に置くと、`hyoui list --all-namespaces` と web gateway の `/api/sessions` に namespace `web` の session `supervisor` として並ぶだけでなく、discovery の handshake と監督者の 1 行読み (JSON 1 行の制御 socket、DR-0034 決定 4) が互いの応答を待ち合い、**監督者の event loop が 1 回 5 秒止まる** (実測: `list --all-namespaces` が 5.05 秒、その最中の `web daemon list` が 4.95 秒)。gateway は `/api/sessions` のたびに discovery を回すので、常駐すれば監督者が繰り返し止まる。discovery は 1 段しか潜らないので `run/` の中は見ない。`units/` / `logs/` に `*.sock` は無いので同じ理由で拾われない。

session socket の木と `hyoui/` 直下の機能別 dir の衝突そのものは、session socket を `hyoui/sessions/` に移す設計 (`docs/issue/2026-10-04-design-session-id-uuid-and-tags.md`) が解く。それまでは `hyoui run --namespace=web` の session socket が `hyoui/web/` に置かれうる (= passkey の state と同じ dir)。これは利用者が明示した namespace であり、本 DR では予約語を足して塞がない (足すと namespace を廃止する設計の前に、消える概念へ例外を 1 つ増やすことになる)。

**古い置き場 (`hyoui-web/`、`~/Library/Logs/hyoui-web/`) からの自動移行は書かない。** v1.0 前で利用者は kawaz だけなので、移行は人が 1 回行う。一度通ったら二度と通らないコードを製品に残さない (DR-0034 決定 11 と同じ理由)。passkey の `auth.json` は移さないと登録済みの passkey が全部失効するので、移行手順に必ず含める。

### 5. service register は場所を決める env を定義に固定し、変わったら止まる

reference の節をそのまま入れる。

- **固定する変数の一覧は、場所の導出コードが読む変数そのもの。** core に `hyoui::paths` を置き、`LocationVar` (`HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `XDG_RUNTIME_DIR`) と、それを読んだ snapshot `Env` を唯一の導出口にする。config の path・web の state dir・session socket の base (DR-0018、gateway の discovery が使う) はすべて `Env` から導き、`service register` はこの列挙をそのまま定義に書く。unit 生成側に別のリストを持たない
- **値の無い変数は書かない** (= 「無い」ことも固定される。launchd / systemd は shell の env を継承しない)
- **`PATH` も同じ枡で固定し、register 時に正規化する** (空要素と重複を落とし、最初に現れた位置を保つ)。register を繰り返しても積もらない
- **re-register で、既存の定義に固定された場所の変数が今の値と違えば、差分 (`{name, registered, current}`) を示して何も書き換えずに止まる。** `--force` で置き換える。`PATH` の違いでは止めない (場所を導かず、shell ごとに違うのが普通)。固定値を持たない定義 (= 本 DR 以前に書かれたもの) は「すべて未設定で固定」と読み、同じく `--force` を要る
- **client 側にも検知を持つ。** 監督者は制御 socket の応答 (`supervisor.locations`) で自分が場所を導いた値を名乗り、`daemon list` / `status` / `service status` は自分の値と違えば `warnings` に出す (stderr にも)。監督者に届かない時は、OS の定義に固定された値を自分の値と比べて同じく警告する (= 場所が食い違えば socket の位置も食い違うので、監督者は名乗れない。届かない理由がそれだと言えるのは定義だけ)
- `service log` は監督者のログの path を定義 (`StandardOutPath`) から読む (= 固定値と今の env が違っても、書き手が使っている path を読む)

### 6. 問い合わせ先は unit の config の listen から組み立てる

llm-gateway DR-0028 決定 6 と同じ。監督者の `status` (`/version`)・`restart --all` の `/healthz` 待ち・`list` / `status` の表示は、登録簿の unit から config を引き、その時点の `[web].listen` を使う。config が読めなければ `listen: null` と `config_error` を返し、推し量らない。監督者は config の他の鍵を解釈しない (= 起動のための解釈は子の `daemon run`、DR-0034 決定 3)。

### 7. `hyoui web` は名前空間。foreground 起動は `daemon run` 1 本

`hyoui web` は `daemon` / `service` / `passkey` / `session` を束ねるだけで、gateway を起動する口を持たない。引数なしと `--help` は help、`--listen` 等の option は `hyoui web daemon run` を案内して断る。DR-0034 決定 1 の「bind 先を明示した `hyoui web --listen=<host:port>`」は削除し、`daemon add` の `--port` / `--listen` / `--binary` / `--web-assets-dir` も削除する (= 値は config に書く)。

### 8. 出力の field

- unit の行 (`list` / `status` / `add`) は `config` / `listen` (config から読んだ値、読めなければ `null`) / `config_error` (読めなかった理由) / `binary_path` / `binary_exists` を持つ。実行ファイルの path は reference の `daemon list` の語に合わせて `binary_path` と呼び、`hyoui version` の `supervisor` / `units[]` と `service status` の `version` も同じ名前にする (DR-0034 決定 7a の `binary` を置き換える)
- 監督者の情報 (`supervisor`) に `binary_path` と `locations` (決定 5) が入る
- `service register` の出力に固定した `env` が入る。差分で止まった時は `kind: "location_env_drift"` と `differences` を stderr の JSON に入れる

## 却下した案

| 案 | 理由 |
|---|---|
| 登録簿に設定値を解決して書く (DR-0034 決定 2 の形) | 設定値が状態の置き場に入り、config を見てもポートが分からない。reference の unit / `list` の形から外れる |
| web の設定を `config.toml` の `[web]` に残し、unit ごとの差分だけ別ファイルにする | gateway が使わない PTY session の設定と同じファイルに web の設定が混ざり、unit の config の土台が「何でも入る `config.toml`」になる。web の設定は web の中で閉じる |
| `binary_path` を起動のたびに config から引く | llm-gateway と食い違い、監督者が子を起こす直前に config を解釈することになる (= 解釈は子、DR-0034 決定 3)。変える時は `remove` → `add` |
| `hyoui web --listen` を `daemon run` の別名として残す | 同じことをする口が 2 本になる。利用者は kawaz だけで、互換のために語彙を濁す相手が居ない |
| 古い置き場からの自動移行 | 一度しか通らないコードが製品に残る。移行は人が 1 回行う |
| `PATH` の違いでも re-register を止める | 場所を導かない変数で止めると、shell ごとに違う `PATH` のせいで毎回 `--force` を要る |
| 固定する変数を unit 生成側で列挙する | 導出コードが読む変数と食い違っても気づけない。列挙は `hyoui::paths::LocationVar` 1 箇所 |

## Consequences

- unit の設定は config ファイルを開けば読め、書き換えれば次の起動から効く。登録簿は「どの config を、どの実行ファイルで、動かしたいか」だけになる
- `config.toml` に `[web] listen` / `[web] assets_dir` を書いていると、全コマンドの config 読み込みが廃止 key で止まる。移し先は案内に出る
- 既存の登録簿・passkey の state・service の定義は新しい置き場に無い。移行するまで `daemon list` は空、passkey は「登録が無い」になる (移行は統括が手で 1 回行う)
- 既存の plist は場所の env を固定していないので、最初の `service register` は差分で止まり `--force` が要る
- `hyoui-web` の名を持つものは crate 名 / ログの接頭辞と OS 登録の label (`jp.kawaz.hyoui-web.supervise`) だけになる。label は OS に登録した契約名なので変えない
- systemd 経路は DR-0034 と同じく書けるが未検証

## 参照した素材

- `docs/issue/2026-10-04-web-unit-registry-holds-settings.md`
- reference `cli-daemon-subcommands` (claude-rules-personal、`daemon` / `service` 体系と「`service register` は場所を決める env を unit に固定し、変わったら止まる」の節)
- llm-gateway `docs/decisions/DR-0028-daemon-service-subcommands.md` (決定 2 / 6) / `DR-0013-config-extends.md`、`crates/gateway-core/src/daemon/registry.rs`、`crates/llm-gateway-cli/src/daemon.rs` / `daemon/run.rs`
- 本リポ: DR-0034 / DR-0031 / DR-0024 / DR-0036 / DR-0018、`crates/hyoui/src/discovery.rs`、`crates/hyoui-cli/src/socket_path.rs`、`docs/issue/2026-10-04-design-session-id-uuid-and-tags.md`
