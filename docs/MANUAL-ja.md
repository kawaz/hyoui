# hyoui ユーザマニュアル

> [English](./MANUAL.md) | 日本語

エンドユーザ (CLI から hyoui を使う人) 向けのユースケース別レシピ集。

- **インストール / 概念紹介** → [`README-ja.md`](../README-ja.md)
- **内部設計・なぜそうなっているか** → [`DESIGN-ja.md`](./DESIGN-ja.md)
- **このファイル**: 「○○ をやりたい」→ 「このコマンド列で実現」のレシピ

> Status: v0.2.x をカバー。自動操作 API (`input` family / `wait` / `screen` /
> `lock` / `record` / `tail`) は実装済。`serve` HTTP gateway と `tx` wrapper は未実装。

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
# detach は Ctrl-A D
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

# 構造化 snapshot (= wire 上は CBOR、--format=json は forward-compat で未配線)
# jq に流す前に CBOR デコーダを通すこと
hyoui screen snapshot "$SESS" --include=Cells,Cursor,Mode
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

> **⚠ stdin の redaction はまだ未配線。** `--input-secrecy`（default は
> `redact-after-prompt`）の値に関わらず stdin は素通しで記録される — redaction の
> state machine は Phase 5 に積み残し
> ([DR-0016](./decisions/DR-0016-tty-io-record.md))。passphrase / token を打つ
> 可能性があるなら `--stdout` のみ録画するか、secret 入力中は録画を避けること。

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

## トラブルシューティング

| 症状 | 対処 |
|---|---|
| `hyoui list` に session が出ない | `XDG_RUNTIME_DIR` / `TMPDIR` の socket dir に stale socket が残っていないか確認 (`docs/runbooks/2026-05-27-stale-socket-detection.md`) |
| attach 直後に切られる | daemon が cap negotiation で reject した可能性 (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| 子プロセスが死んで daemon だけ残る | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

詳細な runbook は `docs/runbooks/INDEX.md` を参照。

## 関連リンク

- [README-ja.md](../README-ja.md) — インストール、コンセプト、最初の hello world
- [DESIGN-ja.md](./DESIGN-ja.md) — 内部アーキテクチャ
- [ROADMAP.md](./ROADMAP.md) — v0.2.0+ のレシピが追加されるタイミング
- [docs/runbooks/](./runbooks/) — 障害対応手順
