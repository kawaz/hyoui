# hyoui ユーザマニュアル

> [English](./MANUAL.md) | 日本語

エンドユーザ (CLI から hyoui を使う人) 向けのユースケース別レシピ集。

- **インストール / 概念紹介** → [`README-ja.md`](../README-ja.md)
- **内部設計・なぜそうなっているか** → [`DESIGN-ja.md`](./DESIGN-ja.md)
- **このファイル**: 「○○ をやりたい」→ 「このコマンド列で実現」のレシピ

> Status: v0.9.x をカバー。自動操作 API (`input` family / `wait` / `screen` /
> `lock` / `record` / `tail`) と web gateway は実装済。`tx` wrapper は未実装。

## 目次

- [基本フロー](#基本フロー)
  - [1. detached でセッションを起動して別端末から attach](#1-detached-でセッションを起動して別端末から-attach)
  - [2. read-only で観察する](#2-read-only-で観察する)
  - [3. 終了させる](#3-終了させる)
- [自動操作](#自動操作)
  - [4. 入力注入 (`input` family)](#4-入力注入-input-family)
  - [5. 画面が特定 state になるまで待つ](#5-画面が特定-state-になるまで待つ)
  - [6. 画面を読む (`screen dump` / `snapshot`)](#6-画面を読む-screen-dump--snapshot)
  - [7. 排他自動操作 (`lock`)](#7-排他自動操作-lock)
  - [8. tty I/O timeline を録画する (`record`)](#8-tty-io-timeline-を録画する-record)
  - [9. session id と面 (状態の root)](#9-session-id-と面-状態の-root)
  - [10. 子プロセスへの env 漏洩を防ぐ (env scrub)](#10-子プロセスへの-env-漏洩を防ぐ-env-scrub)
  - [11. ブラウザから操作する (web)](#11-ブラウザから操作する-web)
- [トラブルシューティング](#トラブルシューティング)
- [関連リンク](#関連リンク)

## 基本フロー

### 1. detached でセッションを起動して別端末から attach

```sh
# 端末 A: detached でセッション起動 (session id = UUID が stdout に出る)
hyoui run --detached -- claude
# → 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f  (例)

# 端末 B: list で確認 → attach
hyoui list
hyoui attach 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f
# Ctrl+Z 単発で client を suspend (= shell に戻る、fg で復帰)
# 接続を畳むなら hyoui detach 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f
```

stdin を pipe / file にすると、その fd が `--detached` の有無によらず子の stdin になる。子の stdout / stderr と制御端末は PTY のまま ([DR-0042](./decisions/DR-0042-non-tty-stdin-is-the-childs-fd.md))。子から見た stdin は直接実行と同じなので、子は pipe を読み、pipe の EOF で終わる。バイナリもそのまま届く。hyoui は pipe を読まないので、`tail -f` のような終わらない入力でも `--detached` の run はすぐ戻る。

```sh
echo "1+2" | hyoui run -- bc                  # attach したまま、bc が 3 を出して終わる
hyoui run --detached -- claude <<<"prompt"    # 直接実行と同じく prompt が最初の入力として送信される
printf 'a\003b\000c\n' | hyoui run -- od -c    # バイナリも化けない
```

- pipe を読みつつキーボードを使う TUI (claude / fzf / less 等) は、直接実行と同じく `/dev/tty` (= hyoui の PTY) からキーを読む。attach のキーも `hyoui input` もそこに届く
- stdin を読むプログラム (`cat` 等) に pipe を渡した後は、`hyoui input` はそのプログラムの stdin には届かない (直接実行で pipe を渡した時と同じ)
- `/dev/null` も他の非 tty と同じく子の stdin になり、子はすぐ EOF を読む。`bash -i` などの対話 shell は終わる。端末の無い起動元 (agent や script) から外で操作し続ける shell / REPL を作る時は `--pty-stdin` を付けて子の stdin も PTY にする: `hyoui run --detached --pty-stdin -- bash -i`
- stdin が端末の時は子の stdin も PTY (従来どおり)
- attach client は stdin を子に流さない。キーは stdin が端末なら stdin、そうでなければ `/dev/tty` から読み、どちらも無ければ出力だけを中継して子の exit で終わる
- `while read l; do hyoui run --detached -- x; done < list` のように stdin を共有するループでは、子が stdin の残りを読む (直接実行と同じ)。`</dev/null` を付ければ残りは読まれない

### 2. read-only で観察する

```sh
hyoui attach --observer 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f
# observer は入力を送らない読み取り専用 attach
```

### 3. 終了させる

```sh
hyoui kill 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f                 # SIGTERM
hyoui kill --signal KILL 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f   # SIGKILL
```

## 自動操作

以下のレシピは `SESS` に session id が入っている前提（例: `SESS=$(hyoui run --detached --pty-stdin -- bash)`。外から操作し続ける shell なので `--pty-stdin` で子の stdin も PTY にする）。

### 4. 入力注入 (`input` family)

`hyoui input` は spec の列を順序保証で子に送る。各引数が 1 spec で、左から右へ適用される。

```sh
# コマンドを打って Enter を押す
hyoui input "$SESS" "text:ls -la" "key:Enter"

# raw 制御 bytes (hex) — ここでは ESC[A = Up arrow
hyoui input "$SESS" "hex:1b5b41"

# 複数行ブロックを bracketed paste で送る (子は 1 回の paste として受け取る)
hyoui input "$SESS" "paste:$(cat script.py)"

# payload をファイルから読む
hyoui input "$SESS" "file:./payload.txt"
```

spec prefix: `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:`。

#### 4.1 ack 機構による sequencing 保証 (DR-0021)

bytes 系 spec (`text:` / `paste:` / `hex:` / `file:` / `key:`) は 1 invocation で
連続指定しても順序が崩れない。daemon は各 spec の master PTY 書き込み完了で ack を返し、
client は ack 受信後に次 spec に進む (= race なし)。

```sh
# text 直後に key:Enter — ack 機構により Enter は text の全 bytes 書き込み完了後に届く
hyoui input "$SESS" "text:ls -la" "key:Enter"
```

ack:Error が返ったら CLI は exit 1 で abort する。代表的なエラーコード:

| code | 意味 |
|---|---|
| `master.write-timeout` | 子が input を 500 ms 読まなかった (ICANON buffer 飽和 / 子停止) |
| `master.write-error` | daemon の I/O エラー |
| `master.write-partial` | partial 書き込み (defense-in-depth) |
| `client.ro-rejected` | Ro client (read-only attach) から input 送信を試みた |
| `client.lock-not-held` | lock 保持者と異なる client から input 送信を試みた |

`RAW_ACK_TIMEOUT` (5 秒) 内に ack が返らない場合は接続を poison して exit 1 する。
再利用不可なので、次の操作は新規 invocation で行う。

#### 4.2 ICANON アプリへの大量 byte 送信制限

bash / python / sh など **ICANON モード**で動く子は、line discipline の input buffer
(典型 1024 B) が満杯になると `master.write-timeout` を返す。1 spec で 1024 B 超を
送ると失敗するので、以下のいずれかで回避する:

- text を改行単位で **複数 spec に分割**して送る
- 1 spec あたりのサイズを 1 KB 未満に抑える

```sh
# NG: bash に 1024 B 超を 1 spec で送ると master.write-timeout になる可能性がある
hyoui input "$SESS" "text:$(cat large_payload.txt)"

# OK: 改行で分割して spec を分ける
hyoui input "$SESS" "text:line1" "key:Enter" "text:line2" "key:Enter"
```

alt screen TUI (vim / claude 等) は ICANON が無効なので大量 byte でも問題なし。

> **`wait:` / `wait-idle:` の目的はこれとは別。** 子の出力 state を待つ用途 (= 確認 prompt
> が出るまで待つ、出力が落ち着くまで待つ) に使う。ack 機構が保証するのは「bytes が
> 子の input stream に届いたこと」であり、「子がその bytes を処理し終えたこと」ではない。
> コマンド実行完了を待ちたい場合は `wait:` spec を別途使う。

#### 4.3 invocation auto-lock (DR-0022)

`hyoui input` は invocation 全体で 1 本の lock を **自動取得** する。これにより
並列に動く別の `hyoui input` (= 他 client) と bytes が混線せず、先着が完了するまで
後着が待つ (= 直列化)。

```sh
# 並列に同じ session へ input を送ると、両者は直列化される
hyoui input "$SESS" "text:hello\n" &
hyoui input "$SESS" "text:world\n" &
wait
# → screen には hello が完全に echo されてから world が echo される
```

- **`wait:` / `wait-idle:` 中も lock は保持される** (= 他 client の input は wait 中も
  block される)。これは invocation を atomic な一連の操作として扱うため
- **外側 token 継承時は auto-acquire を skip**: `--lock-token=<T>` flag か
  `HYOUI_LOCK_TOKEN` env が与えられている場合、外側 lock の token を継承するだけで
  自分は acquire しない (= 外側の lock を壊さない)
- **acquire timeout**: default 30 秒。他 client が長時間 lock を保持している場合は
  exit 1。`--auto-lock-timeout-acquire DUR` で調整可能
- **opt-out なし**: `--no-lock` 等の flag は無い。常に auto-lock 有効

```sh
# 外側で lock を取り、内側 input は token を継承する (= inner は auto-acquire skip)
TOKEN=$(hyoui lock acquire "$SESS" --timeout=10s &)
hyoui input --lock-token="$TOKEN" "$SESS" "text:..."  # inner は skip
hyoui lock release "$SESS" --token="$TOKEN"

# 長時間 wait が予想される場合は timeout を伸ばす
hyoui input --auto-lock-timeout-acquire=2m "$SESS" "text:..."
```

### 5. 画面が特定 state になるまで待つ

`wait` は **現在 visible な画面 state** に対して regex を match させるので、過去の
redraw による誤マッチが起きない。単独でも、`input` 列の中に `wait:` spec として
埋め込んでも使える。

```sh
# 単独: shell prompt が出るまで待つ (visible state に対する regex マッチ)
hyoui wait "$SESS" "^\\$" --timeout=10s

# 埋め込み: 確認 prompt を待ってから答える
hyoui input "$SESS" "wait:^Continue\\?" "key:Enter"
```

### 6. 画面を読む (`screen dump` / `snapshot`)

```sh
# ANSI byte dump — terminal に cat すると見た目を再現
hyoui screen dump "$SESS"
hyoui screen dump "$SESS" --layer=both --rect=0,0,80,5

# 構造化 snapshot (= daemon は CBOR が正本、`--format=json` で CLI 段が JSON 変換)
hyoui screen snapshot "$SESS" --include=Cells,Cursor,Mode               # CBOR (default、機械処理)
hyoui screen snapshot "$SESS" --include=Cursor,Mode --format=json | jq .  # JSON (jq に直接流せる)
# 注: `--format=json` 時、`cells` / `scrollback` の bytes は number array に展開されるため
# 量が増える。jq で見るだけなら `--include` から外しておくのが軽い。
```

### 7. 排他自動操作 (`lock`)

排他を取得して、操作列の途中で他 client が入力注入できないようにする。取得者は
leader 昇格、他は release まで強制 read-only。

```sh
hyoui lock acquire "$SESS" --timeout=30s
hyoui input "$SESS" "text:deploy" "key:Enter"
hyoui lock release "$SESS"   # `hyoui unlock "$SESS" --token=<T>` は alias
```

### 8. tty I/O timeline を録画する (`record`)

bytes-level の I/O timeline をファイルに永続化し、後から解析する (bug 再現、
asciinema 的 export)。`--both` で stdin + stdout、`--format` は `jsonl`
(timestamp + lifecycle event つき timeline) か `raw` (単一方向の生 stream)。

```sh
hyoui record start "$SESS" --output session.jsonl --both
hyoui record list "$SESS"
hyoui record stop "$SESS" --all
```

> **stdin の扱い**: default (`--input-secrecy=record-all`) は stdin を素通しで
> 記録する。passphrase / token を打つ可能性があるなら
> `--input-secrecy=never-record-stdin` を使うと stdin 由来の event は一切
> 記録されない。`redact-after-prompt` (prompt 検出後のみ redact) は Phase 5 予定で
> 現状は指定するとエラーになる ([DR-0016](./decisions/DR-0016-tty-io-record.md) §6a)。

### 9. session id と面 (状態の root)

session id は小文字・ハイフン付きの UUID だけ ([DR-0041](./decisions/DR-0041-session-id-uuid-and-tags.md))。`hyoui list` は今の面の全 session を起動時刻の順に並べる。

```sh
# id を起動側で決める (= stdout を読まずに後続の操作を組める)
SID=$(uuidgen | tr A-Z a-z)
hyoui run --detached --pty-stdin --session-id="$SID" -- bash
hyoui input "$SID" "text:ls" "key:Enter"
```

- 受け付けるのは標準形 `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` だけ。大文字・ハイフン無し・波括弧付き・先頭だけの短縮はエラーで、黙って正規化しない (表記が違うと同じ UUID でも別の socket になるため)。UUID の版は問わない
- 同じ id の socket が既にあると `hyoui run` は子を起こさずにエラーで終わる。相手の daemon が生きていても死んでいても同じで、run は生死を判定しない。動いている session なら `hyoui kill <id>`。daemon が死んで socket だけ残っている時は、lock が残っていれば `hyoui list` が片付ける (接続を断られ、lock を誰も持っていない socket を消す)。`hyoui list` に stale と出たら (lock が無い)、daemon が居ないことを確かめてから手で消す。判定は socket の bind と name lock の時点で行うので、同じ id の run を並行に打っても起動するのは 1 つだけ
- `hyoui kill --wait` は子と session の終了に加えて daemon の終了 (= socket の片付け) まで見届けて戻るので、戻った直後に同じ id で `run` できる。daemon が socket を片付けずに終わった時は exit 1 で、`hyoui list` での片付けを案内する
- `--wait` なしの `hyoui kill` は signal を送って戻る。子の終了後も daemon は遅れて来る attach のために約 2 秒残るので、その間に同じ id で `run` すると「socket が既にある」で失敗する。すぐ同じ id を使うなら `kill --wait` を使う

**tag** — session に `key=value` の tag を付け、`list` で絞り込める。既定は全部見え、絞り込みは指定した時だけ効く。

```sh
hyoui run --detached --tag env=prod --tag team=infra -- claude
hyoui run --detached --tag scratch -- bash          # `--tag scratch` は `--tag scratch=` (値が空) の略
hyoui list --tag env=prod                           # value の完全一致
hyoui list --tag team                               # key があれば一致 (value は問わない)
hyoui list --tag env=prod --tag team                # 繰り返しは AND
hyoui list --format=jsonl | jq 'select(.tags.team | startswith("in"))'   # 細かい条件は jsonl を絞る
```

- key は `[A-Za-z0-9._-]{1,256}`、value は任意の文字列 (最初の `=` で分ける、空も可)。同じ key を繰り返すと後勝ち
- `hyoui.` で始まる key は hyoui が予約しているので付けられない (run がエラーになる。別の接頭辞を使う)。大文字小文字は区別する。`list --tag hyoui.x` はエラーにならず、どの session にも一致しない
- `list --tag key` は key があれば一致、`list --tag key=` は value が空に完全一致で、別の条件。`--tag key=` で付けた session には両方が一致し、`--tag key=bar` で付けた session には `--tag key` だけが一致する。ワイルドカードは無い
- tag は daemon が持ち、`status` (`tags:` 行 / json の `tags`)、`list` (TAGS 列 / jsonl の `tags`)、web の `/api/sessions` に出る。daemon の upgrade をまたいで残る。起動後には変えられない
- `--tag` で絞ると、応答しない (no-response / stale / error) 行は tag が分からないので出ない
- 既定の tag を env で与える仕組みは無い。`HYOUI_NAMESPACE` は読まない
- `--namespace` / `--all-namespaces` は 2026-11 まで受け付けて捨てる (stdout は付けない時と同じ、stderr に 1 行の注意)。その後は unknown option になる

**面** — socket は `<状態の root>/sessions/<id>.sock` に置く。状態の root は次の順に決まり、hyoui の一式 (session の socket、web の監督者・unit・登録簿・passkey・logs) はその中で完結する。

1. `HYOUI_STATE_DIR` (空でなければそのまま。相対パスはエラー)
2. `$XDG_STATE_HOME/hyoui` (絶対パスの時だけ)
3. `$HOME/.local/state/hyoui` (`HOME` が無い時と、相対パスの時はエラー。cwd 相対にはしない)

面を分けたい時は、面の `.envrc` で `HYOUI_STATE_DIR` だけを設定する (`XDG_STATE_HOME` は他のアプリと共有なので書き換えない)。別の面の session は `list` にも id 指定にも出ない。面をまたいで扱う option は無いので、面ごとに環境変数を変えて実行する。config (`~/.config/hyoui/`) は面で分けず共有する。`XDG_RUNTIME_DIR` は使わない (ログインに紐づく寿命で、ログインを越えて動く session と合わない)。

- unix socket の `sun_path` の上限 (macOS 104 / Linux 108 bytes) はフルパスでは判定しない。収まらない時は socket の dir を開いた fd を基準に相対名で bind / connect するので、深い root でも使える
- hyoui が子プロセスへ足す env は `HYOUI_SESSION_ID` だけ。面の `HYOUI_STATE_DIR` は呼び出し元の env に在れば子に届き (`--login` でも)、子の中で起こす hyoui は同じ面を使う
- 次は session として読まず、在れば `hyoui list` と `hyoui web ...` が stderr に警告する: `sessions/` の外 (状態の root 直下や、`sessions/` / `web/` 以外の dir) の socket、`sessions/` の中の id が UUID でない socket、`$XDG_RUNTIME_DIR/hyoui` の下の socket、それらの置き場に残った symlink。手順は `docs/runbooks/session-uuid-migration-dr-0041.md`

### 10. 子プロセスへの env 漏洩を防ぐ (env scrub)

親 hyoui を `claude` 等の AI agent CLI から呼んだ時、親が export している
**Internal Context env** (例: `CLAUDE_CODE_SESSION_ID` / `CLAUDECODE` / `AI_AGENT`)
が子プロセスに POSIX fork→exec で素通しで漏れて、子 session が「親の延長」と
誤認される問題を防ぐための機構
([DR-0024](./decisions/DR-0024-env-scrub-config-file.md))。

**`claude` を子に取る場合は default で透過的に動く** (= builtin で公式 docs に出典
のある 9 env を削除)。設定不要。

| flag | 用途 |
|---|---|
| `--no-scrub-env` | scrub を完全 disable (= debug / 互換目的 escape hatch) |

builtin が未登録の target (= `claude` 以外の AI agent / 独自 tool) で削除したい
env を増やす、あるいは builtin で削除されている env を残したい場合は
`~/.config/hyoui/config.toml` で設定する:

```toml
[scrub_env]
enabled = true                    # 全体 on/off (default: true)

# claude の builtin に独自 env を追加
[scrub_env.targets.claude]
inherit_builtin = true            # default: true、builtin と user 設定を concat
kill_glob = ["CMUXMSG_*"]         # 追加で削除する env
keep_glob = ["AI_AGENT"]          # builtin から除外したい env

# 別 target を新規登録 (= builtin 未登録の独自 CLI)
[scrub_env.targets.my-tool]
inherit_builtin = false           # builtin 無視、user 設定のみ
kill_glob = ["MYTOOL_SECRET"]
```

target は `hyoui run -- <cmd>` の `<cmd>` を basename した値で lookup。
`env` 等の wrapper コマンドは展開せず、user は素直に `hyoui run -- claude` と
書く ([DR-0024 §2](./decisions/DR-0024-env-scrub-config-file.md))。

`HYOUI_*` で始まる env は user の `kill_glob` が当たっても削除されない (= hyoui
自身が `HYOUI_SESSION_ID` を意図的に子へ伝え、`HYOUI_STATE_DIR` で面を引き継がせるため)。

config パースエラー (= 不正 TOML / 型不一致) のときは hyoui の起動を拒否する
(= 意図しない設定での起動は親 Internal Context 漏洩リスクがあるため)。一時的に
迂回したい場合は `--no-scrub-env` を使う。

**`--login` の session には scrub の対象が無い** (= 子の env は呼び出し元から引き継がず最小から始まるため)。

#### ログイン shell として起動する (`--login`)

通常のターミナルアプリと同じログイン shell として起動する
([DR-0039](./decisions/DR-0039-webui-terminal-app-rework.md) 決定 1)。

```sh
hyoui run --login --detached --pty-stdin               # passwd の shell を login shell で
hyoui run --login --detached --pty-stdin -- zsh -f     # コマンド明示 (rc を読ませない例)
```

- 外から操作し続ける shell なので `--pty-stdin` で子の stdin も PTY にする。付けないと呼び出し元の stdin がそのまま子の stdin になり、端末でない起動元 (script や agent) からだと shell が stdin の EOF ですぐ終わる ([DR-0042](./decisions/DR-0042-non-tty-stdin-is-the-childs-fd.md))
- shell は passwd (`getpwuid`) から引く。呼び出し元の `$SHELL` は見ない
- argv[0] は `-<shell の basename>` (例: `-zsh`)。rc は shell が読む
- 子の env は呼び出し元から引き継がず最小から始める: `HOME` / `USER` / `LOGNAME` / `SHELL` / 初期 `PATH` / `LANG` (呼び出し元に在れば) / `TERM` (下記)。初期 `PATH` は macOS では `/etc/paths` と `/etc/paths.d/*` から、それ以外は `/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`
- `HYOUI_SESSION_ID` は最小化しても子に残る。`HYOUI_STATE_DIR` も設定されていれば残る (子の中で起こす hyoui が親と同じ面を使い、自己参照が届く)
- コマンドを明示した時は、そのコマンドを argv[0] の `-` 付けなしでそのまま起動し、env だけ最小にする
- 最小化するのは子の env だけ。hyoui 自身がどの面の root (`HYOUI_STATE_DIR` / `XDG_*`) を使うかは呼び出し元の env のまま決まる

子の `TERM` は `--login` の有無によらず呼び出し元の値を引き継ぐ。呼び出し元に無い (未設定 / 空) 時だけ config の `[session] term_fallback` (default `xterm-256color`) を設定する ([DR-0039](./decisions/DR-0039-webui-terminal-app-rework.md) 決定 1)。

### 11. 子が停止した時のふるまいと Ctrl+Z の action

`~/.config/hyoui/config.toml` の `[session]` / `[attach]` で設定する
([DR-0032](./decisions/DR-0032-child-suspend-unified-enum-and-action-menu.md))。

```toml
[session]
# 子が suspend (stopped) した時のふるまい。default: auto_resume_on_attached
on_child_suspend = "auto_resume_on_attached"
#   auto_resume_always      — daemon が常に即 SIGCONT (attach の有無に関係なく)
#   auto_resume_on_attached — rw attach client が居る間だけ起こす (無人時は停止を維持)
#   show_child_action_menu  — 起こさず、attach client が child action menu を表示
# 呼び出し元に TERM が無い時に子へ設定する端末種別。default: xterm-256color
term_fallback = "xterm-256color"

[attach]
# 単発 Ctrl+Z が確定した後の action。default: client_suspend
ctrlz_x1_action = "client_suspend"
#   client_suspend   — client 自身を suspend (fg で同じ窓に復帰)
#   client_detach    — 窓を畳む (子は走り続ける)
#   select_on_demand — 選択プロンプトを出す (^Z: suspend / ^C: client 終了 / Esc: 戻る)
```

`on_child_suspend` は「子が止まったらどうなるか」の 1 つの選択で、daemon の policy と
attach client のふるまいの両方に写像される (= 設定は 1 箇所)。`hyoui run
--on-child-suspend=notify|auto-resume` は **daemon 側 policy だけ**を上書きする。

**child action menu** (= `show_child_action_menu` を選んだ時): rw attach 中に子が
止まると画面下部にメニューが出て、その場で操作できる。表示中の打鍵は hyoui が飲むので
子には届かない (= 停止中に打った操作が resume 時にまとめて流れ込む事故を防ぐ)。

| キー | 動作 |
|---|---|
| `d` | 脱出: detach (client 終了。子は停止したまま残る) |
| `z` | 脱出: client suspend (`fg` で復帰すると子も起こす) |
| `c` / `Esc` | 子への操作: 起こす (SIGCONT)。Esc はこの停止の「取り消し」として同じ動作 |
| `i` / `h` | 子への操作: SIGINT / SIGHUP (停止中の子にも効くよう SIGCONT を併送) |
| `k` | 子への操作: SIGKILL |

操作キー以外の打鍵はすべて無視して捨てる (= 停止中の子に入力の受け手はいないため
「閉じるだけ」の操作は無い)。メニューが消えるのは、操作を選んだ時か、子が外部要因
(別 shell からの `hyoui kill --signal=CONT` 等) で動き出した時。

無人時 (= attach していない時) に子が止まった場合はメニューを出す先が無いので、次に
`hyoui attach` した時点で表示される。無人でも起こしたいなら `auto_resume_always` を選ぶ。

旧 key の `[session] auto_resume` / `[attach] resume_stopped_child` は削除された。
残っていると起動を拒否して書き換え先を案内する (= 設定が黙って default に倒れるのを防ぐ)。

どのファイルが読まれるか / 結局どう効いているかは以下で確認する:

```bash
hyoui config path   # 設定ファイルのパスを表示 (= 未作成でもパスは出る)
hyoui config show   # 実効設定を TOML で表示 (= 未設定項目も default 込み)
```

`config show` は全 key を実効値で出すので、「何を書いたか」ではなく
「今どう動いているか」が分かる。builtin の scrub default は config の key では
ないので TOML コメントとして併記される。

### 11. ブラウザから操作する (`web`)

```sh
hyoui web daemon add stable     # ~/.config/hyoui/web/stable.toml を書いて登録する
hyoui web daemon run stable
# ブラウザで http://127.0.0.1:43690/ を開く
```

gateway を foreground で起動する口は `hyoui web daemon run` 1 本で、起動する unit を必ず指定する (何も付けなければ help)。`hyoui web` 自体は `daemon` / `service` / `passkey` / `session` を束ねる名前空間でしかない。

インスタンス (unit) は config ファイル 1 つで、登録簿はどのファイルを読むかだけを覚える。`daemon add <name>` は `${XDG_CONFIG_HOME:-~/.config}/hyoui/web/<name>.toml` が無ければ書いて登録する。書く中身は、隣に `base.toml` があればそれへの `extends`、`state_dir` (今の面の状態の root)、`listen` (`--listen`、既定 `127.0.0.1:43690`)、`binary_path` (`--binary`、既定は `daemon add` を打った実行ファイル):

```toml
# ~/.config/hyoui/web/base.toml — 全 unit の土台 (面をまたいで共有する。state_dir は書かない)
[web]
assets_dir = "~/src/hyoui/crates/hyoui-web/assets"

# ~/.config/hyoui/web/unstable.toml — `daemon add unstable --listen 127.0.0.1:43691 --binary ~/src/hyoui/target/release/hyoui` が書く
extends = "base.toml"            # このファイルの隣から解く

[web]
state_dir = "/Users/me/.local/state/hyoui"
listen = "127.0.0.1:43691"
binary_path = "/Users/me/src/hyoui/target/release/hyoui"
```

```sh
hyoui web daemon add unstable --listen 127.0.0.1:43691 --binary ~/src/hyoui/target/release/hyoui
hyoui web daemon add mine --config ~/dotfiles/hyoui-web.toml   # 既存のファイルをこの名前で登録する
hyoui web service register   # 全 unit を抱える監督者を OS に載せる
hyoui web daemon status
```

- `<name>.toml` が既にあれば書き換えず、そのまま登録する (`--listen` / `--binary` を付けると断る)。`--config <path>` は任意の置き場の既存ファイルを登録する
- `[web].state_dir` は unit の config ファイル自身に必須 (`extends` で土台から継いだ値は認めない。土台は面をまたいで共有するため)。`daemon add` / `daemon run` は今の面の状態の root と realpath で比べ、食い違えば「この config は面 X のもので、今は面 Y で実行している」と断る (別の面の config をコピーして登録・起動した事故はここでしか気付けない)
- `daemon add` は面の登録簿の lock の中で検査・生成・登録を行い、別の add が実行中なら何も書かずに断る。生成した config が (壊れた `base.toml` 等で) 読めなければ、登録せずにそのファイルを消す
- `daemon add` は、同じ面の登録簿に同じ宛先の unit があるか、そのポートを今ほかのプロセスが listen しているか (実際に bind を試す) を確かめ、当たれば断る。空いているポートを自動では選ばない
- `daemon run <name>` は登録簿が指す config を、`daemon run --config <path>` は登録簿を通さずそのファイルを読む。どちらもそのファイルと `extends` でたどれるファイルだけを読み、`config.toml` は読まない。`daemon run --no-config [--listen <host:port>]` は config を読まず、組み込みの既定値と CLI 引数だけで起動する (テスト向け)
- 監督者は unit ごとに `<binary_path> web daemon run <name>` を子として起動する

`extends` は土台のファイルに重ねる: 表は鍵ごとに潜り、それ以外は置き換える。相対パスは書いたファイルの隣から解き、`~` は `$HOME` で開く。`binary_path` は `daemon add` の時点で登録簿に写る (変えたら `remove` → `add`)。`listen` / `assets_dir` は起動のたびにファイルから読む。状態 (登録簿・ログ・passkey) は `<状態の root>/web/` に置く。`service register` は場所を決める env (`HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `HYOUI_STATE_DIR`) を OS の定義に固定し、後から違う値で打つと `--force` 無しでは書き換えない。面の `.envrc` が効いた shell で register すれば、その面の監督者が面の root ごとに立つ。

セッション画面のキーボード FAB を開いて「情報」タブへ切り替えると、attach の mode / leader
を確認できる。leader が別 browser にある場合は「leader になる」を押すと接続を切らずに
主導権を移し、新しい browser の viewport に PTY サイズを合わせる。失敗理由は同じ Attach
欄に表示される。

## トラブルシューティング

| 症状 | 対処 |
|---|---|
| `hyoui list` に session が出ない | 面 (`HYOUI_STATE_DIR` 等) が起動時と同じか確認する (別の面の session は見えない)。古い置き場の socket が残っていれば `hyoui list` が stderr に警告する (`docs/runbooks/session-uuid-migration-dr-0041.md`) |
| `hyoui run` が「socket が既にある」で起動しない | 同じ id の session が動いていれば `hyoui kill --wait <id>` (= daemon の終了まで待つ)。daemon が死んで socket だけ残っていれば、lock が残っていれば `hyoui list` が片付ける。stale と出たら daemon が居ないことを確かめてから手で消す。別の id で起動してもよい |
| attach 直後に切られる | daemon が cap negotiation で reject した可能性 (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| 子プロセスが死んで daemon だけ残る | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

詳細な runbook は `docs/runbooks/INDEX.md` を参照。

## 関連リンク

- [README-ja.md](../README-ja.md) — インストール、コンセプト、最初の hello world
- [DESIGN-ja.md](./DESIGN-ja.md) — 内部アーキテクチャ
- [ROADMAP.md](./ROADMAP.md) — v0.2.0+ のレシピが追加されるタイミング
- [docs/runbooks/](./runbooks/) — 障害対応手順
