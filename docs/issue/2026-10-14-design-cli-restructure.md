---
title: CLI 全体の整理 — 現状の採取と観点別の対照表 (裁定は kawaz)
status: open
category: design
created: 2026-10-14T15:20:00+09:00
last_read: 2026-10-14T15:20:00+09:00
open_entered: 2026-10-14T15:20:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# design: CLI 全体の整理 — 現状の採取と観点別の対照表

## 位置づけ

CLI 全体を整理するための設計の素材。実装はしない。裁定は kawaz が行い、決まった内容は新しい DR に起こす (観点 8)。

- 評価軸: reference `cli-design-preferences` (以下「好み」) と reference `cli-daemon-subcommands` (以下「daemon 体系」)
- 隣の issue との分担:
  - screen 配下の中身 (format の整理、`dump` の置き場、overlay) は [feature-cli-restructure-discussion](./2026-05-28-feature-cli-restructure-discussion.md) を正とし、本 issue では扱わない
  - tag の残り (kill の絞り込み等の要否) は [session-labels](./2026-10-14-request-session-labels.md)。起動後の付け替えの置き場だけを観点 4 で扱う
  - `tx` の実装は [tx-lock-unlock-cli-subcommands](./2026-05-27-tx-lock-unlock-cli-subcommands.md)。置き場だけを観点 3 で扱う
  - `hyoui log` の追加案は [session-log-viewer-and-cleanup](./2026-10-09-task-session-log-viewer-and-cleanup.md)。語彙を決める時は観点 2 に合わせる
- 採取の条件 (2026-10-14):
  - バイナリは `target/release/hyoui` (`hyoui 0.16.4 (02fc5cd9)`)
  - `--help` は全 56 コマンドを実機で採取した (`HYOUI_SESSION_ID` / `HYOUI_LOCK_TOKEN` を外し、`HYOUI_STATE_DIR` を一時 dir に向けて実行)
  - **引数なし実行はトップ以外未実測。** 実測のコマンドが auto mode classifier に止められたため、`crates/hyoui/src/cli.rs` の `parse_*` と `crates/hyoui-cli/src/main.rs` の分岐から判定した
  - 補完は `hyoui completion bash|zsh|fish` の実出力を、help の OPTIONS と parser の option literal に機械的に突き合わせた

## 1. 現状の採取

### 1.1 引数なし実行と help の節構成

凡例:

- 「中」= `$HYOUI_SESSION_ID` がある (hyoui session の中から実行)、「外」= 無い。エラーは stderr に `hyoui: <msg>` と ``Run `hyoui --help` for usage.`` を出して exit 2
- 書式 A = 1 行目 `hyoui X — 説明` + `USAGE:` 見出し + 4 字下げ + `-h, --help`。書式 B = 1 行目が usage そのもの + 散文 + 2 字下げ (`--help, -h` と `-h, --help` が混在)
- 行 = help の行数、括弧内は日本語 (ひらがな・カタカナ・漢字) を含む行数
- 節 = 見出しと、その本文の行数

| コマンド | 引数なし | 書式 | 行 | 節 |
|---|---|---|---|---|
| `(top)` | help (実測、exit 0) | A | 34 (2) | USAGE 2, SUBCOMMANDS 20, RESERVED (not yet implemented) 2, GLOBAL OPTIONS 4 |
| `run` | エラー (`no command given`) | A | 98 (71) | USAGE 3, OPTIONS 50, STDIN 6, ENVIRONMENT 14, DURATION FORMAT 6, EXIT STATUS 11 |
| `attach` | エラー (中でも。DR-0020 §3) | A | 70 (44) | USAGE 4, OPTIONS 10, SESSION SELECTOR 6, INPUT 14, CTRL+Z GUARD, ENVIRONMENT 2, EXIT STATUS 10, EXAMPLES 8, RELATED 6 |
| `list` | 実行 | A | 61 (31) | USAGE 2, OPTIONS 7, OUTPUT (plain) 4, COLUMNS 12, OUTPUT (jsonl) 2, SORT ORDER 3, SESSION STATUS 3, SCAN 5, EXIT CODE 2, EXAMPLES 5, RELATED 3 |
| `kill` | 外: エラー / 中: 自 session に SIGTERM (= 自分の終了) | A | 89 (70) | USAGE 6, 2 軸モデル 13, OPTIONS 22, SIGNAL SPEC 7, SESSION SELECTOR 5, EXIT CODE 6, EXAMPLES 18, RELATED 2 |
| `status` | 外: エラー / 中: 自 session の status | A | 36 (15) | USAGE 5, OPTIONS 5, SELF-SESSION 4, OUTPUT 12, EXIT CODE 3 |
| `set` | エラー (外: session 必須 / 中: `<key>=<value>` 必須) | A | 31 (14) | USAGE 5, SUPPORTED KEYS 5, OPTIONS 4, EXIT CODE 4 |
| `tail` | 外: エラー / 中: 自 session の scrollback を流して終了 | A | 49 (29) | USAGE 5, OPTIONS 9, DURATION FORMAT 10, EXIT CODE 3, EXAMPLES 6, RELATED 3 |
| `wait` | エラー (pattern 必須) | A | 44 (17) | USAGE 5, PATTERN 4, MATCH SCOPE 6, OPTIONS 7, DURATION FORMAT 5, EXIT CODE 5, EXAMPLES 3 |
| `screen` | help | A | 13 (0) | USAGE 2, SUBCOMMANDS 3, OPTIONS 3 |
| `screen dump` | 外: エラー / 中: 自画面を dump | A | 52 (30) | USAGE 5, OPTIONS 23, ENVIRONMENT 2, EXIT CODE 5, EXAMPLES 6, RELATED 3 |
| `screen snapshot` | 外: エラー / 中: 自 state を出力 | A | 41 (18) | USAGE 5, OPTIONS 12, ENVIRONMENT 2, EXIT CODE 5, EXAMPLES 6, RELATED 3 |
| `input` | エラー (外: session 必須 / 中: spec 必須) | A | 71 (35) | USAGE 5, SPECS 8, OPTIONS 12, ENVIRONMENT 6, DURATION FORMAT 4, EXIT CODE 4, EXAMPLES 8, NOTE 11, RELATED 2 |
| `lock` | help | A | 17 (0) | USAGE 2, SUBCOMMANDS 3, OPTIONS 2, RELATED 4 |
| `lock acquire` | 外: エラー / 中: 自 session の lock を取って block | A | 53 (24) | USAGE 5, OPTIONS 8, BEHAVIOR 14, OUTPUT 3, DURATION FORMAT 4, EXIT CODE 3, EXAMPLES 4, RELATED 2 |
| `lock release` | 外: エラー / 中: token が無ければエラー、`HYOUI_LOCK_TOKEN` があれば release | A | 39 (14) | USAGE 5, OPTIONS 5, BEHAVIOR 9, ENVIRONMENT 2, EXIT CODE 4, EXAMPLES 3, RELATED 2 |
| `unlock` | `lock release` と同じ | A | 32 (8) | USAGE 5, OPTIONS 8, ENVIRONMENT 2, EXIT CODE 4, EXAMPLES 3, RELATED 2 |
| `detach` | 外: エラー / 中: 自 session の全 client を切断 | A | 36 (16) | USAGE 5, OPTIONS 4, SELF-SESSION 3, EXIT CODE 4, EXAMPLES 3, RELATED 1 |
| `record` | help | A | 22 (6) | USAGE 2, SUBCOMMANDS 4, OPTIONS 4, SECURITY NOTE 6 |
| `record start` | エラー (`--output` 必須) | A | 53 (26) | USAGE 4, OPTIONS 20, ENVIRONMENT 2, EXIT CODE 4, EXAMPLES 12, SECURITY 3 |
| `record stop` | 外: エラー / 中: active な record が 1 つならそれを停止 | A | 28 (13) | USAGE 4, OPTIONS 11, EXIT CODE 4, EXAMPLES 3 |
| `record list` | 外: エラー / 中: 実行 | A | 23 (6) | USAGE 4, OPTIONS 8, EXIT CODE 3, EXAMPLES 2 |
| `config` | help | B | 14 (0) | SUBCOMMANDS 3, OPTIONS 4 |
| `config path` | 実行 (引数を取らない) | B | 14 (0) | OPTIONS 2, Exit codes 2 |
| `config show` | 実行 (引数を取らない) | B | 18 (0) | OPTIONS 2, Exit codes 2 |
| `web` | help | B | 53 (0) | SUBCOMMANDS 5, OPTIONS 2, Config 16, Endpoints 13 |
| `web daemon` | help | B | 31 (0) | SUBCOMMANDS 14, OPTIONS 6 |
| `web daemon run` | help | B | 25 (0) | OPTIONS 4 |
| `web daemon supervise` | 実行 (監督者を foreground で起動) | B | 16 (0) | OPTIONS 1 |
| `web daemon add` | help | B | 43 (0) | OPTIONS 8 |
| `web daemon remove` | help | B | 8 (0) | OPTIONS 1 |
| `web daemon list` | 実行 | B | 11 (0) | OPTIONS 1 |
| `web daemon start` / `stop` / `restart` | help | B | 9 / 12 / 17 (0) | OPTIONS 2 |
| `web daemon status` / `log` | 実行 (全 unit) | B | 18 / 14 (0) | OPTIONS 2 / 5 |
| `web service` | help | B | 23 (0) | SUBCOMMANDS 8, OPTIONS 6 |
| `web service register` | 実行 (OS に登録して起動する) | B | 24 (0) | OPTIONS 5 |
| `web service unregister` / `start` / `stop` / `restart` / `status` / `log` | 実行 (引数を取らない、log は `--follow` だけ) | B | 8〜18 (0) | OPTIONS 1〜2 |
| `web passkey` | help | B | 22 (0) | SUBCOMMANDS 4, OPTIONS 5 |
| `web passkey add` | エラー (`--endpoint` 必須) | B | 21 (0) | OPTIONS 10 |
| `web passkey list` | 実行 | B | 15 (0) | OPTIONS 5 |
| `web passkey remove` | help | B | 13 (0) | OPTIONS 3 |
| `web session` | help | B | 12 (0) | SUBCOMMANDS 3, OPTIONS 1 |
| `web session list` | 実行 | B | 9 (0) | OPTIONS 3 |
| `web session remove` | help | B | 12 (0) | OPTIONS 3 |
| `upgrade` | 外: エラー / 中: 自 session の daemon を self-exec で入れ替える | B | 25 (1) | Options 14 |
| `version` | 実行 (JSON) | B | 14 (0) | OPTIONS 1 |
| `completion` | エラー (shell 名必須) | A | 29 (9) | USAGE 2, OPTIONS 2, SHELLS 4, EXAMPLES 9, EXIT CODE 3, RELATED 1 |
| `tx` | エラー (予約、`--help` も同じエラーで exit 2) | — | 2 (0) | (見出し無し) |

集計:

- 書式 A は 23 本 (session 系と自動操作系、`completion`)、書式 B は 32 本 (`config` / `web` / `upgrade` / `version`)
- EXIT の見出し: `EXIT STATUS` 2 本 (run / attach)、`EXIT CODE` 17 本、`Exit codes` 2 本 (config path / show)、無し 35 本 (web 系のうち 7 本は散文の「Output is JSON; errors are JSON on stderr with a non-zero exit.」)
- help の option 表記: `-h, --help` 4 字下げ 23 本、`-h, --help` 2 字下げ 4 本 (config 3 本と upgrade)、`--help, -h` 2 字下げ 28 本 (web 系と version)
- `GLOBAL OPTIONS` 節はトップだけ。`Options:` (小文字混じり) は upgrade だけ
- DR 番号を本文に含む help は 25 本
- `DURATION FORMAT` 節は run / tail / wait / input / lock acquire の 5 本にあり、文面は 4 種類
- 日本語を含む help は書式 A に集中し、英文と日本語が 1 行の中で混ざる (例: run の `--timeout DUR  Overall timeout (DUR フォーマットは下記参照)`)。parser のエラー文も日本語と英語が混在する

### 1.2 補完と help・実装の突き合わせ

help の OPTIONS 行 (H)、parser の option literal (I)、bash (B)、zsh (Z)、fish (F) の 5 者を、コマンドごとに option 名の集合で比べた。

- **option 名は全 56 コマンドで 5 者が一致する。** enum の値の候補 (`run --on-child-suspend`、`set` の `key=value`、`attach --mode`、`list` / `status` / `record list` / `record start` / `screen dump` / `screen snapshot` の `--format`、`snapshot --include`、`lock acquire --mode`、`kill --signal`) も 3 shell と help で一致する
- parser だけが受け付けるもの:
  - 旧名の alias: `tail --strip` (= `--strip-ansi`)、`tail --last` (= `--last-bytes`)。help は説明文中の「alias:」でだけ触れ、補完は出さない
  - 廃止を案内する専用エラー: `kill --signum`、`run --mode`、`run --on-parent-suspend`、`attach --exclusive` / `--detach-others`。補完に出さないのは意図どおり
- 位置引数の候補は 3 shell とも出さない: session id (`attach` / `kill` / `status` 等の第 1 引数)、web の unit 名 (`web daemon run|start|stop|restart|status|log|remove`)、passkey の sub (`web passkey remove`)、web session の id (`web session remove`)
- トップのサブコマンドの説明文は 4 か所 (トップ help、zsh、fish、bash は説明なし) で文面が別。例: attach はトップ help が「Attach to an existing daemon session」、zsh / fish が「Attach to a running session」。zsh / fish の説明には DR 番号が残る (input「(DR-0006 §8)」、upgrade「(DR-0028)」、version「(DR-0034)」)
- `tx` はトップ help の RESERVED 節に出て、補完には出ない (`completion.rs` の方針どおり)
- 同期の test: 名前の集合は `IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS` 等の定数と `completion_all_shells_mention_*`、補完 → parser の向きは `completion_flags_are_accepted_by_parser` が保証する。help ↔ 補完の向き (説明文と option 表) を突き合わせる test は無い

### 1.3 環境変数

ENVIRONMENT 節がある help は 8 本:

| help | 載っている変数 |
|---|---|
| `run` | `HYOUI_STATE_DIR` / `XDG_STATE_HOME` / `HYOUI_SCROLLBACK_ROWS` / `TERM` / `HYOUI_SESSION_ID` |
| `attach` / `screen dump` / `screen snapshot` / `record start` | `HYOUI_LOCK_TOKEN` |
| `input` | `HYOUI_LOCK_TOKEN` / `HYOUI_MAX_FILE_BYTES` |
| `lock release` / `unlock` | `HYOUI_LOCK_TOKEN` (fallback token として) |

節の外で触れている help: `HYOUI_SESSION_ID` は self-session を書く 12 本の USAGE / 本文、`HYOUI_WAIT_POLL_MS` は wait の `--poll-interval` の説明、`XDG_CONFIG_HOME` は config / config path / web daemon / web daemon add、`HYOUI_STATE_DIR` / `XDG_STATE_HOME` は list の SCAN と web passkey、web service register は焼き込む変数 (`HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `HYOUI_STATE_DIR` / `PATH`) を振る舞いとして説明する。

ずれ:

- `HYOUI_LOCK_TOKEN` は session に接続するすべてのコマンドが handshake の token として読む (`main.rs` で 14 か所) が、ENVIRONMENT 節に載るのは 7 本で、文面は 4 種類
- `docs/MANUAL.md` / `docs/MANUAL-ja.md` に Environment 節は無い

コードが読む変数の全集合 (MANUAL の Environment 節に書く候補):

| 変数 | 読む側 | 区分 |
|---|---|---|
| `HYOUI_STATE_DIR` | session を扱う全コマンド、web 系、version (状態の root) | 利用者向け |
| `XDG_STATE_HOME` | 同上 (`HYOUI_STATE_DIR` が無い時の root の親、絶対パスの時だけ) | 利用者向け |
| `HOME` | 同上の最後の fallback、config の置き場の fallback | 利用者向け |
| `XDG_CONFIG_HOME` | run / attach の `config.toml`、config path / show、web の unit config の既定の置き場 | 利用者向け |
| `HYOUI_SESSION_ID` | daemon が子へ常に注入する。attach / run 以外の session 系が session 省略時に読む (DR-0020) | 利用者向け |
| `HYOUI_LOCK_TOKEN` | session に接続する全コマンド (handshake の token)。run は daemon の期待 token に取り込み、子の env からは消す。lock release / unlock の fallback、input の継承 | 利用者向け |
| `HYOUI_SCROLLBACK_ROWS` | run (`--scrollback-rows` が優先) | 利用者向け |
| `HYOUI_MAX_FILE_BYTES` | input (`--max-file-bytes` が優先) | 利用者向け |
| `HYOUI_WAIT_POLL_MS` | wait と input の `wait:` spec (`--poll-interval` が優先) | 利用者向け |
| `TERM` | run (子に引き継ぐ。無ければ config の `[session] term_fallback`) | 利用者向け |
| `PATH` | web service register が重複を除いて unit 定義に焼く | 利用者向け |
| `HYOUI_WEB_SERVICE_LABEL` | web service (監督者の label を差し替える。実機で本番と並べて確かめる口) | 検証用 |
| `HYOUI_ALLOW_CORE` | session daemon (core dump 抑止をやめる)。2026-07-29 に内部デバッグ専用と裁定済み | 内部デバッグ |
| `XDG_RUNTIME_DIR` | 旧置き場の session を見つけて警告する時だけ (DR-0041 決定 7 の移行) | 移行期 |
| `HYOUI_DAEMONIZE_INIT` / `HYOUI_UPGRADE_*` (11 個) | fork / self-exec の受け渡し | 内部 (書かない) |
| `HYOUI_BUILD_ID` | build 時 (`build.rs`) | 内部 (書かない) |

## 2. 観点ごとの対照表

### 観点 1: 引数なし実行

規範の読み方: 好みは「トップ・子・孫の全サブコマンドで引数なしは help」、daemon 体系はそれを具体化して「子を持つレベルと必須引数のあるコマンドは引数なしで help。省略可能な引数だけのコマンドは実行」とする。下の表は daemon 体系で判定した (どちらで読むかは CLI-R1)。

| 対象 | 現状 | 案 | 根拠 |
|---|---|---|---|
| 子を持つレベル (トップ、screen / lock / record / config / web / web 配下 4 つ) | help | そのまま | 好み、daemon 体系 |
| 必須引数を欠く leaf のうち既に help のもの (web daemon run / add / remove / start / stop / restart、web passkey remove、web session remove) | help | そのまま | daemon 体系 |
| 必須引数を欠く leaf のうちエラーのもの: `run` / `attach` / `wait` / `input` / `set` / `record start` / `completion` / `web passkey add` | エラー exit 2 | help (exit 0) | daemon 体系、DR-0004 (引数なしは help) |
| session 引数が外では必須・中では省略可の leaf: `kill` / `status` / `tail` / `screen dump` / `screen snapshot` / `lock acquire` / `lock release` / `unlock` / `detach` / `upgrade` / `record stop` / `record list` | 外: エラー、中: 自 session に対して実行 | CLI-R2 | DR-0020 §2 |
| 省略可能な引数だけ / 引数を取らない leaf: `list` / `version` / `config path` / `config show` / `web daemon supervise\|list\|status\|log` / `web service register\|unregister\|start\|stop\|restart\|status\|log` / `web passkey list` / `web session list` | 実行 | daemon 体系ならそのまま。好みを字義どおり取るなら help | CLI-R1 |
| エラー時の案内 | どの階層でも ``Run `hyoui --help` for usage.`` (トップ help を指す) | そのレベルの help を指す (``Run `hyoui web daemon --help` for usage.``) | 好み (階層ごとの help) |
| 2 段目以下の未知語 | screen / lock / record / config は `did you mean` を出す。web / web daemon / web service / web passkey / web session は出さない | 全階層で同じ suggest | 一貫性 |

補足:

- 中での引数なし実行で副作用が大きいもの: `kill` (自分の終了)、`detach` (自分の窓を含む全 client の切断)、`upgrade` (自 daemon の self-exec)、`lock acquire` (lock を取って block)。`parse_kill` のコメントは「kill の self default は `exit` 相当として許容」とする
- 依頼文にあった「`web daemon` 等の 2 段目で unknown subcommand `web daemon` としてトップ help に落ちる」は、現ソースに経路が無い。`parse_web_daemon` は引数なしで `HelpTopic::WebDaemon` を返し、`hyoui web daemon --help` の実測も web daemon の help を出す (brew の 0.16.4 9052e6c も同じ)。トップ help に落ちるのは 1 段目の未知語 (例: `hyoui daemon`) だけで、その時は ``error: unknown subcommand `daemon` `` に続けてトップ help を stderr に出し exit 2 (ソース判定)。引数なしの実測は classifier で止まったので未確認。再現した時の argv が分かれば追記する

### 観点 2: 語彙の統一

| 概念 | 現状 | 案 | 根拠 |
|---|---|---|---|
| session を終わらせる | `kill` (signal を送る、既定 SIGTERM、`--no-terminate` で畳まない) | `kill` のまま | kill(1) と同じ「signal を送る」動詞。web の `stop` は監督者に desired state (`enabled`) を書かせる操作で別物 (daemon 体系の stop の定義) |
| web の unit / 監督者 / 録画を止める | `web daemon stop` / `web service stop` / `record stop` | そのまま | daemon 体系、start と対 |
| 登録する | `web daemon add` / `web passkey add` / `web service register` | そのまま | add (unit の登録簿) と register (OS への登録) は daemon 体系が定義する別の操作 |
| 登録を外す・取り消す | `web daemon remove` / `web passkey remove` / `web session remove` | `remove` で揃っている。passkey / session は help が「Revoke」と書く操作なので `revoke` も候補 | interface-wording (利用者が認識する操作名) |
| lock を外す | `lock release` と `unlock` (完全な alias) | どちらか 1 つ (CLI-R3) | 同じ操作の口を 2 つ持たない。DR-0010 §1 は lock family (`lock acquire` / `release` / `tx`) への統合を決めている |
| 一覧 / 状態 / foreground 起動 | `list`・`status`・`run` がトップと web で同じ動詞 | そのまま | daemon 体系 |
| 「session」 | PTY session (`list` / `attach` 等) と web の認証 session (`web session`)。`web session --help` は「not the PTY sessions that `hyoui list` prints」と断り書きを要している | `web session` を別名にする (CLI-R7) | 同じ語が別物を指す |
| 「daemon」 | session の daemon (トップ help の「List daemon sessions」「Attach to an existing daemon session」、`upgrade`) と web の unit 群 (`web daemon`) | 利用者向けの説明では session 側を「session」と呼び、「daemon」は web の unit 群に限る | 同上 |
| 出力形式の名前 | `list` plain\|jsonl、`status` plain\|json、`record list` table\|jsonl、`record start` jsonl\|raw、`screen dump` ansi\|binary\|cbor\|text/plain、`screen snapshot` cbor\|json。web 系と `version` は JSON 固定で `--format` 無し | 表の名前を 1 つにし (plain か table)、1 件は json・複数は jsonl と規約にする。screen 系の名前は 05-28 issue | CLI-R6 |
| 旧名の alias | `tail --strip` / `tail --last` | 消す (CLI-R14) | 補完に無い面を残さない (3 者同期)、v1.0 前は breaking 可 |
| 廃止の案内 | `kill --signum` / `run --mode` / `run --on-parent-suspend` / `attach --exclusive` / `--detach-others` が専用エラー | 消して unknown option に倒すか、残すか (CLI-R14) | |
| `--timeout` | run: 全体の上限 / input: spec ごと / wait: 全体 / lock acquire: 取得まで / screen dump・snapshot: 応答待ち (help に「現状 no-op」と注記) | 名前ごとの意味をそろえる必要は無いが、no-op の option は消すか配線する | DR-0014 (効かない option を露出しない) |
| short option | `-h` (全階層)、`-V` (トップのみ)、`kill -N` / `-NAME` | `kill -9` 等は kill(1) の慣習として残す候補。`-h` / `-V` も慣習 | 好み (指示なく short を足さない)。既存のものの扱いは CLI-R11 に含める |

### 観点 3: 階層の深さと群分け

トップ 19 個 (+ 予約 `tx`) を群で見るとこうなる:

- session の生成と接続: `run` / `attach` / `detach` / `list` / `status` / `kill` / `set` / `upgrade`
- 自動操作と観測: `input` / `wait` / `screen {dump,snapshot}` / `tail` / `lock {acquire,release}` / `unlock` / `record {start,stop,list}`
- 運用: `web {daemon,service,passkey,session}` / `config {path,show}` / `version` / `completion`

最深は `hyoui web daemon run` (トップから 3 段) で、web だけ。session 系は 1〜2 段。web 配下に置くことは DR-0034 で裁定済み (SVC-Q1=a)。

| 対象 | 現状 | 案 | 根拠 |
|---|---|---|---|
| `unlock` | トップにある `lock release` の alias | 一本化 (CLI-R3) | DR-0010 §1 |
| `tx` | トップに予約 (help の RESERVED 節に出る、補完には出ない)。DR-0006 §7 と cli.rs は `hyoui tx`、DR-0006 §8.8 の例と DR-0010 §1 は `hyoui lock tx` | 置き場を決めるか予約を外す (CLI-R4) | DR 間で置き場が食い違っている |
| `version` | トップ。中身は web の監督者と unit の版だけ (JSON)。session daemon の版は `list` の VERSION 列と `status` の `daemon-version` | 範囲を決める (CLI-R8) | daemon 体系の `version` は「置いてある版と走っている版を並べる」。hyoui では走っているものが web の unit と session daemon の 2 系統ある |
| `upgrade` | トップ。session daemon 1 つを self-exec で入れ替える。`--all` は無い | CLI-R9 | 名前が「hyoui 自身の更新」とも読める。新しい版を入れた後に全 session を入れ替える口が無い |
| `set` | 1 key (`on-child-suspend`) | 観点 4 | |
| `screen` | dump / snapshot | 05-28 issue | |
| session 系を `hyoui session <verb>` にまとめる案 | 無い | 作らない (CLI-R18) | 最頻の操作に毎回名詞を打たせる。`web session` とも衝突する |

### 観点 4: `set` と tag

現状:

- `hyoui set <session> <key>=<value>`。key は `on-child-suspend` だけで、wire は `set-v1`。DR-0032 は「`hyoui set` / `status` の値域と protocol は変えない」と決めている。DR-0020 の Consequences は `until` / `timeout` / `idle-timeout` / `scrollback-rows` を set の key に足す構想を記録している
- tag は `run --tag` で付け、`list --tag` で絞り、`status` / `list` に出る。run の help は「起動後は変えられない」と書く。DR-0041 の「未決」に「tag を起動後に変える手段 (`hyoui set` 等) を持つか」が残っている
- kawaz 裁定 (2026-10-14): tag は起動後に付け替えられる、key で一意 (上書き)、削除は key を指定する

前提になる制約: `run --tag KEY` は「value が空の tag」の略記 (DR-0041 決定 1)。削除を `KEY` 単独で書くとこの略記と衝突するので、削除には明示の option が要る。

| 案 | 形 | 長所 | 短所 |
|---|---|---|---|
| (a) 専用コマンド | `hyoui tag <session> <key>=<value>... [--unset <key>]...` | 値の書き方が `run --tag` / `list --tag` と同じで、`KEY` 単独 = 空 value の略記もそのまま使える。削除は `--unset` で略記と衝突しない。set を runtime 設定に閉じたまま (DR-0032 の値域を広げない) | トップが 1 つ増える (実装 20 個)。session を変更する口が set と tag の 2 つになる |
| (b) set の key | `hyoui set <session> tag.<key>=<value>` | set の key=value にそのまま載り、DR-0020 の「set の key 拡張」構想と同じ形 | 削除の文法が set に無い (`tag.k=` は空 value)。`--unset` を set に足すと `on-child-suspend` の unset (既定に戻す) の意味も決める必要が出る。設定と分類が同じ key 空間に混ざる |
| (c) set の flag | `hyoui set <session> --tag <key>=<value> --unset-tag <key>` | `--tag` の名前が run / list と同じ | set の中で位置引数の key=value と flag の 2 文法が並ぶ。set が runtime 設定と分類の両方を持つ |

推し候補: (a) (統括推しと同じ)。削除の key 指定と `run --tag KEY` の略記が衝突しない形を自然に書けるのが (a) だけで、set の値域を広げずに済む。悪い面はトップが増えること。

付随して決まること:

- `hyoui tag` を引数なしで打った時は help (観点 1)。tag の表示は `status` / `list` に任せ、`hyoui tag <session>` だけで表示する口は持たない、で足りるか
- daemon への tag 更新要求は新しい control message か `set-v1` の拡張になる。どちらにしても DR-0014 の self-check で必然性を書く (webui-terminal-app-rework issue も「tag 更新要求の必然性を DR に書く」としている)
- `hyoui.` で始まる key の予約 (DR-0041) は付け替えにも同じく効かせる

### 観点 5: ENVIRONMENT 節の集約

方針 (kawaz 2026-10-14): 各 `--help` から環境変数の本文を外し、`docs/MANUAL.md` の Environment 節 1 か所にまとめ、help は参照 1 行だけにする。

| 対象 | 現状 | 案 | 根拠 |
|---|---|---|---|
| 本文の置き場 | help 8 本の ENVIRONMENT 節 + 散文に分散。MANUAL に節が無い | MANUAL.md / MANUAL-ja.md に Environment 節を足し、1.3 の表の「利用者向け」「検証用」を書く。内部の変数は「ここに無い `HYOUI_` の変数は内部用」の 1 行で済ませる | 好み (環境変数の本文は MANUAL に集約) |
| help 側 | 節がある help と無い help が混在、同じ変数の文面が 3 種類 | 全 help に参照 1 行 (CLI-R13 で形を決める) | 同上 |
| option の説明の中の env 名 | `--max-file-bytes` の「env HYOUI_MAX_FILE_BYTES より優先」、`--poll-interval` の「`HYOUI_WAIT_POLL_MS` でも override 可」等 | option の優先順位の説明として残す (変数の説明は MANUAL) | option の振る舞いの一部 |
| web service register の焼き込み | 振る舞いとして help 本文で説明 | そのまま | env の説明でなく、そのコマンドが何をするかの説明 |
| `DURATION FORMAT` | 5 本に 4 種類の文面 | env と同じく MANUAL に 1 か所、help は参照 1 行 | 同じ型の重複 |

参照 1 行の形の選択肢は CLI-R13。MANUAL は英日ペアなので、just push の翻訳ペア鮮度検証に乗る。

### 観点 6: help の節構成

好みの順は「サブコマンド一覧 → そのレベルの option → global option → 環境変数への参照 1 行」。daemon 体系の雛形は 1 行目が `<tool> — 説明`、次に `USAGE`。

| 対象 | 現状 | 案 | 根拠 |
|---|---|---|---|
| 書式 | A (23 本) と B (32 本) の 2 系統 | 1 つに: `hyoui X — 説明` / `USAGE:` / 説明 1 段落 / `SUBCOMMANDS:` (子を持つレベルだけ) / `OPTIONS:` / `GLOBAL OPTIONS:` / `ENVIRONMENT:` (参照 1 行) | 好み、daemon 体系の雛形 |
| global option | トップにしか節が無い | 全階層に `GLOBAL OPTIONS:` (今は `--help` だけ、トップは `--version` も) | 好み |
| EXIT の見出し | `EXIT STATUS` 2 / `EXIT CODE` 17 / `Exit codes` 2 / 無し 35 | `EXIT STATUS` に統一し、exit を持つ全コマンドに置く (web 系の「errors are JSON on stderr with a non-zero exit」も節にする) | man の節名。run / attach が既に使っている |
| `--help` の表記 | `-h, --help` と `--help, -h`、字下げ 4 と 2 | 1 つに | |
| DR 番号 | 25 本の help に出る | help から外す (DR は設計の記録で、利用者の語彙ではない) | interface-wording |
| 言語 | 書式 A は英日混在、書式 B は英語。parser のエラー文も混在 | 1 言語に (CLI-R10) | |
| 4 節以外 (EXAMPLES / OUTPUT の列定義 / RELATED / SIGNAL SPEC / CTRL+Z GUARD / BEHAVIOR / SECURITY NOTE / 2 軸モデル 等) | help に直書き。run 98 行、kill 89 行、input 71 行、attach 70 行 | CLI-R12 | |

### 観点 7: completion の 3 者同期

現状の齟齬は 1.2 のとおり。option 名と enum の値は揃っていて、ずれているのは次の 4 点。

| 齟齬 | 現状 | 案 | 根拠 |
|---|---|---|---|
| 旧名の alias | `tail --strip` / `--last` を parser だけが受ける | alias を消す (CLI-R14) | 好み (3 者同期) |
| 位置引数の候補 | session id / unit 名 / passkey の sub / web session の id を 3 shell とも出さない | 動的補完を足す (CLI-R16) | 好み (過不足のないコンテキスト対応補完) |
| サブコマンドの説明文 | トップ help・zsh・fish で別文面、zsh / fish に DR 番号 | 1 か所の定義から出す (CLI-R15) | 好み (3 者同期) |
| help ↔ 補完の同期の test | 補完 → parser の向きと名前の存在だけを test が見る | help の OPTIONS / SUBCOMMANDS と補完を突き合わせる仕組み (CLI-R15) | 片方向だけの同期は drift する |

### 観点 8: DR-0006 との関係

DR-0006 は CLI の形 (flag 名・既定値・exit code) と意味論 (architecture、透明性 invariant、lock の semantics、input spec のカタログ、wait の match の意味) を 1 本に持ち、形の側が実装からずれている:

| DR-0006 の箇所 | 書いてあること | 実装 |
|---|---|---|
| 冒頭 | CLI Design は `~/.claude-personal/rules/cli-design-preferences.md` と整合させる | 参照先は reference `cli-design-preferences` に移っている |
| §3 | `--exclusive` で起動時に rw 1 個までを宣言 | run に無い。attach は parse 段で拒否 (CLI-Q1) |
| §5 | `--read-only` / `--no-leader` / `--detach-others` | `--mode=rw\|ro\|rw-no-leader`。`--detach-others` は拒否 |
| §6 | `--window-size=leader\|manual:WxH\|...` | 無い |
| §7 | `hyoui tx` / `hyoui lock <name> --timeout-* --mode` / `hyoui unlock --token\|--force` | `lock acquire --mode --timeout` / `lock release` / `unlock`。tx の置き場は DR-0010 §1 と食い違う |
| §8.6 / §8.7 | `--spool` / `--max-size` / `--line-ending` / `--trailing-newline` | 無い。`--max-file-bytes` がある |
| §9.6 | wait の exit code: 0 match / 1 timeout / 2 子 exit / 3+ error、`--print` / `--process-bound` | help: 0 match / 1 timeout・I/O / 2 cancelled・usage / 3 regex・daemon error。`--print` 等は無い |
| §10 | `screen dump --format=ansi\|binary\|json\|cbor`、`screen snapshot` の既定は json | dump は ansi / binary / cbor / text/plain (json 無し)、snapshot の既定は cbor |
| §11 | `tail` は `--follow` が既定、`--strip` / `--last` / `--newline-convert` | 省略時は 1 回流して終わる、`--strip-ansi` / `--last-bytes` (旧名は alias)、`--newline-convert` は無い |
| §12 | 環境変数は `HYOUI_LOCK_TOKEN` だけ | 1.3 の全集合 |

見立て:

- 新しい DR「CLI surface の規約」を起こし、引数なし・help の節構成・語彙・env と duration の MANUAL 集約・completion の同期の仕組みを決める。DR-0006 は CLI の形の記述 (上表) をこの DR で Supersede (部分) し、意味論 (§1〜§2、§4、§7 の lock semantics、§8 の spec カタログ、§9 の match の意味) の DR として残す
- DR-0004 の「引数なし / 不明サブコマンドは `--help`」(トップだけの規定) は新 DR に吸収して全階層の規定にする (DR-0004 の部分 Supersede)
- CLI-R2 で C を選ぶ場合は DR-0020 §2 を、CLI-R3 / R4 の結論によっては DR-0010 §1 を改める
- 観点 4 の結論で DR-0041 の「未決」(tag を起動後に変える手段) が決着する。tag の付け替えは protocol に関わるので、新 DR とは別の DR (または DR-0041 の追記) にする方が境界が明確

## 3. kawaz に裁定してほしい点

各項目の「推し」は本 issue を書いた側の推し候補。統括推しが別にあるものは明記した。

- **CLI-R1 引数なしの規範**: (a) daemon 体系の読み (子を持つレベルと必須引数のある leaf は help、省略可能な引数だけの leaf は実行) / (b) 好みの字義 (引数を取る leaf はすべて help)。推し (a)。(b) だと `list` や `web daemon status` を引数なしで実行する手段が無くなる。悪い面: `web service register` のように副作用の大きい命令も引数なしで走る
- **CLI-R2 中からの引数なし実行 (self-session)**: A. 外のエラーを help に直すだけ、中では自 session に実行 (DR-0020 のまま) / B. 引数ゼロは中でも常に help、他の引数がある時だけ session 省略を自 session に解決する (DR-0020 §2 は「引数ゼロ以外」に狭まる) / C. 自 session は常に明示 (予約語 `self` 等)、省略時の解決をやめる (DR-0020 §2 を Supersede)。推し B。引数なしで `kill` / `detach` / `upgrade` / `lock acquire` が自分に効く事故が消え、好みの「引数なしは help」とも揃う。悪い面: 中で `hyoui status` と打つ習慣が help になる (引数ゼロで自 session を指す口として予約語 `self` を足すかは同時に決める。id は UUID だけなので `self` は衝突しない)。`hyoui kill --wait` のように option があれば中では自分に効くのは残る
- **CLI-R3 lock の解放口**: (a) `lock release` に一本化して `unlock` を消す / (b) `unlock` に一本化して `lock release` を消す / (c) 両方残す。推し (a)。DR-0010 §1 の lock family に合い、`hyoui lock --help` で取得と解放が並ぶ。悪い面: 打つ字数が増える
- **CLI-R4 `tx` の置き場**: (a) `hyoui lock tx` / (b) トップの `hyoui tx` / (c) 予約を外し help から消す (置き場は実装する時の DR で決める)。推し (c)。実装されていない予約が help に出続けるのは発見性のノイズ。今決めるなら (a) (DR-0006 §8.8 の例と DR-0010 §1 に合う)
- **CLI-R5 tag の付け替えの置き場**: (a) 専用 `hyoui tag <session> k=v... [--unset k]...` / (b) `hyoui set <session> tag.k=v` / (c) `hyoui set <session> --tag k=v --unset-tag k`。推し (a) (統括推しも (a))。理由と悪い面は観点 4
- **CLI-R6 出力形式の規約**: daemon 体系は「help 以外の出力は JSON / JSONL」。hyoui は web 系と `version` だけが従い、session 系は人が読む表が既定で `--format` を持つ。(a) この境界を規約として DR に書き、名前だけ揃える (表は 1 つの名前、1 件 json / 複数 jsonl) / (b) session 系も JSON 既定に寄せる / (c) web 系にも `--format` を足す。推し (a)。`list` / `status` は端末で人が読むのが主用途。悪い面: daemon 体系からの逸脱を DR に書く必要があり、他の kawaz 製 CLI への持ち帰りでは例外になる
- **CLI-R7 `web session` の改名**: (a) そのまま / (b) 改名する (候補: `web login` / `web token` / `web signin`)。推し (b)。help が断り書きを要している時点で語が衝突している。名前の選択は kawaz
- **CLI-R8 `version` の範囲**: (a) web の監督者と unit だけ (今のまま) / (b) session daemon の版も並べる (`running` / `on_disk` / `restart_needed`) / (c) `hyoui web version` に移す。推し (b)。走っている hyoui の版を並べるという daemon 体系の問いに、session daemon も答える対象として入る。悪い面: session の数だけ問い合わせるので遅くなり、出力が大きくなる
- **CLI-R9 `upgrade`**: (a) そのまま / (b) `--all` を足す / (c) 名前を変える (例: 「session daemon の入れ替え」と読める名前へ)。推し (b)、名前は (a)。新しい版を入れた後に全 session を入れ替える口が無い。CLI-R8 (b) の `restart_needed` と対になる
- **CLI-R10 help とエラー文の言語**: (a) 英語に統一 / (b) 日本語に統一 / (c) 今のまま。推し (a)。MANUAL.md が英語の正本で、書式 B の 32 本は既に英語。悪い面: 書式 A の 23 本と parser のエラー文の書き直しが要る
- **CLI-R11 help の書式**: 観点 6 の雛形 (`hyoui X — 説明` / `USAGE:` / 説明 / `SUBCOMMANDS:` / `OPTIONS:` / `GLOBAL OPTIONS:` / `ENVIRONMENT:`) に揃える、`EXIT STATUS` に統一、DR 番号を外す、`--help` の表記を 1 つにする。推しはこの全部。既存の short option (`-h` / `-V` / `kill -N`) は慣習として残すかも合わせて決める (推し: 残す)
- **CLI-R12 help に残す節**: (a) コマンドの契約 (usage / option / exit status) は help、解説 (EXAMPLES / OUTPUT の列定義 / 背景の説明 / DURATION FORMAT / SIGNAL SPEC 等) は MANUAL / (b) 今のまま help に全部。推し (a)。境界は「版ごとに変わる契約は help、使い方の解説は MANUAL」。悪い面: 端末だけで例を引けなくなる (EXAMPLES を help に残すかは個別に決めてもよい)
- **CLI-R13 環境変数の参照 1 行の形**: (i) 全 help で同じ 1 行 (`ENVIRONMENT: see https://github.com/kawaz/hyoui/blob/main/docs/MANUAL.md#environment`。run の `--pty-stdin` が既に docs/manual/pty-stdin.md を URL で指している) / (ii) そのコマンドに効く変数名を並べて URL (`ENVIRONMENT: HYOUI_STATE_DIR, HYOUI_LOCK_TOKEN — see ...`) / (iii) `hyoui help environment` のような手元で読める口を足してそれを指す。推し (i)。好みの「参照 1 行」に一番近く、ずれる面が増えない。悪い面: オフラインで読めず、main の MANUAL と手元の版がずれうる。(ii) はコマンドごとの名前リストという同期面を増やす (今のずれと同じ型)。(iii) はサブコマンドか help topic が増え、MANUAL と 2 重に持つ。あわせて `HYOUI_ALLOW_CORE` (内部デバッグ専用) と `XDG_RUNTIME_DIR` (移行期の警告だけ) を MANUAL に載せるか (推し: 載せない、内部用の 1 行に含める)
- **CLI-R14 旧名の alias と廃止の案内**: (a) `tail --strip` / `--last` と廃止案内 (`kill --signum` / `run --mode` / `run --on-parent-suspend` / `attach --exclusive` / `--detach-others`) を消す / (b) 残す。推し (a)。v1.0 前は breaking 可で、3 者同期の外にある面が消える。悪い面: 古い script が unknown option で落ち、移行先の案内も消える (エラー文の migration hint は no-historical-noise でも許容されているので、(b) も規約上は可)
- **CLI-R15 help と補完の同期の仕組み**: (a) サブコマンド名・説明文・option 表・enum 値を 1 か所の定義に持ち、help の SUBCOMMANDS / OPTIONS と 3 shell をそこから出す / (b) 手書きのまま、help ↔ 補完を突き合わせる test を足す。推し (a)。説明文が 4 か所で別文面になっている原因 (手書きの写しが 4 つ) を構造で消せる。悪い面: shell ごとの文脈依存の補完 (排他 option、input の spec prefix、`file:` の path) は定義表で表しきれず、生成器に例外が積もる。DR-0004 の「clap を使わず自前 parse」とは両立する (生成元は自前の表)
- **CLI-R16 位置引数の動的補完** (session id / unit 名 / passkey の sub / web session の id): (a) 足す / (b) 足さない。推し (a)。悪い面: 補完のたびに `hyoui list` 相当が走ると、no-response の session があると 5 秒待つ。session id は socket file 名を読むだけにする等、問い合わせをしない実装方針が要る
- **CLI-R17 DR の扱い**: 新 DR「CLI surface の規約」を起こし、DR-0006 の CLI の形の記述と DR-0004 の引数なし規定を部分 Supersede する (観点 8)。DR-0006 を Update 節で書き直す案もあるが、DR-0006 は既に Archive 付きで 774 行あり、意味論と形を分ける方が次にずれた時に直す場所が明確。推し: 新 DR
- **CLI-R18 session 系の群**: `hyoui session <verb>` にまとめるか。推し: まとめない (観点 3)

## 4. 整理と独立に直せる help / docs の誤り

裁定を待たずに直せる、現在の help / docs が実装と食い違っている箇所:

- `attach --help` の SESSION SELECTOR と EXAMPLES が「`attach 1` は session_id="1"」「数字も valid な session-id」と書く。DR-0041 で id は UUID だけなので、`kill --help` の「`kill 1` の数字も id として読み、UUID でないのでエラー」が正しい
- `record stop --help` の EXAMPLES が `my-session` を id に使っている (UUID だけ)
- `kill` の session 指定なしエラーの例に `hyoui kill 1` がある (UUID でないのでエラーになる例)
- `lock acquire --help` の BEHAVIOR が `/tmp/lockpid` の固定パスを使う例を載せ、「将来 `hyoui tx`」と書く
- トップ help の `tail` 行の「(--follow で継続)」と `tx` 行の「将来 protocol 拡張用に予約」が、英語の一覧の中で日本語
- `docs/MANUAL.md` と `docs/MANUAL-ja.md` で「11.」の節が 2 つある (子の停止と Ctrl+Z / ブラウザから操作)。MANUAL.md の冒頭は「covers v0.9.x」
- DR-0004 の Related が `~/.claude/rules/cli-design-preferences.md`、DR-0006 冒頭が `~/.claude-personal/rules/cli-design-preferences.md` を指す (reference `cli-design-preferences` に移っている)
