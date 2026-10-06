# runbook: session を DR-0041 の形 (UUID の id、`<状態の root>/sessions/`、面 = `HYOUI_STATE_DIR`) へ移す

- Status: Active
- 対象: kawaz の macOS 実機で、id が UUID でない session (`run-<pid>-<hex>` や名前を付けた id) が状態の root 直下 (`~/.local/state/hyoui/<id>.sock`) と、その下の dir (`~/.local/state/hyoui/<dir>/<id>.sock`) で動いている状態。web gateway (stable / unstable の 2 unit と監督者) も同じ版で動いている
- 関連: [DR-0041](../decisions/DR-0041-session-id-uuid-and-tags.md) (決定 4 / 6 / 7)、[DR-0038](../decisions/DR-0038-web-unit-is-a-config-file.md) (監督者と場所の env の固定)、[web-unit-migration-dr-0038.md](./web-unit-migration-dr-0038.md) (書き方の手本)

移行は手作業で行う。DR-0041 は自動移行のコードを持たない。

**いま動いている session は移さない** (DR-0041 決定 7)。そのまま動かし続け、終われば消える。socket を動かすものは無いので、古い置き場に symlink を作る手順も無い。この runbook がするのは、新しい版を入れて古い版と並べる間の扱いと、古い版が 1 つも動かなくなった後の片付けである。

## 症状 (この runbook を開く時)

新しい版の `hyoui list` / `hyoui web ...` が stderr に次を出す:

- `<path> has N session socket(s) in the old layout; this hyoui does not read them (sessions now live in <state root>/sessions). ...`
- `<path> is a symlink left for older hyoui binaries; remove it once none of them run (DR-0041)`

または、古い版で起動した session が新しい版の `hyoui list` に出ず、id を指定しても見つからない。

## 切り分け (始める前の状態を確かめる)

```sh
ls -la ~/.local/state/hyoui/
find ~/.local/state/hyoui -maxdepth 2 -name '*.sock'
/opt/homebrew/bin/hyoui --version
/opt/homebrew/bin/hyoui list --all-namespaces
```

確かめること:

- 状態の root 直下に `*.sock` / `*.lock` / `.dir.lock` が、root 直下の dir (`web/` と `sessions/` 以外) に `*.sock` がある。これが古い置き場で、新しい版は読まない
- 古い版の `list --all-namespaces` で、どの session が今動いているか (STATUS が live / stopped / no-response) を控える。新しい版からは見えなくなるので、ここが最後に一覧できる場所
- `HYOUI_NAMESPACE` を設定している `.envrc` があるか (`grep -rl HYOUI_NAMESPACE ~/.local/share/repos --include=.envrc`)。新しい版はこの変数を読まない (下の「判断待ち」)

## 対処

### 1. 古い版の実行ファイルを残す (新しい版を入れる前)

新しい版は古い session を id で指定できない (kill / input / attach 等)。古い session を外から操作し続けたい間は、古い版の実行ファイルを手元に残す。`brew upgrade` の後は `brew cleanup` で古い Cellar が消えうるので、先に写す。

```sh
cp "$(readlink -f /opt/homebrew/bin/hyoui)" ~/hyoui-before-dr-0041
~/hyoui-before-dr-0041 --version
```

確かめること:

- 写した実行ファイルが古い版 (DR-0041 を含まない版) を名乗る

### 2. 新しい版を入れる

```sh
brew upgrade hyoui
hyoui --version
(cd ~/.local/share/repos/github.com/kawaz/hyoui/main && just build)
~/.local/share/repos/github.com/kawaz/hyoui/main/target/release/hyoui --version
hyoui list
```

確かめること:

- `hyoui list` が古い置き場の警告を stderr に出し、古い session を一覧に出さない (= 読んでいない)。新しく起こした session だけが並ぶ
- 新しい版で起こした session の socket が `~/.local/state/hyoui/sessions/<uuid>.sock` にある

### 3. 新旧が混在する間の操作

| したいこと | 方法 |
|---|---|
| 古い session を操作する | 手順 1 で残した古い版で打つ: `~/hyoui-before-dr-0041 attach <id>` (dir にある session は `--namespace=<dir>`) |
| 古い session を止める | 古い版で `kill <id>`。古い版が無ければ、古い版の `status` か `ps` で daemon の pid を引き、`kill <pid>` を送る (DR-0041 決定 7) |
| 新しい session を古い版から見る | 古い版は `sessions/` を namespace `sessions` として読むので、`~/hyoui-before-dr-0041 list --namespace=sessions` / `attach --namespace=sessions <uuid>` で届く |

### 4. web gateway を新しい版で起こし直す

監督者は unit ごとに `<binary_path> web daemon run <unit>` を起こす。子が新しい版になると、gateway の一覧 (`/api/sessions`) は `sessions/` の session だけになり、古い session はブラウザから見えなくなる (古い session を見る口は手順 3 の古い版だけになる)。

```sh
hyoui web service register
hyoui web daemon restart --all
hyoui web service status
hyoui version
```

確かめること:

- `service register` が差分で止まらない (場所を決める変数に `HYOUI_STATE_DIR` が加わったが、未設定同士なので既存の定義と食い違わない)。出力の `env` に `XDG_RUNTIME_DIR` が無い
- `service status` の `root` が `~/.local/state/hyoui` の実体、`warnings` に場所の食い違いが無い
- `version` で監督者と 2 unit の `restart_needed` が `false`

### 5. 片付け (古い版が手元で 1 つも動かなくなってから)

古い置き場を消してよい条件は版で決める: DR-0041 を含む版より前の hyoui (brew 版・repo build・監督者・その子・古い session の daemon の全部) が手元で 1 つも動いていないこと (DR-0041 決定 7)。

```sh
pgrep -fl hyoui
~/hyoui-before-dr-0041 list --all-namespaces
```

確かめること:

- `pgrep` に出る hyoui が全部新しい版である (`ps -o pid,lstart,command -p <pid>` で起動時刻と実行ファイルを見る。古い Cellar の path や手順 1 の写しが居ないこと)
- 古い版の `list --all-namespaces` で、`sessions` 以外の namespace に live の session が無い (残っているのが stale だけなら、古い版の `list` が片付ける)

満たしたら、状態の root 直下の古いファイルと、`sessions/` / `web/` 以外の dir を消す:

```sh
cd ~/.local/state/hyoui
ls -la
rm -f ./*.sock ./*.lock ./.dir.lock
# sessions/ と web/ 以外の dir を 1 つずつ中身を見てから消す
ls -la <dir> && rm -r <dir>
rm ~/hyoui-before-dr-0041
hyoui list
```

確かめること:

- `hyoui list` / `hyoui web daemon status` が古い置き場の警告を出さない
- 警告を出すコードは、この片付けを済ませた後の版で外す (DR-0041 決定 7。DR-0038 の移行節と同じ)

## 判断待ち (この runbook では決めない)

DR-0041 の「未決」節にある次の 2 つは、この runbook の手順に入れない。

- `HYOUI_NAMESPACE` を設定している `.envrc` の扱い。新しい版はこの変数を読まないので、その `.envrc` の dir で起こした session は既定の面 (`~/.local/state/hyoui/sessions/`) に入り、`hyoui list` で他の session と並ぶ。面を分けるなら `HYOUI_STATE_DIR` を設定する形になるが、どの面に分けるかは決まっていない
- ccmsg の hyoui terminal 連携は、socket の置き場を直書きで discovery している (ccmsg 側に起票済み)。新しい版の session は、連携側が `sessions/` を読むまで ccmsg から見えない

## ロールバック

新しい版で起こした session は `sessions/` にあり、古い版からは namespace `sessions` として届く (手順 3)。版を戻すだけで、置き場を戻す作業は無い。

1. 新しい版で起こした session を古い版から止めるか、終わるのを待つ (`~/hyoui-before-dr-0041 kill --namespace=sessions <uuid>`)
2. brew 版と repo build を DR-0041 より前の版に戻す
3. `hyoui web daemon restart --all` で gateway を古い版で起こし直す (`service register` で固定した env に `HYOUI_STATE_DIR` は入っていないので、そのままでよい)
