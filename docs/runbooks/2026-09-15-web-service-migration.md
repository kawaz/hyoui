# runbook: web gateway を監督者 + 2 unit に移す

DR-0034 で `hyoui web service` の載せる対象が「gateway 1 台」から「監督者 1 つ」に
変わった。この手順は実機 (kawaz の macOS) の既存 1 台をその形へ移す。

**旧 label を引き取る経路は CLI に持たない** (決定 11)。対象が 1 つしか無いものを
CLI に入れると、一度通ったら二度と通らないコードが製品に残る。だから移行は手作業。

## 前提

- `stable` unit が指すのは brew 配布版なので、**その版が `web daemon run` を持って
  いること**。持っていない版では監督者が子を起こせない (決定 11)
- 現行の 1 台は label `com.github.kawaz.hyoui-web` で `~/Library/LaunchAgents` に
  載っている。plist の `ProgramArguments` は `/opt/homebrew/bin/hyoui web` の 2 語で、
  待ち先は config (無ければ `127.0.0.1:43690`) から決まる
- 前段 (canddy) は現在 43690 単体を指している。**stable を 43690 に据え置く**ので、
  canddy の設定を入れ替える前に移行を終えられる (= 移行中に到達が切れない)

## 手順

### 1. 新しい版を入れる

```bash
brew upgrade hyoui   # or: brew reinstall した版
hyoui --version      # `hyoui <version> (<build_id>)` が出ること
hyoui web daemon --help > /dev/null   # 新 verb を持っている確認
```

### 2. 旧 1 台を降ろす

```bash
launchctl bootout "gui/$UID/com.github.kawaz.hyoui-web"
launchctl print "gui/$UID/com.github.kawaz.hyoui-web"   # 「Could not find service」になること
rm ~/Library/LaunchAgents/com.github.kawaz.hyoui-web.plist
```

この時点で 43690 は空く (= 前段からの到達が切れる)。以降を続けて閉じるまでが断。

### 3. unit を 2 つ登録する

```bash
hyoui web daemon add stable   --port=43690
hyoui web daemon add unstable --port=43691 \
  --binary="$HOME/.local/share/repos/github.com/kawaz/hyoui/main/target/release/hyoui"
hyoui web daemon list
```

`stable` の `binary` は brew 版 (`/opt/homebrew/bin/hyoui`) になる — `add` は
`current_exe` をそのまま焼くので、**brew 版の `hyoui` から打つこと** (決定 2)。
`unstable` は `--binary` を明示して repo build を指す。まだビルドしていなければ
`binary_exists: false` の warning が出るが、登録は通る。

### 4. 監督者を載せる

```bash
hyoui web service register    # → {"changed": true, "argv": [..., "web", "daemon", "supervise"]}
hyoui web service status      # → registered / running / service / version / instances
hyoui web daemon status       # → stable と unstable が running
curl -sS http://127.0.0.1:43690/healthz   # ok
curl -sS http://127.0.0.1:43691/healthz   # ok (unstable をビルド済みなら)
```

ここで 43690 が復活し、断が閉じる。

### 5. 前段に unstable を足す (別リポの作業)

canddy の hyoui ブロックを `reverse_proxy 127.0.0.1:43691 127.0.0.1:43690` +
`lb_policy first` + health check に変える。**設定の正本は canddy が持つので hyoui
からは書き換えず、canddy リポの issue として依頼する** (決定 8)。これは P6。

## 以降の運用

| やりたいこと | 打つもの |
|---|---|
| unstable を入れ替える | `just build` → `hyoui web daemon restart unstable` |
| 全 unit を入れ替える (断なし) | `hyoui web daemon restart --all` |
| 今の状態を見る | `hyoui web daemon status` / `hyoui version` |
| ログを見る | `hyoui web daemon log <name> --follow` |
| 監督者自身を見る | `hyoui web service status` / `hyoui web service log` |
| 監督者を新しい版に入れ替える (全断) | `hyoui web service restart` |

**`service` 層は監督者自身を入れ替える時だけ触る。** `service restart` / `service stop` と
`service register` の再実行は監督者の再起動を伴い、抱えている子が道連れで一度落ちる
(決定 6) = 全断。gateway を更新したいだけなら `daemon restart` を使う。

brew で `hyoui` を上げた後は `service restart` だけでよい (定義に焼いた path は
安定な場所を指しているので、`register` のやり直しは要らない)。`register` を
やり直す必要があるのは監督者の定義そのもの (label / 環境 / log path / 焼く binary の
path) を変えた時だけで、unit を足しても消しても定義は変わらない。

## 戻し方

監督者を使う前の形に戻すなら:

```bash
hyoui web service unregister      # 監督者を降ろす (子も止まる)
hyoui web daemon remove unstable
hyoui web daemon remove stable
```

そのうえで旧 plist を書き戻す。旧 plist の中身は `ProgramArguments` が
`/opt/homebrew/bin/hyoui` と `web` の 2 語、`RunAtLoad` と `KeepAlive` が `true`。
ただし戻した先は 1 台構成なので、前段の upstream も 43690 単体に戻す。

## 検証の記録

移行後に確認すること (DR-0034 P5 の gate):

- `hyoui web daemon status` が stable / unstable の `build_id` を**別々に**出す
- unstable を再ビルドした時点で `hyoui version` が `restart_needed: true` を出し、
  `hyoui web daemon restart unstable` だけで `running` の `build_id` が `on_disk` に
  一致して `restart_needed: false` に戻る (`service register` も plist の確認も要らない)
