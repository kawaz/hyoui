# DR-0034: `hyoui service` を multi-unit にし、stable / unstable 2 インスタンスの HA を組む

- Status: Active
- Date: 2026-09-15
- Related: DR-0027 (web gateway 同居), DR-0031 (`web service` 単一 unit 登録、本 DR の P4 完了時点で Superseded になる), DR-0006 (CLI 地盤ルール / registry を持たない), DR-0024 (config ファイル機構), DR-0014 (介入 self-check / 検証主義)
- Origin: `docs/issue/2026-09-15-service-subcommand-multi-unit-ha.md` (kawaz 裁定 2026-09-15、QUESTIONS ECO-Q2 への回答)

## Context

### 現状

`hyoui web service register|unregister|status` (DR-0031) は web gateway を **1 unit 固定**で OS に登録する。label は `com.github.kawaz.hyoui-web` 1 つ、binary は `stable-which` が選んだ 1 つ。実機はこの形で 1 台だけ常駐しており、plist の `ProgramArguments` は `/opt/homebrew/bin/hyoui web` の 2 語 — **`--listen` は焼かれていない**ので、待ち先は起動時に config (無ければ既定の `127.0.0.1:43690`) から決まる。canddy はその 43690 を指している。

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

**将来 kind が増える見込みが 1 つある。** 「login 時に決まった PTY session を起こす」(常用 session の自動復元) は reference の `daemon add <unit>` / `supervise` がそのまま当てはまる形で、hyoui の実際の使い方 (常時走らせている session がある) からすると現実的である。ただし現時点で issue も DR も無く、本 DR はこれを実装しない。この見込みが階層の判断に効くため、OQ-A として裁定に出す。

### 参照した先行実装

llm-gateway は同じ reference 体系を先に当てている (DR-0028)。unit = 設定ファイル 1 つ、登録簿は `~/.local/state/llm-gateway/daemon/units/<name>.toml`、unit ごとに `binary_path` を持ち (stable = brew / unstable = repo build)、`daemon supervise` 1 つだけを launchd に載せる形。本 DR は **unit と binary を分ける判断は踏襲し、監督者を置く判断は踏襲しない** (理由は決定 2)。

## 介入判断 self-check (CLAUDE.md / DR-0014)

- PTY / child / signal / protocol への介入は無い。DR-0031 と同じく OS service manager に起動を依頼する運用層で、透過原則を変更しない
- 新 protocol message / cap flag / daemon state を追加しない。unit の状態は OS 側に聞く
- OS 標準機能を再実装しない。再起動・login 時起動・プロセス監視は launchd / systemd に委ねる (決定 2)
- 既存 DR の実装漏れではない。DR-0031 は実装済で、本 DR はその適用範囲を広げる

## Decision

### 1. reference の 2 系統を `service` 1 系統に畳む

本 DR が決めるのは **verb 群と unit モデル**で、その前に付く階層 (`hyoui service` か `hyoui web service` か) は OQ-A の裁定で決まる。以下の表記は `hyoui service` を仮に置いたもので、`hyoui web service` に決まれば prefix が 1 語増えるだけで中身は変わらない。

```text
hyoui service add <name> [--port=<n> | --listen=<host:port>] [--binary=<path>] [--web-assets-dir=<path>]
hyoui service remove <name>
hyoui service list
hyoui service start   <name> | --all
hyoui service stop    <name> | --all
hyoui service restart <name> | --all
hyoui service status  [<name>] | --all
hyoui service log     [<name>] | --all [--follow]
```

この verb 集合は **reference の `daemon` 群 (`add` / `remove` / `list` / `start` / `stop` / `restart` / `status` / `log`、`--all` 込み) を `service` の名前の下に置いたもの**で、「`service` 側の verb だけを採った」わけではない。reference で 2 系統に割れていたものが、hyoui では 1 系統に畳まれる。畳まれる理由は 2 つある。

**1. `hyoui daemon` という名前は使えない。PTY session が既に埋めている。**

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

`status` の形も reference から変わる。reference の `service status` は監督者 1 つの状態に `instances: [...]` を添える形だが、監督者が無いので **unit の配列**になる (`[{name, label, enabled, loaded, running, pid, listen, binary_path, version, build_id, definition_path}]`)。畳んだことの帰結で、これも OQ-B に含める。

`--all` は登録 unit 全部を対象にする。`hyoui service` の引数なし実行と、必須引数を欠く verb は help を出す。help 以外の出力は JSON、`log --follow` は JSONL、エラーは JSON を stderr に出して exit を非 0 にする (reference の出力規約)。

### 2. unit は OS の service 1 つ。監督者を置かない

unit 1 つにつき launchd job / systemd user unit を 1 つ載せる。`add` は定義を書いて enable + start まで行い、`remove` は stop + 定義削除まで行う。`start` / `stop` / `restart` / `status` は `launchctl` / `systemctl --user` への shell-out で、desired state も OS 側が持つ。

**`stop` を `launchctl stop` で実装してはいけない。** 定義は `KeepAlive=true` (DR-0031、実機 plist で確認) なので、`launchctl stop` が送る SIGTERM の後 launchd が即座に上げ直す。停止を頼んだのに走り続けるという、CLI としては嘘になる挙動になる。systemd は逆で、`Restart=always` でも明示 `stop` は尊重する。この非対称を verb ごとに吸収する:

| verb | macOS (launchd) | Linux (systemd --user) |
|---|---|---|
| `start` | `enable gui/$UID/<label>` → `bootstrap gui/$UID <plist>` (既に載っていれば `kickstart`) | `systemctl --user start <unit>` |
| `stop` | `bootout gui/$UID/<label>` → `disable gui/$UID/<label>` | `systemctl --user stop <unit>` |
| `restart` | `kickstart -k gui/$UID/<label>` | `systemctl --user restart <unit>` |
| `log` | `~/Library/Logs/hyoui-web/<name>.log` を読む | `journalctl --user -u hyoui-web-<name>` |

**`stop` 済みの unit への `restart` は、対象の指定方法で分ける。**

- `restart <name>` (名前を明示) は `start` と同義。その unit を上げてほしいと言われているので、`enabled` を立てて起動する。error + hint にする理由が無い
- `restart --all` は `enabled` な unit だけを対象にし、停止中の unit は**触らない** (出力にはスキップした旨を載せる)

`--all` で停止中の unit まで上げると、`stop` が書いた desired state を `restart --all` が黙って覆すことになる。意図的に降ろしてある unit が、無関係な再起動のついでに復活するのは事故。名前を明示した時だけ desired state を書き換える。

`disable` を併せて叩くのは、これが再 bootstrap / 再 login を跨いで残る「上げない」指示だからで、`bootout` 単体だと次の login で `RunAtLoad` によって復活する。`start` の `enable` はその対。

`status` は 3 つを分けて出す。1 つに畳むと、停止指示のまま落ちているのか、上げたいのに上がらないのかが読めなくなる:

| field | 意味 | 取得元 |
|---|---|---|
| `enabled` | 上げたいかどうか (desired state) | launchd: `print-disabled gui/$UID` に label が載っていないこと / systemd: `is-enabled` |
| `loaded` | 定義が OS に載っているか | launchd: `print gui/$UID/<label>` が成功する / systemd: `show -p LoadState --value` |
| `running` | プロセスが居るか (+ `pid`) | launchd: `print` の pid 行 / systemd: `show -p MainPID` |

llm-gateway (DR-0028 §3) が監督者を置いたのは、監督者 1 つを launchd に載せて子を複数抱える形を採ったため。hyoui でそれを真似ると、launchd が既に提供している「落ちたら上げる」「login で上げる」を hyoui 内に作り直すことになり、CLAUDE.md の self-check (`kernel / OS の標準機能を再発明していないか`) に反する。gateway は互いに独立で、順序依存も共有状態も無いため、束ねる理由が無い。

この判断の帰結として、監督者用の unix socket・制御 protocol・backoff・ログ集約 (DR-0028 §10/§11 が扱っている問題) は hyoui には発生しない。

### 3. unit の識別子は名前。差分は option で与える

`<name>` は unit の識別子で、`add` の必須位置引数。`[A-Za-z0-9_-]{1,32}` に限り、path separator を含む名前は拒否する (定義ファイル名に使うため)。

`add` が受ける差分:

| option | 意味 | 既定 |
|---|---|---|
| `--port=<n>` | `--listen=127.0.0.1:<n>` の短縮 | — |
| `--listen=<host:port>` | bind 先 | config `[web].listen`、無ければ `127.0.0.1:43690` |
| `--binary=<path>` | この unit が起動する実行ファイル | 下記のとおり `current_exe` が PATH 上にあるかで分ける |
| `--web-assets-dir=<path>` | 静的 assets の差し替え (dev) | 未指定なら焼かない (embedded assets) |

option 名は `hyoui web` 側 (`--listen` / `--web-assets-dir`) と一字一句揃える。`add` が組み立てるのは `hyoui web` の argv なので、同じものを 2 通りに呼ばない。

`--port` と `--listen` の同時指定はエラー。

**`add` は listen を解決し切って必ず argv に焼く。** 解決順は `--listen` → `--port` → config `[web].listen` → `127.0.0.1:43690` で、決まった値を `--listen=<host:port>` として定義に書く。現行の `ServiceDefinition::for_web` は listen が `None` なら `--listen` を argv に足さない実装で (`crates/hyoui-cli/src/web_service.rs:27-33`)、実機 plist の `ProgramArguments` も `/opt/homebrew/bin/hyoui web` の 2 語しか無い。この既定を本 DR で変える。理由は 3 つある。

- **config の変更で unit が黙って移動する**。argv に無ければ listen は起動時に config から読まれるので、`[web].listen` を書き換えた瞬間に登録済み unit 全部の待ち先が変わる。前段 proxy が指している先が人の知らないうちに動く
- **同 port の衝突を検出できない**。listen が定義に無いと、2 unit が同じ port を取ろうとしていることを `add` の時点で判定できない。実際に起きるのは 2 台目の bind 失敗と KeepAlive による再起動ループで、原因が log にしか出ない
- **`list` / `status` が listen を答えられない**。決定 4 の「属性は argv から復元する」が成立しない

**`--web-assets-dir` も同じ扱いにする。** `add` の時点で `--web-assets-dir` → config `[web].assets_dir` の順に解決し、値があれば argv に焼く。どちらも無ければ焼かず、その unit は embedded assets で固定される (起動時に config を読んで assets_dir が生えることは無い)。config を後から書き換えても既存 unit の見る assets が変わらないので、`list` の `web_assets_dir` は「この unit が実際に使う値」として読める。

**`add` は listen の衝突を拒否する。** 比較は文字列ではなく `SocketAddr` に解決してから行う (`localhost:43690` と `127.0.0.1:43690` は同じ)。解決できない listen 値 (名前解決に失敗する host 等) は文字列で比較し、`add` を止めずに warning を出す — 解決できないことを理由に登録を断ると、後から名前が引けるようになる環境で登録できなくなる。

完全一致した場合はエラーにし、どの unit が既にその宛先を持っているかを示す。`0.0.0.0:43690` と `127.0.0.1:43690` のような包含関係は一致ではないので拒否せず、warning に留める。

**`stable` = 43690 据え置き、`unstable` = 43691 で確定。** stable を既存の port から動かさないのは、canddy の hyoui ブロックが現在 43690 単体を指しているため。ここを動かさなければ、canddy 側の設定を入れ替える前に unit の移行を終えられ、移行中に到達が切れない。連番を取るのは llm-gateway の 11301 / 11302 と同じ形。

`--binary` を unit ごとに持つのは、stable (brew の `/opt/homebrew/bin/hyoui`) と unstable (repo の `target/release/hyoui`) を同時に走らせることが本 DR の動機そのものだからで、1 系統に統一するとその前提が消える。

**`--binary` の既定は `resolve_stable_path` に丸投げしない。** DR-0031 は `resolve_stable_path(current_exe, SameBinary)` で「同じ binary を指す安定な PATH 上の場所」を選ぶが、これは 1 unit しか無い前提での最適化で、本 DR では罠になる。`target/release/hyoui` が brew 版と同一内容だった瞬間 (= tag を打った直後にビルドした場合) に `/opt/homebrew/bin/hyoui` が選ばれ、**unstable unit が stable の binary を指す**。以降 unstable をビルドし直しても、その unit は brew 版を起動し続ける。

そこで `add` の既定はこうする:

- `current_exe` が PATH 上の安定な場所そのものなら、それを焼く (brew 版から `add` した場合 = stable の期待どおり)
- PATH 外なら **`current_exe` の絶対パスをそのまま焼く** (repo build から `add` した場合 = unstable の期待どおり)。安定な場所を探して差し替えない

`resolve_stable_path` を通さないので「その path は rebuild で壊れ得る」警告は出さない。unstable unit では壊れ得ることが仕様であり、警告は毎回出て意味を失う。`--binary` を明示した場合は常にその値を焼く。

**`--binary` が指す先は消えうる。** unstable が指す `target/release/hyoui` は `cargo clean` や失敗したビルドで消え、その状態だと launchd は起動に失敗して KeepAlive で再試行を続ける。`status` は `binary_path` と併せて **その path が今存在するか** (`binary_exists`) を出す。これが false なら `running: false` の原因が読める。`add` の時点でも存在を確認し、無ければ拒否せず warning を出す (ビルド前に登録する順序を禁じない)。

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

label に kind (`hyoui-web`) が入り、`<name>` はその中での識別子になる。unit 名に kind を含めないので、OQ-A がどちらに決まっても label 規約は変わらず、kind が増えた時は prefix が別 (`jp.kawaz.hyoui-<kind>.<name>`) になるだけで既存 unit を踏まない。

逆引き domain を `com.github.kawaz` から `jp.kawaz` に変えるのは、kawaz 製ツールの label を 1 つの名前空間に揃えるため (llm-gateway は既に `jp.kawaz.llm-gateway.supervise` を使っている)。この変更の副産物として、移行の途中で旧 label (`com.github.kawaz.hyoui-web`) と新 label が同時に載っても互いを踏まない (決定 8)。

`list` は定義ディレクトリを label prefix (`jp.kawaz.hyoui-web.` / `hyoui-web-`) で走査して unit を数える。unit の属性 (listen / binary / web-assets-dir) は定義の argv から復元する。**この復元が成立するのは、`add` が listen を解決し切って argv に焼くから** (決定 3)。argv に無い属性は復元対象にしない — 「定義に書かれていない値は起動時に config から読まれる」という経路を残すと、`list` の出力が実際の待ち先と食い違う。

別に登録簿ファイルを置かないのは、置けば OS 側の定義と二重になり、片方だけ人手で触られた時にどちらが正かを決められなくなるため。DR-0006 §1 が `hyoui list` に対して採った判断 (registry を持たず socket dir を正本にする) と同じ形を service にも適用する。

renderer は DR-0031 の `render_launchd_plist` / `render_systemd_unit` を unit 名でパラメタ化して再利用する。plist / unit は固定の小さな形なので、純関数 renderer + golden test を維持する (DR-0031 §5)。

**復元には renderer の逆関数が要る。XML / ini の crate は足さない。** 現行にあるのは renderer と `systemd_quote` だけで、逆方向が無い (`crates/hyoui-cli/src/web_service.rs:127-141`)。読む対象は自分が書いた形に限るので、`ProgramArguments` の `<string>` 列と `ExecStart=` の quoted value を取り出す逆関数を書き、**renderer と round-trip する test で固定する** (書いて読んで同じ unit に戻る)。汎用 parser を入れるより、書ける形と読める形が同じ 1 組であることを test で担保する方が、DR-0031 §5 が renderer で採った判断と揃う。

**人が手で足した要素は落とさない。** 未知の key / 未知の argv 要素 / 読めない値があっても unit を `list` から消さず、その unit に `error` を添えて載せる。消すと「CLI から見えないが OS には載っている unit」が生まれ、決定 4 の「定義ファイルが正本」が破れる。

### 5. gateway に `/healthz` と `/version` を足す

`hyoui web` の現在の route は `/`、`/sessions/{id}`、`/assets/{*path}`、`/api/sessions*` だけで、死活監視に使える口が無い (実装 `crates/hyoui-web/src/lib.rs:72-80` で確認)。

- `GET /healthz` → 200、body `ok`
- `GET /version` → 200、`{"version": "<crate version>", "build_id": "<build 識別子 | null>"}`

どちらも既存の `/` / `/api/*` と同じ扱い (認証を持たず、到達制限は bind 先と前段の tailnet 制限が担う) で、認証境界を変えない。

**`version` だけでは走っているビルドを識別できない。** stable (brew の `/opt/homebrew/bin/hyoui`) と unstable (repo の `target/release/hyoui`) は実測でどちらも `hyoui 0.9.42` を答える。crate version は tag を打つまで動かないので、unstable に変更を入れても version は変わらず、「今 unstable で走っているのは自分がビルドしたあれか」を version では判定できない。本 DR の運用ではこれが判定の中心になるので、`build_id` を併せて返す。

**`build_id` の既定値は build script が git から導出する。** 環境変数の明示だけに頼ると、通常の unstable ビルド経路 (`just build` = `cargo build --release --workspace`、justfile に env の注入は無く、build script も現状存在しない) で `null` になり、まさに区別したい stable / unstable が両方 `null` で並ぶ。

実装は build script (`build.rs`) で次の順に決める:

1. `HYOUI_BUILD_ID` が環境に与えられていればその値を使う (CI / 配布ビルドが明示する経路)
2. 無ければ `git rev-parse --short HEAD` を実行し、作業ツリーに変更があれば dirty を示す接尾を付ける
3. git が使えない / リポジトリでない場合 (brew の tarball ビルド等) は注入せず、実行時は `null`

build script は `cargo:rustc-env=HYOUI_BUILD_ID=<値>` で値を渡し、`cargo:rerun-if-changed=.git/HEAD` と `cargo:rerun-if-env-changed=HYOUI_BUILD_ID` を宣言して、commit を移った時と env を変えた時に再ビルドされるようにする。実行側は `option_env!("HYOUI_BUILD_ID")` を読むだけ。

`null` を異常扱いしないのは、3 の経路 (ビルド環境を自分で制御できない配布) が正常にありうるため。逆に言えば、`null` は「brew 等の配布ビルド」の印として読める。

`status` はこの `/version` を各 unit の listen に聞き、`version` と `build_id` を載せる。答えない版が走っていることはあるので、答えられなければ両方 `null` (異常ではない)。

**listen が wildcard の unit への問い合わせ先**は loopback に読み替える (`0.0.0.0:<port>` → `127.0.0.1:<port>`、`[::]:<port>` → `[::1]:<port>`)。wildcard はそのままでは宛先にならない。

`/api/` の下に置かないのは、`/api/*` が session 一覧という業務機能で、その仕様変更 (認証追加・スキーマ変更・daemon socket 走査の失敗) が可用性監視を壊すため。canddy が llm-gateway で `/v1/models` から `/llm-gateway/healthz` へ移した理由 (`Caddyfile:179-185` のコメント) と同じ。

`/healthz` は **プロセスが HTTP を返せること**だけを意味し、PTY session の有無・daemon socket の健全性は含めない。gateway は session が 0 でも正常である。

### 6. `restart --all` は 1 台ずつ

**順序は unit 名の昇順**で、`enabled` な unit を 1 台ずつ `restart` → その unit の `/healthz` が 200 を返してから次へ進む (停止中の unit を対象にしない理由は決定 2)。登録簿を持たない (決定 4) ので「登録の逆順」は定義できず、定義ディレクトリの走査順も OS 任せになる。名前の昇順なら、どの環境でも同じ順序になり、`list` の並びと一致する。

`/healthz` 待ちには上限を置く。**1 unit あたり 10 秒**で、超えたら次の unit へ進まずに止まり、どの unit で待ちが尽きたかを出して非 0 で終わる。上限が無いと、上がらない unit で無限に待つか、待たずに次を落として全台を落とすかのどちらかになる。10 秒は gateway の起動 (bind + assets 準備) に対して十分で、KeepAlive による再起動ループに入っている unit を待ち続けない長さ。

前段が優先順で振り分ける構成 (決定 7) では、順に上げ直せば外から見た断が出ない。全台を同時に落とす経路は持たない。

### 7. stable / unstable の HA は前段 proxy が担う。hyoui は担わない

hyoui 側が持つのは「2 unit を独立に常駐させること」と「死活を答える口」だけ。優先順・fallback・health check の間隔は canddy (Caddy) の設定が持つ。

canddy へ依頼する形 (実機の llm-gateway ブロック `Caddyfile:174-194` と同型)。upstream は `unstable` (43691) を先に置き、`stable` (43690) を後ろに置く:

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

### この HA が救う壊れ方の範囲

**fallback が起きるのは、unstable のプロセスに到達できない時だけ。** `unhealthy_status` を指定しないので、失敗判定は dial 失敗に限られる (canddy 側で実測、`Caddyfile:171-173`)。救えるのは「上がらない」「落ちた」「bind に失敗して KeepAlive が再起動ループに入っている」といった、プロセスとして立っていない壊れ方。

**応答するが誤るタイプの壊れ方は救えない。** unstable が起動して HTTP を返すなら、前段はそれを健全とみなす。具体的には次のどれも stable に回らない:

- 新しい cap を要求する gateway が、古い daemon (= 別途走っている PTY session の daemon) と handshake に失敗して 5xx を返す
- screen dump が壊れた内容を 200 で返す
- WS の handshake が通った後で attach が機能しない

### cap 差は gateway 側で吸収する。前段は関与しない

2 つの gateway は同じ socket dir を走査して各 daemon と handshake する。gateway は現在 `MVP_CAPS` 全部を要求して接続し、handshake の失敗は 500 になる (`crates/hyoui-web/src/lib.rs:200-204` ほか、失敗は `INTERNAL_SERVER_ERROR`)。cap は intersect で落ちる仕様なので (`crates/hyoui/src/protocol/caps.rs:30-38`)、新しい gateway が新 cap を必要とする機能を旧 daemon に対して呼ぶと、その機能だけが成立しない。

**この差は gateway 側で機能単位に落とす。** daemon と intersect した結果に無い cap を要する操作は、その操作だけを「未対応」として返し (501 相当)、gateway 全体を 5xx にしない。逆方向 (新しい daemon + 古い stable gateway) も同じで、古い gateway が知らない message は使わないだけになる。

**新しい message を要する DR は、2 版の gateway が同居する前提で互換を書く。** 本 DR 以降、stable と unstable は常に別版なので、「gateway と daemon の版が揃っている」前提を置ける場面が無くなる。

`/healthz` がプロセスの生存だけを意味する (決定 5) のは、この線引きをそのまま反映したもの。gateway 側の応答内容を健全性の条件にすると、不健全の定義が business ロジックに侵食し、復旧の入口 (Web UI) ごと切り離す事故を招く。

同じ理由で、**TCP は生きているのに応答が返らない (hang) 場合の窓**も残る。active health check が unhealthy と判定するまで最大 `health_interval` + `health_timeout` = 7 秒あり、その間に来たリクエストは unstable に渡る。`lb_try_duration` は **dial に失敗した時に次の upstream を試す猶予**なので、dial が成功してから応答が来ない場合には効かない。渡ったリクエストは前段の response timeout (未設定ならクライアント側の timeout) まで待たされる。ここを詰めるのは前段の設定の話なので、hyoui 側の決定には含めない。

**WebSocket attach (`/api/sessions/{id}/attach`) は fallback の対象外**。確立済みの WS は選ばれた upstream に固定され、その unit が落ちれば切れる。回るのは新規接続だけで、これは reverse proxy の性質であって hyoui 側で埋められるものではない。client 側の再接続の要否は本 DR の範囲外とし、必要なら別 issue で扱う。

canddy 側の変更は **canddy リポの issue として依頼する**。設定の正本は canddy が持ち、hyoui が書き換えない。

### 8. 既存 `hyoui web service register|unregister|status` は廃止する

alias も移行期間も設計に入れない。この CLI の利用者は kawaz だけで、破壊的変更を受ける第三者が存在しない (kawaz 明言、2026-09-15)。

移行は既存 1 台 (`com.github.kawaz.hyoui-web`) だけが対象で、`launchctl bootout gui/$UID/com.github.kawaz.hyoui-web` → plist 削除 → `hyoui service add stable --port=43690` の 3 手。**この 3 手は runbook の手作業とし、`add` に旧 label を引き取る経路は作らない。** 対象が 1 台しか無いものを CLI に持たせると、一度通ったら二度と通らないコードが製品に残る。手順は runbook に置く。

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
| 再起動時の graceful drain (処理中リクエストの待ち合わせ) | `restart` は前段が新規接続を他の unit に回す構成の下で打つので、落ちる unit が処理中の分を待つ必要が無い。`kickstart -k` の SIGTERM で即座に降りる前提で組む。WS attach は決定 7 のとおり切れる (前段の fallback 対象外) ので、drain を足しても救われる範囲は増えない |
| `on_disk` 版と `restart_needed` (llm-gateway DR-0028 §9) | 「置いてある版」と「走っている版」の比較は、`binary_path` の実行ファイルに `--version` を聞く経路を足すことで成り立つが、本 DR の `build_id` は tag を跨がないビルドを識別するためのもので、再起動が必要かの判定には使っていない。`status` が出すのは走っている側の実測値だけにする。必要になったら別 DR で足す |

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
| P1 | `GET /healthz` / `GET /version` (`build_id` 込み) を gateway に追加 | 単体 test で 200 / JSON 形 / `build_id` 未設定時の `null`。実機で `curl` 確認 |
| P2 | label / 定義 path / renderer の unit 名パラメタ化、`add` / `remove` / `list` | parser test (各 leaf、`--port` と `--listen` の排他、不正な unit 名、必須引数欠落時の help)。golden test (2 unit 分の plist / systemd unit、argv に `--listen` が焼かれていること)。listen 衝突の拒否 test。隔離 HOME での CLI E2E |
| P3 | `start` / `stop` / `restart` / `status` / `log` (`--all` / `--follow` 含む) | 実機で stable + unstable の 2 台常駐。`stop` した unit が KeepAlive で復活しないことを観測 (決定 2 の要点)。`status` が `enabled` / `loaded` / `running` を分けて出し、2 台の `build_id` が異なることを確認。`restart --all` を **127.0.0.1 直叩き**で観測 (前段経由の断の観測は P5) |
| P4 | 旧 `web service register/unregister/status` 撤去、help / completion / 実装の 3 者同期、移行 runbook | `hyoui service --help` と completion 定義の突き合わせ test。既存 1 台の移行を実機で完了 |
| P5 | canddy リポへ upstream 設定の issue 起票、前段経由の HA 実機検証 | unstable を `stop` (= bootout + disable) した状態で新規リクエストが stable に回ることを観測。stable のみ停止でも同様。`restart --all` 実行中に前段経由の断が出ないことを観測。3 つが揃って初めて完了 |

P5 の「断が出ないこと」の検証は、DR-0014 の検証主義に従い 1 回の観察で結論しない。停止させる側 (unstable / stable)、止め方 (`stop` / `kill -9` / `kill -STOP` で TCP は受けるが応答しない状態 / bind 失敗させて再起動ループに入れる)、リクエストの種類 (HTML / `GET /api/sessions`) の組合せでマトリクスを埋める。`kill -STOP` の列は決定 7 の「窓」の実測値になる (前段が落とすまで最大 7 秒、その間のリクエストは待たされる) ので、「断が出ない」と書ける範囲をこの列の結果で限定する。

決定 7 の「救えない範囲」も観測して記録する。WS attach は fallback 対象外なので「切れることを確認した」と書く (「該当なし」ではない)。応答するが誤る壊れ方 (5xx を返す gateway) を 1 ケース作り、**stable に回らないこと**を観測する — これは期待どおりの挙動であり、後から「HA があるのに救われなかった」と読まれないために記録側に残す。

## Consequences

- gateway の常駐が unit 単位になり、台数の増減が CLI で完結する。plist を人が書く経路が無くなる
- unit ごとに binary が違ってよくなるため、unstable に開発ビルドを常駐させる dogfooding が成立する。ただし前段が吸収するのは **unstable のプロセスに到達できない壊れ方だけ** (決定 7)。応答するが誤る壊れ方は unstable を叩いた人にそのまま出る。「壊れた変更が閉じ込められる」と読まない
- `/healthz` / `/version` の 2 つが増えるが、既存の `/` / `/api/*` も認証を持たないので認証境界は変わらない。どちらも session の内容を返さない
- `build_id` の注入がビルド手順に入る (未設定でもビルドは通り、`null` になる)
- 前段 proxy への依存が製品の可用性設計に入る。canddy が落ちれば tailnet 経由の到達は失われる (127.0.0.1 直結は影響を受けない)
- systemd 経路は DR-0031 と同じく **書けるが未検証**のまま残る。macOS の launchd 経路だけが実証済みという扱いを維持する

## Open Questions

port の割り当て (決定 3) と既存 1 台の移行手順 (決定 8) は本 DR 内で確定させたので、裁定が要るのは次の 2 点だけ。

### OQ-A: verb 群を `hyoui service` に置くか、`hyoui web service` に置くか

判断軸は kawaz 提示のとおり「web 以外に OS 常駐させる unit の種類が出てくるか」。Context の洗い出しでは **現時点で web gateway 以外に無い** (record / redaction / screen watch はいずれも daemon 内、PTY session daemon は `run` が unit を決めるので OS から起こせない、graceful upgrade は走っているプロセスの self-exec)。将来候補として「login 時に決まった PTY session を起こす」が 1 つ挙がるが、issue も DR も無い。

**統括推し: `hyoui web service`。** 決め手は option 集合が kind と一緒に動くこと。`add` の option は `hyoui web` の引数の写し (`--listen` / `--web-assets-dir`) で、session を起こす kind が来れば必要になるのは `-- cmd args...` と namespace になり、両者は共有できない。階層で割れば各階層の option 集合が閉じ、`add` の引数がそのまま「何の unit を足すか」を示す。kind が 1 種類の今は階層が 1 段余るのが弱点。採る場合は決定 1 の verb 表を `hyoui web service <verb>` に読み替える (決定 4 の label は変わらない)。

**対案 `hyoui service` (= 決定 1 の現行記述) の利点**: kind が 1 種類のうちは階層を 1 段減らせる。kind が増えたら `add --kind` を足す形になるが、その時点で `--kind` に応じて有効な option が分岐する。これは表示都合の問題ではなく、モデル自体が kind ごとに違う option 集合を持つという話なので、help と completion の両方で説明しづらくなる。

**第 3 の形: kind を `hyoui service` の内側に置く。** `hyoui service add <kind> <name> ...` (kind を `add` の位置引数) か `hyoui service <kind> add <name> ...` (kind を verb の手前) のどちらか。どちらも verb 群を 1 箇所に集めたまま kind ごとに option 集合を閉じられる (`service add web stable --port=...` / `service add session claude -- claude`)。`remove` 以降は `<name>` だけで引ける (kind は label から判る)。この形が成り立つなら、`hyoui web service` の優位は「kind 名が階層のどこに出るか」だけに縮む。弱点は、kind が 1 種類の今は `add web stable` の `web` が冗長に見えること。

対案の `add --kind` は 3 形の中で最も弱い (option 集合が flag の値で分岐する)。裁定は上の 3 形から選ぶ形にしたい。

実装コストは 3 形ともほぼ同じ (verb 群は 1 kind 分で変わらない)。違うのは、後から形を動かす時に runbook と label 名を書き直すかどうか。「session を起こす kind」に手を付ける見込みがあるかは kawaz にしか判断材料が無い。

### OQ-B: reference 体系からの乖離を認めるか

kawaz の原指示は「個人 reference `cli-daemon-subcommands` の daemon/service 設計に沿った作り」だが、本 DR は乖離している。

**根の乖離は 1 つ**: 監督者 (`supervise`) を置かないこと (決定 2)。reference の 2 系統は「`daemon` = 監督者に対する instance 操作」「`service` = 監督者の OS 登録」という形で監督者を軸に割れているので、監督者が無いと割る軸そのものが消え、**2 系統が 1 系統に畳まれる**。

畳んだ帰結として、次の 3 つが reference と違う形になる:

- **verb 集合**: `add` / `remove` / `list` / `start` / `stop` / `restart` / `status` / `log` は reference では `daemon` 群の verb だが、これを `service` の名前の下に置く (決定 1)
- **`status` の形**: 監督者 1 つ + `instances: [...]` ではなく、unit の配列になる (決定 1)
- **`register` / `unregister` の消滅**: unit 登録 = OS 登録なので `add` / `remove` に吸収される (決定 1)

**統括推し: 乖離を認める。** 監督者を置くと launchd / systemd が既に持つ「落ちたら上げる」「login で上げる」を hyoui 内に作り直すことになり、CLAUDE.md の self-check (OS 標準機能の再発明) に触る。gateway は互いに独立で束ねる理由も無い。unit 登録 = OS 登録である以上、verb を 2 つに割ると同じ行為に 2 つの口ができる。

**認めない場合の形** (llm-gateway DR-0028 と同型): `hyoui daemon supervise` を新設して登録 unit を子として抱えさせ、launchd に載せるのはこの監督者 1 つだけにする。`daemon add <name>` は登録簿 (`~/.local/state/hyoui/…/units/<name>.toml`) に書くだけで OS には触らず、`service register` が監督者を OS に載せる 2 段になる。`start` / `stop` / `restart` / `status` は監督者への unix socket 要求になるので、監督者用の制御 protocol・backoff・ログ集約 (DR-0028 §10/§11) が hyoui 側に必要になる。この形を採ると、決定 2 / 決定 4 (定義ファイルが正本) と決定 1 の verb 表が入れ替わる。

## 参照した素材

- `docs/issue/2026-09-15-service-subcommand-multi-unit-ha.md`
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/cli-daemon-subcommands.md`
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/design-spec/spec-preflight.md`
- `~/.local/share/repos/github.com/kawaz/llm-gateway/main/docs/decisions/DR-0028-daemon-service-subcommands.md`
- `~/.local/share/repos/github.com/kawaz/llm-gateway/main/crates/llm-gateway/src/daemon/registry.rs` / `protocol.rs`、`crates/llm-gateway-cli/src/service.rs` / `service/platform.rs`
- `~/.local/share/repos/github.com/kawaz/canddy-app-proxy/main/README.md` / `Caddyfile` (hyoui ブロック 127-133、llm-gateway ブロック 144-194)
- 本リポ: `docs/decisions/DR-0031-web-service-subcommand.md`、`docs/decisions/DR-0006-cli-ground-rules.md`、`crates/hyoui-cli/src/web_service.rs`、`crates/hyoui-web/src/lib.rs`、`crates/hyoui/src/config/mod.rs`
- 本リポの現行実装の該当箇所: `crates/hyoui-cli/src/web_service.rs:27-33` (listen が `None` なら `--listen` を argv に足さない) と `:127-141` (renderer 側の quote のみ、逆関数なし)、`crates/hyoui/src/protocol/caps.rs:30-38` (`MVP_CAPS` と intersect の説明)、`crates/hyoui-cli/src/main.rs` の `hyoui web` の listen 解決順 (flag > config > 既定)、`crates/hyoui-cli/src/completion.rs` (bash/zsh の `web service` leaf)、`crates/hyoui-cli/tests/web_service_e2e.rs`
- 実機出力: `hyoui --help` / `hyoui web --help` / `hyoui web service --help` / `hyoui web service status` (`/opt/homebrew/bin/hyoui` 0.9.42)、`~/Library/LaunchAgents/com.github.kawaz.hyoui-web.plist` の全文 (`ProgramArguments` は `hyoui web` の 2 語のみ、`KeepAlive=true`)、`target/release/hyoui --version` (= 0.9.42、brew 版と同一で build 識別不能)

## 関連

- DR-0031 — 本 DR が置き換える単一 unit 版の service 登録
- DR-0027 — gateway を同 repo に置く判断
- DR-0006 — registry を持たずファイルシステムを正本にする形の初出
- DR-0014 — 介入 self-check とマトリクス検証
