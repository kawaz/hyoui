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
  - [9. session を namespace でグループ分けする](#9-session-を-namespace-でグループ分けする)
  - [10. 子プロセスへの env 漏洩を防ぐ (env scrub)](#10-子プロセスへの-env-漏洩を防ぐ-env-scrub)
  - [11. ブラウザから操作する (web)](#11-ブラウザから操作する-web)
- [トラブルシューティング](#トラブルシューティング)
- [関連リンク](#関連リンク)

## 基本フロー

### 1. detached でセッションを起動して別端末から attach

```sh
# 端末 A: detached でセッション起動 (session id が stdout に出る)
hyoui run --detached -- claude
# → run-<pid>-<rand>  (例)

# 端末 B: list で確認 → attach
hyoui list
hyoui attach run-<pid>-<rand>
# Ctrl+Z 単発で client を suspend (= shell に戻る、fg で復帰)
# 接続を畳むなら hyoui detach run-<pid>-<rand>
```

### 2. read-only で観察する

```sh
hyoui attach --observer run-<pid>-<rand>
# observer は入力を送らない読み取り専用 attach
```

### 3. 終了させる

```sh
hyoui kill run-<pid>-<rand>            # SIGTERM
hyoui kill --signal KILL run-<pid>-<rand>  # SIGKILL
```

## 自動操作

以下のレシピは `SESS` に session id が入っている前提（例: `SESS=$(hyoui run --detached -- bash)`）。

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

### 9. session を namespace でグループ分けする

普段使いの `claude` と一時的な worker 群のように、無関係な session グループが
`hyoui list` で混ざるのが邪魔なときは **namespace**
([DR-0018](./decisions/DR-0018-session-namespace.md)) を使う。解決順は
`--namespace` flag > env `HYOUI_NAMESPACE` > `default` で、全 session 系コマンドが
同じ解決を共有する。`default` namespace は従来の socket 配置そのままなので、
既存 session には影響しない。

```sh
# worker 群を隔離する
hyoui run --detached --namespace=workers --session=w1 -- worker-cmd
hyoui run --detached --namespace=workers --session=w2 -- worker-cmd

hyoui list                            # default のみ — worker は混ざらない
hyoui list --namespace=workers        # worker 群のみ
hyoui list --all-namespaces           # 全部 (= 先頭に NS 列)
hyoui list --all-namespaces --prune-stale  # 全 namespace の stale socket を掃除

# selector は全部 namespace スコープ (session id / --index / kill --all / ...)
hyoui attach w1 --namespace=workers
hyoui input --namespace=workers w1 "text:ls" "key:Enter"
hyoui kill --all --namespace=workers
```

**direnv レシピ** — プロジェクトの `.envrc` に書く:

```sh
export HYOUI_NAMESPACE=myproj
```

これでそのプロジェクト dir 内で実行する `hyoui run` / `list` / `attach` が
flag なしで全部自動分離される。

**継承** — `hyoui run` は解決済 namespace を子プロセスの env に
`HYOUI_NAMESPACE` として **常時注入**する (= `default` でも入れる)。tmux の
`TMUX` / screen の `STY` と同じ慣行で、namespace 内の session からネスト起動した
hyoui は指定なしで同じ namespace に入る。別 namespace で起動したい場合は
`--namespace=<別ns>` (例: `--namespace=default`) を明示する。この env 変数は
「自分は hyoui 配下か、どの namespace か」の自己検出にも使える。

namespace 名は session id と同じ文字集合 (`[A-Za-z0-9._-]`、最大 64 bytes)。
`/` は現状 reject される (= 将来の階層 namespace 用に予約)。`default` は
base socket dir 直下にマップされる予約名。

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
自身が `HYOUI_NAMESPACE` / `HYOUI_SESSION_ID` 等を意図的に子へ伝えるため)。

config パースエラー (= 不正 TOML / 型不一致) のときは hyoui の起動を拒否する
(= 意図しない設定での起動は親 Internal Context 漏洩リスクがあるため)。一時的に
迂回したい場合は `--no-scrub-env` を使う。

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
hyoui web --listen=127.0.0.1:43690
# ブラウザで http://127.0.0.1:43690/ を開く
```

セッション画面のキーボード FAB を開いて「情報」タブへ切り替えると、attach の mode / leader
を確認できる。leader が別 browser にある場合は「leader になる」を押すと接続を切らずに
主導権を移し、新しい browser の viewport に PTY サイズを合わせる。失敗理由は同じ Attach
欄に表示される。

## トラブルシューティング

| 症状 | 対処 |
|---|---|
| `hyoui list` に session が出ない | `$XDG_RUNTIME_DIR/hyoui` / `${XDG_STATE_HOME:-$HOME/.local/state}/hyoui` の socket dir に stale socket が残っていないか確認 (`docs/runbooks/2026-05-27-stale-socket-detection.md`) |
| attach 直後に切られる | daemon が cap negotiation で reject した可能性 (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| 子プロセスが死んで daemon だけ残る | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

詳細な runbook は `docs/runbooks/INDEX.md` を参照。

## 関連リンク

- [README-ja.md](../README-ja.md) — インストール、コンセプト、最初の hello world
- [DESIGN-ja.md](./DESIGN-ja.md) — 内部アーキテクチャ
- [ROADMAP.md](./ROADMAP.md) — v0.2.0+ のレシピが追加されるタイミング
- [docs/runbooks/](./runbooks/) — 障害対応手順
