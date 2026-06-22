# DR-0024: 子 PTY env scrub の config ファイル化と CLI flag 最小化

- Status: Active
- Date: 2026-06-22
- Related: DR-0005 (透明性最優先), DR-0014 (介入判断 self-check / マトリクス検証), DR-0018 (`HYOUI_NAMESPACE` 注入 = 透過例外の先例), DR-0020 (`HYOUI_SESSION_ID` 注入 = 同上), DR-0023 (本 DR で **Superseded**)
- Origin: kawaz feedback on DR-0023 CLI flag overdesign (= `--scrub-env-add` / `--scrub-env-keep` / `--scrub-env-target` は config の役割を CLI に出張させたもの、設定ファイル機構の方が筋)

## Context

DR-0023 で target-aware env scrub を導入したが、CLI flag が 4 個に膨らんだ:

| flag | 本来の役割 |
|---|---|
| `--no-scrub-env` | 全体 on/off — **CLI として妥当** |
| `--scrub-env-target=<name>` | target 自動推定の override |
| `--scrub-env-add=<glob>` | builtin kill_glob 追加 |
| `--scrub-env-keep=<glob>` | builtin kill_glob から除外 |

下 3 個は **「user が一度決めて使い回す設定」** であり、起動毎に CLI で打つものではない。
DR-0023 §6 で「config ファイル機構は scope 外、別 DR」と倒した制約に縛られて、CLI flag に
無理矢理載せた結果になっている = 設計のレイヤ誤り。

加えて env wrapper unwrap (`hyoui run -- env FOO=bar claude` の `env` を読み飛ばして `claude` を
target にする) は DR-0023 で future work に倒されていたが、user 方針として **サポート対象外**
が確定 (= 「env を自分で書いて wrap するのは user が自分で制御したいだけかも」)。

## 介入判断 self-check (DR-0014)

- **既存 DR で justify されているか?** → env scrub 自体は DR-0023 で justify 済。本 DR は
  「介入の方法」だけを redesign する (= 介入の必要性は変わらない)
- **透過原則を破る理由は必然か?** → Yes、DR-0023 と同根拠 (Internal Context env が継承
  されると子 session の identity が親に塗りつぶされる)
- **最小介入か?** → Yes。**削除のみ**は不変、CLI flag 数を 4 → 1 に減らす方が CLI 表面積
  最小
- **kernel / PTY / shell の再発明か?** → No。target 別 kill list の知識を hyoui 側に集約
  する点が新規価値
- **新 protocol message / cap flag 追加か?** → No。`DaemonizeInit.scrub_env` の中身が
  user pattern 平坦 list から target 別 ScrubPlan に変わるだけ (= wire 形式の変更は v1.0
  前 breaking で許容)
- **既存 DR の実装漏れではないか?** → No。DR-0023 実装済の上に redesign

## Decision

### 1. CLI flag を `--no-scrub-env` 1 個に絞る

| flag | 残す/削除 | 理由 |
|---|---|---|
| `--no-scrub-env` | **残す** | 全体 on/off は debug / 互換目的の escape hatch として CLI に置く価値あり |
| `--scrub-env-target=<name>` | **削除** | env wrapper サポートなしに方針確定、target は argv basename 推定のみ |
| `--scrub-env-add=<glob>` | **削除** | 設定ファイルで管理 |
| `--scrub-env-keep=<glob>` | **削除** | 設定ファイルで管理 |

### 2. target 推定は argv basename のみ

`hyoui run -- <cmd> <args...>` の `<cmd>` を basename した値が target。
env wrapper (`hyoui run -- env FOO=bar claude`) は **サポート対象外** (= `env` が target に
なり claude builtin は当たらない)。user は wrapper を使わず素直に `hyoui run -- claude` する
か、自前で env scrub する。

### 3. 設定ファイル機構を導入

#### パス解決

1. `$XDG_CONFIG_HOME/hyoui/config.toml` (環境変数指定時)
2. `~/.config/hyoui/config.toml` (XDG 不在時の fallback)
3. ファイル不在 = builtin default のみで動作 (= scrub なし target は no-op、claude は builtin 9 env 削除)

#### 形式: TOML

```toml
[scrub_env]
# 全体 on/off (= --no-scrub-env と同等の制御)
# default: true
enabled = true

# target 別設定
[scrub_env.targets.claude]
inherit_builtin = true     # default: true
kill_glob = []             # 追加で削除する pattern
keep_glob = []             # builtin kill_glob から除外する pattern

[scrub_env.targets.codex]
inherit_builtin = true
kill_glob = ["CODEX_INTERNAL_*"]
keep_glob = []

[scrub_env.targets.my-tool]
inherit_builtin = false    # builtin なしから始める (target が builtin 未登録なら true/false 同義)
kill_glob = ["MYTOOL_SECRET"]
keep_glob = []
```

将来 hyoui に別 persistent setting (log / notify 等) を追加する場合は同じく
`[<feature>]` セクションとして並列に置く (= top-level prefix を避けて機能ごとに
namespace を切る、Cargo の `[package]` / `[profile.release]` と同じ慣習)。

#### `inherit_builtin` 意味論

- `true` (default): builtin kill_glob + user kill_glob を **concat**、builtin keep_glob + user keep_glob を **concat**
- `false`: builtin 完全無視、user 設定 (kill_glob / keep_glob) のみ適用

builtin が無い target (例: `vim` / `cat` / `my-tool`) では true/false 同義 (= builtin が空集合)。

#### merge 計算順 (= apply 時の準備手順)

1. target = argv basename を resolve
2. `[scrub_env.targets.<target>]` を config から lookup (= 未指定なら全 field default)
3. `effective_kill = builtin_kill (inherit_builtin が true の時のみ) ∪ user_kill`
4. `effective_keep = builtin_keep (inherit_builtin が true の時のみ) ∪ user_keep`
5. environ walk → `effective_kill` に match → `effective_keep` に match しないものを削除

### 4. 組み込み defaults (DR-0023 から不変)

| target | builtin kill_glob (exact match) |
|---|---|
| `claude` | `CLAUDECODE` / `CLAUDE_CODE_CHILD_SESSION` / `CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_AGENT` / `CLAUDE_CODE_ENTRYPOINT` / `CLAUDE_CODE_EXECPATH` / `CLAUDE_JOB_DIR` / `CLAUDE_PLUGIN_DATA` / `AI_AGENT` |
| (他全 target) | 組み込みなし |

`builtin keep_glob` は現状なし。

### 5. 保護対象 (= scrub list に含まれていても削除しない、DR-0023 §4 から不変)

`HYOUI_*` プレフィックスを持つ全 env は **強制的に削除されない**。

- `HYOUI_NAMESPACE` (DR-0018 §1)
- `HYOUI_SESSION_ID` (DR-0020 §1)
- `HYOUI_LOCK_TOKEN` (DR-0006 §12, DR-0022)
- `HYOUI_SOCK` / `HYOUI_NAME` (DR-0006 §12 nest 検知)
- その他 hyoui が意図的に子に伝える env

理由: scrub の目的は **親 hyoui の Internal Context 漏洩防止** であり、hyoui 自身が
透過例外として justify した env 注入を user 設定で削れるとしてはならない。

### 6. glob 仕様 (DR-0023 §5 から不変)

- `*` = 0 文字以上の任意マッチ
- `?` = 1 文字マッチ
- それ以外は literal
- 大文字小文字区別あり (= POSIX env 慣習)
- 先頭 `!` で否定は採用しない (= `keep_glob` がある)

### 7. config file 不在 / 不正時の挙動

- **不在**: silent fallback to builtin-only (= scrub なし target は no-op、claude は builtin 9 env 削除)
- **パースエラー** (= TOML syntax error / 型不一致 / 未知 field の構造不整合): **stderr に error 出力 + exit non-zero で hyoui 起動を拒否**。意図しない設定での起動は害 (= 親 host Internal Context 漏洩を伴う子 session 汚染リスク) であり、user が config を直すまで起動させない。retreat-is-last-resort 観点からも撤退 (= scrub 無効化 fallback) ではなく user に修正を強制する方が正しい
- **unknown field**: パースエラーとして扱わず、warn なしで無視 (= 将来拡張用 field を前方互換で許容、`#[serde(deny_unknown_fields)]` は付けない)

### 8. 実装場所と境界

責務:

- **CLI 側 (`hyoui run` 親)**:
  - config 読み込み (= `~/.config/hyoui/config.toml` の load + parse)
  - target = argv basename 推定
  - `[scrub_env_targets.<target>]` lookup + builtin merge
  - 結果を flat な `ScrubPlan { kill: Vec<String>, keep: Vec<String> }` に解決して `DaemonizeInit.scrub_env: Option<ScrubPlan>` に詰める
  - `None` = 完全 disable (= `--no-scrub-env` または config の `scrub_env_enabled = false`)
- **daemon child 側**: `init.scrub_env` を読んで `env_scrub::apply` を呼ぶ。environ walk →
  glob match → protected guard → `remove_var` の流れは daemon 内 (= 子 environ を直接見るのが
  必要なので)

ファイル:

| 層 | file | 変更 |
|---|---|---|
| config 読み込み | `crates/hyoui/src/config/mod.rs` (新規) | TOML parse + path resolve + builtin merge logic |
| CLI parse | `crates/hyoui/src/cli.rs::RunConfig` | flag 3 個削除 (`scrub_env_target` / `scrub_env_add` / `scrub_env_keep`)、`--no-scrub-env` のみ残存 |
| 親 → daemon 配線 | `crates/hyoui-cli/src/main.rs::run_command`, `crates/hyoui-cli/src/daemonize.rs` | config load → target resolve → ScrubPlan 解決 → `DaemonizeInit.scrub_env` に詰める |
| daemon child | `crates/hyoui-cli/src/daemonize.rs::run_daemon_child` | `set_var_at_startup` 前に `env_scrub::apply` 実行 (= 変更なし) |
| scrub logic | `crates/hyoui/src/sys/env_scrub.rs` | 構造体 update (= flat `ScrubPlan`)、builtin defaults は維持、protected guard は維持 |

### 9. crate 依存

TOML パーサ: **`toml`** crate (= rust-lang/toml、serde 統合済、依存最小)。

### 10. log / 観測性 (DR-0023 §8 から不変)

- default は無音 (= 削除有無に関わらず stderr 出力なし)
- `apply` は `ScrubResult` (= 削除した env 名 + protected で skip した env 名) を return
- 観測したい場合は将来 `HYOUI_VERBOSE` 等で opt-in (= 別 DR)

## Consequences

### Pros

- CLI 表面積が 4 → 1 に縮小、`hyoui run --help` がすっきり
- 「user が一度決めて使い回す設定」が設定ファイルに収まる正しい責務分離
- hyoui 初の config ファイル機構が出来上がり、将来の拡張 (= 他の persistent setting) への
  道筋がつく
- builtin に無い target (= codex / gemini-cli / 独自 tool) も config だけで対応可、CLI 学習コスト不要

### Cons

- v0.9.x の CLI flag (3 個) を削除する **breaking change** (= v1.0 未満なので memory
  方針通り許容)
- hyoui に設定ファイル機構が新規導入される (= 初の persistent state、責務範囲を明確に保つ
  必要あり = scrub 以外の設定を勢いで増やさない)
- config 不在時の builtin-only 動作で誤動作した場合、user が「config が無いから?」と疑う
  可能性 → README で明示

### Future work

- **他 AI agent CLI** (codex / gemini-cli 等) の builtin default 追加は、公式 docs /
  一次資料の出典確認後 (= empirical-verification、推測で組み込まない)
- **観測性 (= log opt-in)**: `HYOUI_VERBOSE` 等で opt-in 経路を用意 (= 別 DR)
- **config file 機構の他用途**: 本 DR で導入した config ファイル機構を他の persistent
  setting に流用するかは、需要が出た時点で別 DR (= 勢いで広げない)
- **env wrapper unwrap**: 方針として **採用しない** (= user 自身で env 制御する場合は
  user が明示的に scrub 制御する責務、本 DR では future work からも外す)
- **multi-session daemon への移行**: 本 DR は DR-0023 同様「1 daemon = 1 session = 1
  child」(DR-0015) の invariant に依存。将来 multi-session daemon に拡張する場合は新規 DR
  で介入箇所を移し替える

## Verification (DR-0014 §マトリクス検証)

### category 横断 (= scrub 配線が target 推定で正しく分岐するか)

| category | 検証コマンド | 期待 |
|---|---|---|
| AI agent (claude) | `hyoui run -- claude` | builtin 9 env 削除、独立 session 化 |
| TUI alt screen (vim) | `hyoui run -- vim` | target=vim、builtin なし → scrub なし、従来挙動 |
| line-oriented (cat) | `hyoui run -- cat` | target=cat、同上、従来挙動 |

### claude core ケース

| 親環境 | 引数 | 期待 |
|---|---|---|
| Claude Code session 内 | `hyoui run -- claude --session-id <UUID> --name child` | 子 self-name が `child` に反映、claude.ai UI で親配下にネストされない |
| Claude Code session 内 | `hyoui run -- claude` | 親 Internal Context 漏洩なし、子は独立 session として新規起動 |
| 素の terminal | `hyoui run -- claude` | environ に Internal Context env が無い → 削除対象なし、scrub は no-op |
| Claude Code session 内 | `hyoui run --no-scrub-env -- claude` | 全継承、issue #1 の症状再現 (= 退行確認) |

### config 機構

| config 状態 | 検証コマンド | 期待 |
|---|---|---|
| ファイル不在 | `hyoui run -- claude` | builtin-only で動作、stderr 無音 |
| `[scrub_env] enabled = false` | `hyoui run -- claude` | 全継承 (= `--no-scrub-env` と同等) |
| `[scrub_env.targets.claude] keep_glob = ["AI_AGENT"]` + `inherit_builtin = true` | `hyoui run -- claude` | `AI_AGENT` は残る、他 8 個削除 |
| `[scrub_env.targets.claude] kill_glob = ["CMUXMSG_*"]` + `inherit_builtin = true` | `hyoui run -- claude` | builtin 9 + `CMUXMSG_*` 削除 |
| `[scrub_env.targets.claude] inherit_builtin = false` | `hyoui run -- claude` | builtin 9 個 **無視**、user kill_glob のみ適用 |
| `[scrub_env.targets.my-tool] kill_glob = ["MYTOOL_SECRET"]` | `hyoui run -- my-tool` | `MYTOOL_SECRET` 削除、他無干渉 |
| 不正 TOML | `hyoui run -- claude` | stderr error 出力 + exit non-zero (= 起動拒否、user に config 修正を強制) |
| unknown field 含む | `hyoui run -- claude` | unknown field は無視、他の field は正常に適用、起動継続 |

### protected guard

| 設定 | 検証コマンド | 期待 |
|---|---|---|
| `[scrub_env.targets.claude] kill_glob = ["HYOUI_*"]` | `hyoui run -- claude` | `HYOUI_*` は protected で削除されない (= `ScrubResult.protected_hits` に積まれるが stderr 無音) |

## Migration from DR-0023

v0.9.x で `--scrub-env-target` / `--scrub-env-add` / `--scrub-env-keep` を使っていた user は:

| 旧 CLI flag | 新 (config TOML) |
|---|---|
| `--scrub-env-target=claude` | (target 推定が argv basename 固定になったため、env wrapper 経由は非対応) |
| `--scrub-env-add=CMUXMSG_*` | `[scrub_env.targets.claude]` の `kill_glob = ["CMUXMSG_*"]` |
| `--scrub-env-keep=AI_AGENT` | `[scrub_env.targets.claude]` の `keep_glob = ["AI_AGENT"]` |

CLI で flag を渡している script は次の v 系で動かなくなる (= breaking、v1.0 未満)。
