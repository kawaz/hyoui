# DR-0034: `hyoui web` を multi-unit にし、stable / unstable 2 インスタンスの HA を組む

- Status: Active
- Date: 2026-09-15
- Related: DR-0027 (web gateway 同居), DR-0031 (`web service` 単一 unit 登録、本 DR の P5 完了時点で Superseded になる), DR-0006 (CLI 地盤ルール), DR-0024 (config ファイル機構), DR-0014 (介入 self-check / 検証主義), DR-0033 (`leader.request` = cap 差の具体例)
- Origin: `docs/issue/2026-09-15-service-subcommand-multi-unit-ha.md` (kawaz 裁定 2026-09-15、QUESTIONS ECO-Q2 への回答)
- 裁定: 階層は `hyoui web` 配下 (SVC-Q1=a)、reference `cli-daemon-subcommands` の 2 系統をそのまま採る (SVC-Q2=b、2026-09-15)

## Context

### 現状

`hyoui web service register|unregister|status` (DR-0031) は web gateway を **1 unit 固定**で OS に登録する。label は `com.github.kawaz.hyoui-web` 1 つ、binary は `stable-which` が選んだ 1 つ。実機はこの形で 1 台だけ常駐しており、plist の `ProgramArguments` は `/opt/homebrew/bin/hyoui web` の 2 語 — **`--listen` は焼かれていない**ので、待ち先は起動時に config (無ければ既定の `127.0.0.1:43690`) から決まる。canddy はその 43690 を指している。

2 台目を足す手段が CLI に無い。足すなら plist を人が手で書くことになり、「今どの gateway が何番で何のバイナリで動いているか」を CLI が答えられない状態になる。

### 目的

gateway インスタンスを **複数前提**で扱えるようにし、片方が壊れてももう片方が受ける構成を、CLI と reverse proxy の設定だけで再現できるようにする。

**第一の目的は、障害時に「まず plist を探す」作業を無くすこと** (kawaz、2026-09-15)。gateway を複数 launchd に登録すると、状態を見たい / 再起動したいと思った時に、まず**どの label の plist が何台あるか**を思い出す必要が出る。この操作は普段から頻繁にやるものではないので、そのたびに launchd の使い方から確認し直すことになる。障害が起きた瞬間に見たいのは状態とログで、探したいのは plist ではない。

そこで **`hyoui web daemon list | status | log | restart` だけで全 gateway が見え、操作できる**状態にする。台数が増えても入口は 1 つのまま。

監督者 (`supervise`) を置くのは、この入口を成立させるためであり、次の 3 つがその内訳:

- **単一入口**: 複数の gateway の一覧・start / stop / restart / status / log が 1 つのコマンド体系に集まる。OS 側に散らばった定義を人が突き合わせる作業が消える
- **binary 更新をまたいで安定する契約**: unit の binary を差し替えても、監督者より上のレイヤ (OS への登録) は触らなくてよい。gateway を更新するたびに `service register` をやり直す必要が無い (決定 3)
- **status / log のスコープ分離**: `service` は OS 登録の話 (監督者 1 つが載っているか、その log) だけを扱い、`daemon` は子 gateway の話 (生きているか、再起動、その log) だけを扱う。見たい層を選んで見られる

**launchd / systemd と監督者は見る対象が違うので、責務は重複しない。** launchd は監督者 1 つだけを見て、監督者は子 gateway だけを見る。同じプロセスを 2 人が監視する構造にはならない。

ここで増やしたくないもの (目的と同格):

- **人が触る OS 側の定義**。台数を増やしても、launchd / systemd に載るものは増やさない
- **状態の置き場**。unit の属性を登録簿と OS 側の定義に二重に持たない
- **「daemon」の指す対象の曖昧さ**。hyoui で daemon は PTY session の daemon (DR-0006 §1: 1 daemon 1 socket 1 子) を指す。本 DR が足すのは `hyoui web daemon` で、`web` の下にあることで対象が gateway に限定される

### 前提条件と、満たさない場合

| 前提 | 満たさない場合 |
|---|---|
| OS の service manager (launchd / `systemd --user`) が監督者 1 つを login 時に上げ、落ちたら上げ直す | 監督者の可用性が人の手に戻る。unit の起動は監督者経由なので、監督者が落ちている間は台数ゼロになる |
| 前段に優先順と fallback を書ける reverse proxy が居る (実機は canddy) | HA は成立しない。unit 複数化だけが残り、切り替えは人が行う |
| stable / unstable が **別ビルドの同一 CLI** である | unit ごとの `binary` は不要になり、決定 2 の binary 指定が過剰になる |

### OS 常駐にすべき unit の kind は、現時点では web gateway だけ

階層を `hyoui web` 配下に置く裁定の前提として、現行コードと DR 群を洗った結果を残す:

| 候補 | 常駐 unit になるか | 根拠 |
|---|---|---|
| web gateway (`hyoui web`) | **なる** | login 時に上がっていて欲しく、落ちたら上がって欲しい。DR-0031 が既に launchd に載せている |
| PTY session daemon (`hyoui run` が fork するもの) | ならない | 子コマンドと 1:1 で、`run` の呼び出しが unit を決める (DR-0015)。login 時に「何を起こすか」を OS に教える手段が無く、socket dir を正本とする設計 (DR-0006 §1) と衝突する |
| tty I/O record (DR-0016) | ならない | daemon 内の bounded queue + writer thread (DR-0016 §8/§8a)。別プロセスではない |
| record の secret redaction (DR-0016 §6 / Phase 5) | ならない | 同じく daemon の hot path 内 (`crates/hyoui/src/daemon/record.rs`) |
| screen region watch + 検出通知 (`docs/issue/2026-07-21-screen-region-watch-api.md`) | ならない | 母体は DR-0025 Screen domain の `WatchRegistration` で daemon 内。外部への通知経路は CLI と web gateway が兼ねる |
| daemon graceful upgrade (DR-0028) | ならない | 走っているプロセスの self-exec。起動させる対象ではない |

将来 kind が増える見込みとして「login 時に決まった PTY session を起こす」(常用 session の自動復元) がある。これは reference の `daemon add <unit>` / `supervise` がそのまま当てはまる形で、その時は `hyoui session daemon` / `hyoui session service` を並べればよい。**kind を階層に出しておくのは、この並べ方を確保するため。** 本 DR はこの kind を実装しない。

### 参照した先行実装

llm-gateway が同じ reference 体系を先に当てている (DR-0028)。unit = 設定ファイル 1 つ、登録簿は `~/.local/state/llm-gateway/daemon/units/<name>.toml`、unit ごとに `binary_path` を持ち (stable = brew / unstable = repo build)、`daemon supervise` 1 つだけを launchd に載せる。本 DR は **この形をそのまま hyoui に持ち込む** (裁定 SVC-Q2=b)。

踏襲する点と、hyoui で変える点:

| 項目 | llm-gateway | hyoui (本 DR) |
|---|---|---|
| 監督者を 1 つだけ OS に載せる | そう | 踏襲 (決定 6) |
| 制御経路 | unix socket 1 本、JSON 1 行、監督者不在は `supervisor_not_running` + hint | 踏襲 (決定 4) |
| backoff / grace | 初回 1 秒から倍々・上限 60 秒、SIGTERM → grace → SIGKILL | 踏襲、値も同じ (決定 3) |
| 監督者を止めた時の子 | 道連れ (行儀よく降りる限り全部止める) | 踏襲 (決定 6) |
| 監督者死亡後の残り子 | 引き取らない。次の監督者は起こし直して port 衝突 | 踏襲 (決定 10) |
| ログ | 監督者が `logs/<unit>.log` に集約、回転は OS | 踏襲 (決定 9) |
| unit の中身 | 設定ファイルの path + `binary_path` + `enabled` | **変更**: 設定ファイルを持たず `listen` / `binary` / `web_assets_dir` / `enabled` を解決して書く (決定 2)。gateway に設定ファイルの概念が無い |
| 子の argv | `<binary> daemon run <unit>` (子が unit の中身を解釈) | 踏襲: `<binary> web daemon run <name>` (決定 3)。監督者は unit の中身を解釈しない |
| `binary` の既定 | 設定の `binary_path`、無ければ登録時の自分自身 | **変更**: `current_exe` が PATH 上ならそれ、PATH 外ならそのまま焼く。`resolve_stable_path` を通さない (決定 2) |
| `restart --all` の順序 | 登録の逆順 (`supervisor.rs:286-288`、前段が手前を優先しているため) | **変更**: unit 名の昇順 (決定 5)。順序の根拠を前段の設定に置かない — 前段の優先順は canddy が持つ知識で、監督者は知らない |
| 停止中 unit への `restart` | 無条件に `set_enabled(name, true)` (`supervisor.rs:318`) | **変更**: 名前指定なら同じ (enable して起こす)、`--all` は `enabled` な unit だけ (決定 5)。`stop` の意思を `--all` が覆さない |
| 版の表示 | `version` が `on_disk` / `running` / `restart_needed` を並記 (DR-0028 §9)。`on_disk` は実行ファイルに `--version` を聞いて取る | 踏襲 (決定 7a)。**加えて** `build_id` を並べ、比較を `(version, build_id)` の組で行う — hyoui は crate version が tag まで動かないので、版だけでは入れ替えを検出できない |
| 出力の field 名 | `unit` / `since_ms` など | **命名は独自**。借りるのは考え方だけで綴りは揃えない。識別子は `name`、時刻は ISO 8601 の絶対時刻 (`started_at`)、単位を名前に埋めない (決定 4) |

## 介入判断 self-check (CLAUDE.md / DR-0014)

- PTY / child / signal / protocol への介入は無い。DR-0031 と同じく OS service manager に起動を依頼する運用層で、透過原則を変更しない
- hyoui の既存 protocol (CBOR / cap flags) には触らない。監督者の制御は独立した unix socket + JSON 1 行で、daemon socket とは別物 (決定 4)
- **OS 機能の再発明ではない。** launchd / systemd は監督者 1 つの生存だけを見て、監督者は子 gateway の生存だけを見る。監視対象が重ならないので、同じ仕事を 2 箇所で持つ構造にならない。監督者が担うのは、OS 側が持っていない「複数の子を 1 つの入口から一覧・操作できること」と「子の binary を差し替えても OS 登録を触らなくてよい契約」(目的節)
- 既存 DR の実装漏れではない。DR-0031 は実装済で、本 DR はその適用範囲を広げる

## Decision

### 1. `hyoui web` の下に reference の 2 系統を置く

```text
hyoui web daemon run [name]                この unit を foreground で起動する (未指定なら config の listen)
hyoui web daemon supervise                 foreground の監督者。登録簿の unit を子として抱え、落ちたら上げる
hyoui web daemon add <name> [options]      unit を登録簿に足す
hyoui web daemon remove <name>             登録簿から外す
hyoui web daemon list                      → {units: [{name, enabled, running, pid, listen, binary, binary_exists}], supervisor, note} (監督者が居なくても動く)
hyoui web daemon start   <name> | --all    監督者に起動を要求する
hyoui web daemon stop    <name> | --all    監督者に停止を要求する
hyoui web daemon restart <name> | --all    stop → start (--all は 1 台ずつ)
hyoui web daemon status  [<name>] | --all  → {units: [{name, enabled, running, pid, listen, binary, version: {...}, ...}], supervisor, notes}
hyoui web daemon log     [<name>] | --all [--follow]

hyoui web service register | unregister    監督者 (`hyoui web daemon supervise`) を launchd / systemd に載せる / 外す
hyoui web service start | stop             監督者の起動 / 停止
hyoui web service status                   → {registered, running, pid, service: {...}, version: {...}, instances: [...]}
hyoui web service log [--follow]           監督者と OS 側のログ

hyoui version                              → {cli, supervisor: {running, on_disk, binary, restart_needed}, units: [...]}
```

`hyoui version` は `web` 配下ではなく top-level に置く (CLI 自身・監督者・全 unit の版を 1 回で並べる口なので、web に限った話にならない)。

`add` の options:

| option | 意味 | 既定 |
|---|---|---|
| `--port=<n>` | `--listen=127.0.0.1:<n>` の短縮 | — |
| `--listen=<host:port>` | bind 先 | config `[web].listen`、無ければ `127.0.0.1:43690` |
| `--binary=<path>` | この unit が起動する実行ファイル | 決定 2 のとおり `current_exe` が PATH 上かで分ける |
| `--web-assets-dir=<path>` | 静的 assets の差し替え (dev) | config `[web].assets_dir`、無ければ embedded |

option 名は `hyoui web` 側 (`--listen` / `--web-assets-dir`) と一字一句揃える。`add` が書く値は `daemon run` がそのまま使うものなので、同じものを 2 通りに呼ばない。

`daemon run <name>` は登録簿の値で gateway を foreground 起動する。監督者が子を exec するのと同じ経路で、CLI から手元で 1 台だけ確かめる時にも使う (決定 3)。

**`daemon run` の名前を省いた時は登録簿を見ず、`hyoui web` と同じ解決 (config `[web].listen`、無ければ `127.0.0.1:43690`) で起動する。** 「既定の unit」は持たない — 登録簿に 1 つしかない時それを選ぶような推測を入れると、2 つ目を足した瞬間に同じコマンドの意味が変わる。

**`list` と `status` は配列ではなく `{units, supervisor, ...}` を返す。** 監督者が居ない時に「なぜ `running` が分からないのか」を添える必要があり (下記)、配列にはその置き場が無い。行ごとに `supervisor_running` を重ねるより、答えた相手を 1 箇所に置く。

`hyoui web` の引数なし実行は gateway の foreground 起動 (`daemon run` と同義) のまま維持する。`hyoui web daemon` / `hyoui web service` の引数なし実行と、必須引数を欠く verb は help を出す。help 以外の出力は JSON、`log --follow` は JSONL、エラーは JSON を stderr に出して exit を非 0 にする (reference の出力規約)。

### 2. unit = 登録簿の 1 ファイル。listen も binary も解決して書く

`${XDG_STATE_HOME:-~/.local/state}/hyoui-web/units/<name>.toml` に 1 unit 1 ファイル。`<name>` は `[A-Za-z0-9_-]{1,32}` に限り、path separator を含む名前は拒否する (ファイル名に使うため)。

**root は `hyoui/` ではなく `hyoui-web/` にする。** `${XDG_STATE_HOME}/hyoui/` と `$XDG_RUNTIME_DIR/hyoui/` は session discovery の走査 base で、その**サブ dir 名が namespace、中の `*.sock` が session** として扱われる (`crates/hyoui/src/discovery.rs:9-13` / `:167-190`、DR-0018 の socket 配置と対称)。監督者の socket をここに置くと `hyoui list` に namespace `web` の session として現れ、hyoui protocol を話さないので stale 扱いで並ぶ。session の名前空間と gateway の運用状態を同じ木に置かない。

この root に `units/` (登録簿)、`logs/` (決定 9)、`supervisor.sock` (決定 4) をまとめる。

unit が持つのは `listen` / `binary` / `web_assets_dir` / `enabled` (desired state) / `added_at`。

**書き込みは tmp ファイル + rename で差し替える。** 同じ dir に書いて rename すれば、読み手は古い内容か新しい内容のどちらかを見る (途中の半端な内容を読まない)。`enabled` を書き換えるのは監督者、`add` / `remove` は CLI で、書き手が 2 者いるのでこれは要件。

**listen は `add` の時点で解決し切って書く。** 解決順は `--listen` → `--port` → config `[web].listen` → `127.0.0.1:43690`。`web_assets_dir` も同様に `--web-assets-dir` → config `[web].assets_dir` の順で解決し、どちらも無ければ書かない (= その unit は embedded assets で固定)。config を後から書き換えても既存 unit の待ち先と assets が動かないので、`list` / `status` の値は「この unit が実際に使う値」として読める。解決せずに実行時の config 読みに委ねると、`[web].listen` を 1 つ書き換えた瞬間に全 unit の待ち先が変わり、前段が指している先が人の知らないうちに動く。

**listen の衝突は `add` が拒否する。** 比較は `SocketAddr` に解決してから行う (`localhost:43690` と `127.0.0.1:43690` は同じ)。解決できない値は文字列で比較し、`add` を止めずに warning を出す。完全一致はエラーにして、どの unit が既にその宛先を持っているかを示す。`0.0.0.0:43690` と `127.0.0.1:43690` のような包含関係は一致扱いせず warning に留める。

**`binary` の既定は `resolve_stable_path` に丸投げしない。** DR-0031 は `resolve_stable_path(current_exe, SameBinary)` で「同じ binary を指す安定な PATH 上の場所」を選ぶが、これは 1 unit 前提の最適化で、本 DR では罠になる。`target/release/hyoui` が brew 版と同一内容だった瞬間 (= tag を打った直後にビルドした場合) に `/opt/homebrew/bin/hyoui` が選ばれ、**unstable unit が stable の binary を指す**。以降 unstable をビルドし直しても、その unit は brew 版を起動し続ける。そこで:

- `current_exe` が PATH 上の安定な場所そのものなら、それを書く (brew 版から `add` した場合 = stable の期待どおり)
- PATH 外なら `current_exe` の絶対パスをそのまま書く (repo build から `add` した場合 = unstable の期待どおり)。安定な場所を探して差し替えない

`--binary` を明示した場合は常にその値を書く。「その path は rebuild で壊れ得る」警告は出さない — unstable unit では壊れ得ることが仕様であり、毎回出る警告は意味を失う。

**binary は消えうる。** unstable が指す `target/release/hyoui` は `cargo clean` や失敗したビルドで消える。`status` は `binary` と併せて **その path が今存在するか** (`binary_exists`) を出す。これが false なら `running: false` の原因が読める。`add` の時点でも存在を確認し、無ければ拒否せず warning (ビルド前に登録する順序を禁じない)。

**実機の 2 unit は `stable` = 43690 据え置き、`unstable` = 43691。** stable を動かさないのは、canddy の hyoui ブロックが現在 43690 単体を指しているため。ここを動かさなければ、canddy 側の設定を入れ替える前に unit の移行を終えられ、移行中に到達が切れない。連番を取るのは llm-gateway の 11301 / 11302 と同じ形。

**OS 側の定義は監督者 1 つ分だけ。** unit ごとの plist / systemd unit は作らないので、定義ファイルを読み返して属性を復元する経路も要らない (登録簿は serde で読み書きする)。plist renderer は監督者 1 本のために残り、DR-0031 §5 の純関数 renderer + golden test をそのまま使える。台数を増やしても renderer の出力は 1 種類のままで、XML / ini の parser を足す必要も生じない。

### 3. `supervise` は exec するだけの監督者

登録簿の `enabled` な unit ごとに **`<binary> web daemon run <name>`** を子プロセスとして起動し、落ちたら backoff を置いて上げ直す。

**unit の値を読むのは子。監督者は argv に展開しない。** 監督者が渡すのは unit 名だけで、`listen` / `web_assets_dir` は子が登録簿から読む。監督者は「登録簿の `enabled` を見て起こし、落ちたら上げ直す」だけの役に閉じ、unit の中身を解釈しない (llm-gateway が採った線引きと同じ = 解釈するのは子)。展開する形にすると、監督者が読んだ値と子が使う値の 2 系統ができ、`add` 直後のような書き換えの前後で食い違う余地が生まれる。登録簿を唯一の正本にする。

`hyoui web daemon run <name>` は CLI からも打てる (手元で 1 台だけ foreground で動かして確かめる用)。監督者が exec するのと同じ経路。

SIGTERM / SIGINT を受けたら子を順に止めて終わる (SIGTERM → grace 待ち → SIGKILL)。

監督者自身は HTTP を持たず、gateway の設定も解釈しない。解釈するのは子の `daemon run` である。

backoff は llm-gateway と同じ形 (初回 1 秒から倍々、上限 60 秒)。bind に失敗し続ける unit (= 同 port が既に埋まっている) を無限に叩かないための上限で、この状態は `status` の `restarts` と `last_exit` に出る。

**gateway は状態を持たない。** 各 gateway は socket dir を走査して daemon に繋ぐだけで、自前の永続状態を持たない。だから監督者は子を任意の順序で起こしてよく、起動順の依存も引き継ぎも無い。llm-gateway から踏襲するのは「設定の解釈は子」という線引きだけ。

#### 監督者の binary は unit の binary と独立に決まる

監督者に焼く path は **安定な場所** (`resolve_stable_path(current_exe, SameBinary)`、通常は `/opt/homebrew/bin/hyoui`) を選ぶ。`service register --binary=<path>` で明示もできる。unit 側 (決定 2) が `current_exe` をそのまま焼くのと逆なのは、役割が違うため:

- 監督者は「登録簿を読んで子を exec し、落ちたら上げ直す」だけの役で、gateway の機能を持たない。どの版でも同じ仕事をする
- unit は gateway 本体で、stable / unstable の違いがまさに試したいもの

**この分離が「binary 更新をまたいで安定する契約」を成立させる** (目的節)。unstable の gateway を何度ビルドし直しても、変わるのは登録簿が指す `target/release/hyoui` の中身だけで、監督者の定義は 1 文字も変わらない。だから `service register` をやり直す必要が無く、`hyoui web daemon restart unstable` で足りる。

**`service register` を再実行するのは、監督者自身を更新する時だけ。** 具体的には (a) `hyoui` を brew で上げて監督者の argv / path を新しい版に向け直す時、(b) 監督者の定義そのもの (label / 環境 / log path) を変えた時。どちらも普段の gateway 更新では起きない。

ただし `register` は同じ label を降ろして載せ直すので、**監督者の再起動を伴い、子も道連れで一度落ちる** (決定 6)。つまり `service register` の再実行は全断を含む操作で、この点でも「gateway の更新では打たない」が正しい。

### 4. 制御は監督者への unix socket 1 本。監督者が居なければ断る

`start` / `stop` / `restart` / `status` / `log` は子を直接叩かず、監督者へ要求する。socket は ``${XDG_STATE_HOME:-~/.local/state}/hyoui-web/supervisor.sock``、要求は JSON 1 行、答えも JSON 1 行 (`log --follow` だけ JSONL が続く)。

`start` / `stop` は登録簿の `enabled` を監督者が書き換えて子へ反映する。**`stop` された unit は登録簿に残る** (`enabled = false`)。unit の停止は launchd の `bootout` ではなく監督者が子を止める操作なので、launchd 側の `disable` は関与しない (launchd の停止意味論が効くのは監督者の plist だけ = 決定 6)。

監督者が起動していなければ、CLI は `supervisor_not_running` エラーと、`hyoui web daemon supervise` または `hyoui web service start` を実行するための hint を返す。**監督者不在時に CLI が子を直接起こす経路は持たない** — 子の所有者が CLI と監督者の 2 つになり、停止・再起動・状態確認の経路が分岐する。

登録簿の変化を定期的に舐めて差分を見つける作りにはしない。ポーリングは間隔に根拠が無く、間隔の内側で起きた往復 (stop → start) を取りこぼす。代わりに `add` / `remove` が監督者へ要求を送る (下記)。`reload` (登録簿を読み直して望みとの差を埋める) は **socket の内部 op として持つが、CLI の verb としては出さない** — 人が打つ必要のある場面が無く、出すと「`add` の後に `reload` を打つべきか」という迷いを生む。

#### `add` / `remove` は走行中の監督者に即反映する

| 操作 | 登録簿 | 監督者が走っている | 監督者が居ない |
|---|---|---|---|
| `add <name>` | `enabled = true` で書く | `reload` を送り、読み直した監督者がその場で起こす | 次に監督者が上がった時に起きる。出力に「監督者が停止中なので未起動」と添える |
| `remove <name>` | ファイルを消す | 先に `stop <name>` を送り、子が降りてから消して `reload` を送る | そのまま消す |

`add` の `enabled` 初期値を `true` にするのは、`add` が「この gateway を動かしたい」という意思表示だから。`add` してから `start` を打たせるのは、2 手を要求する理由が無い。動かさずに登録だけしたい場面は今のところ無いので、`--no-start` のような option も持たない (必要になったら足す)。

`remove` が `stop` を先に送るのは、登録簿から消えた子を監督者が抱えたままになるのを避けるため。消してから `reload` に任せる形にすると、「登録簿に居ないが走っている子」を監督者が畳む経路が必要になり、決定 10 で作らないと決めた引き取り判断に近い曖昧さが入る。

**`add` が送るのは `start <name>` ではなく `reload` である。** 足したばかりの unit は監督者がまだ名前を知らないので、`start <name>` は「そんな unit は無い」で断られる (実装時に観測)。監督者が知らない名前を受けたら登録簿を読み直す、という含みを `start` に持たせる手もあるが、それは `reload` が既に担っている仕事で、`start` の意味 (= desired state を立てる) に別の役を足すことになる。`add` は `enabled = true` を書いてから読み直させ、起こすのは監督者の判断に委ねる。

`remove` が最後に `reload` を送るのは、子が降りて登録も消えたことを監督者に読み直させるため (= 抱えている unit の一覧から外す)。`stop` の相手が既に監督者の知らない unit だった場合は止める相手が居ないだけなので、そのまま消して進む。

`status` が返すのは `{units: [...], supervisor: {...}}` で、`units` の 1 行はこの形:

```json
{"name": "unstable", "enabled": true, "running": true, "pid": 4242,
 "started_at": "2026-09-15T16:02:31+09:00",
 "listen": "127.0.0.1:43691",
 "binary": "/Users/…/target/release/hyoui", "binary_exists": true,
 "version": {"running": {"version": "0.9.44", "build_id": "9f0e1d2"},
             "on_disk":  {"version": "0.9.44", "build_id": "7c8b9a0-dirty"},
             "restart_needed": true},
 "restarts": 0, "last_exit": null}
```

`version` の 3 つ組は決定 7a のとおり。`hyoui version` が出すのと同じ値で、`status` は「今どうなっているか」を見る口、`version` は「版だけを並べて見る」口として同じ事実を返す。

**field 名は llm-gateway から引き写さず、hyoui として決める。** 借りるのは考え方 (版を 2 つ並べる、監督者不在時の答え方、`enabled` と `running` を分ける) で、綴りは揃えない。時刻は単位を名前に埋めず **ISO 8601 の絶対時刻** (`started_at`) で出す — 経過ミリ秒のような相対値は、出力を保存した後に読むと意味が変わる。継続時間を出す必要が生じたら秒の整数にする。unit の識別子は `name` 1 本にする (reference の例は `{id, unit}` だが、`id` は登録簿を持たない実装での連番で、名前がある本 DR では同じものを 2 通りに呼ぶだけになる)。`enabled` と `running` を分けるのは、停止指示のまま降りているのか、上げたいのに上がらないのかを区別するため。

**`list` は監督者が居なくても動く。** 登録簿を読むだけで答えられる範囲 (`name` / `enabled` / `listen` / `binary`) を出し、`running` / `pid` は監督者に聞けないので `false` / `null` にして「監督者が停止中」と添える。障害時に最初に打つコマンドが監督者の生死に依存すると、目的節の「サクッと状態を見る」が成り立たない。`status` も同じ扱いで、監督者不在時は登録簿由来の列だけが埋まる。

**`start --all` は停止中の unit も上げる** (`enabled` を立てる)。`restart --all` が `enabled` な unit だけを対象にするのと対照的だが、`start` は「上げてほしい」という意思表示そのもので、`restart` は「走っているものを入れ替える」操作だから — 前者が desired state を書き換えるのは意図どおり、後者が書き換えるのは事故 (決定 5)。

### 5. `restart --all` は 1 台ずつ

**順序は unit 名の昇順**で、`enabled` な unit を 1 台ずつ停止 → 起動し、その unit の `/healthz` が 200 を返してから次へ進む。前段が優先順で振り分ける構成 (決定 8) では、順に上げ直せば外から見た断が出ない。全台を同時に落とす経路は持たない。

`/healthz` 待ちは 200ms 間隔で叩き、**1 unit あたり 30 秒**で打ち切る。超えたら次の unit へ進まずに止まり、どの unit で待ちが尽きたかを出して非 0 で終わる。上限が無いと、上がらない unit で無限に待つか、待たずに次を落として全台を落とすかのどちらかになる。値は llm-gateway の監督者と揃えた。

**`stop` 済みの unit への `restart` は、対象の指定方法で分ける。**

- `restart <name>` (名前を明示) は `start` と同義。`enabled` を立てて起動する
- `restart --all` は `enabled` な unit だけを対象にし、停止中の unit は触らない (出力にスキップした旨を載せる)

`--all` で停止中の unit まで上げると、`stop` が書いた desired state を `restart --all` が黙って覆す。意図的に降ろしてある unit が無関係な再起動のついでに復活するのは事故なので、名前を明示した時だけ desired state を書き換える。

### 6. OS に載せるのは監督者 1 つだけ

| 意味 | macOS LaunchAgent | Linux systemd user |
|---|---|---|
| label / unit | `jp.kawaz.hyoui-web.supervise` | `hyoui-web-supervise.service` |
| 定義 path | `~/Library/LaunchAgents/<label>.plist` | `$XDG_CONFIG_HOME/systemd/user/<unit>` (`~/.config` fallback) |
| 起動 argv | `<binary> web daemon supervise` | 同じ |
| login 時起動 | `RunAtLoad=true` | `WantedBy=default.target` + enable |
| 継続起動 | `KeepAlive=true` | `Restart=always` |
| 環境 | 最小 `PATH` のみ | 最小 `PATH` のみ |
| log | `~/Library/Logs/hyoui-web/supervise.log` | journald |

逆引き domain を `com.github.kawaz` から `jp.kawaz` に変えるのは、kawaz 製ツールの label を 1 つの名前空間に揃えるため (llm-gateway は既に `jp.kawaz.llm-gateway.supervise` を使っている)。副産物として、移行の途中で旧 label (`com.github.kawaz.hyoui-web`) と同時に載っても互いを踏まない。

監督者に焼く binary の path は決定 3 のとおり安定な場所を選ぶ (`resolve_stable_path`、`--binary` で明示可)。安定な場所が無くても登録は止めず、warning を出す。

**監督者を止めると子も止まる。** 監督者は SIGTERM / SIGINT で抱えている子を全部止めてから終わる (決定 3)。したがって `service stop` は全 gateway の停止、`service stop` → `service start` は**全断を伴う入れ替え**になる。子を生かしたまま監督者だけを入れ替える経路は持たない — 残ったプロセスが自分の子かどうかは pid では確かめられないので、引き取りを作らない判断 (決定 10) と同じ理由でできない。

この帰結として、**日常の更新は `daemon restart --all` (1 台ずつ、断なし) を使い、`service` 層の操作は監督者自身を入れ替える時だけに限る**。llm-gateway も同じ形 (DR-0028 §11: 行儀よく降りる限り子は道連れ) で、hyoui で変える理由は無い。gateway は状態を持たないので、道連れにされても失われるのは確立済みの WS 接続だけ (決定 8 のとおり、これは前段でも救えない)。

**`service stop` を `launchctl stop` で実装してはいけない。** `KeepAlive=true` なので `launchctl stop` が送る SIGTERM の後 launchd が即座に上げ直す。systemd は逆で、`Restart=always` でも明示 `stop` は尊重する。この非対称を verb ごとに吸収する:

| verb | macOS (launchd) | Linux (systemd --user) |
|---|---|---|
| `register` | `bootout gui/$UID/<label>` (既存を外す) → plist 置換 → `enable` → `bootstrap gui/$UID <plist>` | unit 書き込み → `daemon-reload` → `enable` → `restart` |
| `unregister` | `bootout` → `disable` → plist 削除 | `disable --now` → unit 削除 → `daemon-reload` |
| `start` | `enable` → `bootstrap` (既に載っていれば `kickstart`) | `systemctl --user start <unit>` |
| `stop` | `bootout gui/$UID/<label>` → `disable gui/$UID/<label>` | `systemctl --user stop <unit>` |
| `status` | `print gui/$UID/<label>` + `print-disabled gui/$UID` | `is-enabled` / `show -p LoadState --value` / `show -p MainPID --value` |
| `log` | `~/Library/Logs/hyoui-web/supervise.log` を読む | `journalctl --user -u hyoui-web-supervise` |

`disable` を対で叩くのは、これが再 bootstrap / 再 login を跨いで残る「上げない」指示だからで、`bootout` 単体だと次の login で `RunAtLoad` により復活する。`start` の `enable` はその対。

`register` は冪等。描いた定義が既にそのまま置かれ OS 側にも載っていれば何もせず `changed: false`、違えば同じ label を降ろして置き換え載せ直して `changed: true`。「既に登録されている」を理由に断ると、中身を直したいだけの操作に `unregister` を挟ませることになる。

`service status` は reference の形 (`{registered, running, pid, service: {loaded, running, pid, last_exit}, instances: [...]}`) を採り、そこに監督者の `version` (決定 7a の 3 つ組) を足す:

```json
{"registered": true, "running": true, "pid": 4211,
 "label": "jp.kawaz.hyoui-web.supervise",
 "service": {"loaded": true, "running": true, "pid": 4211, "last_exit": 0},
 "version": {"running": {"version": "0.9.43", "build_id": null},
             "on_disk":  {"version": "0.9.44", "build_id": null},
             "binary": "/opt/homebrew/bin/hyoui",
             "restart_needed": true},
 "instances": [ /* daemon status と同じ行 */ ]}
```

監督者の `running` 版は制御 socket の status 応答に載る `supervisor_version` から取り、`on_disk` は OS 側の定義に焼かれた path の実行ファイルに聞く。`restart_needed: true` は「brew を上げたが監督者を上げ直していない」状態で、そこで打つのが `service stop` → `service start` (全断を伴う、決定 6)。`instances` は監督者に聞いた unit の配列で、監督者が居なければ空配列と `running: false` になる。

### 7. gateway に `/healthz` と `/version` を足す

`hyoui web` の現在の route は `/`、`/sessions/{id}`、`/assets/{*path}`、`/api/sessions*` だけで、死活監視に使える口が無い (`crates/hyoui-web/src/lib.rs:72-80`)。

- `GET /healthz` → 200、body `ok`
- `GET /version` → 200、`{"version": "<crate version>", "build_id": "<build 識別子 | null>"}`

どちらも既存の `/` / `/api/*` と同じ扱い (認証を持たず、到達制限は bind 先と前段の tailnet 制限が担う) で、認証境界を変えない。

`/api/` の下に置かないのは、`/api/*` が session 一覧という業務機能で、その仕様変更 (認証追加・スキーマ変更・daemon socket 走査の失敗) が可用性監視を壊すため。canddy が llm-gateway で `/v1/models` から `/llm-gateway/healthz` へ移した理由 (`Caddyfile:179-185`) と同じ。

`/healthz` は **プロセスが HTTP を返せること**だけを意味し、PTY session の有無・daemon socket の健全性は含めない。gateway は session が 0 でも正常である。

**`version` だけでは走っているビルドを識別できない。** stable (brew の `/opt/homebrew/bin/hyoui`) と unstable (repo の `target/release/hyoui`) は実測でどちらも `hyoui 0.9.42` を答える。crate version は tag を打つまで動かないので、unstable に変更を入れても version は変わらない。本 DR の運用ではこの判別が中心になるので `build_id` を併せて返す。

**`build_id` の既定値は build script が git から導出する。** 環境変数の明示だけに頼ると、通常の unstable ビルド経路 (`just build` = `cargo build --release --workspace`、justfile に env の注入は無く、build script も現状存在しない) で `null` になり、まさに区別したい stable / unstable が両方 `null` で並ぶ。build script (`build.rs`) で次の順に決める:

1. `HYOUI_BUILD_ID` が環境に与えられていればその値を使う (CI / 配布ビルドが明示する経路)
2. 無ければ `git rev-parse --short HEAD` を実行し、作業ツリーに変更があれば dirty を示す接尾を付ける
3. git が使えない / リポジトリでない場合 (brew の tarball ビルド等) は注入せず、実行時は `null`

**dirty の判定は git に聞いて、失敗したら jj に聞く。** 本リポは jj workspace で、git dir は作業ツリーを持たない bare repository として外に在る。そこでは `git status --porcelain` が `fatal: this operation must be run in a work tree` で失敗し、`--work-tree` を与えても git index が最後の jj commit 時点で止まっているため実態と食い違う (実測: git は無関係な 2 ファイルを挙げ、`jj diff --name-only` が実際の 8 ファイルを挙げた)。つまり **git だけに聞くと、まさに unstable を建てる環境で dirty が常に付かない**。git を先に試し、失敗した時だけ jj に聞くことで、jj を他の環境の要件にせずこの環境を救う。

build script は `cargo:rustc-env=HYOUI_BUILD_ID=<値>` で値を渡し、`cargo:rerun-if-changed=<HEAD の実体 path>` と `cargo:rerun-if-env-changed=HYOUI_BUILD_ID` を宣言する。path は `git rev-parse --git-path HEAD` で解決したものを使う — bare repo では `.git/HEAD` というリテラルが存在せず、存在しない path を宣言すると cargo が build script を毎回再実行する。実行側は `option_env!("HYOUI_BUILD_ID")` を読むだけ。`null` は異常ではなく「配布ビルド」の印として読める。

**`-dirty` は「その時ビルドされた版」の印であって、今の作業ツリーの状態ではない。** working copy を編集しても HEAD は動かないので、cargo は build script を再実行せず `build_id` は据え置かれる。毎回再実行させると常時再ビルドになるため、この取りこぼしは受け入れる。`restart_needed` の判断 (決定 7a) は `daemon restart` の前にビルドし直す運用が前提で、ビルドを跨がない編集を検出する仕組みは持たない。

監督者はこの `/version` を各 unit の listen に聞き、**走っている版** (`running`) として持つ。答えない版が走っていることはあるので、答えられなければ `null`。**listen が wildcard の unit への問い合わせ先**は loopback に読み替える (`0.0.0.0:<port>` → `127.0.0.1:<port>`、`[::]:<port>` → `[::1]:<port>`)。

`hyoui --version` の 1 行テキストに `build_id` を含める (`hyoui <version> (<build_id>)`、`build_id` が `null` なら版だけ)。理由は下記の `on_disk` の取り方。

### 7a. 置いてある版と走っている版を並べる

扱う binary が複数になり、入れ替えても走っているプロセスは変わらないので、**「置いてある版」と「走っている版」を並べて出す** (llm-gateway DR-0028 §9 と同じ形)。

- **`running`**: 動いているプロセス自身が答えた版。gateway は `/version`、監督者は制御 socket の status 応答に載せる `supervisor_version`。走っているプロセスがメモリに載せている版は本人にしか言えない
- **`on_disk`**: `binary` が指す実行ファイルを **実行して `--version` を聞いた**もの。次に上がる時の版。ファイルを読んで版を推し量る経路は持たない (ファイルの中の文字列が何を指すかはその binary の作りしだいで、こちらから決められない)。名乗りが `hyoui ` で始まらない出力は「分からない」にする — `--binary` で別の何かが焼かれていることもあり、その出力を版として読むと嘘になる
- **`restart_needed`**: 両方が分かって食い違う時だけ `true`。片方が `null` なのは「食い違っていない」ではなく「比べられない」であって、そこで `true` を出すと答えない版を毎回上げ直させることになる

**比較は `(version, build_id)` の組で行う。** hyoui では crate version が tag を打つまで動かないので、version だけを比べると unstable の入れ替えを検出できない (決定 7 の実測: brew 版も repo build も `0.9.42`)。`build_id` まで含めて初めて「入れ替えたが上げ直していない」が言える。

`binary` (path) を必ず並記する。同じ名前の binary が何箇所にも置かれる環境 (brew / repo の build / 手で置いたもの) では、版だけ見ても「では何を入れ替えればよいのか」に届かない。

#### `hyoui version` を足す

`hyoui --version` は 1 行テキストで自分の版しか言えないので、並記は別の subcommand にする。`hyoui version` が JSON で出す:

```json
{
  "cli": {"version": "0.9.44", "build_id": "a1b2c3d"},
  "supervisor": {
    "running": {"version": "0.9.43", "build_id": null},
    "on_disk": {"version": "0.9.44", "build_id": null},
    "binary": "/opt/homebrew/bin/hyoui",
    "restart_needed": true
  },
  "units": [
    {"name": "stable",
     "running": {"version": "0.9.44", "build_id": null},
     "on_disk": {"version": "0.9.44", "build_id": null},
     "binary": "/opt/homebrew/bin/hyoui",
     "restart_needed": false},
    {"name": "unstable",
     "running": {"version": "0.9.44", "build_id": "9f0e1d2"},
     "on_disk": {"version": "0.9.44", "build_id": "7c8b9a0-dirty"},
     "binary": "/Users/…/target/release/hyoui",
     "restart_needed": true}
  ]
}
```

`supervisor` の `binary` は OS 側の定義 (plist / systemd unit) に焼かれている path から読む。登録が無ければ `supervisor` は `null` — 対象自体が存在しない。監督者が居なければ `running` が `null` になる (`on_disk` は読める)。

`hyoui --version` (テキスト 1 行) はこの CLI 自身の版を言う口として残す。

### 8. stable / unstable の HA は前段 proxy が担う。hyoui は担わない

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

canddy 側の変更は **canddy リポの issue として依頼する**。設定の正本は canddy が持ち、hyoui が書き換えない。

#### この HA が救う壊れ方の範囲

**fallback が起きるのは、unstable のプロセスに到達できない時だけ。** `unhealthy_status` を指定しないので失敗判定は dial 失敗に限られる (`Caddyfile:171-173`)。救えるのは「上がらない」「落ちた」「bind に失敗して監督者が backoff で再試行している」といった、プロセスとして立っていない壊れ方。

**応答するが誤るタイプの壊れ方は救えない。** unstable が起動して HTTP を返すなら、前段はそれを健全とみなす。次のどれも stable に回らない:

- 新しい cap を要求する gateway が古い daemon と handshake に失敗して 5xx を返す
- screen dump が壊れた内容を 200 で返す
- WS の handshake が通った後で attach が機能しない

**WebSocket attach (`/api/sessions/{id}/attach`) は fallback の対象外**。確立済みの WS は選ばれた upstream に固定され、その unit が落ちれば切れる。回るのは新規接続だけで、これは reverse proxy の性質であって hyoui 側で埋められるものではない。

**TCP は生きているのに応答が返らない (hang) 場合の窓**も残る。active health check が unhealthy と判定するまで最大 `health_interval` + `health_timeout` = 7 秒あり、その間のリクエストは unstable に渡る。`lb_try_duration` は dial に失敗した時に次の upstream を試す猶予なので、dial 成功後の無応答には効かない。渡ったリクエストは前段の response timeout (未設定ならクライアント側の timeout) まで待たされる。ここを詰めるのは前段の設定の話で、hyoui 側の決定には含めない。

#### cap 差は gateway 側で吸収する

2 つの gateway は同じ socket dir を走査して各 daemon と handshake する。gateway は現在 `MVP_CAPS` 全部を要求して接続し、handshake の失敗は 500 になる (`crates/hyoui-web/src/lib.rs:200-204` ほか)。cap は intersect で落ちる仕様なので (`crates/hyoui/src/protocol/caps.rs:30-38`)、新しい gateway が新 cap を必要とする機能を旧 daemon に対して呼ぶと、その機能だけが成立しない。

**この差は gateway 側で機能単位に落とす。** daemon と intersect した結果に無い cap を要する操作は、その操作だけを「未対応」として返し (501 相当)、gateway 全体を 5xx にしない。逆方向 (新しい daemon + 古い stable gateway) も同じで、古い gateway が知らない message は使わないだけになる。

**新しい message を要する DR は、2 版の gateway が同居する前提で互換を書く。** 本 DR 以降、stable と unstable は常に別版なので、「gateway と daemon の版が揃っている」前提を置ける場面が無くなる。

### 9. ログは監督者が unit 名で分けて書き、回転は OS に任せる

子は stdout / stderr に書くだけで、置き場を知らない。監督者がそれを受けて ``${XDG_STATE_HOME:-~/.local/state}/hyoui-web/logs/<name>.log`` へ追記する。同じ行は追従している client へも配るので、`daemon log --follow` は書かれた先を読み直さずに済む。子に置き場を教えないのは、置き場が変わるたびに全 unit の登録を書き直すことになり、「監督者が抱える」という決定 3 の形が崩れるため。

**回転は hyoui が持たない。** OS の仕組み (macOS は newsyslog、Linux は logrotate) に任せ、hyoui 側は追記しかしない。自前で持つと、大きさの上限・世代数・圧縮の有無という運用ごとに違う判断を hyoui が代わりに決めることになり、しかも OS の仕組みと二重になる。

ただし監督者は追記の fd を子が終わるまで握り続ける。rename で回転させる方式 (newsyslog / logrotate の既定) では、回転後も監督者は元の inode に書き続け、新しいファイルは子が入れ替わるまで空のままになる。回転の設定はシグナルを送らない形で置き、実際に切り替わるのは次の入れ替えの時、と読む。具体的な書き方は移行 runbook に置く。

### 10. 監督者が落ちても子は残る。次の監督者は拾わずに起こし直す

行儀よく降りる限り (SIGTERM / SIGINT)、監督者は抱えている子を全部止めてから終わる。一方 SIGKILL や異常終了で監督者が消えた場合、子は残る。

**引き取りは作らない。** 走っているプロセスが誰の子かは、pid を書き留めても再起動を跨いで確かめられない (pid は使い回される)。確かめられないものを頼りに「これは自分の子だ」と決めると、無関係のプロセスを畳む経路ができる。

次に上がった監督者は登録簿どおりに子を起こし直すので、残った子と待ち受けポートが衝突し、新しい子は bind に失敗して backoff に入る。**この状態は `status` から読める** — `running: false` + `restarts` の増加 + `last_exit` に bind failure が出る。残った子は人が畳む。

**孤児が port を握っている間、前段からは健全に見える。** 孤児も gateway なので `/healthz` に 200 を返し、canddy は普通に振り分ける。外から見た可用性は保たれるが、`daemon status` は `running: false` を出し続けるので、CLI の見え方と実際の応答者が食い違う。この食い違いは `status` の `restarts` / `last_exit` から気づける形にしておく (孤児が居ることの唯一の手がかりになる)。

### 11. 既存 `hyoui web service register|unregister|status` は置き換える

verb 名は残るが、載せる対象が gateway 1 台から監督者に変わる。alias も移行期間も設計に入れない (この CLI の利用者は kawaz だけで、破壊的変更を受ける第三者が存在しない — kawaz 明言、2026-09-15)。

**移行の前に brew リリースを出す。** `stable` unit が指すのは brew 配布版で、その版が `web daemon run` を持っていなければ監督者は子を起こせない。旧版でも通る argv を用意する形は採らない (Alternatives) ので、順序で解く:

1. 本 DR の P2〜P4 を含む版をリリースし、`brew upgrade` で `/opt/homebrew/bin/hyoui` を入れ替える
2. `launchctl bootout gui/$UID/com.github.kawaz.hyoui-web` → plist 削除
3. `hyoui web daemon add stable --port=43690` / `hyoui web daemon add unstable --port=43691 --binary=<repo>/target/release/hyoui`
4. `hyoui web service register` (監督者が載り、2 unit が上がる)

**この手順は runbook の手作業とし、`register` に旧 label を引き取る経路は作らない。** 対象が 1 台しか無いものを CLI に持たせると、一度通ったら二度と通らないコードが製品に残る。

`hyoui web` 自身 (foreground 起動) と `--listen` / `--web-assets-dir` は変わらない。

## やらないこと

| やらないこと | 理由 |
|---|---|
| PTY session daemon の multi-unit 化 | session は既に複数走り、socket dir が正本 (DR-0006 §1/§2)。本 DR の unit は gateway インスタンスに限る |
| hyoui 自身が優先順付き proxy / HA を持つ | 前段 proxy が既に持つ機能の再実装になり、その front 自体が単一障害点になる |
| unit ごとの launchd / systemd 登録 | 決定 6。OS に載せるのは監督者 1 つ。Alternatives の「監督者を置かない案」を裁定で不採用にした帰結 |
| 監督者不在時に CLI が子を起こす | 決定 4。子の所有者が 2 つになる |
| 監督者が登録簿を定期的に舐めて差分を取る | 間隔に根拠が無く、間隔の内側で起きた往復を取りこぼす |
| 残った子の引き取り | 決定 10。pid は再起動を跨いで同一性を保証しない |
| 子を生かしたまま監督者だけを入れ替える | 決定 6。引き取りができないのと同じ理由。日常の更新は `daemon restart --all` が担うので、この経路が無くても断なしの入れ替えは成立する |
| 再起動時の graceful drain | 前段が新規接続を他 unit に回すので待ち合わせ不要。SIGTERM で即座に降りる前提。WS は fallback 対象外 (決定 8) なので drain を足しても救われる範囲は増えない |
| ログ回転 | 決定 9。追記のみ、回転は OS の仕組みに任せる |
| 版をファイルの中身から読み取る (hash / 埋め込み文字列の走査) | 決定 7a。ファイルの中の文字列が何を指すかはその binary の作りしだいで、こちらから決められない。`on_disk` は実行して `--version` を聞く |
| gateway 間の状態共有・session の引き継ぎ | 各 gateway は daemon socket を走査するだけで自前の状態を持たないので、共有すべき状態が無い |

## Alternatives Considered

| 案 | 不採用理由 |
|---|---|
| **監督者を置かず、unit 1 つ = OS service 1 つにする** | 目的が達成できない。台数ぶんの plist / systemd unit が並ぶので、状態を見る・再起動する時に「どの label が何台あるか」を先に思い出す作業が残り、操作も `launchctl` / `systemctl` の使い方に戻る (目的節の第一の目的)。`service` と `daemon` で status / log のスコープも分かれず、OS 登録の話と子の生死の話が 1 つの出力に混ざる。kawaz 製 CLI 共通の reference 体系から外れる点も同じ方向 (kawaz 裁定 2026-09-15、SVC-Q2=b) |
| verb 群を `hyoui service` / `hyoui daemon` (top-level) に置く | kawaz 裁定 (SVC-Q1=a) で不採用。`hyoui daemon` は PTY session の daemon と語が衝突し、kind が増えた時に `--kind` で option 集合が分岐する。`hyoui web` 配下なら `add` の option が `hyoui web` の引数の写しになり、将来の kind は `hyoui session daemon` として並べられる |
| 監督者が旧版 binary でも通る argv (`web --listen=...`) を exec する | 旧版との互換を設計判断に入れない方針。`web daemon run <name>` を選ぶ理由は「監督者が unit の中身を解釈しない」という責務の線引き (決定 3) であり、どの版で通るかは判断材料にしない。配布版が新しい subcommand を知らない問題は、リリースを先に出す順序で解く (決定 11) |
| 旧 `web service` を alias として温存する | 同じことをする口が 2 つ増え、help と completion にも 2 つ載る。利用者は kawaz だけで、互換のために語彙を濁す相手が居ない |
| unit を port で識別する (名前を持たない) | port は「今どこで待つか」であって unit の同一性ではない。port を変えた瞬間に別 unit になり、`stable` の設定を 43690 → 43695 に移す操作が表現できない。stable / unstable という役割も名前でしか書けない |
| unit 定義を `~/.config/hyoui/config.toml` に `[[web.unit]]` として書く | config.toml は人が編集する設定 (DR-0024)、登録簿は CLI が書き換える状態。`enabled` のような desired state を人の設定ファイルに書き込むと、人の編集と CLI の書き込みが同じファイルで競合する |
| `restart --all` を同時再起動にする | 短くても全断が出る。rolling で避けられるものを受け入れる理由がない |
| 死活監視に `/api/sessions` を使う | 業務エンドポイントの変更が可用性監視を壊す。canddy が llm-gateway で同じ理由で移した実績がある |
| `/healthz` が daemon socket の健全性まで見る | gateway は session が 0 でも正常。daemon 側の異常で gateway を切り離すと、復旧の入口 (Web UI) ごと失う |
| `build_id` を env の明示だけに頼る | `just build` に env 注入が無く、識別したい 2 つが両方 `null` で並ぶ (決定 7) |

## Implementation phases

| Phase | 内容 | gate |
|---|---|---|
| P1 | `GET /healthz` / `GET /version` (`build_id` 込み)、`build.rs` の git 由来既定、`hyoui --version` に `build_id` を載せる | 単体 test で 200 / JSON 形 / `build_id` 未設定時の `null`。repo build と brew 版で `build_id` が異なることを実機で確認。`--version` の 1 行から `(version, build_id)` を取り出す parse の test (名乗りが違う出力は「分からない」になること) |
| P2 | 登録簿 (`units/<name>.toml`) と `add` / `remove` / `list`、`daemon run <unit>` | parser test (各 leaf、`--port` と `--listen` の排他、不正な unit 名、必須引数欠落時の help)。listen / assets_dir の解決と衝突拒否の test。登録簿の round-trip test と tmp + rename の atomic write test。**`hyoui list` に `hyoui-web` root の中身が現れないことを確認** (決定 2、discovery の走査 base と分かれているか)。`daemon run <unit>` が登録簿の listen で上がることを実機確認 |
| P3 | `supervise` + 制御 socket + `start` / `stop` / `restart` / `status` / `log` | 監督者不在時に `supervisor_not_running` + hint を返す test。子が落ちたら上がる / backoff 上限に達する test (backoff の定数は注入可能にして test では短い値を使う。実時間で 60 秒を待つ test は書かない)。`add` / `remove` が走行中の監督者に即反映される test と、監督者不在時に登録簿だけが変わる test。`version` / `status` の 3 つ組の test: 両方分かって食い違う時だけ `restart_needed: true`、片方 `null` なら `false` (ディスクを読む口を差し替えて、実際の実行ファイルを走らせずに組み立てを検証する)。`restart --all` を 127.0.0.1 直叩きで観測 (前段経由は P6)。`stop` した unit が復活しないことを観測 |
| P4 | `service register` / `unregister` / `start` / `stop` / `status` / `log` を監督者向けに置き換え | plist / systemd unit の golden test。隔離 HOME での未登録 status と全 help topic。`service stop` 後に監督者が KeepAlive で復活しないことを実機で観測。`service stop` で子 gateway も落ちることを観測 (決定 6 の明文化どおりか) |
| P5 | brew リリース (P2〜P4 を含む版) → 旧 `web service` の意味の切り替え完了、help / completion / 実装の 3 者同期、移行 runbook、既存 1 台の移行 | `hyoui web daemon --help` / `hyoui web service --help` と completion 定義の突き合わせ test。実機で stable + unstable の 2 台が監督者の下に常駐し、`status` が両方の `build_id` を別々に出す。**目的節の契約を実機で確認**: unstable を再ビルドした時点で `hyoui version` が `restart_needed: true` を出し、`daemon restart unstable` だけで `running` の `build_id` が `on_disk` に一致して `restart_needed: false` に戻ること (`service register` も plist の確認も要らない) |
| P6 | canddy リポへ upstream 設定の issue 起票、前段経由の HA 実機検証 | unstable を `stop` した状態で新規リクエストが stable に回る。stable のみ停止でも同様。`restart --all` 実行中に前段経由の断が出ない。3 つが揃って完了 |

P6 の検証は DR-0014 の検証主義に従い 1 回の観察で結論しない。停止させる側 (unstable / stable)、止め方 (`daemon stop` / `kill -9` / `kill -STOP` で TCP は受けるが応答しない状態 / bind 失敗させて backoff に入れる)、リクエストの種類 (HTML / `GET /api/sessions`) の組合せでマトリクスを埋める。`kill -STOP` の列は決定 8 の「窓」の実測値になるので、「断が出ない」と書ける範囲をこの列の結果で限定する。

決定 8 の「救えない範囲」も観測して記録する。WS attach は「切れることを確認した」と書く (「該当なし」ではない)。応答するが誤る壊れ方 (5xx を返す gateway) を 1 ケース作り、**stable に回らないこと**を観測する — 期待どおりの挙動であり、後から「HA があるのに救われなかった」と読まれないために記録に残す。

## Consequences

- gateway の常駐が unit 単位になり、台数の増減が CLI で完結する。plist を人が書く経路が無くなる
- 常駐プロセスが 1 種類増える (監督者)。gateway の生死は launchd → 監督者 → 子の 2 段になり、`status` を読む時はどちらの段の話かを区別する必要がある
- unit ごとに binary が違ってよくなるため、unstable に開発ビルドを常駐させる dogfooding が成立する。ただし前段が吸収するのは **unstable のプロセスに到達できない壊れ方だけ** (決定 8)。応答するが誤る壊れ方は unstable を叩いた人にそのまま出る
- `hyoui` に build script が入る。`build_id` の注入がビルド手順に加わる (未設定でもビルドは通り `null` になる)
- `hyoui version` (JSON) が増える。`hyoui --version` の 1 行にも `build_id` が付く (テキスト出力の変更)
- `version` / `status` は登録簿の `binary` を **実行して** `--version` を聞くので、`status` を打つと unit ごとに短命のプロセスが 1 つ走る。`binary` が消えている unit では失敗して `on_disk: null` になり、`binary_exists: false` と併せて原因が読める
- `/healthz` / `/version` の 2 つが増えるが、既存 route も認証を持たないので認証境界は変わらない。どちらも session の内容を返さない
- 前段 proxy への依存が可用性設計に入る。canddy が落ちれば tailnet 経由の到達は失われる (127.0.0.1 直結は影響を受けない)
- systemd 経路は DR-0031 と同じく **書けるが未検証**のまま残る。macOS の launchd 経路だけが実証済みという扱いを維持する

## 参照した素材

- `docs/issue/2026-09-15-service-subcommand-multi-unit-ha.md`
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/cli-daemon-subcommands.md` (`daemon` / `service` 体系の正本)
- `~/.local/share/repos/github.com/kawaz/claude-rules-personal/main/reference/design-spec/spec-preflight.md`
- `~/.local/share/repos/github.com/kawaz/llm-gateway/main/docs/decisions/DR-0028-daemon-service-subcommands.md` (§3 監督者、§9 版の並記、§10 ログ、§11 監督者死亡時)
- `~/.local/share/repos/github.com/kawaz/llm-gateway/main/crates/llm-gateway/src/daemon/` の `registry.rs` / `protocol.rs` / `supervisor.rs` (`Unit` / `Request` / `Which` / `UnitStatus`、backoff と grace の定数)、`crates/llm-gateway-cli/src/service.rs` / `service/platform.rs` / `version.rs` (`on_disk` を実行して聞く / `pair` の restart_needed 判定 / ディスクを読む口の注入)
- `~/.local/share/repos/github.com/kawaz/canddy-app-proxy/main/README.md` / `Caddyfile` (hyoui ブロック 127-133、llm-gateway ブロック 144-194)
- 本リポ: `docs/decisions/DR-0031-web-service-subcommand.md`、`DR-0006-cli-ground-rules.md`、`crates/hyoui/src/discovery.rs:9-13` / `:167-190` (走査 base とサブ dir = namespace の扱い)、`crates/hyoui-cli/src/web_service.rs`、`crates/hyoui-cli/src/completion.rs`、`crates/hyoui-cli/tests/web_service_e2e.rs`、`crates/hyoui-web/src/lib.rs`、`crates/hyoui/src/protocol/caps.rs`、`crates/hyoui/src/config/mod.rs`、`justfile`
- llm-gateway `crates/llm-gateway/src/daemon/supervisor.rs:280-295` (`restart --all` の逆順) / `:314-325` (停止中 unit を無条件 enable)
- 実機出力: `hyoui --help` / `hyoui web --help` / `hyoui web service --help` / `hyoui web service status` (`/opt/homebrew/bin/hyoui` 0.9.42)、現行 plist の全文 (`ProgramArguments` は `hyoui web` の 2 語、`KeepAlive=true`)、`target/release/hyoui --version` (= 0.9.42、brew 版と同一で build 識別不能)

## 関連

- DR-0031 — 単一 unit 版の service 登録 (本 DR の P5 完了時点で Superseded)
- DR-0027 — gateway を同 repo に置く判断
- DR-0033 — `leader.request`。cap 差が具体化する例 (決定 8)
- DR-0014 — 介入 self-check とマトリクス検証
