# DR-0038: web の unit を config ファイル 1 つにし、置き場を `hyoui/web/` に揃え、service に場所の env を固定する

- Status: 🚧 Active (2026-10-04)。決定 1〜9 は実装済。移行済みで、古い置き場の symlink の撤去 (決定 4 移行節の 4) が残っている
- Date: 2026-10-04
- Supersedes (部分): DR-0034 決定 1 の `add` の形と `hyoui web` 単体起動 / 決定 2 の unit の中身と置き場 / 決定 6 の環境と log の置き場 / 決定 9 の log の置き場 / 決定 11 の「`hyoui web` 自身は変わらない」、DR-0036 決定 4 の `auth.json` / `pending.json` の置き場
- Related: DR-0034 (2 系統の体系と監督者、本 DR が置き換えない部分はすべて有効), DR-0024 (config ファイル機構), DR-0018 (session namespace と socket dir), DR-0036 (passkey の state file), DR-0014 (介入 self-check)
- Origin: `docs/issue/2026-10-04-web-unit-registry-holds-settings.md` (kawaz と合意 2026-10-04)、決定 9 は `docs/issue/2026-10-05-web-unit-config-state-dir-and-add-generates.md` (kawaz と合意 2026-10-05)

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
state_dir = "~/.local/state/hyoui"   # この unit の面 (決定 9)。土台には書かない
listen = "127.0.0.1:43691"
binary_path = "~/src/hyoui/target/release/hyoui"
```

`[web]` の鍵は `listen` (既定 `127.0.0.1:43690`) / `assets_dir` (無ければ埋め込み assets) / `binary_path` (決定 2) / `state_dir` (unit の config では必須、決定 9)。

**web の設定は web の config の中で閉じる。** gateway は PTY session の設定 (`config.toml` の `[scrub_env]` / `[attach]` / `[session]`) を使わないので、`config.toml` を土台にせず、gateway の起動経路は `config.toml` を読まない (`daemon run` が読むのは決定 9 の 3 形態だけ)。`config.toml` から `[web]` を外し、`[web] listen` / `[web] assets_dir` が書かれていれば移し先を案内して起動を断る (DR-0032 §1 の廃止 key と同じ扱い。黙って無視すると、書いた人の意図が既定値に倒れる)。

### 2. 登録簿は `{config, binary_path, enabled, added_at}`。登録簿が持つのは config の path

`add` / `run` の形は決定 9。

- `add` は config を絶対 path にして登録簿に書く。symlink は解かない (= 利用者が symlink の向き先を差し替えれば unit も追従する)。登録の時点で config が読めることを確かめ、読めなければ断る (= 監督者が起こすたびに子が config で落ちる unit を作らない)
- `binary_path` は config の `[web].binary_path` を正とし、無ければ `add` を打った自分自身 (`current_exe`) を焼く (`add` が生成する config には常に書く、決定 9)。`add` の時点で登録簿に写すので、config の `binary_path` を変えたら `remove` → `add` で入れ直す (llm-gateway DR-0028 決定 2 と同じ)。`resolve_stable_path` を通さない理由は DR-0034 決定 2 のまま
- `listen` / `assets_dir` は登録簿に写さない。子の `daemon run <name>`・監督者・`list` / `status` が読むたびに config から引く。config を書き換えれば次の起動から効き、登録し直す必要が無い
- `add` の listen 衝突の検査 (DR-0034 決定 2) は残す。既存 unit の listen は各 config から引き、読めない config の unit は比べず warning にする。port 0 (= kernel に任せる) はどれとも衝突しない。登録簿に無いプロセスが掴んでいるポートの確認は決定 9
- 登録簿は `deny_unknown_fields` のままにし、設定値を書いた古い形のファイルは読まない (= 一部の鍵だけを黙って使わない)

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
| web の config の既定 | `${XDG_CONFIG_HOME:-~/.config}/hyoui/web/` (土台 `base.toml`、unit ごとに `<unit>.toml`。`add <unit>` が生成する、決定 9) |
| 登録簿 | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/units/<name>.toml` |
| unit のログ | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/logs/<name>.log` |
| 監督者自身のログ (launchd) | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/logs/<label>.log` |
| passkey の state | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/auth.json` / `pending.json` (+ `.lock`) |
| 監督者の制御 socket | `${XDG_STATE_HOME:-~/.local/state}/hyoui/web/run/supervisor.sock` |
| 監督者の OS 登録名 | launchd label `com.github.kawaz.hyoui.web.supervise.<hash>`、systemd user unit は同じ文字列 + `.service` |

`hyoui-web` のような別名は、dir にも OS 登録名にも作らない。理由は 3 つ:

- **CLI の階層と揃う。** `hyoui web ...` の state と config が `hyoui/web/` にあれば、どこを見ればよいかを CLI の語から辿れる
- **アクセス許可の書き方と揃う。** auto mode classifier の環境説明は「リポで作業中のセッションから `$XDG_*_HOME/<リポ名>/` へのアクセスを許可」という形で書かれている。`hyoui-web` はこの形から外れて拒否されやすく、classifier 側に例外を持ち込むより置き場をこの単純な形に合わせる
- **OS 登録名もアプリの名前空間をぶらさない。** reference `cli-daemon-subcommands` の「OS 登録の名前」節に従い、逆引き DNS は**所有を確かめた名前空間だけを使い、既定は `com.github.kawaz.<repo>`** とする (GitHub アカウントの所有と常に一致するので確認が要らない。自前ドメインは手放しや更新切れで正当性を失いうる)。先頭をリポ名にして `hyoui.` の下に置き、その下を CLI の階層と同じ `web.supervise` にする。systemd の unit も同じ文字列に `.service` を付け、OS ごとに名前を分けない。組み立ては `web_service.rs` の 1 か所

**label の末尾には状態 root の hash を常に付ける。** `<hash>` は hyoui の状態 root (`${XDG_STATE_HOME:-~/.local/state}/hyoui`、決定 5 の場所を決める env から導く) を realpath で正規化した絶対パスの sha256 の先頭 4 byte (16 進 8 桁)。

- 面 (状態 root) ごとに監督者を 1 つずつ OS に並べて載せられる。既定の面も含めて常に付け、既定かどうかの判定は持たない (= 判定の分岐を作らない)
- realpath で正規化するので、symlink 越しに同じ root を指せば同じ label になる。`service register` は label を決める前に root を作る (= realpath が取れる)。一度も登録していない面で root が無い時は、正規化前の絶対 path で代える
- label は人が覚える名前ではない (CLI 経由で操作する)。どの面の監督者かは `service status` が label と並べて出す `root` (label を作った root) と `env` (定義に固定した env) で読む。登録の列挙は接頭辞 `com.github.kawaz.hyoui.web.supervise.` で引ける
- 場所を決める env が違う shell から見ると別の label になる (= 別の面の監督者として扱われる)。決定 5 の差分検知は同じ root の定義を書き直す時に効き、別の root の定義は別物として残る

監督者のログを state の中に置くのは先行の llm-gateway / ccmsg と同じで、label の名前にするので unit のログ (unit 名は `.` を含まない) と衝突しない。

**監督者の socket は `web/` 直下に置かず `run/` に 1 段下げる。** `${XDG_STATE_HOME}/hyoui/` は session socket の base (DR-0018) で、discovery (`crates/hyoui/src/discovery.rs`) は直下の dir をすべて namespace とみなし、その中の `*.sock` に hyoui protocol で問い合わせる。`web/supervisor.sock` に置くと、`hyoui list --all-namespaces` と web gateway の `/api/sessions` に namespace `web` の session `supervisor` として並ぶだけでなく、discovery の handshake と監督者の 1 行読み (JSON 1 行の制御 socket、DR-0034 決定 4) が互いの応答を待ち合い、**監督者の event loop が 1 回 5 秒止まる** (実測: `list --all-namespaces` が 5.05 秒、その最中の `web daemon list` が 4.95 秒)。gateway は `/api/sessions` のたびに discovery を回すので、常駐すれば監督者が繰り返し止まる。discovery は 1 段しか潜らないので `run/` の中は見ない。`units/` / `logs/` に `*.sock` は無いので同じ理由で拾われない。

**`hyoui/web/` と session socket の木の衝突は session 側の別 DR で解消する (それまでは namespace `web` の session を作らない運用)。** session socket を `hyoui/sessions/<uuid>.sock` にフラット化し namespace を廃止する設計 (`docs/issue/2026-10-04-design-session-id-uuid-and-tags.md`) の範囲で、本 DR は session の socket の場所・namespace・discovery を変えない。予約語は足さない (namespace を廃止する設計の前に、消える概念へ例外を増やさない)。

#### 移行: 移動して古い置き場に symlink を残し、後で必ず消す

1. **状態 dir は丸ごと新しい置き場へ移し、古い dir 名を symlink にする。** `$XDG_STATE_HOME/hyoui-web` → `$XDG_STATE_HOME/hyoui/web` へ移動し、`$XDG_STATE_HOME/hyoui-web` を `hyoui/web` への symlink にする。古いバイナリも同じ実体 (passkey の `auth.json` を含む) を見るので、新旧が混在する間も登録済みの passkey が失効しない。監督者のログ dir も同じで、`~/Library/Logs/hyoui-web` の中身を `$XDG_STATE_HOME/hyoui/web/logs/` へ移し、`~/Library/Logs/hyoui-web` をそこへの symlink にする
2. **新しいバイナリは新しい置き場だけを見る。** symlink は古いバイナリのためだけにあり、新しいバイナリは古い名前を読まない (= 同じ実体を二重に拾わない)
3. **新しいバイナリは、古い置き場 (状態 dir とログ dir) が残っていれば `hyoui web ...` の起動時に stderr へ警告する。** 全 verb の入口で見るので、監督者と子の `daemon run` も警告し、それは監督者のログに残る。symlink なら「古いバイナリ用の symlink が残っている、後で消す」、実体の dir なら「移行していない (このバイナリは読まない)」と言い分ける
4. **symlink を消す条件は版で決める:** 本 DR を含む版より前の hyoui (= 古い置き場を読む版) が手元で 1 つも動いていない (brew 版・repo build・監督者・その子の全部が本 DR 以降の版) こと。消したら (3) の警告は黙る。警告のコードは、その次の版で外す

5. **OS 登録名が変わるので、旧 label (`jp.kawaz.hyoui-web.supervise` / systemd `hyoui-web-supervise`) の定義を外してから新しい label (`com.github.kawaz.hyoui.web.supervise.<hash>`) で register する。** 新しいバイナリは旧 label を操作しない。旧 label の定義ファイルが残っていれば (3) と同じ入口で警告する (= 旧い監督者が新しい監督者と並んで同じ port を取り合うのを見逃さない)

**移動と symlink の作成、旧 label の取り外しを自動で行うコードは書かない。** v1.0 前で利用者は kawaz だけなので、移行は人が 1 回行う。一度通ったら二度と通らないコードを製品に残さない (DR-0034 決定 11 と同じ理由)。古い登録簿 (`units/*.toml`) は形が変わったので、移した後に新しい形で `daemon add` し直す。

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

`hyoui web` は `daemon` / `service` / `passkey` / `session` を束ねるだけで、gateway を起動する口を持たない。引数なしと `--help` は help、`--listen` 等の option は `hyoui web daemon run` を案内して断る。DR-0034 決定 1 の「bind 先を明示した `hyoui web --listen=<host:port>`」は削除する。`daemon add` の `--port` / `--web-assets-dir` も削除する (= 値は config に書く)。`daemon add` の `--listen` / `--binary` は、add が生成する config に書く値として持つ (決定 9)。

### 8. 出力の field

- unit の行 (`list` / `status` / `add`) は `config` / `listen` (config から読んだ値、読めなければ `null`) / `config_error` (読めなかった理由) / `binary_path` / `binary_exists` を持つ。`add` はさらに `generated` (config を書いたか) と `state_dir` (今の面の状態の root) を持つ。実行ファイルの path は reference の `daemon list` の語に合わせて `binary_path` と呼び、`hyoui version` の `supervisor` / `units[]` と `service status` の `version` も同じ名前にする (DR-0034 決定 7a の `binary` を置き換える)
- 監督者の情報 (`supervisor`) に `binary_path` と `locations` (決定 5) が入る
- `service register` の出力に固定した `env` が入る。差分で止まった時は `kind: "location_env_drift"` と `differences` を stderr の JSON に入れる

### 9. unit の config は `<unit>.toml` で `state_dir` を必須に持ち、`add` が生成する。unit の名前に既定値は持たせない

```text
hyoui web daemon add <unit> [--listen <host:port>] [--binary <path>]
hyoui web daemon add <unit> --config <path>
hyoui web daemon run <unit>
hyoui web daemon run --config <path>
hyoui web daemon run --no-config [--listen <host:port>]
```

面と置き場の前提は DR-0041 決定 6 (面は状態の root を決める環境変数で決まる。config は面で分けず共有し、面ごとに違うのは状態だけ)。

**unit の config は `<unit>.toml`。** ファイル名に面の key 等を入れない。面は中の `state_dir` で分かり、ファイル名にも入れると二重管理になる。

**`state_dir` は unit の config ファイル自身の `[web]` に必須で書く。** `daemon add` と config を読む `daemon run` (`<unit>` / `--config`) は、config の `state_dir` と今の面の状態の root を realpath で正規化して比べ、食い違えば「この config は面 X のもので、今は面 Y で実行している」と断る。

- 面同士は互いの登録簿を見られないので、別の面の config を (コピー等で) 登録・起動した事故に気付ける場所は config 自身しかない
- **`extends` で土台から継いだ `state_dir` は認めない。** 確かめるのは、`extends` を畳む前の unit の config ファイル単体の `[web]` に `state_dir` が書かれていること。書かれていなければ (土台から継いでいても) 書くべき値を案内して断る。土台は面をまたいで共有するので、土台に書くと同じ土台を指す他の面の unit が全部それを継ぎ、面の食い違いを config で捕まえられなくなる。`--config` で登録・起動する config も同じ扱い
- 土台 (`base.toml`) には `state_dir` を書かない。土台に書かれていること自体は機械的には断らない (unit の config ファイル自身が書いていれば、その値が勝つ)
- 他の path の鍵と同じく、書いたファイルの隣から解き `~` を `$HOME` で開く (決定 3)
- 監督者・`list` / `status` は `state_dir` を見ない (= listen だけを引く、決定 6)。起動するかを決めるのは子の `daemon run` で、食い違えば子が起動を断り、監督者の `last_exit` とログに理由が残る

**`daemon add <unit>` が config を生成する。**

- `--config` が無ければ `${XDG_CONFIG_HOME:-~/.config}/hyoui/web/<unit>.toml` を使う。無ければ書く: 隣に `base.toml` があれば `extends = "base.toml"`、`state_dir` (今の面の状態の root)、`listen` (`--listen`、既定 `127.0.0.1:43690`)、`binary_path` (`--binary`、既定は `add` を打った自分自身。相対 path は cwd から絶対 path にする)
- 既にそのファイルがあれば生成せず、中の `state_dir` を確かめて登録する。食い違えば断る (= 別の面が同じ名前を使っている。黙って上書きしない)。既にあるファイルに `--listen` / `--binary` は書けないので、付いていれば断る (= 黙って捨てると、書いた人の意図が config の値に倒れる)
- `--config <path>` は既存のファイルをその名前で登録するだけで、生成しない (同じく `state_dir` を確かめる)。`--listen` / `--binary` とは併用できない
- **add 全体を面の登録簿の排他 lock の中で行う。** 名前と listen の検査、config の生成、登録簿への書き込みを 1 つの lock (`<web の状態の置き場>/units.lock`、登録簿のファイルとは別のファイルに `flock`) の中で行う。lock の外で検査すると、並行する add 同士が同じ名前・同じポートを通し合い、片方の後始末がもう片方の参照する config を消しうる。add は lock を待たずに取り、他の add が持っていれば何も書かずに断る (= 人が打つ add 同士が重なった時、後の add は前の add の結果を見て打ち直す)。`remove` と監督者の `enabled` の書き換えも同じ lock を取る (こちらは待つ)。監督者への通知は lock を離してから行う
- 名前の重複・listen の衝突・使用中のポートは、ファイルを書く前に断る
- 生成した config も、登録する前に `extends` を含めて読めることを確かめる (決定 2)。土台が壊れている (不正な TOML、`extends` の先が無い、循環する等) と読めないので、登録せず、生成したファイルを消して断る
- 生成は隣の一時ファイルに書き切ってから、同名が無い時だけ `<unit>.toml` として公開する。書き込み途中で失敗しても半端なファイルは現れない。登録まで済まなかった add は、自分が生成したファイルだけを消す (既にあったファイルや土台は消さない)
- config を読めなかった時・書けなかった時のエラーは、どのファイルをどう直してから打ち直すかを `hint` に書く

**unit の名前に既定値は持たせない。** `default` という名前は既定値を管理しているように見える。名前を省いた `add` と、何も付けない `run` は help。reference `cli-daemon-subcommands` の `run [unit]` は名前の既定値を案件に委ねており、hyoui は持たない側を選ぶ。DR-0034 決定 1 の「既定の unit は持たない」(= 登録簿に 1 つしか無い時それを選ぶ推測を入れない) もこれで保たれる。

**listen の既定値 (`127.0.0.1:43690`) は残す。** `add` の時点で、同じ面の登録簿に同じ宛先の unit が無いか (決定 2)、そのポートを今ほかのプロセスが listen していないか (実際に bind を試す) を確かめ、当たれば「使用中、`--listen` で指定する」と断る (既存の config を登録する時は「その config の `listen` を変える」)。空いているポートを自動で選ばない。port 0 は確かめない。bind が使用中以外の理由で失敗した時 (解決できない宛先等) は warning に留める。

**`daemon run` は 3 形態のどれか 1 つを取る。**

- `run <unit>`: 登録簿が指す config で起動する
- `run --config <path>`: 登録簿を通さずその config で起動する
- `run --no-config [--listen <host:port>]`: config を読まず、組み込みの既定値と CLI 引数だけで起動する (テスト向け、例: 状態の root を一時 dir に向けて `hyoui web daemon run --no-config --listen 127.0.0.1:0`)

`<unit>` と `--config` は、その config ファイルと `extends` でたどれるファイルだけを読み、共通の `config.toml` は暗黙に読まない。`--listen` は `--no-config` とだけ併用できる (config を読む起動で listen だけを差し替えると、config の listen を問い合わせ先にする `list` / `status` / 監督者と実際の bind 先が食い違う、決定 6)。

**監督者は unit ごとに `<binary_path> web daemon run <unit>` を子として起動する。** plist に載るのは `hyoui web daemon supervise` だけで、監督者は自分の面の登録簿を読む。子に `--config` は渡さない: config の path の正本を登録簿 1 か所に保ち、ps で unit 名が読める。

## 却下した案

| 案 | 理由 |
|---|---|
| 登録簿に設定値を解決して書く (DR-0034 決定 2 の形) | 設定値が状態の置き場に入り、config を見てもポートが分からない。reference の unit / `list` の形から外れる |
| web の設定を `config.toml` の `[web]` に残し、unit ごとの差分だけ別ファイルにする | gateway が使わない PTY session の設定と同じファイルに web の設定が混ざり、unit の config の土台が「何でも入る `config.toml`」になる。web の設定は web の中で閉じる |
| `binary_path` を起動のたびに config から引く | llm-gateway と食い違い、監督者が子を起こす直前に config を解釈することになる (= 解釈は子、DR-0034 決定 3)。変える時は `remove` → `add` |
| `hyoui web --listen` を `daemon run` の別名として残す | 同じことをする口が 2 本になる。利用者は kawaz だけで、互換のために語彙を濁す相手が居ない |
| 古い置き場からの自動移行 | 一度しか通らないコードが製品に残る。移行は人が 1 回行う |
| unit の config のファイル名に面の key を入れる | 面は config の `state_dir` で分かる。ファイル名にも入れると二重管理になり、食い違った時にどちらが正か決められない |
| unit の名前に既定値 (`default` 等) を持たせ、名前を省いた `run` / `add` をそれに向ける | `default` という名前は既定値を管理しているように見える。名前を省いた時に何を起こすかを推測させない |
| `add` で使用中のポートを見つけたら空いているポートを自動で選ぶ | 選ばれたポートは利用者が書いた値ではなく、どこに居るかを config を開くまで知らないことになる。断って `--listen` で指定させる |
| 監督者が子に `--config <path>` を渡す | config の path の正本が登録簿と子の argv の 2 か所になる。`run <unit>` なら ps で unit 名が読める |
| `PATH` の違いでも re-register を止める | 場所を導かない変数で止めると、shell ごとに違う `PATH` のせいで毎回 `--force` を要る |
| 固定する変数を unit 生成側で列挙する | 導出コードが読む変数と食い違っても気づけない。列挙は `hyoui::paths::LocationVar` 1 箇所 |

## Consequences

- unit の設定は config ファイルを開けば読め、書き換えれば次の起動から効く。登録簿は「どの config を、どの実行ファイルで、動かしたいか」だけになる
- `config.toml` に `[web] listen` / `[web] assets_dir` を書いていると、全コマンドの config 読み込みが廃止 key で止まる。移し先は案内に出る。gateway が `config.toml` を読む経路は無く、`[web]` 節は不要
- unit の config に `state_dir` が無いと `add` と config を読む `run` が断る。既に登録した unit の config には、その面の状態の root を `state_dir` として書き足す必要がある (監督者が起こす子の `run <unit>` も断るため)
- 移行 (状態 dir の移動 + 古い名前の symlink) をするまで、新しいバイナリから見た登録簿と passkey は空で、`hyoui web ...` は古い置き場が残っていると警告する。移行は人が手で 1 回行う
- 既存の plist は場所の env を固定していないので、最初の `service register` は差分で止まり `--force` が要る
- `hyoui-web` の名を持つものは crate 名 / ログの接頭辞と、古い置き場・旧 label を検知して警告するための名前だけになる
- OS 登録名が変わるので、移行で旧 label を外して新しい label で register し直す。監督者のログのファイル名も新しい label になる
- systemd 経路は DR-0034 と同じく書けるが未検証

## 参照した素材

- `docs/issue/2026-10-04-web-unit-registry-holds-settings.md`
- reference `cli-daemon-subcommands` (claude-rules-personal、`daemon` / `service` 体系と「`service register` は場所を決める env を unit に固定し、変わったら止まる」の節)
- llm-gateway `docs/decisions/DR-0028-daemon-service-subcommands.md` (決定 2 / 6) / `DR-0013-config-extends.md`、`crates/gateway-core/src/daemon/registry.rs`、`crates/llm-gateway-cli/src/daemon.rs` / `daemon/run.rs`
- 本リポ: DR-0034 / DR-0031 / DR-0024 / DR-0036 / DR-0018、`crates/hyoui/src/discovery.rs`、`crates/hyoui-cli/src/socket_path.rs`、`docs/issue/2026-10-04-design-session-id-uuid-and-tags.md`
