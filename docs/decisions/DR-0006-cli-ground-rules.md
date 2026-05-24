# DR-0006: CLI 設計の地盤ルール — 動作モデル、自動操作 API、排他制御

- Status: Active
- Date: 2026-05-26
- Related: DR-0001 (jobcontrol 2 軸), DR-0004 (CLI subcommand 採用), DR-0005 (外側自動操作主軸), DR-0007 (MVP scope)

## Context

[[DR-0005]] で hyoui の方向性 (外側からの自動操作主軸、TUI multiplexer ではない) を確定した。
本 DR ではその思想を具体的な CLI 形・動作モデルに落とす。対象範囲:

- daemon ライフサイクル (起動・detach・attach・終了)
- socket の物理配置と名前解決
- 複数 attach 時の動作 (rw/ro、leader、winsize)
- 自動操作 API (send / keys / paste / tail / wait)
- 排他制御 (lock / tx)
- 透明性 invariant (in-band escape の不在)

## Decision

### 1. Architecture: screen 型 (1 daemon 1 socket 1 子)

- `hyoui run --name X -- cmd` = `<XDG>/hyoui/X.sock` を持つ独立 daemon が 1 つ起動、子 = cmd
- tmux 型 (1 server 多 session) は採用しない (本 DR 末尾の Rejected alternatives 参照)
- `hyoui list` は socket dir 走査 (registry ファイル持たない、ファイルシステムが source of truth)
- daemon は子 exit で即終了、全 client detach 中でも生存

### 2. Socket 配置

```
Linux:    $XDG_RUNTIME_DIR/hyoui/<name>.sock
macOS:    $TMPDIR/hyoui-$UID/<name>.sock
override: --socket /any/path.sock
```

- dir mode 0700, sock mode 0600
- 起動時の stale socket: ping → 応答なければ自動削除、`--force` で奪取は不要 (= 別 owner の sock は触らない)

### 3. Name と起動形

- default name = pid、`--name` 明示で override、衝突 = error
- 起動 = daemon + 自動 attach (foreground)、`--detached` で初期 detach 起動
- `--detached` 時は stdout に name 1 行印字 (`name=$(hyoui run --detached -- cmd)` で取れる)
- `--exclusive` で起動時に「rw 1 個まで」を宣言 (= 後続 attach は ro or 蹴る)

### 4. detach 操作 (透明性)

- **in-band escape は一切なし** (DR-0005 の透明性思想)
- detach は **out-of-band のみ**: `hyoui detach <name> [--all | --others]`
- prefix キーバインドは設定オプションすら存在させない
- nest 起動 (hyoui の中で hyoui) は **許可 + 1 行 warn**、`$HYOUI_NAME`/`$HYOUI_SOCK` を子 env に injection

### 5. 複数 attach: rw/ro 区別 + leader 内部メカニズム

- default attach = rw + leader 取得、`--read-only` で ro、`--no-leader` で rw だが leader 取らない
- **複数 rw 可** (ユーザ責務)、`--detach-others` で attach 時奪取
- **leader は内部メカニズム** (winsize 主体): cascade policy `latest` default
- leader は rw 必須 (ro 化と leader 喪失は連動)、ro は winsize 計算除外
- **MVP では leader CLI を露出しない** (`hyoui leader take/give/show` は v0.3.0 で解放)
- 「最後に attach した rw client が leader」「leader detach/exit → 残り rw のうち latest に自動移譲」

### 6. Winsize mode

```
--window-size=leader        # default、leader のサイズに従う (= MVP 主軸)
--window-size=manual:WxH    # 固定 (テスト・スクショ・録画用途)
--window-size=smallest      # 全 rw client の min (後付け、v0.2.0 以降)
--window-size=largest       # 全 rw client の max (後付け)
```

### 7. Lock + tx: 自動操作排他

CLI:

```bash
hyoui tx <name> [--timeout-* ...] -- cmd args...
  # 起動時 lock 取得 → 子 env に HYOUI_LOCK_TOKEN 注入 → 子 exit で自動 unlock

hyoui lock <name> [--timeout-* ...] [--mode wait|fail]
hyoui unlock <name> [--token T | --force]
```

Timeout 3 種共通フラグ (OR 評価、どれか発火で解放):

| flag | 意味 |
|---|---|
| `--timeout-absolute DUR` | lock 取得後 N で強制解放 |
| `--timeout-idle DUR` | 最後の操作から N 無操作で解放 |
| `--process-bound` | 紐付けプロセス exit で解放 (tx の子) |

Default:

| コマンド | absolute | idle | process |
|---|---|---|---|
| tx | 5min (safety net) | 30s | ⭕ (子プロセス) |
| lock (低レベル) | 5min | 30s | ❌ |
| send/keys/paste/wait 単発 (lock 未取得時) | 内部 short lock | — | — |

Lock semantics:

- lock owner ⇒ leader 強制昇格 (winsize 主体)、他 rw は ro 一時降格
- 終了で leader cascade policy 発動 (元 leader 残存ならそこに戻す)
- 全 send/keys/paste/wait は `--token T` 受け、未指定なら env `HYOUI_LOCK_TOKEN` 自動使用
- nested lock: 同 token なら no-op 成功 (refcount)、別 owner は wait/fail
- `unlock --force` で他 owner の lock も剥がせる (救済用、stderr warn)
- 他 client の扱いは **強制 ro** (バッファ・ブロックは将来 opt-in)

### 8. send / keys / paste 棲み分け

| | 用途 | 入力源 | 用例 |
|---|---|---|---|
| `send` | raw bytes (binary 安全、制御文字込み) | stdin / `--file` / argv | binary stream, 古い shell への制御文字 |
| `keys` | symbolic + text mixing | argv (key spec list) | 自動操作の核 |
| `paste` | bracketed paste で囲んだテキスト塊 | `--text` / `--file` / stdin | 大きなテキスト・コード貼り付け |

### 9. keys の spec syntax

```
text:TEXT            # raw 文字列
key:STRICT_KEY       # strict (alias 不可、case-sensitive、正規系のみ)
wait:DUR             # 固定 sleep
wait-idle:DUR        # idle 待ち
(prefix なし)        # 緩い key 表記 (alias OK、modifier case-insensitive)
```

複雑な wait (pattern/timeout) は **専用 `hyoui wait` コマンドに分離** (lock 環境変数で atomic 性継承)。

Modifier alias:

| modifier | alias |
|---|---|
| Ctrl | `C`, `Ctrl`, `ctrl`, `^`, `⌃` |
| Alt | `A`, `Alt`, `alt`, `M`, `Meta`, `meta`, `⎇`, `⌥` |
| Shift | `S`, `Shift`, `shift`, `⇧` |
| Super | `Super`, `⌘`, `Cmd`, `cmd`, `Win`, `❖` |

Meta default は Alt (Mac Option)、`--meta-is-super` で切替可。

主要キー alias: `Enter/↩/⏎`, `Tab/⇥`, `Esc/⎋`, `Backspace/⌫`, `Delete/⌦`, `↑↓←→`, `Home/⤒`, `End/⤓`, `PageUp/⇞`, `PageDown/⇟` 等。

Case-sensitivity:
- modifier 部 大文字小文字無視
- modifier ありの key は case-insensitive (= `Ctrl+a` ≡ `Ctrl+A`、Shift 明示が必要なら `Ctrl+Shift+A`)
- modifier なしの単キーは case-sensitive (`a` と `A` は別、`A` は Shift+a 相当を送信)

正規系 (内部 dump、`hyoui keys --dry-run` で確認):
```
Modifiers 順序: Ctrl, Alt, Shift, Super
Format: "Ctrl+Shift+Enter"
```

### 10. paste API

```bash
hyoui paste <name> [入力源] [spool/size] [オプション]
```

入力源 (排他):

| flag | 意味 |
|---|---|
| `--text TEXT` | 引数 (size 確定) |
| `--file PATH` | ファイル (- で stdin、省略時も stdin) |

Spool (排他値):

```
--spool=memory        # default、RAM に貯める
--spool=tmpfile       # auto path tempfile
--spool=<path>        # 相対/絶対パス、予約語衝突は ./xxx で回避
--spool=none          # stream (貯めない)
```

`--max-size SIZE` (default 16MB、0 で無制限):
- spool=確定モード: 拒否閾値 (超過 = 1 byte も送らず error)
- spool=none: 切り捨て位置 (この byte 数送ったら stop)

Atomicity:

| Mode | Atomic | read error |
|---|---|---|
| spool=memory/tmpfile/file | ⭕ | spool 削除 + error、子に 1 byte も送らない |
| spool=none | ❌ | 既送分は子に確定、daemon は best-effort で `ESC[201~` 補完 |

Bracketed paste:
- 自動検出 (子が `ESC[?2004h` を出してたら bracketed mode と判定)
- `--bracketed`/`--no-bracketed` で override
- daemon は **in-flight paste state** を持ち、異常終了 path で `ESC[201~` を best-effort 送信 (子 hang を防ぐ)

その他:
- `--newline=preserve|lf|crlf` (default `preserve`、bytes 透過。バイナリ判定不能なため自動変換 off が安全側)
- `--add-newline` 末尾改行追加
- `--chunk SIZE` (default 4096)、`--chunk-delay DUR` (default 0)
- file spool: `--spool-overwrite` `--spool-append` `--spool-delete-after-send`

サイズ超過時 error message で誘導 4 つ提示:
```
--max-size SIZE       Raise the limit
--spool=tmpfile       Spool to disk (still bounded)
--spool=<path>        Spool to specific file (debug/inspection)
--spool=none          Stream without buffering (incomplete on truncation)
```

### 11. wait L0 (MVP)

```bash
hyoui wait <name> [--idle DUR | --text S | --pattern R | --then-idle DUR] [--timeout DUR]
```

- L0 = 出力 stream に対する text/regex match + idle 待ち
- L1 (画面 rect 指定 + pty screen emulator)、L2 (named area + JSON 述語) は後付け
- `--then-idle DUR` (pattern 出現後の安定待ち) は TUI 自動化の主用途

詳細仕様 (match スコープ・buffer・stdout 印字) は MVP 実装フェーズで詰める (本 DR の射程外、journal 残論点参照)。

### 12. 環境変数

| env | 内容 |
|---|---|
| `HYOUI_NAME` | nest 起動検知用 (子に注入) |
| `HYOUI_SOCK` | nest 起動時の親 daemon socket path (子に注入) |
| `HYOUI_LOCK_TOKEN` | lock 取得 token (tx の子に注入、全自動操作系コマンドが自動継承) |

## Rejected alternatives

### tmux 型 (1 server 多 session)

- 1 hyoui server に複数 session を吊る形
- 利点: window/pane 概念を後付けしやすい、`list-clients` `kill-server` が綺麗
- 欠点: server lifecycle 管理が必要、複雑度爆増、思想 (DR-0005) と不一致 (= TUI multiplexer 領域)
- → screen 型を採用

### prefix キー方式 (tmux/screen 流の in-band escape)

- ユーザが prefix を覚える必要、透明性に反する
- 子プロセスへの入力に escape を挟む = 完全透過ではない
- DR-0005 の思想 (透明性最優先) に明確に反する

### 全 attach 排他 (rw 1 個まで固定)

- 構造的に安全だが、ペアプロや「複数端末で同じ shell」ユースケースを潰す
- 競合制御は lock/tx で意図明示的に行う方式に振った

### keys 内に複雑な wait 述語を全部詰める

- `wait-text:30s:Welcome` のような構文は `:` エスケープが脆弱
- `wait-pattern:regex` で regex 中の `:` がしばしば出る
- 「自動操作 DSL」を keys 1 コマンドに集約すると、shell の薄い再発明になる
- → keys 内は `text:/key:/wait:/wait-idle:` の 4 種に絞り、複雑系は `hyoui wait` に分離

### `--file -` への誘導 (stdin size 不明時)

- stdin と `--file -` は本質的に同じ (どちらも size 不明)、誘導しても解決しない
- 代わりに spool mode を統一 (`--spool=memory|tmpfile|<path>|none`)、`--max-size` の意味を spool mode 別に定義
- ユーザの size 不明承諾は `--spool=none` or `--max-size=0` で明示

### `--newline=none`

- `none` が「改行 0 個」(= 削除) と誤読されるリスク
- 「正規化しない」を `--newline=preserve` で表現、誤解の余地を消す

### leader CLI を MVP に露出

- ユーザ (kawaz) の主用途は 1 人運用、leader の手動操作は不要
- 将来 HTTP gateway 展開時 (ローカル端末 + ブラウザ端末の同時 attach) に解放する余地を残す形 (DR-0007)

## Consequences

### 実装への波及

- daemon は in-flight paste state を持つ必要 (best-effort end 保証のため Drop impl/signal handler)
- protocol message: handshake/resize/signal forward/data/detach/leader change/status query/lock acquire/release/mode change (broadcast)
- protocol design は transport (Unix socket / TCP / WebSocket) から独立した wire format
- attach client を library として export (v0.2.0 で `hyoui-serve` から呼ぶ)

### CLI parser

- `text:` / `key:` / `wait:` / `wait-idle:` prefix 解析、絶対パス・予約語の `--spool` 値判定、
  3 種 timeout 同時指定、symbolic key alias 多数 — 自前 parser の責務が増える
- `hyoui keys --dry-run` で正規化結果確認の道具を提供

### 未確定事項 (journal 2026-05-26-cli-design-discussion.md 参照)

以下は本 DR の射程外、MVP 実装フェーズで詰める:

- wait L0 詳細 (match スコープ、scrollback buffer サイズ、stdout 印字)
- exit code 伝搬 (子 exit → daemon exit → client exit の連鎖)
- daemon 寿命の細部
- `hyoui run` 起動と attach の race 解消手順
- stale socket 検出の細部
- list / status の出力 schema
- kill の流儀 (子に SIGTERM → grace → SIGKILL)
- `--detached` の stdout フォーマット
- `$TERM` 等 tty 属性の継承戦略

## 関連

- [[DR-0001]] — bg/fg jobcontrol 2 軸 (透明性思想の起点)
- [[DR-0004]] — CLI サブコマンド方式採用 (本 DR で具体化)
- [[DR-0005]] — 外側自動操作主軸の思想
- [[DR-0007]] — MVP scope と段階リリース
- `docs/journal/2026-05-26-cli-design-discussion.md` — 議論の経緯と残論点
