# runbook: web gateway を DR-0038 の形 (unit = config ファイル、置き場 `hyoui/web/`、hash 付き label) へ移す

- Status: Active
- 対象: kawaz の macOS 実機で DR-0034 の形 (登録簿に設定値、状態 `~/.local/state/hyoui-web/`、label `jp.kawaz.hyoui-web.supervise`) で常駐している監督者と 2 unit (stable / unstable)
- 関連: [DR-0038](../decisions/DR-0038-web-unit-is-a-config-file.md) (決定 2 / 4 / 5 と移行節)、[DR-0034](../decisions/DR-0034-service-multi-unit-and-stable-unstable-ha.md)、[DR-0036](../decisions/DR-0036-passkey-auth-for-web-endpoints.md) (passkey の state)

移行は手作業で行う。DR-0038 は自動移行のコードを持たない (一度しか通らないコードを製品に残さない)。

## 症状 (この runbook を開く時)

新しい版の `hyoui web ...` が stderr に次のどれかを出す:

- `~/.local/state/hyoui-web is not read by this hyoui; move it to ...`
- `~/Library/Logs/hyoui-web is not read by this hyoui; ...`
- `.../jp.kawaz.hyoui-web.supervise.plist is the supervisor definition under an old label; ...`

または `hyoui web daemon list` が空で、`hyoui web service status` が `registered: false` を返す。

## 切り分け (始める前の状態を確かめる)

```sh
ls -la ~/.local/state/hyoui-web ~/.local/state/hyoui-web/units
cat ~/.local/state/hyoui-web/units/*.toml
ls ~/Library/LaunchAgents/ | grep -i hyoui
ls -d ~/.config/hyoui ~/.local/state/hyoui/web 2>&1
```

確かめること:

- 旧登録簿に `stable` (`127.0.0.1:43690`、`/opt/homebrew/bin/hyoui`) と `unstable` (`127.0.0.1:43691`、repo の `target/release/hyoui`) がある。値が違えば手順 4 の config をその値に合わせる
- `~/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist` がある
- `~/.config/hyoui` と `~/.local/state/hyoui/web` がまだ無い (あれば中身を見てから進める)

## 対処 (移行手順)

### 0. 新しい版を用意する (全断なし)

```sh
brew upgrade hyoui
hyoui --version
(cd ~/.local/share/repos/github.com/kawaz/hyoui/main && just build)
~/.local/share/repos/github.com/kawaz/hyoui/main/target/release/hyoui --version
hyoui web service status | grep -E '"(label|root)"'
```

確かめること:

- brew 版と repo build の両方が DR-0038 を含む版である (監督者は各 unit の binary に `web daemon run <name>` を渡すので、古い版の binary は新しい登録簿を読めず子が上がらない)
- `label` が `com.github.kawaz.hyoui.web.supervise.<hash>` (8 桁) で、`root` が `~/.local/state/hyoui` の実体である。この label を手順 6 で使う

**ここから手順 6 の register が終わるまで全断** (stable / unstable の両方が止まる)。

### 1. 旧 label の監督者を降ろす (全断の始まり)

```sh
launchctl bootout "gui/$UID/jp.kawaz.hyoui-web.supervise"
launchctl enable "gui/$UID/jp.kawaz.hyoui-web.supervise"
mv ~/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist ~/jp.kawaz.hyoui-web.supervise.plist.bak
```

確かめること:

- `launchctl print "gui/$UID/jp.kawaz.hyoui-web.supervise"` が「Could not find service」になる
- `lsof -iTCP:43690 -iTCP:43691 -sTCP:LISTEN` が何も返さない (子 gateway も止まった)
- plist は消さずに `.bak` に退避してある (ロールバック用)

`enable` は、以前の `service stop` が残した「上げない」指示を label から外すため。新しいバイナリは旧 label を操作しないので、ここは launchctl を直接叩く。

### 2. 状態 dir を移し、古い名前を symlink にする

```sh
mkdir -p ~/.local/state/hyoui
mv ~/.local/state/hyoui-web ~/.local/state/hyoui/web
ln -s hyoui/web ~/.local/state/hyoui-web
mkdir -p ~/hyoui-web-units.bak
mv ~/.local/state/hyoui/web/units/*.toml ~/hyoui-web-units.bak/
rm -f ~/.local/state/hyoui/web/supervisor.sock
```

確かめること:

- `ls -la ~/.local/state/hyoui-web` が `hyoui/web` への symlink である
- `~/.local/state/hyoui/web/auth.json` と `pending.json` (+ `.lock`) がある (passkey の登録はこのファイルにあるので、失うと全部失効する)
- `~/.local/state/hyoui/web/units/` が空 (旧形式の登録簿は新しいバイナリが読まないので、`.bak` に退避して手順 5 で作り直す)

### 3. 監督者のログ dir を移し、古い名前を symlink にする

```sh
mkdir -p ~/.local/state/hyoui/web/logs
mv ~/Library/Logs/hyoui-web/* ~/.local/state/hyoui/web/logs/
rmdir ~/Library/Logs/hyoui-web
ln -s ~/.local/state/hyoui/web/logs ~/Library/Logs/hyoui-web
```

確かめること:

- `ls -la ~/Library/Logs/hyoui-web` が `~/.local/state/hyoui/web/logs` への symlink である
- 旧監督者のログ (`jp.kawaz.hyoui-web.supervise.log` 等) と unit のログ (`stable.log` / `unstable.log`) が `~/.local/state/hyoui/web/logs/` に並ぶ

### 4. unit の config を書く

```sh
mkdir -p ~/.config/hyoui/web
cat > ~/.config/hyoui/web/base.toml <<'TOML'
# 全 unit の土台。共通の設定 (assets_dir 等) はここに書く。
[web]
TOML
cat > ~/.config/hyoui/web/stable.toml <<'TOML'
extends = "base.toml"

[web]
listen = "127.0.0.1:43690"
binary_path = "/opt/homebrew/bin/hyoui"
TOML
cat > ~/.config/hyoui/web/unstable.toml <<'TOML'
extends = "base.toml"

[web]
listen = "127.0.0.1:43691"
binary_path = "~/.local/share/repos/github.com/kawaz/hyoui/main/target/release/hyoui"
TOML
```

確かめること:

- stable.toml に `binary_path` がある (無いと手順 5 で `add` を打った binary が焼かれ、repo build から打つと stable が repo build を指す)
- listen が切り分けで見た旧登録簿の値と同じ (canddy が 43690 / 43691 を指しているので変えない)

### 5. unit を登録する

```sh
hyoui web daemon add ~/.config/hyoui/web/stable.toml
hyoui web daemon add ~/.config/hyoui/web/unstable.toml
hyoui web daemon list
```

確かめること:

- `add` の出力の `name` が `stable` / `unstable`、`listen` が 43690 / 43691、`binary_path` が brew 版 / repo build、`binary_exists: true`
- `add` が `supervisor.running: false` と「次に監督者が上がった時に起きる」旨の note を出す (この時点では監督者が居ないので正しい)
- `cat ~/.local/state/hyoui/web/units/stable.toml` が `config` / `binary_path` / `enabled` / `added_at` だけを持つ

### 6. 新しい label で監督者を載せる (全断の終わり)

kawaz の対話 shell から打つ (その shell の `HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` と `PATH` が定義に固定される。別の env の shell から打つと別の面の監督者になる)。

```sh
hyoui web service register
```

確かめること:

- 出力の `label` が手順 0 で見た `com.github.kawaz.hyoui.web.supervise.<hash>`、`changed: true`
- 出力の `env` に `HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `PATH` が入っている
- 新しい label の定義はまだ無かったので `--force` は要らない。`location_env_drift` で止まった場合は `differences` を読み、意図した値なら `--force` を付けて打ち直す

### 7. 確かめる

```sh
hyoui web service status
hyoui web daemon status
hyoui version
hyoui web passkey list
hyoui list --all-namespaces
curl -s http://127.0.0.1:43690/healthz; echo
curl -s http://127.0.0.1:43691/healthz; echo
```

確かめること:

- `service status`: `label` が新しい label、`root` が `~/.local/state/hyoui` の実体、`registered: true`、`running: true`、`instances` に 2 unit、`warnings` に場所の食い違いが無い
- `daemon status`: 2 unit とも `enabled: true` / `running: true`、`config` が手順 4 のファイル、`listen` が 43690 / 43691
- `version`: 監督者と 2 unit の `running` / `on_disk` が並び、`restart_needed: false`
- `passkey list`: 移行前と同じ登録が並ぶ (失効していない)
- `list --all-namespaces`: `supervisor` という session が出ない
- `/healthz` が両方 `ok`
- stderr の警告は「`~/.local/state/hyoui-web` / `~/Library/Logs/hyoui-web` が symlink として残っている」の 2 件だけ。旧 label の定義の警告が出ない
- 前段 (canddy) 経由でブラウザから開け、passkey でサインインできる

### 8. 退避したものを消す (確かめ終えてから)

```sh
rm ~/jp.kawaz.hyoui-web.supervise.plist.bak
rm -r ~/hyoui-web-units.bak
```

## ロールバック (手順 1 以降で戻す時)

新しいバイナリは旧 label を操作せず、旧形式の登録簿も読まないので、戻す時は**古い版の hyoui** で旧 label を載せ直す。

1. 新しい label を降ろす: `hyoui web service unregister` (新しい版で打つ)
2. 退避した登録簿を戻す: `mkdir -p ~/.local/state/hyoui/web/units && mv ~/hyoui-web-units.bak/*.toml ~/.local/state/hyoui/web/units/` (新しい形で `add` した `stable.toml` / `unstable.toml` は先に消す)
3. symlink を外して dir を元の名前へ戻す: `rm ~/.local/state/hyoui-web && mv ~/.local/state/hyoui/web ~/.local/state/hyoui-web`、`rm ~/Library/Logs/hyoui-web && mkdir ~/Library/Logs/hyoui-web && mv ~/.local/state/hyoui-web/logs/jp.kawaz.hyoui-web.supervise.log ~/Library/Logs/hyoui-web/`
4. brew 版を DR-0038 より前の版に戻す (unit stable の binary が brew 版なので、新しい版のままだと旧形式の登録簿を読めない)。repo build も同様に DR-0038 より前の commit で build し直す
5. 旧 plist を戻して載せる: `mv ~/jp.kawaz.hyoui-web.supervise.plist.bak ~/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist && launchctl bootstrap "gui/$UID" ~/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist`
6. 確かめる: 古い版の `hyoui web daemon status` で 2 unit が running、`/healthz` が両方 `ok`、`hyoui web passkey list` が移行前と同じ

`~/.config/hyoui/web/` は古い版が読まないので、残しておいても害は無い。

## 予防 (symlink と警告を消す条件)

- **symlink を消す条件**: DR-0038 を含む版より前の hyoui が手元で 1 つも動いていないこと (brew 版、repo build、監督者とその子の全部が DR-0038 以降)。`ps -o pid,args -ax | grep '[h]youi'` で動いているバイナリを確かめ、各 path の `--version` が DR-0038 以降であれば満たす
- 満たしたら symlink を消す: `rm ~/.local/state/hyoui-web ~/Library/Logs/hyoui-web`。消すと「symlink が残っている」の警告は出なくなる
- **旧 label の警告が消える条件**: `~/Library/LaunchAgents/jp.kawaz.hyoui-web.supervise.plist` (と Linux なら `~/.config/systemd/user/hyoui-web-supervise.service`) が無いこと。手順 1 で退避していれば移行直後から出ない
- **警告のコードを外す条件**: symlink を消した次の版で、古い置き場と旧 label を検知するコード (`crates/hyoui-cli/src/web_daemon/mod.rs` の `LEGACY_DIR_NAME` / `LEGACY_SERVICE_LABELS` と `warn_legacy_state_dir`) と、その test を消す

## 関連

- DR-0038 決定 4 (置き場と OS 登録名、label の hash)、決定 5 (env の固定と差分での停止)、移行節 (symlink を残す方式)
- `crates/hyoui-cli/src/web_service.rs` (`SERVICE_LABEL_PREFIX` / `label_for_root` / `register_checked`)
- `crates/hyoui-cli/src/web_daemon/mod.rs` (`warn_legacy_state_dir`)
- 前の移行: [2026-09-15-web-service-migration.md](./2026-09-15-web-service-migration.md) (DR-0034 の形へ移した時の手順)
