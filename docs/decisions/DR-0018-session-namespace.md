# DR-0018: session namespace — socket dir 分離による list 混在防止

- Status: Active
- Date: 2026-06-11
- Related: DR-0005 (思想 — 透明性最優先), DR-0006 (CLI ground rules), DR-0014 (透過原則 + self-check — 本 DR の env 注入 justify), DR-0004 (subcommand 設計)
- Origin: docs/issue/2026-06-11-feature-namespace.md (kawaz 提案 + 合意 2026-06-11)

## Context

普段使いの claude (= hyoui 経由で起動してリモート attach / 外部操作する) と、特定用途で
一斉起動する hyoui+claude 群 (例: 過去セッション要約を分担する worker 群) が
`hyoui list` で混ざると邪魔。用途グループごとに session を分離したい。

## Decision

### 1. 方式 (a): socket dir 分離

socket 配置を namespace ごとの dir に分離する:

| namespace | socket path |
|---|---|
| `default` (= 予約名) | `<base>/<session>.sock` (= **従来 dir 直下、既存セッションと完全互換**) |
| その他 `<ns>` | `<base>/<ns>/<session>.sock` |

`<base>` は `$XDG_RUNTIME_DIR/hyoui` (実在時) / **`/tmp/hyoui-<uid>`** (それ以外、
= macOS 含む)。後者は tmux の `/tmp/tmux-<uid>` と同じ前例で、unix socket の
`sun_path` 上限 (macOS 104 / Linux 108 bytes) に namespace + session 名を載せる予算を
確保するため `$TMPDIR` を使わず `/tmp` 固定にしている (= macOS の per-user TMPDIR
`/var/folders/.../T/` が長すぎて namespace path が ENAMETOOLONG になる bug への対処、
2026-06-11、breaking だが v0.x で許容)。
namespace dir の作成・検証は base dir と同じ規律 (= 新規作成時 mode 0700、既存 dir は
所有者 + mode 検証) を 2 段で適用する (`socket_path::resolve_in_namespace`)。

resolve 時に最終 socket path の byte 長を `sun_path` 上限と照合し、超過する場合は
「現在長 / 上限 / ns・session 名を短くする方法」を含む人間可読のエラーで bind 前に
弾く (`socket_path::check_sun_path_len` + `sys::socket::sun_path_max`)。

- `hyoui list` は現在の namespace のみ scan する (= dir が分かれているので
  **socket probe 自体が ns 外に発生しない**、混在コストゼロ)
- attach / kill / input / status / tail / wait / screen / lock / record の session 名・
  `--index` 解決も同じ namespace スコープ (= `--socket` 直指定は従来通り任意パス)

### 2. namespace の解決: flag > env > default

```
--namespace=X  >  env HYOUI_NAMESPACE  >  "default"
```

全 session 系コマンドが同じ解決 (`socket_path::resolve_namespace`) を使う。
direnv 運用と相性が良い (= プロジェクトの .envrc に `export HYOUI_NAMESPACE=...` で
そのプロジェクト内の起動・list・attach が全部自動分離)。

### 3. validate: session_id と同等 + `/` 禁止 (= 階層は将来拡張に予約)

namespace 名は `validate_namespace` (= `hyoui::cli`) で whitelist 検証する:

- 許可: `[A-Za-z0-9._-]{1,64}` (= `validate_session_id` と同等、path traversal 防止)
- 明示 reject: 空文字 / `.` / `..` / `/` / `\`
- `default` は予約名 (= base dir 直下マッピング)。`--namespace=default` の明示指定は
  未指定と完全に同じ挙動

**`/` 禁止は将来の階層 namespace 用の予約** (= §Rejected「階層 ns」参照)。

### 4. 子 env への `HYOUI_NAMESPACE` 常時注入

`hyoui run` は解決済 namespace を子プロセスの env に **常時注入**する
(= 指定なし時も `HYOUI_NAMESPACE=default` を入れる)。

実装: daemonize init (`HYOUI_DAEMONIZE_INIT` JSON) に解決済 `namespace` を乗せ、
daemon child が `Session::start` (= fork + execvp) **前** に自 env へ
`HYOUI_NAMESPACE=<ns>` を set → execvp される子 PTY が継承する。
新規 protocol message / cap flag は不要 (= 既存の env 伝搬経路に field 1 個)。

#### 透過原則との緊張と justify (= DR-0014 self-check)

env 注入は「子から観測可能な介入」であり透過原則 (DR-0005/0014) に抵触する。
本 DR はこれを **namespace 継承の必然**として justify する:

- ns 内でネスト起動した hyoui が指定なしで同 ns を引き継ぐのが自然
  (= worker が更に `hyoui run` するケース)。env 以外にこの継承を実現する経路がない
- flag 経由と env (direnv) 経由で子への伝播挙動が揃う (= 非対称の解消。注入しないと
  「flag で起動した時だけネスト起動が default に漏れる」非対称が生まれる)
- 前例: tmux の `TMUX`、screen の `STY` (= ラッパーが管理下を env で示す確立した慣行)
- 「hyoui 配下には必ず `HYOUI_NAMESPACE` がある」という不変条件は自己検出にも使える
- 最小介入: env 変数 1 個のみ。子の fd / signal / termios / 画面には一切触れない

ns 内から別 ns で起動するには `--namespace=<別ns>` を明示する
(`--namespace=default` で default に戻る)。MANUAL に記載。

### 5. `hyoui list` の表示

- default: 現在の namespace のみ表示 (= default ns では従来と同じ見え方、NS 列なし)
- `--all-namespaces`: 全 ns 横断 scan + **NS 列を先頭に追加**。`--namespace` と排他
- `--prune-stale` も ns スコープ (= `--all-namespaces` 併用で全 ns 掃除)
- `--format=jsonl` は `namespace` field を**常時**出力 (= default ns は `"default"`)

## Rejected alternatives

### 方式 (b): session 名 prefix 規約 (`ns/name`) + list フィルタ

実装は軽量だが、全 socket probe 後のフィルタになるため**混在コスト (= probe I/O +
status query) が残る**。また命名規約の強制力が弱く、規約に従わない session が混ざる。

### 方式 (c): daemon がメタデータとして ns を保持し list でフィルタ

daemon の status.response に ns field を足す案。**旧 daemon との互換処理**
(= field が無い response の扱い) が必要になる割に、(a) に対する利点がない。
probe 後フィルタである点も (b) と同じで混在コストが残る。

### 階層 namespace (= `a/b/c`)

当面入れない (YAGNI)。フラットな一意名 (`task-xx-team`) でグルーピングの動機は
満たせる。階層が本当に必要なのは「親 ns ごと再帰一括操作」のニーズが出た時だけ。

階層を入れると以下を背負う (= 今の動機に対して過剰):

- 相対/絶対の解決規則 (= ns 内で `--namespace=X` は `parent/X` か `X` か)
- env 継承での無際限な深化
- validate の path traversal 境界の複雑化
- 再帰操作の意味論

**拡張余地**: ns 名 validate で `/` を当面禁止しておく。将来階層が必要になったら
`/` を区切りとして新 DR を立てて導入する (= 既存のフラット ns 名と衝突せず
後方互換で階層化できる)。

## Consequences

- 既存セッション (= base dir 直下) は `default` namespace としてそのまま見える
  (= dir 移動なし、migration 不要)
- `--socket` 明示指定は namespace 解決を bypass する (= 従来通り任意パス)
- `hyoui run` (非 detached) が exec する `hyoui attach` には、`--socket` 未指定時に
  `--namespace=<解決済ns>` を明示で渡す (= flag 経路で起動した場合に env が無くても
  attach が同じ socket を解決できるように)
- jsonl 出力に `namespace` field が増える (= v1.0 未満、breaking 許容方針)

## Implementation

- `hyoui::cli`: `DEFAULT_NAMESPACE` / `validate_namespace` / 各 Config に
  `namespace: Option<String>` / list に `all_namespaces`
- `hyoui-cli::socket_path`: `resolve_namespace` (flag > env > default) /
  `resolve_in_namespace` (= validate + 2 段 ensure_socket_dir)
- `hyoui-cli::main`: `list_candidate_dirs(ns)` / `list_candidate_dirs_all_namespaces()` /
  `resolve_session_by_index(idx, ns)` / `resolve_target_socket(.., ns)` / list の NS 列
- `hyoui-cli::daemonize`: `DaemonizeInit.namespace` (serde default = "default" で旧 JSON 互換) +
  daemon child の `HYOUI_NAMESPACE` set (= `sys::env::set_var_at_startup`)
