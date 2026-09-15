# DR-0034: `hyoui service` を multi-unit にし、stable / unstable 2 インスタンスの HA を組む

- Status: Active
- Date: 2026-09-15
- Related: DR-0027 (web gateway 同居), DR-0031 (`web service` 単一 unit 登録、本 DR の P4 完了時点で Superseded になる), DR-0006 (CLI 地盤ルール / registry を持たない), DR-0024 (config ファイル機構), DR-0014 (介入 self-check / 検証主義)
- Origin: `docs/issue/2026-09-15-service-subcommand-multi-unit-ha.md` (kawaz 裁定 2026-09-15、QUESTIONS ECO-Q2 への回答)

## Context

### 現状

`hyoui web service register|unregister|status` (DR-0031) は web gateway を **1 unit 固定**で OS に登録する。label は `com.github.kawaz.hyoui-web` 1 つ、listen は `register --listen` で与えた 1 つ、binary は `stable-which` が選んだ 1 つ。実機はこの形で 1 台だけ常駐している (`~/Library/LaunchAgents/com.github.kawaz.hyoui-web.plist`、pid 実在、canddy が `127.0.0.1:43690` へ向けている)。

2 台目を足す手段が CLI に無い。足すなら plist を人が手で書くことになり、「今どの gateway が何番で何のバイナリで動いているか」を CLI が答えられない状態になる。

### 目的

gateway インスタンスを **複数前提**で扱えるようにし、片方が壊れてももう片方が受ける構成を、CLI と reverse proxy の設定だけで再現できるようにする。

ここで増やしたくないもの (目的と同格):

- **常駐プロセスの種類**。gateway 以外の常駐 (監督者など) を増やさない
- **状態の置き場**。unit の定義を OS 側とツール側の二重管理にしない
- **「daemon」という語の意味**。hyoui で daemon は PTY session の daemon (DR-0006 §1: 1 daemon 1 socket 1 子) を指す語であり、gateway を指す第 2 の意味を与えない

### 前提条件と、満たさない場合

| 前提 | 満たさない場合 |
|---|---|
| OS の service manager (launchd / `systemd --user`) が再起動を担う | hyoui 側に再起動の責務が戻る。本 DR の「監督者を持たない」判断 (決定 2) が崩れ、DR-0028 (llm-gateway) 型の supervisor を検討し直すことになる |
| 前段に優先順と fallback を書ける reverse proxy が居る (実機は canddy) | HA は成立しない。本 DR の unit 複数化だけが残り、切り替えは人が行う |
| stable / unstable が **別ビルドの同一 CLI** である | unit ごとの `binary_path` は不要になり、決定 3 の `--binary` が過剰になる |

### OS 常駐にすべき unit の kind は、現時点では web gateway だけ

階層 (`hyoui service` か `hyoui web service` か) の判断軸は「web 以外に OS 常駐させる unit の種類が出てくるか」である (kawaz 提示、2026-09-15)。現行コードと DR 群を洗った結果:

| 候補 | 常駐 unit になるか | 根拠 |
|---|---|---|
| web gateway (`hyoui web`) | **なる** | login 時に上がっていて欲しく、落ちたら上がって欲しい。DR-0031 が既に launchd に載せている |
| PTY session daemon (`hyoui run` が fork するもの) | ならない | 子コマンドと 1:1 で、`run` の呼び出しが unit を決める (DR-0015)。login 時に「何を起こすか」を OS に教える手段が無く、registry を持たない設計 (DR-0006 §1、socket dir が正本) と衝突する |
| tty I/O record (DR-0016) | ならない | daemon 内の bounded queue + writer thread (DR-0016 §8/§8a)。別プロセスではない |
| record の secret redaction (DR-0016 §6 / Phase 5) | ならない | 同じく daemon の hot path 内 (`crates/hyoui/src/daemon/record.rs`) |
| screen region watch + 検出通知 (`docs/issue/2026-07-21-screen-region-watch-api.md`) | ならない | 母体は DR-0025 Screen domain の `WatchRegistration` で daemon 内。外部への通知経路は CLI と web gateway が兼ねる |
| daemon graceful upgrade (DR-0028) | ならない | 走っているプロセスの self-exec。起動させる対象ではない |

**将来 kind が増える見込みが 1 つある。** 「login 時に決まった PTY session を起こす」(常用 session の自動復元) は reference の `daemon add <unit>` / `supervise` がそのまま当てはまる形で、hyoui の実際の使い方 (常時走らせている session がある) からすると現実的である。ただし現時点で issue も DR も無く、本 DR はこれを実装しない。この見込みが階層の判断に効くため、Q3 として裁定に出す。

### 参照した先行実装

llm-gateway は同じ reference 体系を先に当てている (DR-0028)。unit = 設定ファイル 1 つ、登録簿は `~/.local/state/llm-gateway/daemon/units/<name>.toml`、unit ごとに `binary_path` を持ち (stable = brew / unstable = repo build)、`daemon supervise` 1 つだけを launchd に載せる形。本 DR は **unit と binary を分ける判断は踏襲し、監督者を置く判断は踏襲しない** (理由は決定 2)。

## 介入判断 self-check (CLAUDE.md / DR-0014)

- PTY / child / signal / protocol への介入は無い。DR-0031 と同じく OS service manager に起動を依頼する運用層で、透過原則を変更しない
- 新 protocol message / cap flag / daemon state を追加しない。unit の状態は OS 側に聞く
- OS 標準機能を再実装しない。再起動・login 時起動・プロセス監視は launchd / systemd に委ねる (決定 2)
- 既存 DR の実装漏れではない。DR-0031 は実装済で、本 DR はその適用範囲を広げる

## Decision

### 1. verb 群は `service` 側だけを採る

本 DR が決めるのは **verb 群と unit モデル**で、その前に付く階層 (`hyoui service` か `hyoui web service` か) は Q3 の裁定で決まる。以下の表記は `hyoui service` を仮に置いたもので、`hyoui web service` に決まれば prefix が 1 語増えるだけで中身は変わらない。

```text
hyoui service add <name> [--port=<n> | --listen=<host:port>] [--binary=<path>] [--assets-dir=<path>]
hyoui service remove <name>
hyoui service list
hyoui service start   <name> | --all
hyoui service stop    <name> | --all
hyoui service restart <name> | --all
hyoui service status  [<name>] | --all
hyoui service log     [<name>] | --all [--follow]
```

reference (`cli-daemon-subcommands.md`) の `daemon` / `service` 2 系統のうち、**本 DR が採るのは `service` 側だけ**。`daemon` 群 (= instance のプロセス操作) を新設しない理由は 2 つある。

**1. `daemon` 群は hyoui に既にある。unit が PTY session なだけ。**

| reference の `daemon` verb | hyoui の既存コマンド (unit = PTY session) |
|---|---|
| `run [unit]` | `hyoui run [--name X] -- cmd` (`--detached` で初期 detach) |
| `list` | `hyoui list` (socket dir 走査) |
| `status [<unit>]` | `hyoui status <session>` |
| `stop <unit>` | `hyoui kill <session>` |
| `log [<unit>]` | `hyoui tail <session> [--follow]` |
| `add` / `remove` / `start` / `restart` / `supervise` | 無い。session は `run` の呼び出しが unit を決めるので、登録して後から起こす概念が存在しない (DR-0006 §1) |

つまり hyoui の `daemon` 群は subcommand 階層を持たず top-level に平置きされている形で、意味としては既に埋まっている。ここに `hyoui daemon` を新設すると、同じ語が PTY session と gateway の 2 つを指すことになる (Context の「増やしたくないもの」に反する)。

**2. gateway 側の instance を起こすのは OS で、ツールではない。** 決定 2 のとおり監督者を置かないので、ツール側に「プロセスを起こす口」は無く、あるのは「OS への希望を伝える口」だけ。2 系統に割る対象が存在しない。

reference の verb のうち採らないものと理由:

| verb | 扱い |
|---|---|
| `run [unit]` | 既存の `hyoui web [--listen=...]` が同じもの。新しい verb を作らず、これを foreground 起動の口として維持する |
| `supervise` | 採らない。監督者を置かない判断 (決定 2) の帰結。PTY session の監督とも無関係 |
| `register` / `unregister` | `add` / `remove` に吸収する。unit の登録と OS への登録が同じ行為になった (決定 2) ので、2 つの verb に割る意味が消えた |

`--all` は登録 unit 全部を対象にする。`hyoui service` の引数なし実行と、必須引数を欠く verb は help を出す。help 以外の出力は JSON、`log --follow` は JSONL、エラーは JSON を stderr に出して exit を非 0 にする (reference の出力規約)。

### 2. unit は OS の service 1 つ。監督者を置かない

unit 1 つにつき launchd job / systemd user unit を 1 つ載せる。`add` は定義を書いて enable + start まで行い、`remove` は stop + 定義削除まで行う。`start` / `stop` / `restart` / `status` は `launchctl` / `systemctl --user` への shell-out で、`enabled` (desired state) も OS 側が持つ。

llm-gateway (DR-0028 §3) が監督者を置いたのは、監督者 1 つを launchd に載せて子を複数抱える形を採ったため。hyoui でそれを真似ると、launchd が既に提供している「落ちたら上げる」「login で上げる」を hyoui 内に作り直すことになり、CLAUDE.md の self-check (`kernel / OS の標準機能を再発明していないか`) に反する。gateway は互いに独立で、順序依存も共有状態も無いため、束ねる理由が無い。

この判断の帰結として、監督者用の unix socket・制御 protocol・backoff・ログ集約 (DR-0028 §10/§11 が扱っている問題) は hyoui には発生しない。

### 3. unit の識別子は名前。差分は option で与える

`<name>` は unit の識別子で、`add` の必須位置引数。`[A-Za-z0-9_-]{1,32}` に限り、path separator を含む名前は拒否する (定義ファイル名に使うため)。

`add` が受ける差分:

| option | 意味 | 既定 |
|---|---|---|
| `--port=<n>` | `--listen=127.0.0.1:<n>` の短縮 | — |
| `--listen=<host:port>` | bind 先 | config `[web].listen`、無ければ `127.0.0.1:43690` |
| `--binary=<path>` | この unit が起動する実行ファイル | DR-0031 と同じ `resolve_stable_path(current_exe, SameBinary)`。安定な path が無ければ現在の path を焼き、stderr と出力の `warning` に理由を添える |
| `--assets-dir=<path>` | 静的 assets の差し替え (dev) | 未指定なら焼かない (embedded assets) |

`--port` と `--listen` の同時指定はエラー。`--binary` を unit ごとに持つのは、stable (brew の `/opt/homebrew/bin/hyoui`) と unstable (repo の `target/release/hyoui`) を同時に走らせることが本 DR の動機そのものだからで、1 系統に統一するとその前提が消える。

`add` は冪等。描いた定義が既に同じ内容で置かれ OS 側にも載っていれば何もせず `changed: false`、違えば同じ label を降ろして置き換え載せ直して `changed: true` を返す。「既に登録されている」を理由に断ると、中身を直したいだけの操作に `remove` を挟ませることになる。

### 4. 定義ファイルが正本。登録簿を別に持たない

| 意味 | macOS | Linux |
|---|---|---|
| label / unit 名 | `jp.kawaz.hyoui-web.<name>` | `hyoui-web-<name>.service` |
| 定義 path | `~/Library/LaunchAgents/jp.kawaz.hyoui-web.<name>.plist` | `$XDG_CONFIG_HOME/systemd/user/hyoui-web-<name>.service` (`~/.config` fallback) |
| 起動 argv | `<binary> web --listen=<listen> [--web-assets-dir=<path>]` | 同じ |
| login 時起動 | `RunAtLoad=true` | `WantedBy=default.target` + enable |
| 継続起動 | `KeepAlive=true` | `Restart=always` |
| 環境 | 最小 `PATH` のみ | 最小 `PATH` のみ |
| log | `~/Library/Logs/hyoui-web/<name>.log` | journald |

label に kind (`hyoui-web`) が入り、`<name>` はその中での識別子になる。unit 名に kind を含めないので、Q3 がどちらに決まっても label 規約は変わらず、kind が増えた時は prefix が別 (`jp.kawaz.hyoui-<kind>.<name>`) になるだけで既存 unit を踏まない。

`list` は定義ディレクトリを label prefix (`jp.kawaz.hyoui-web.` / `hyoui-web-`) で走査して unit を数える。unit の属性 (listen / binary / assets_dir) は定義の argv から復元する。

別に登録簿ファイルを置かないのは、置けば OS 側の定義と二重になり、片方だけ人手で触られた時にどちらが正かを決められなくなるため。DR-0006 §1 が `hyoui list` に対して採った判断 (registry を持たず socket dir を正本にする) と同じ形を service にも適用する。

renderer は DR-0031 の `render_launchd_plist` / `render_systemd_unit` を unit 名でパラメタ化して再利用する。plist / unit は固定の小さな形なので、純関数 renderer + golden test を維持する (DR-0031 §5)。

### 5. gateway に `/healthz` と `/version` を足す

`hyoui web` の現在の route は `/`、`/sessions/{id}`、`/assets/{*path}`、`/api/sessions*` だけで、死活監視に使える口が無い (実装 `crates/hyoui-web/src/lib.rs:72-80` で確認)。

- `GET /healthz` → 200、body `ok`、認証なし
- `GET /version` → 200、`{"version": "<crate version>"}`、認証なし

`/api/` の下に置かないのは、`/api/*` が session 一覧という業務機能で、その仕様変更 (認証追加・スキーマ変更・daemon socket 走査の失敗) が可用性監視を壊すため。canddy が llm-gateway で `/v1/models` から `/llm-gateway/healthz` へ移した理由 (`Caddyfile:179-185` のコメント) と同じ。

`/healthz` は **プロセスが HTTP を返せること**だけを意味し、PTY session の有無・daemon socket の健全性は含めない。gateway は session が 0 でも正常である。

`status` はこの `/version` を各 unit の listen に聞き、`version` として載せる。答えない版が走っていることはあるので、答えられなければ `null` (異常ではない)。

### 6. `restart --all` は 1 台ずつ

登録の逆順に 1 台ずつ停止 → 起動し、その unit の `/healthz` が 200 を返してから次へ進む。前段が優先順で振り分ける構成 (決定 7) では、順に上げ直せば外から見た断が出ない。全台を同時に落とす経路は持たない。

### 7. stable / unstable の HA は前段 proxy が担う。hyoui は担わない

hyoui 側が持つのは「2 unit を独立に常駐させること」と「死活を答える口」だけ。優先順・fallback・health check の間隔は canddy (Caddy) の設定が持つ。

想定する形 (実機の llm-gateway ブロック `Caddyfile:174-194` と同型):

```caddyfile
@hyoui host hyoui.kawaz-mbp16-20211217.kawaz.jp
handle @hyoui {
	reverse_proxy 127.0.0.1:43691 127.0.0.1:43690 {
		lb_policy first

		health_uri /healthz
		health_interval 5s
		health_timeout 2s
		health_status 200

		fail_duration 10s
		lb_try_duration 5s
		lb_try_interval 100ms
	}
}
```

`fail_duration` + `lb_try_duration` を併用するのは、active health check だけだと稼働中の upstream が落ちた瞬間のリクエストが 502 になるため (canddy 側で実測済み、`Caddyfile:163-169`)。`unhealthy_status` は指定しない。

**WebSocket attach (`/api/sessions/{id}/attach`) は fallback の対象外**。確立済みの WS は選ばれた upstream に固定され、その unit が落ちれば切れる。回るのは新規接続だけで、これは reverse proxy の性質であって hyoui 側で埋められるものではない。client 側の再接続の要否は本 DR の範囲外とし、必要なら別 issue で扱う。

canddy 側の変更は **canddy リポの issue として依頼する**。設定の正本は canddy が持ち、hyoui が書き換えない。

### 8. 既存 `hyoui web service register|unregister|status` は廃止する

alias も移行期間も設計に入れない。この CLI の利用者は kawaz だけで、破壊的変更を受ける第三者が存在しない (kawaz 明言、2026-09-15)。

移行は既存 1 台 (`com.github.kawaz.hyoui-web`) だけが対象で、`launchctl bootout gui/$UID/com.github.kawaz.hyoui-web` → plist 削除 → `hyoui service add stable --port=43690` の 3 手。手順は runbook に置く。

label の名前空間が `com.github.kawaz.hyoui-web` から `jp.kawaz.hyoui-web.<name>` に変わるので、移行の途中で両方が載っても互いを踏まない。

`hyoui web` 自身 (foreground 起動) と `--listen` / `--web-assets-dir` は変わらない。

## やらないこと

| やらないこと | 理由 |
|---|---|
| PTY session daemon の multi-unit 化 | session は既に複数走り、socket dir が正本 (DR-0006 §1/§2)。本 DR の unit は gateway インスタンスに限る |
| hyoui 自身が優先順付き proxy / HA を持つ | 目的「増やしたくないもの: 常駐プロセスの種類」に反する。前段 proxy が既に持つ機能の再実装になる |
| 監督者プロセス (`supervise`) | 決定 2。OS service manager が担う |
| gateway 間の状態共有・session の引き継ぎ | 各 gateway は daemon socket を走査するだけで自前の状態を持たないので、共有すべき状態が無い |
| ログ回転 | 追記のみ。回転は OS の仕組み (newsyslog / logrotate) に任せる |

## Alternatives Considered

| 案 | 不採用理由 |
|---|---|
| 旧 `web service register/unregister/status` を alias として温存する | 同じことをする口が 2 つ増え、help と completion にも 2 つ載る。利用者は kawaz だけで、互換のために語彙を濁す相手が居ない |
| `hyoui daemon` 群を新設して gateway のプロセス操作を置く | 決定 1。同じ語が PTY session と gateway の 2 つを指すことになり、既存の `run` / `list` / `status` / `kill` / `tail` が扱う対象と読み分けられなくなる |
| unit を port で識別する (`service add --port` だけで名前なし) | port は「今どこで待つか」であって unit の同一性ではない。port を変えた瞬間に別 unit になり、`stable` の設定を 43690 → 43695 に移す操作が表現できない。stable / unstable という運用上の役割も名前でしか書けない |
| hyoui 自身が front で受けて背後の 2 台に振る | 常駐プロセスが 1 種類増え、その front 自体が単一障害点になる。HA を足したつもりで可用性が下がる |
| llm-gateway と同じく監督者 1 つを OS に載せる | 決定 2。launchd / systemd が持つ機能を作り直すことになる。llm-gateway が採ったのは「1 監督者が別ビルドの子を複数抱える」形を先に決めたためで、hyoui にその前提は無い |
| unit 定義を `~/.config/hyoui/config.toml` に `[[web.unit]]` として書く | config.toml は人が編集する設定 (DR-0024) で、OS 側の定義は結局そこから生成することになる。同じ事実が 2 箇所に載り、片方だけ手で直された時にどちらが正かを決められない |
| 死活監視に `/api/sessions` を使う | 業務エンドポイントの変更が可用性監視を壊す。canddy が llm-gateway で同じ理由で移した実績がある |
| `/healthz` が daemon socket の健全性まで見る | gateway は session が 0 でも正常。daemon 側の異常で gateway を切り離すと、復旧の入口 (Web UI) ごと失う |

## Implementation phases

| Phase | 内容 | gate |
|---|---|---|
| P1 | `GET /healthz` / `GET /version` を gateway に追加 | 単体 test で 200 / JSON 形。実機で `curl` 確認 |
| P2 | label / 定義 path / renderer の unit 名パラメタ化、`add` / `remove` / `list` | golden test (2 unit 分の plist / systemd unit)、名前 validation test、隔離 HOME での CLI E2E |
| P3 | `start` / `stop` / `restart` / `status` / `log` (`--all` / `--follow` 含む) | 実機で stable + unstable の 2 台常駐。`status` が両方の pid と version を出す。`restart --all` 実行中に前段経由のリクエストが落ちないことを観測 |
| P4 | 旧 `web service register/unregister/status` 撤去、help / completion / 実装の 3 者同期、移行 runbook | `hyoui service --help` と completion 定義の突き合わせ test。既存 1 台の移行を実機で完了 |
| P5 | canddy リポへ upstream 設定の issue 起票、HA 実機検証 | unstable を `stop` した状態で新規リクエストが stable に回ることを観測。stable のみ停止でも同様。両立して初めて完了 |

P3 の「断が出ないこと」の検証は、DR-0014 の検証主義に従い 1 回の観察で結論しない。停止させる側 (unstable / stable)、停止のさせ方 (`stop` / `kill -9`)、リクエストの種類 (HTML / `GET /api/sessions`) の組合せでマトリクスを埋める。WS は決定 7 のとおり fallback 対象外なので、対象外であることを観測結果として記録する (「該当なし」ではなく「切れることを確認した」と書く)。

## Consequences

- gateway の常駐が unit 単位になり、台数の増減が CLI で完結する。plist を人が書く経路が無くなる
- unit ごとに binary が違ってよくなるため、「壊れた変更を unstable に閉じ込める」dogfooding が常時成立する
- `/healthz` / `/version` という無認証の口が 2 つ増える。どちらも session の内容を返さない
- 前段 proxy への依存が製品の可用性設計に入る。canddy が落ちれば tailnet 経由の到達は失われる (127.0.0.1 直結は影響を受けない)
- systemd 経路は DR-0031 と同じく **書けるが未検証**のまま残る。macOS の launchd 経路だけが実証済みという扱いを維持する

## Open Questions

- **Q1: unstable の port をいくつにするか。** 統括推し: stable を既存の `43690` のまま据え置き、unstable を `43691` にする。canddy の hyoui ブロックは現在 43690 単体を指しており、stable を動かさなければ移行中も到達が切れない。llm-gateway が 11301/11302 と連番を取っているのと同じ形。
- **Q2: 既存 1 台の移行を `add` が引き取るか、runbook の手作業にするか。** 統括推し: 手作業 (`launchctl bootout` + plist 削除) にする。`add` に「旧 label を探して降ろす」経路を入れると、二度と通らないコードが製品に残る。対象は 1 台だけで、runbook 3 行で足りる。

- **Q3: verb 群を `hyoui service` に置くか、`hyoui web service` に置くか。** 判断軸は「web 以外に OS 常駐させる kind が出てくるか」(kawaz 提示)。Context の洗い出しでは現行の kind は web gateway のみで、将来候補として「login 時に決まった PTY session を起こす」が 1 つ挙がる。

  この DR の推し: **`hyoui web service` を維持する。** 理由は option 集合が kind と一緒に動くこと。`--port` / `--assets-dir` は gateway 固有で、session を起こす kind が来れば必要なのは `-- cmd args...` と namespace になる。共通の `hyoui service add` に両方を載せると、kind に応じて有効な option が分岐する形 (`--kind` で枝分かれする option 集合) になり、これは help でも completion でも説明しづらい。kind ごとに `hyoui web service` / `hyoui <kind> service` と階層で割れば、各階層の option 集合が閉じる。`hyoui web` 配下に web の運用が全部あるのも素直。

  対立する見方 (統括の暫定推し): 常駐 kind が 1 種類しかない今の時点で階層を 1 段深くするのは、将来の仮定的要件のために複雑さを先に払う形になる。`hyoui service` で始めて、kind が実際に増えた時に `hyoui web service` へ動かせばよい (v1.0 未満なので動かせる)。

  どちらもコストはほぼ同じ (verb 群の実装は 1 kind 分で変わらない)。違うのは、後から動かす時に runbook と plist の label 名を書き直すかどうかだけ。上記の「session を起こす kind」に手を付ける見込みがあるかは kawaz にしか判断材料が無いので、裁定に出す。

## 参照した素材

- `docs/issue/2026-09-15-service-subcommand-multi-unit-ha.md`
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/cli-daemon-subcommands.md`
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/design-spec/spec-preflight.md`
- `~/.local/share/repos/github.com/kawaz/llm-gateway/main/docs/decisions/DR-0028-daemon-service-subcommands.md`
- `~/.local/share/repos/github.com/kawaz/llm-gateway/main/crates/llm-gateway/src/daemon/registry.rs` / `protocol.rs`、`crates/llm-gateway-cli/src/service.rs` / `service/platform.rs`
- `~/.local/share/repos/github.com/kawaz/canddy-app-proxy/main/README.md` / `Caddyfile` (hyoui ブロック 127-133、llm-gateway ブロック 144-194)
- 本リポ: `docs/decisions/DR-0031-web-service-subcommand.md`、`docs/decisions/DR-0006-cli-ground-rules.md`、`crates/hyoui-cli/src/web_service.rs`、`crates/hyoui-web/src/lib.rs`、`crates/hyoui/src/config/mod.rs`
- 実機出力: `hyoui --help` / `hyoui web --help` / `hyoui web service --help` / `hyoui web service status` (`/opt/homebrew/bin/hyoui` 0.9.42)

## 関連

- DR-0031 — 本 DR が置き換える単一 unit 版の service 登録
- DR-0027 — gateway を同 repo に置く判断
- DR-0006 — registry を持たずファイルシステムを正本にする形の初出
- DR-0014 — 介入 self-check とマトリクス検証
