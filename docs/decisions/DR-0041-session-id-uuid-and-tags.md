# DR-0041: session id を UUID にし、namespace を廃止して tag にし、socket を `hyoui/sessions/` にフラットに置く

- Status: ✅ 実装済 (2026-10-07)。決定 1〜7 を tag まで実装済み。廃止した namespace の option を受け付けて捨てる処理 (決定 1) は 2026-11 に削除する。「未決」節の項目は本 DR では確定させない
- Date: 2026-10-04
- Supersedes: DR-0018 (session namespace)
- Related: DR-0020 (`HYOUI_SESSION_ID` による自己参照。値が UUID になる), DR-0015 (`hyoui run --detached`。起動側が id を先に決められるようになる), DR-0006 (CLI ground rules。session 引数と `--index`), DR-0038 (web の置き場 `hyoui/web/`、監督者 label `com.github.kawaz.hyoui.web.supervise.<hash>`、場所を決める env の固定、移行の symlink 方式), DR-0039 決定 1 (`hyoui run --login` と面の root), DR-0005 / DR-0014 (透過原則と介入 self-check)
- Origin: `docs/issue/2026-10-04-design-session-id-uuid-and-tags.md` (kawaz との議論で合意 2026-10-04)

## Context

### 本 DR が置き換えるもの

DR-0018 は session を用途ごとに分けるため、socket の置き場を namespace ごとの dir に分けた (`<base>/<session>.sock` が `default`、それ以外は `<base>/<ns>/<session>.sock`)。本 DR は DR-0018 を置き換える。

namespace は「名前を付ける」と「見えなくする」を 1 つの仕組みに同居させている。後者が次の不都合を生む。

- **list に出ない。** 別の namespace で起動した session は `hyoui list` に並ばず、在ることに気づけない
- **namespace の指定が漏れると操作できない。** attach / kill / input 等は今の namespace の中で session を探すので、指定を忘れると「無い」と言われる

session id は元々一意なので、id を引くために階層は要らない。分類は見え方を変えずに付けられれば足りる。

### DR-0018 が tag 案を退けた理由と、それが当たらなくなったこと

tag (= session のメタデータとして持ち、list で絞り込む) は DR-0018 の方式 (c) にあたる。DR-0018 はこれを次の 2 つの理由で退けた。

| DR-0018 の理由 | 本 DR での評価 |
|---|---|
| 旧 daemon との互換処理 (field が無い status 応答の扱い) が要る | v1.0 前で互換を持たない方針なので当たらない |
| 全 socket に問い合わせた後の絞り込みなので、混在の I/O が残る | list は元々全件に問い合わせる作りで、件数も数十なので実害が小さい |

DR-0018 は「方式 (a) に対する利点が無い」とも書いたが、上の「list に出ない」「指定漏れで操作できない」が (a) の欠点として表に出たので、隠さないこと自体が利点になる。

### 置き場の衝突

DR-0038 決定 4 は web の状態の置き場を `hyoui/web/` にした。session socket の木は base 直下のサブ dir をすべて namespace とみなすので、`web/` が namespace `web` と衝突する。DR-0038 は「session 側の別 DR で解消する (それまでは namespace `web` の session を作らない運用)」とした。本 DR がその解消にあたる (決定 4)。

### `sun_path` の長さの扱いの誤り

現行の `check_sun_path_len` (`crates/hyoui-cli/src/socket_path.rs`、`crates/hyoui/src/sys/socket.rs`) は socket のフルパスの byte 長を `sun_path` の上限 (macOS 104 / Linux 108 から NUL 終端を引いた値) と比べ、超えれば bind の前に断る。上限が効くのは bind / connect に渡す `sun_path` 引数の長さで、ファイルシステム上のフルパスの長さではないので、この判定は誤り (決定 5)。

## 介入判断 self-check (CLAUDE.md / DR-0014)

- **子 PTY への介入は減る。** hyoui が子へ足す env は `HYOUI_SESSION_ID` (DR-0020) だけで、値が UUID になる。面の変数 `HYOUI_STATE_DIR` は hyoui が足すものではなく、呼び出し元の env に在る時だけ子に届く (`--login` で子の env を最小にする時も残す、決定 6)
- **新しい protocol message / cap flag は無い。** tag は session のメタデータとして daemon が持ち、status 応答で返す (= DR-0018 の方式 (c) の形)。status 応答の field が増える
- **fork する子は hyoui 自身の処理で、PTY の子には触れない。** `sun_path` に収まらないパスを開く時だけ、hyoui のプロセスが短命の子を fork する (決定 5)
- **kernel 標準機能の再発明は無い。** 重複の判定は bind と name lock の失敗に任せ、生死を自前で判定しない (決定 3)

## Decision

### 1. namespace を廃止し、分類は tag で行う

- session の分類と絞り込みは tag (session のメタデータ) で行う。**既定は全部見える。** 絞り込みは指定した時だけ効く
- namespace の概念 (`--namespace`、`HYOUI_NAMESPACE`、`list --all-namespaces`、list の NS 列、jsonl の `namespace` field、子への `HYOUI_NAMESPACE` 注入) はすべて持たない
- tag は分類であって、認証境界の分離には使わない (分離は面で行う、決定 6)
- **tag は `key=value`。** 単語の集合 (label) は持たない (2026-10-07 裁定)
  - 付ける: `hyoui run --tag <key>=<value>` を繰り返す。同じ key は後勝ち。key は `[A-Za-z0-9._-]{1,256}`、value は任意の文字列 (最初の `=` で分ける、空も可)。`--tag <key>` は `--tag <key>=` (value が空の tag) の略
  - 絞る: `hyoui list --tag <key>=<value>` は value の完全一致、`hyoui list --tag <key>` は key があれば一致 (value は問わない)。`--tag <key>=` は value が空に完全一致で、`--tag <key>` とは別の条件。繰り返しは AND。ワイルドカードは持たない (CLI の絞り込みは簡単な用途のためで、細かい条件は jsonl を絞る)
  - 保持と出力: daemon が session のメタデータとして持ち、status 応答の field で返す (新しい protocol message / cap flag は無い)。daemon の upgrade (self-exec) をまたいで残る。`status` / `list` の jsonl / web の API は `tags: {key: value, ...}` を出し、plain の `list` は TAGS 列、`status` は `tags:` 行に出す
  - **`hyoui.` で始まる key は hyoui が予約し、付けられない** (2026-10-07 裁定)。hyoui が組み込みの読み取り専用の値 (`hyoui.pid` 等) を tag と同じ名前空間で見せるために取っておく。大文字小文字は区別する (`HYOUI.x` は予約に当たらない)。絞り込み (`list --tag hyoui.x`) はエラーにせず、どの session にも一致しない (組み込みの値で絞れるようにする余地)
  - 起動後に変える手段は持たない (未決)
- **既定の tag を env で与える仕組みは持たない。** `HYOUI_NAMESPACE` は読まず、子へ注入もしない (2026-10-07 裁定)
- **namespace を指定する option (`--namespace` / `--namespace=<ns>` / `--all-namespaces`) は 2026-11 まで受け付けて値を捨てる** (既存の呼び出しを起動できなくしないため、2026-10-07 裁定)。help と completion には出さない。stdout は option が無い時と同じで、stderr に「廃止され無視される、2026-11 に削除する」を 1 行だけ出す (絞り込みを期待した呼び出しが、気づかずに全件を受け取らないように)。`run --session` (id の旧 option) はエラーのまま

理由: 「見えなくする」を分類の副作用にしないため。分類は付けても付けなくても session の見え方と操作の届き方を変えない。

### 2. session id は UUID だけ

- 自動の id (`run-<pid>-<hex>`) と自分で付ける id の混在をやめ、id は UUID に揃える
- 起動側は `hyoui run --session-id <UUID>` で id を最初から指定できる。指定が無ければ hyoui が振る
- **UUID の版は規定しない。** 外部から指定するユースケースがある以上、版を縛っても意味が無い。list の並び順は session の起動時刻 (started_at) で決め、id の版に依存しない
- **受け付けるのは小文字・ハイフン付きの標準形 (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`) だけ** (2026-10-04 裁定)。大文字やハイフン無しは run のエラーにし、黙って正規化しない。表記が違うと同じ UUID でも別のファイル名になり重複判定をすり抜けるため、外から渡した値と socket のファイル名を常に一致させる
- **人が打つための短縮指定 (先頭一致等) は入れない。** id はコピペで渡す前提
- 子に注入する `HYOUI_SESSION_ID` (DR-0020) は値が UUID になるだけで、自己参照の規則は変わらない

理由: 起動側が id を先に決められれば、起動後に stdout から id を読み取る手順が要らない (DR-0015 の `--detached` を外から呼ぶ経路、DR-0039 決定 1 の gateway からの新規 session 作成)。id の形が 1 種類なら、id を扱う側は形の分岐を持たない。

### 3. id の重複と古い socket は run のエラー、判定は bind / lock の時点で原子的に行う

- 同じ id の socket が既にあれば `hyoui run` 自体をエラーにする
- **古い socket が残っている (daemon は死んでいる) 場合も重複として扱う。** run は socket の先の daemon の生死を判定しない。死んだ socket の片付けは片付けの経路の責務で、生死の判定経路を 2 つに分けない
- **重複の判定は bind / lock の時点で原子的に失敗させる。** 「確認してから作る」の 2 段にしない。name lock (`<uuid>.lock`) が取れない、または bind が既存の socket file に当たって失敗する、のどちらかで重複とする。古い socket も重複として扱うので、bind の前に既存の socket file を消さない
- name lock と dir lock (`.dir.lock`) は socket と同じ `sessions/` に置く
- エラーには原因 (同じ id の socket が既にある) と対処 (別の id で起動する / 片付けてから再実行する、片付けのコマンド名) を書く

理由: 確認と作成の間に別の run が入ると、2 段の判定は重複を見逃す。生死の判定を run にも持たせると、片付けの経路と run で判定が食い違いうる (片方は死んだと見て消し、片方は生きていると見て残す)。

### 4. socket は `hyoui/sessions/<uuid>.sock` にフラットに置く

- session の socket は `hyoui/sessions/<uuid>.sock` に置く。`hyoui/` の場所は今の session socket の base と同じく `hyoui::paths` の `Env` から導く (DR-0038 決定 5)
- **`hyoui/` 直下は機能別のサブ dir (`sessions/`、`web/` ...) だけにする。** session の socket を `hyoui/` 直下に置かない
- discovery (`crates/hyoui/src/discovery.rs`) は `sessions/` の中だけを見る。id 1 件を引く時は `sessions/<uuid>.sock` を直接組む

理由: `hyoui/` 直下を機能ごとの置き場にすれば、web の状態の置き場 (`hyoui/web/`) と session socket の木が衝突しない (Context「置き場の衝突」の解消)。namespace が無いので socket を階層化する理由も無い。

### 5. `sun_path` の長さは引数の長さで判定し、収まらないパスは共有 fd と fork した子の `fchdir` で開く

- 長さの判定は bind / connect に渡す `sun_path` 引数の長さで行い、**フルパスの長さで弾かない。** 現行の `check_sun_path_len` はフルパスで弾いており、これをやめる
- パスが `sun_path` の上限に収まれば、今どおり直接 bind / connect する
- 収まらない時は、socket の dir を開いた fd を基準に、相対名で bind / connect する。手順は次のとおり
  1. 親が socket を作り、dir を開いた fd (`dirfd`) を持ってから fork する
  2. 子は `fchdir(dirfd)` → 相対名で `bind` / `connect` → `_exit(結果)`
  3. 親は子の終了を待ち、終了 status から結果を受け取る
- fork 後の fd は親子で同じ open file description を指すので、子の bind / connect は親の fd にそのまま効く。SCM_RIGHTS での受け渡しは要らない
- cwd が変わるのは子だけで、親のスレッドは巻き込まれない。fork から `_exit` までは async-signal-safe な呼び出し (`fchdir` / `bind` / `connect` / `_exit`) だけなので、マルチスレッドのプロセス (gateway の tokio、daemon) から fork しても POSIX の範囲で安全
- fork するのは上限を超える時だけ

理由: `chdir` はプロセス全体に効くので、マルチスレッドのプロセスでは他のスレッドの相対パス解決を巻き込む。macOS には dirfd 基準の `bindat` / `connectat` が無い (SDK ヘッダと libsystem_kernel の export で確認、2026-10-04)。残る手段のうち、公開 API だけで、置き場に制約を足さず、後始末を持たないのがこの方式になる (退けた候補は「却下した案」)。

### 6. 面は状態の root を決める環境変数 1 つで決まり、面をまたぐ仕組みは持たない

- 面は hyoui 専用の環境変数 `HYOUI_STATE_DIR` 1 つで決まる (2026-10-04 裁定、`CLAUDE_CONFIG_DIR` と同じ考え方)。hyoui の一式 (CLI、session の socket、web の監督者・unit・登録簿・auth.json・logs) はその root の中で完結する
- 状態の root の決め方 (先行の kawaz/ccmsg の `CCMSG_STATE_DIR` と同じ 3 段):

  | 何 | 1. 専用の変数 (空でなければ) | 2. XDG (絶対パスの時だけ) | 3. 既定 |
  |---|---|---|---|
  | 状態の root | `$HYOUI_STATE_DIR` (そのまま使う) | `$XDG_STATE_HOME/hyoui` | `$HOME/.local/state/hyoui` |

  `XDG_STATE_HOME` / `XDG_CONFIG_HOME` は他のアプリと共有の変数なので、面を分けるために書き換えない (書き換えると同じ shell の他のツールの置き場まで動く)。面を分ける時は面の `.envrc` で `HYOUI_STATE_DIR` だけを設定する。`$HOME` も無い時はエラーにし、cwd 相対には倒さない
- **config は面で分けない** (2026-10-05 裁定)。config の root は `$XDG_CONFIG_HOME/hyoui` (無ければ `$HOME/.config/hyoui`) 1 つで全部の面が共有し、専用の変数は持たない。面ごとに違うのは状態だけで、web の unit の config (listen 等) は登録簿が path で参照する (DR-0038)
- `XDG_RUNTIME_DIR` は使わない (2026-10-04 裁定)。runtime dir はログインに紐づく寿命 (Linux の logind は最後のセッション終了で消す、再起動でも消える) で、ログインを越えて動き続ける hyoui の session と合わない。socket は状態の root に置く。長く生きる multiplexer の先例 (tmux は `$TMUX_TMPDIR` か `/tmp`、screen は `$SCREENDIR` か `/tmp/screens`) も runtime dir を使っていない
- 面をどの変数で識別するかについて、ccmsg は Claude の config home (`CLAUDE_CONFIG_DIR` 等) から instance を自動で導くが、hyoui は Claude 以外 (vim や shell) も動かすので Claude 用の変数には結びつけず、専用の変数で選ぶ
- **面をまたぐ仕組みは持たない。** 複数の面を横断する option、1 つの監督者で複数の面の unit を抱える等は作らない。複数の面を扱う時は、面ごとに環境変数を指定してそれぞれで実行して回る
- web の監督者も面ごとに立つ。登録簿はその面の状態の root の中にあり、1 つの監督者が読むのは 1 つの面の登録簿だけである (ccmsg の監督者がホストに 1 つなのは 1 プロセスで複数の instance を抱える作りだからで、面をまたがない hyoui とは前提が違う)。面の `.envrc` が効いた状態で `hyoui web service register` すれば、その面の root (`HYOUI_STATE_DIR` を含む) が定義に固定される (DR-0038 決定 5 の env 固定)
- 面ごとの監督者の label (`com.github.kawaz.hyoui.web.supervise.<hash>`、`<hash>` は状態 root から導く) は DR-0038 決定 4 が正本で、本 DR は参照するだけ
- `hyoui run --login` は子の shell に渡す env を最小にする (DR-0039 決定 1) が、`HYOUI_STATE_DIR` が設定されていればそれは子に渡す (2026-10-07 裁定)。`HYOUI_SESSION_ID` はどの面の session id かとセットで初めて自己参照になるので、子の中で起こす hyoui が親と同じ面を使う。設定されていない時は何も渡さない (既定の面のまま)。hyoui 自身の面は今の env で決まる

理由: 面の分離は認証境界の分離で、分類 (tag) とは目的が違う。面をまたぐ仕組みを入れると、一式が 1 つの root の中で完結する前提が崩れ、どの面の状態を読み書きしているかを env だけで判断できなくなる。

### 7. 移行: 置き場を移したら古い置き場に symlink を残し、後で必ず消す

**いま動いている (id が UUID でない) session は移行しない** (2026-10-04 裁定)。そのまま動かし続け、終われば消える。新しいバイナリからは id で指定できない (kill 等) が、ccmsg 経由の会話は今どおり届き、数も少ない。止めたい時は OS から daemon の pid に kill を送る

DR-0038 決定 4 の移行と同じ方式を採る。

1. 置き場を移す時は、移動して古い置き場に新しい置き場への symlink を残す。古い CLI は symlink をたどって `connect` できる
2. **新しいバイナリは新しい置き場だけを見る。** symlink は古いバイナリのためだけにあり、新しいバイナリは古い置き場を読まない (= 同じ実体を二重に拾わない)
3. 新しいバイナリは、古い置き場の symlink が残っていれば警告する
4. symlink を消す条件は版で決める。本 DR を含む版より前の hyoui が手元で 1 つも動いていないこと

理由: 新旧の hyoui が混在する間も、動いている session に古い CLI から届くようにするため。symlink を残したままにすると新旧どちらの置き場が正本かが曖昧になるので、消す条件を先に決めておく。

## 却下した案

| 案 | 理由 |
|---|---|
| namespace を残す (DR-0018 の方式 (a)) | 名前付けと隠蔽が同居し、「list に出ない」「指定漏れで操作できない」が残る |
| 自動の id と自分で付ける id を併存させる | id の形が 2 種類になり、扱う側が形で分岐する。起動側が id を先に決める経路は UUID の指定で足りる |
| UUID の版を規定する | 外部から指定するユースケースがあり、版を縛っても意味が無い。並び順は起動時刻で決める |
| 先頭一致等の短縮指定 | id はコピペで渡す前提。曖昧一致の解決規則を持ち込まない |
| run が生死を判定し、死んだ socket なら置き換えて起動する | 生死の判定経路が run と片付けの経路の 2 つになる |
| 重複を確認してから socket を作る (2 段) | 確認と作成の間に別の run が入ると重複を見逃す |
| フルパスの長さで `sun_path` の上限を判定する (現行) | 上限が効くのは `sun_path` 引数の長さで、フルパスではない |
| `chdir` して相対名で開く | cwd はプロセス全体に効き、マルチスレッドのプロセスで他のスレッドを巻き込む |
| `bindat` / `connectat` | macOS に無い |
| macOS の `pthread_fchdir_np` (スレッドごとの cwd) | libsystem_pthread が export しているが、ヘッダに公開宣言が無い (fts.h に「private」とあるだけ) 非公開 API |
| Linux の `/proc/self/fd/<dirfd>/<name>` | macOS に無い |
| 短いパスに symlink を置いて開く | 置き場の制約 (macOS は `/tmp` を掃除する) と後始末を背負う |
| tag で面 (認証境界) を分ける | tag は分類。認証境界は状態の root で分ける |
| 面をまたぐ option / 1 つの監督者で複数の面を抱える | 一式が 1 つの root の中で完結する前提が崩れる |

## Consequences

- `hyoui list` は既定で全 session を出す。分類は tag の絞り込みで行う
- namespace に関わる CLI の語彙 (`--namespace`、`--all-namespaces`、NS 列、jsonl の `namespace` field) と env (`HYOUI_NAMESPACE`) が消える。option は 2026-11 まで受け付けて捨て、その後は unknown option になる。v1.0 前で breaking を許容する方針の範囲
- hyoui が子へ足す env は `HYOUI_SESSION_ID` だけになる。面の `HYOUI_STATE_DIR` は `--login` でも子に届く
- `hyoui run` の id 指定は `--session-id <UUID>` になる (現行の flag 名は `--session`)
- 起動側は id を先に決めて渡せるので、`--detached` の stdout を読まずに後続の操作を組める
- 同じ id での起動は、相手が生きていても死んでいてもエラーになる。死んだ socket が残っている時は、片付けてから起動し直す
- `sun_path` の上限を理由に深い置き場が使えない制約が消える
- `hyoui/` 直下に session の socket が無くなるので、DR-0038 決定 4 が監督者の socket を `web/run/` に 1 段下げた理由 (discovery が `hyoui/` 直下の dir を namespace とみなす) は当たらなくなる
- 面ごとに監督者・登録簿・passkey の状態が分かれる。web gateway が一覧に出すのは、その gateway の面の session だけになる

## 実装の当たり所

- `crates/hyoui/src/cli.rs`: `validate_namespace` / `DEFAULT_NAMESPACE` / 各 Config の `namespace` / list の `all_namespaces` を外す。`validate_session_id` を UUID の検証にする。`--session` を `--session-id` にする
- `crates/hyoui-cli/src/socket_path.rs`: `auto_session_id` を UUID の生成にする。`resolve_namespace` / `resolve_in_namespace` を外し、`sessions/` の 1 段にする。`check_sun_path_len` を外す
- `crates/hyoui/src/sys/socket.rs`: bind の前に既存の socket file を消さない。`sun_path` に収まらない時の fork + `fchdir` 経路
- `crates/hyoui/src/discovery.rs`: `namespace_dirs` を外し、`sessions/` だけを走査する。`SessionEntry.namespace` を外す
- `crates/hyoui-cli/src/daemonize.rs`: `DaemonizeInit.namespace` と子への `HYOUI_NAMESPACE` の set を外す
- tag の保持と status 応答の field、list の絞り込み

## 未決

- tag を起動後に変える手段 (`hyoui set` 等) を持つか
- 現行の `HYOUI_NAMESPACE` を使っている箇所の移行: 業務面の `.envrc`、ccmsg の hyoui terminal 連携 (`src/terminals/hyoui.ts` が base と namespace を直書きで discovery している。ccmsg 側に起票済み: kawaz/ccmsg `docs/issue/2026-10-06-hyoui-namespace-removal-and-socket-dir.md`)

