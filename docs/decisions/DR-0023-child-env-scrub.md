# DR-0023: 子 PTY env scrub — 親 Internal Context env の漏洩防止

- Status: **Superseded by DR-0024** (= CLI flag 過剰、config ファイル機構の方が筋という kawaz feedback で redesign)
- Date: 2026-06-21
- Related: DR-0005 (透明性最優先), DR-0014 (介入判断 self-check / マトリクス検証), DR-0015 (`hyoui run` = fork daemon + attach client), DR-0018 (`HYOUI_NAMESPACE` 注入 = 透過例外の先例), DR-0020 (`HYOUI_SESSION_ID` 注入 = 同上), [DR-0024](./DR-0024-env-scrub-config-file.md) (= Supersede 先)
- Origin: GitHub issue #1 (kawaz/hyoui)

## Context

`hyoui run -- claude` を Claude Code session 内から起動すると、親 Claude Code が
export している以下の **Internal Context env** が子 claude process に POSIX
fork→exec で素通しで継承される:

| env | 公式 docs での役割 |
|---|---|
| `CLAUDECODE` | Claude Code 配下マーカー |
| `CLAUDE_CODE_CHILD_SESSION` | 子セッション flag |
| `CLAUDE_CODE_SESSION_ID` | 親 session id (= 子の name lookup 主犯) |
| `CLAUDE_CODE_AGENT` | agent kind |
| `CLAUDE_CODE_ENTRYPOINT` | 起動経路 |
| `CLAUDE_CODE_EXECPATH` | claude バイナリ path |
| `CLAUDE_JOB_DIR` | background job 専用 dir |
| `CLAUDE_PLUGIN_DATA` | plugin data dir |
| `AI_AGENT` | Vercel `@vercel/detect-agent` convention (Claude Code バイナリ内に hardcoded export) |

結果: 子 claude session が「親 session の延長として起動された」と認識し:

1. 子の display name が **親と同じ** になる (= `--name <new>` 引数を渡しても上書きされない)
2. `claude agents --json` の通常リストに **子として隠れて表示されない** (= child session 扱い)
3. claude.ai UI で「親の display name 配下のサブ session」として現れる

issue #1 でユーザが検証したマトリクス (= 並列/逐次 × unset pattern B/C/D):
**`CLAUDE_CODE_SESSION_ID` の unset が決定的**で、全 Internal Context env を unset
(= D パターン) すれば確実に独立 session 化することが判明している。

## 介入判断 self-check (DR-0014)

- **既存 DR で justify されているか?** → No、本 DR で新規 justify
- **透過原則を破る理由は必然か?** → **Yes、必然**:
  - **主根拠 (= 実測)**: issue #1 のマトリクス検証で「親 Internal Context env が子に
    継承されると子 session が親と同一 identity と誤認する」「`CLAUDE_CODE_SESSION_ID`
    の unset が決定的、全 Internal Context env unset で確実に独立化する」が観測済。
    放置すると `--name <new>` 引数を明示しても子 session の identity が親に塗りつぶされ、
    子 claude が「親の延長」として扱われる = **「外側自動操作の透明性」** (DR-0005)
    そのものが成立しない
  - **副根拠**: DR-0018 / DR-0020 で「透過例外として子 env への意図的介入 (= 注入)
    を target-aware に justify する」枠が確立済。env scrub は同枠の対極操作 (= 削除)
    であり、判断軸を共有
  - 副根拠 (補助): kernel / PTY / shell の標準機能 (= `env -u CLAUDECODE ... claude`)
    は対症療法として使えるが、対象 env list の知識を user が持つ必要があり、
    UX 観点で hyoui 側に集約する価値はある (= ただしこれは「便利」寄りなので主根拠
    にはしない)
- **最小介入か?** → Yes、**削除のみ**。追加・改変・書き換えは行わない
- **kernel / PTY / shell の再発明か?** → No。env 削除自体は `env -u` の機能だが、
  target 別の正確な kill list を組み込む点が新規価値。`env wrapper` (= `hyoui run --
  env FOO=bar claude`) の 1 段 unwrap は本 DR Phase 1 では実装せず、`--scrub-env-target=`
  明示経路で対応
- **新 protocol message / cap flag 追加か?** → No。CLI flag + `DaemonizeInit` JSON
  フィールドのみ (= wire protocol は不変)
- **既存 DR の実装漏れではないか?** → 確認済。env scrub に関連する既存 DR なし

## Decision

### 1. target-aware env scrub を `hyoui run` に導入する

daemon が子 PTY を fork+execvp する **直前** (= `run_daemon_child` 内、`set_var_at_startup`
で `HYOUI_NAMESPACE` / `HYOUI_SESSION_ID` を注入する **前**) で、target に応じた
`kill_glob` list を使って `std::env::remove_var` で削除する。fork→exec の environ
継承は POSIX 標準 (DR-0015 §2.3.5) のままで、削除は daemon child process 自身の
environ を弄ることで実現する (= async-signal-safe な execve envp 配列を組む方式は
複雑度が増すので採らない。daemon = 1 session = 1 child の構造により、daemon の
environ を直接弄ることに副作用なし)。

### 2. target 推定

- 既定: argv の最初の token (= `--` 以降の `argv[0]`) を `basename` した値
- `--scrub-env-target=<name>` で明示 override 可 (= `hyoui run -- env FOO=bar claude` の
  ような env wrapper 経由の場合に有用)
- `--no-scrub-env` 指定時は推定そのものをスキップ

### 3. 組み込み defaults (Phase 1)

| target | kill_glob (exact match) |
|---|---|
| `claude` | `CLAUDECODE` / `CLAUDE_CODE_CHILD_SESSION` / `CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_AGENT` / `CLAUDE_CODE_ENTRYPOINT` / `CLAUDE_CODE_EXECPATH` / `CLAUDE_JOB_DIR` / `CLAUDE_PLUGIN_DATA` / `AI_AGENT` |
| (他全 target) | 組み込みなし (= 追加は flag で) |

根拠:

- 上記 9 個のうち **8 個は Claude Code 公式 env-vars docs** の "Claude Internal
  Context" セクション (= "auto-exported to child processes" 明記)
  - ref: <https://code.claude.com/docs/en/env-vars>
- **`AI_AGENT`** は Vercel `@vercel/detect-agent` convention。Claude Code バイナリ内に
  hardcoded で `claude-code_<version>_agent` を export している
  - ref: <https://www.npmjs.com/package/@vercel/detect-agent>

scrub **しない** env (= 公式 docs で "User-Settable Configuration" 分類):

- `CLAUDE_CONFIG_DIR` (= 認証境界の切替に必須)
- `CLAUDE_CODE_DISABLE_*` / `CLAUDE_CODE_EFFORT_LEVEL` 等 30+ 個のユーザ設定 env
- `ANTHROPIC_*` (= 認証 / endpoint / model 設定)

### 4. 保護対象 (= scrub list に含まれていても削除しない)

以下は user の `--scrub-env-add=<glob>` や将来の config 拡張で glob が当たっても
**強制的に削除されない**:

- `HYOUI_*` プレフィックスを持つ全 env
  - `HYOUI_NAMESPACE` (DR-0018 §1)
  - `HYOUI_SESSION_ID` (DR-0020 §1)
  - `HYOUI_LOCK_TOKEN` (DR-0006 §12, DR-0022)
  - `HYOUI_SOCK` / `HYOUI_NAME` (DR-0006 §12 nest 検知)
  - その他 hyoui が意図的に子に伝える env

理由: scrub の介入は **親 hyoui の Internal Context 漏洩防止** が目的であり、
hyoui 自身が DR-0018 / DR-0020 で透過例外として justify した env 注入を user 設定で
削れるとしてはならない (= 透過例外の意図を破る)。

### 5. CLI flag

`hyoui run` に以下を追加:

| flag | 意味 |
|---|---|
| `--no-scrub-env` | scrub を完全 disable (= **debug / 互換目的の escape hatch**。指定時は親 env が全て子に漏れる = 従来挙動と等価。protected guard も働かない) |
| `--scrub-env-target=<name>` | 明示 target 指定 (= default は argv basename。`env` wrapper や script wrapper 経由で argv basename が target を指さない場合に使う) |
| `--scrub-env-add=<glob>` | 組み込みに追加 (= 複数指定可、繰り返し) |
| `--scrub-env-keep=<glob>` | 組み込み + add から除外 (= 複数指定可、繰り返し) |

glob 仕様:

- `*` = 0 文字以上の任意マッチ
- `?` = 1 文字マッチ
- それ以外は literal
- 大文字小文字区別あり (= POSIX env 慣習)
- 先頭 `!` で否定は採用しない (= `--scrub-env-keep` flag があるので不要)

### 6. config ファイル機構

`~/.config/hyoui/config.yaml` 等の config ファイル読み込みは **本 DR の scope 外**
とする。現 hyoui は flag + env 主軸で config ファイル機構なし、新規導入は
別 DR が必要。issue #1 提案の user config 拡張は将来 DR で扱う。本 DR では
`--scrub-env-add` / `--scrub-env-keep` で必要十分な拡張余地を提供する。

### 7. 実装場所と境界

責務:

- **CLI 側 (`hyoui run` 親)**: RunConfig から target 推定 + builtin + add + keep を
  合成して flat な glob patterns `Vec<String>` を解決、`DaemonizeInit.scrub_env:
  Option<Vec<String>>` に詰める。`None` で完全 disable (= `--no-scrub-env`)、
  `Some(vec)` で daemon 側に適用させる (= 空 vec は no-op target を表現可能)
- **daemon child 側**: `init.scrub_env` を読んで `env_scrub::apply` を呼ぶ。
  environ walk → glob match → protected guard → `remove_var` の流れは daemon 内
  (= 子 environ を直接見るのが必要なので)

理由:

- target 推定 / builtin lookup は user-facing rule なので CLI 側に置く方が test
  しやすい (= environ 依存なし、pure function)
- environ walk は daemon process の environ を見る必要があるので daemon 側
- 境界が明確 = wire 形式が `Vec<String>` 1 個で済み、`DaemonizeInit` schema が
  最小

ファイル:

| 層 | file | 変更 |
|---|---|---|
| CLI parse | `crates/hyoui/src/cli.rs::RunConfig` | flag 4 個 + フィールド 4 個追加 |
| 親 → daemon 配線 | `crates/hyoui-cli/src/main.rs::run_command`, `crates/hyoui-cli/src/daemonize.rs::spawn_detached_daemon_and_wait_ready` / `run_detached_parent` | RunConfig から `resolve_globs` で解決 → `DaemonizeInit.scrub_env` に詰める |
| daemon child | `crates/hyoui-cli/src/daemonize.rs::run_daemon_child` | `set_var_at_startup` 前に `env_scrub::apply` 実行 |
| scrub logic | `crates/hyoui/src/sys/env_scrub.rs` (新規) | builtin defaults + glob match + resolve_globs + apply + protected guard |

### 8. log / 観測性

- **default は無音** (= 削除有無に関わらず stderr 出力なし)。常時 stderr は
  「親実行環境を露呈する」副作用が透過原則上望ましくない、daemon 起動時に毎回
  noise が出ると DR-0020 §5 の `--quiet` 系一貫性とも合わない
- 観測したい場合は将来 `HYOUI_VERBOSE` 等で opt-in を提供 (= Phase 1 scope 外、
  別 DR)
- `apply` は `ScrubResult` (= 削除した env 名 + protected で skip した env 名) を
  return するので、debugger / 観測道具側からは取り出せる
- protected hit (= `HYOUI_*` を user pattern が当てた) も **default 無音**。
  `apply` の戻り値で観測可能

## Consequences

### Pros

- 親 host process (例: Claude Code session) の Internal Context 漏洩を解消、子
  session の独立性を確保
- 公式 docs に出典のある env に対象を限定 (= 推測ベースでない、empirical-verification
  ルール準拠)
- user flag で hyoui 組み込みが想定しない target にも応用可 (= codex / gemini-cli /
  独自スクリプト等)

### Cons

- 透過原則を破る介入が 1 個増える (= DR-0018 / DR-0020 と同じ枠の例外、本 DR で
  明示 justify)
- target 推定 (argv basename) は env wrapper (例: `hyoui run -- env FOO=bar claude`)
  で誤推定する → `--scrub-env-target=claude` で user が明示する経路を提供
- 組み込み default list が公式 docs 改訂で陳腐化する可能性 → 定期 review が必要
  (= 別 issue で追跡)

### Future work

- **config ファイル機構** の導入は別 DR (= scope 外)
- 他 AI agent CLI (codex / gemini-cli 等) の組み込み default 追加は、公式 docs / 一次
  資料の出典確認後 (= empirical-verification、推測で組み込まない)
- `--scrub-env-clear` (= 全削除 + allowlist) 方式は本 DR では採用せず、必要性が
  明確になったら別 DR
- **env wrapper 1 段 unwrap** (= `argv[0] == "env"` の場合に `-u VAR` / `KEY=VAL` /
  `--` を読み飛ばして実 command を target にする経路) は Phase 1 では実装しない。
  user は `--scrub-env-target=claude` で明示する。需要が観測されたら別 DR で追加
- **multi-session daemon への移行**: 本 DR は「**1 daemon = 1 session = 1 child**」
  (DR-0015) の invariant に依存している (= daemon 自身の environ を直接
  `remove_var` で書き換える方式が成立するのはこの前提下のみ)。将来 multi-session
  daemon に拡張する場合、`Command::env_remove` 相当 or `execve` envp 配列の明示構築
  へ移行が必須。本 DR を Superseded に倒すか、新規 DR で介入箇所を移し替える
- **観測性 (= log opt-in)**: 削除有無を観測したい場面が出てきたら `HYOUI_VERBOSE`
  等で opt-in 経路を用意 (= 別 DR)

## Verification (DR-0014 §マトリクス検証)

最低 3 種類の category で実機検証する。core ケース (= claude) は親環境 / 引数の
細分化で複数検証する。

### category 横断 (= scrub 配線が target 推定で正しく分岐するか)

| category | 検証コマンド | 期待 |
|---|---|---|
| AI agent (claude) | `hyoui run -- claude` | 9 env 削除、独立 session 化 |
| TUI alt screen (vim) | `hyoui run -- vim` | target=vim 推定 → 組み込み default なし → 何も削除されない、従来挙動 |
| line-oriented (cat) | `hyoui run -- cat` | target=cat 推定 → 同上、従来挙動 |

### claude core ケース (= 主 issue の独立化検証)

| 親環境 | 引数 | 期待 |
|---|---|---|
| Claude Code session 内 | `hyoui run -- claude --session-id <UUID> --name child` | 子 self-name が `child` に反映、claude.ai UI で親配下にネストされない |
| Claude Code session 内 | `hyoui run -- claude` (引数なし) | 親 Internal Context 漏洩なし、子は独立 session として新規起動 |
| 素の terminal (= 親なし) | `hyoui run -- claude` | environ に Internal Context env が無い → 削除対象なし、scrub は no-op |
| Claude Code session 内 | `hyoui run --no-scrub-env -- claude` | 全継承、issue #1 の症状再現 (= 退行確認) |

### flag 個別

| flag | 検証コマンド | 期待 |
|---|---|---|
| 明示 target | `hyoui run --scrub-env-target=claude -- env FOO=bar claude` | env wrapper 経由でも 9 env 削除 |
| keep override | `hyoui run --scrub-env-keep=AI_AGENT -- claude` | `AI_AGENT` は残る、他 8 個は削除 |
| add 追加 | `hyoui run --scrub-env-add='CMUXMSG_*' -- claude` | 組み込み 9 個 + `CMUXMSG_*` マッチを削除 |
| protected guard | `hyoui run --scrub-env-add='HYOUI_*' -- claude` | `HYOUI_*` は protected で削除されない (= `ScrubResult.protected_hits` に積まれるが stderr 無音) |
