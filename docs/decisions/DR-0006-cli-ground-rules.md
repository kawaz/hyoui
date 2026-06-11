# DR-0006: CLI 設計の地盤ルール — 動作モデル、自動操作 API、排他制御

- Status: Active
- Date: 2026-05-26 (初版) / 2026-05-27 (§8 input family / §9 wait / §10 snapshot / §11 tail を state-based に改訂)
- Related: [[DR-0001]] (jobcontrol 2 軸), [[DR-0004]] (CLI subcommand 採用), [[DR-0005]] (外側自動操作主軸), [[DR-0007]] (MVP scope), [[DR-0010]] (input family 整理 = 本 DR §8 で確定), [[DR-0013]] (screen emulator、本 DR §8-§11 の state-based 基盤)

## Update (2026-05-27): §8-§11 を state-based に改訂済

[[DR-0013]] で daemon = screen state 正本 / wait / snapshot / tail を state ベースで再定義したのに合わせ、本 DR の input / wait / snapshot / tail 該当 section を本文ごと書き直した。旧 §8 (send / keys / paste 棲み分け) / §9 (keys spec) / §10 (paste API) / §11 (wait L0) / §11.5 (tail) / §11.6 (scrollback ring buffer) は末尾の Archive section (= 2026-05-27 以前の旧仕様) に保全する。

CLI Design は `~/.claude-personal/rules/cli-design-preferences.md` (= subcommand 階層 / spec prefix / 引数なし時 `--help` / 補完前提のロングオプション / `--` セパレータ等) と整合させる。

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
other:    /tmp/hyoui-$UID/<name>.sock   (incl. macOS; /tmp 固定, $TMPDIR は読まない)
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

### 8. input family — 自動操作 API の唯一の入口

[[DR-0010]] §1 で「v0.2.0 subcommand を 11 → 7 に統合、`input` family を採用」が確定した。本 §8 で **input family の spec syntax を確定**する (= spec prefix カタログ正本)。

#### 8.1 leaf を廃止、`hyoui input <session> <spec>...` 一本化

旧案 (= [[DR-0010]] §1) では `input text` / `input keys` / `input paste` の 3 leaf に分けていたが、本 DR で **leaf 自体を廃止**する。理由:

- spec prefix で意味が一意に決まる (= `text:hello` / `paste:...` / `key:Enter` で leaf 不要)
- leaf があると「`text:` を `input keys` で送れるのか?」「`paste:` を `input text` で送れるのか?」のような matrix 質問が発生する
- 順序保証 (= spec の出現順) を leaf 跨ぎで保ちたい場合に leaf 構造が邪魔になる
- subcommand 数を更に減らせる (= `input` family 内の 3 leaf を 1 leaf に圧縮)

**採用**: `hyoui input <session> <spec>...` 一本。spec は **prefix で type 判別**、複数 spec を順序通り送信する。

#### 8.2 spec prefix カタログ (= 確定)

| prefix | 引数 | 意味 | 経路 |
|---|---|---|---|
| `text:<string>` | UTF-8 文字列 | direct text を bytes でそのまま送信 | direct (no bracket) |
| `hex:<hex>` | hex 文字列 (= `1b5b41` 等) | binary を hex decode して送信 | direct |
| `file:<path>` | ファイルパス | ファイル内容を bytes として送信 | direct (大規模 input 用) |
| `paste:<string>` | UTF-8 文字列 | bracketed paste で囲んで送信 (= `ESC[200~` + 中身 + `ESC[201~`) | bracketed paste |
| `key:<name>` | キー名 (= `Enter` / `Tab` / `C-c` / `M-x` 等) | symbolic key を escape sequence に変換して送信 | direct |
| `wait:<pattern>` | regex pattern | visible state に pattern が現れるまで block | (= 入力ではない、§9 wait の inline 形) |
| `wait-idle:<duration>` | 期間 (= `500ms` / `2s` 等) | 入力 idle 期間経過まで block | (= 同上) |

順序保証は spec の出現順 (= bash の argv 順そのまま)。例:

```bash
hyoui input <session> "text:hello" "key:Enter" "wait:^Prompt>" "text:world\n"
```

= `hello` を送信 → `Enter` キー → "Prompt>" が visible に現れるまで wait → `world\n` を送信。

#### 8.3 direct vs bracketed paste の使い分け

`text:` (= direct) と `paste:` (= bracketed) を **prefix で明示的に切り替え可能**にする。なぜ両方必要か:

| 経路 | 用途 | 子が解釈 |
|---|---|---|
| `text:` direct | 通常の text 入力 (= shell に command を打つ、TUI に 1 文字入力) | line discipline 経由、改行で実行確定 |
| `paste:` bracketed | multi-line script を 1 paste block で送る (= editor / shell / claude TUI 等の paste mode 活用) | `ESC[200~` 〜 `ESC[201~` の間は **paste 内容として扱われ実行されない**、最後にまとめて挿入 |

子が bracketed paste 対応 (= `ESC[?2004h` を出している) なら `paste:` で送ると意図通り「貼り付け」として扱われる。非対応の子に `paste:` を送ると `ESC[200~` / `ESC[201~` が text としてそのまま流れるリスクがあるので、daemon は **送信時点の子の bracketed paste mode 状態** (= vt100 wrapper が `?2004h` を hook して mode 保持) を参照する。

bracketed paste 自動判定 (= `auto`) は daemon に任せず、**ユーザが prefix で明示**する方針:

- 「shell に paste mode が有効か?」を CLI が自動判定すると semantics が不安定 (= timing 依存)
- prefix で明示すれば「これは paste 扱いで送って欲しい」が CLI 表現に乗る
- daemon は mode 不一致時 (= 子が `?2004h` を出していないのに `paste:` が来た) には warn を stderr に出すが、送信自体は実行する (= ユーザの明示を尊重)

#### 8.4 key spec (= `key:<name>`)

modifier + key の正規系 (= 内部表現):

```
Modifiers 順序: Ctrl, Alt, Shift, Super
Format: "Ctrl+Shift+Enter"  / "C-Tab" / "M-x" 等
```

modifier alias (= 互換性のため複数表記許容):

| modifier | alias |
|---|---|
| Ctrl | `C`, `Ctrl`, `ctrl`, `^`, `⌃` |
| Alt | `A`, `Alt`, `alt`, `M`, `Meta`, `meta`, `⎇`, `⌥` |
| Shift | `S`, `Shift`, `shift`, `⇧` |
| Super | `Super`, `⌘`, `Cmd`, `cmd`, `Win`, `❖` |

主要キー alias: `Enter`/`↩`/`⏎`, `Tab`/`⇥`, `Esc`/`⎋`, `Backspace`/`⌫`, `Delete`/`⌦`, `↑↓←→`, `Home`/`⤒`, `End`/`⤓`, `PageUp`/`⇞`, `PageDown`/`⇟` 等。

Case-sensitivity:
- modifier 部 大文字小文字無視
- modifier ありの key は case-insensitive (= `C-a` ≡ `C-A`、Shift 明示は `C-S-A`)
- modifier なしの単キーは case-sensitive (`a` と `A` は別、`A` は Shift+a 相当を送信)

Meta default は Alt (Mac Option)、`--meta-is-super` で切替可。

**multi-modifier (= Ctrl-Shift-A 等) は MVP 後回し**: terminal capability negotiation (= xterm modifyOtherKeys / kitty keyboard protocol) が必要で、子の対応 / 子の termios 状態を読み取って escape を切り替える実装が要る。MVP では正規系で表現できる範囲 (= `C-a` / `C-S-A` を「対応する伝統的な escape sequence」に変換) のみ送信し、対応していない子では期待動作にならない可能性を doc に明記する。ROADMAP `追加予定` に登録済。

#### 8.5 lock / tx は input family の primitive として位置付け

[[DR-0010]] §1 で `lock` family は別 family として確定済 (= `hyoui lock acquire` / `release` / `tx`)。本 DR §7 に詳細仕様あり。

input family 内で lock 動作を inline に書く案 (= `lock:<scope>` / `tx:begin` / `tx:end` を spec prefix にする) も検討したが、**採用しない**:

- lock は **session 全体への排他制御** であり、input spec の sequence の 1 要素として扱うと semantics が混乱する (= spec sequence の途中で `tx:begin` した場合、その後の error で `tx:end` が呼ばれない可能性がある)
- 排他境界は **subcommand 境界に揃える** のが筋 (= `hyoui lock tx <session> -- hyoui input <session> ...` で外側に lock、内側に input)
- `--lock-token T` (env `HYOUI_LOCK_TOKEN` 自動使用) で input が自動継承するので、spec prefix に lock を入れる必要がない

#### 8.6 入力源と spool / size 制御

旧 paste API の spool / size / atomicity 設計 (= 旧 §10) は **`file:` prefix に限定**して継承する:

- `text:` / `hex:` / `paste:` / `key:` の引数は argv なので size 確定、spool 不要
- `file:<path>` のみ大規模 input の可能性、size 制御が必要
- spool option (= `--spool=memory|tmpfile|<path>|none`) は **`file:` 入力時のみ**有効。default は `memory`
- `--max-size SIZE` (default 16MB、0 で無制限) は超過時 1 byte も送らず error (= atomic)
- atomic 保証は `--spool=none` (= stream) 時のみ崩れる (= 既送分は子に確定、daemon は best-effort で `ESC[201~` 補完で paste hang 防止)

stdin から大量データを paste する場合は `file:-` (= stdin) を使う。または事前に `cat > /tmp/payload.txt` してから `file:/tmp/payload.txt` を使う。

#### 8.7 paste の改行制御 (= `paste:` のみ)

`paste:` で送る text の改行コードは bracketed paste の中身として扱われるが、子側の挙動を意図通りに揃えるため改行制御 flag を 2 つ用意する (= 旧 §10 から継承):

```
--line-ending=preserve|lf|crlf            # 中身の改行コード正規化 (default preserve)
--trailing-newline=keep|auto|force|strip|trim   # 末尾改行制御 (default keep)
```

`--line-ending`:
| 値 | 動作 |
|---|---|
| `preserve` (default) | bytes 透過、何もしない |
| `lf` | CRLF/CR → LF に正規化 |
| `crlf` | CRLF に統一 |

`--trailing-newline`:
| 値 | 動作 | 用途 |
|---|---|---|
| `keep` (default) | bytes 透過 | 通常 |
| `auto` | 末尾改行なければ 1 個追加 | shell に paste で確実に確定 |
| `force` | 既存有無に関わらず 1 個追加 | 連続改行を意図的に作る |
| `strip` | 末尾改行 1 個削除 | `echo ls \| hyoui input ... "paste:..."` で実行を防ぐ |
| `trim` | 末尾改行を全て削除 (連続分) | 外部入力で末尾改行数が不定、整える |

`auto` / `force` で足す改行種別は `--line-ending` 指定に従う (`preserve` の場合は LF 固定)。

これらの flag は **input spec 全体に適用**される (= `paste:` 複数あれば全部に適用)。spec 単位で改行制御を変えたい場合は分割実行する。

#### 8.8 例 (= CLI 露出形)

```bash
# 単純な direct text
hyoui input <session> "text:hello\n"

# key + text 組合せ
hyoui input <session> "text:ls -la" "key:Enter"

# multi-line script を bracketed paste で
hyoui input <session> "paste:$(cat script.py)"

# binary 制御文字
hyoui input <session> "hex:1b5b41"   # = ESC[A (= Up arrow、key:Up の direct 形)

# 大規模 file paste
hyoui input <session> "file:./payload.txt" --spool=tmpfile --max-size=32MB

# wait → input の組合せ (= 自動操作の核)
hyoui input <session> "wait:^\\$" "text:export FOO=bar" "key:Enter"

# Ctrl-C 送信 (= key の modifier 表記)
hyoui input <session> "key:C-c"

# lock 下で実行 (= 外側で lock、内側で input)
hyoui lock tx <session> --timeout-idle=30s -- hyoui input <session> "wait:^Prompt>" "text:..." "key:Enter"
```

### 9. wait — state-based pre-condition 評価

[[DR-0013]] で daemon = screen state 正本になったことを受け、wait は **scrollback bytes regex から state-based match に再定義**する。旧 §11 (= scrollback regex に対する match、過去 ANSI 混在で誤マッチ多発) は廃止し、Archive section に保全する。

#### 9.1 マッチ対象 = daemon の **現在 visible state**

- 旧仕様: daemon の ring buffer (= scrollback bytes) に対する正規表現 match
- 新仕様: daemon の **screen state** (= vt100 wrapper の `VirtualScreen`) の **現在 visible cells** から構築した text に対する match

visible state の構築:

1. vt100 `Screen::rows()` で visible 範囲 (= viewport rows × cols) を走査
2. 各 cell の `Cell::contents() -> &str` を結合 (= grapheme cluster 保持、wide char は 1 cell として処理)
3. 行末の trailing space は trim (= TUI が padding として書き込んでいる場合の誤マッチ防止)
4. 行間は `\n` で結合 (= regex の `^` / `$` が行頭/行末に効くように)
5. ANSI escape は **そもそも含まれない** (= state は cell 化された後の表現、色や cursor 移動の trace は escape として残らない)

これにより:

- 過去 redraw の混入 = なし (= state は「現在 visible」だけ、redraw されたら state は新しい内容で上書きされる)
- alternate screen 切替 = state が自動で切り替わる (= primary / alternate の区別不要、現在 active な buffer を見る)
- cursor 移動による「同じ cell 上書き」= cell 単位で上書き反映済 (= match 対象に古い文字は出ない)

#### 9.2 spec syntax (= input family と整合)

wait は **2 経路** で使える:

```
(A) 単独 subcommand:  hyoui wait <session> <spec> [--timeout DUR] [--print=...] [--lock-token T]
(B) input spec inline: hyoui input <session> ... "wait:<pattern>" ... "wait-idle:<duration>" ...
```

両者で spec syntax は同一 (= §8.2 カタログから wait 系を抜粋):

| spec | 意味 |
|---|---|
| `wait:<pattern>` | visible state に <pattern> (regex) が現れるまで block |
| `wait-idle:<duration>` | 入力 idle 期間 (= 子からの新 bytes が止まる期間) が <duration> 経過するまで block |

将来拡張 (= ROADMAP `追加予定` の L2 wait に位置付け):

| spec | 意味 | status |
|---|---|---|
| `wait-cursor:<row>,<col>` | cursor が指定位置に来るまで block | L2 |
| `wait-prompt:<pattern>` | cursor 行の prompt が pattern にマッチするまで block (= shell prompt 検出用) | L2 |
| `wait-mode:<flag>` | alternate screen 切替 / mouse mode 変化等の mode flag 検出 | L2 |

#### 9.3 scope (= visible / scrollback / both)

L1 (= MVP) は **visible のみ**。理由は §9.1 の通り、scrollback regex は redraw 混入で誤マッチが多発するため。

L2 (= ROADMAP `追加予定`) で scrollback 範囲指定を opt-in 可能にする:

```
--scope=visible       # default (= MVP)
--scope=scrollback    # vt100 内蔵 scrollback 範囲、--scrollback-rows N で行数指定
--scope=both          # visible + scrollback
```

旧仕様の `--from=now` / `--from=history` は **廃止** (= state-based では scope が「現在 visible」固定で意味を成さない、history は scrollback に rename 済)。

#### 9.4 装飾 / 改行の扱い

旧仕様の `--strip-escapes` / `--raw` / `--newline-convert` は **廃止**:

- state-based では ANSI escape はそもそも対象に入らない (= §9.1 の通り、cell 化後の text)
- 改行も cell 化済の `\n` (= row separator) で統一、CRLF / CR / LF の区別は state 上に存在しない (= vt100 が cooked モード / ONLCR を内部処理する)

debug 用に「state を構築せず raw bytes に match したい」場合は `hyoui tail` (= §11) を使う想定。

#### 9.5 match の細部

- regex は Rust `regex` crate (= 既存 hyoui dep)。flags はデフォルト (= unicode-aware、case-sensitive)
- `(?i)` で case-insensitive にできる (= regex syntax で表現)
- multiline (`^` / `$` が行頭/行末) は **デフォルト ON** (= state-based では「行」の概念が明確、`(?m)` を毎回書くのは面倒)
- `--text S` (= substring) は **廃止**: `wait:<regex>` 一本に統一、substring が欲しければ `wait:\Qstring\E` で regex 内 quote する
- AND / OR 合成は MVP では非対応、複数 wait を順番に並べる (= `"wait:A" "wait:B"` で A → B 順)

#### 9.6 timeout / process-bound

```
--timeout DUR             # default infinite、明示必須 (= 無限 block を防ぐため doc で推奨)
--process-bound           # default 有効 (= 子 exit で wait も exit)
```

exit code:

| code | 意味 |
|---|---|
| `0` | match 成功 |
| `1` | timeout |
| `2` | 子 process exit (= 紐付け先消滅) |
| `3+` | error (= state 取得失敗、socket 切断等) |

#### 9.7 print (= match 情報の出力)

```
--print=none (default)    # exit code のみ
--print=match             # マッチ部分 (regex の match 全体)
--print=line              # マッチを含む行 (state 上の row 全体)
--print=json              # 構造化情報 (content / row / col / captures / timing / state seqno)
```

`--print=json` の schema は MVP 実装フェーズで詰める。最低限 `match` / `row` / `col` / `state_seqno` (= DR-0013 §3 の per-line SequenceNo) を含める。

#### 9.8 protocol 連動 (= [[DR-0013]] §9)

wait の実装は daemon に対して以下を発行する:

- `StateSnapshotRequest { include: { Cells, Cursor } }` を poll (= 子からの bytes 更新 trigger 経由、idle-only polling は避ける)
- Phase B 移行時は `DirtyLinesNotify` で dirty 通知を受け、変更行のみ再評価 (= 効率化)

#### 9.9 lock 連動

`--lock-token T` (= env `HYOUI_LOCK_TOKEN` 自動使用) で lock 環境変数を継承。lock 取得中の wait は他 client が input している場合でも安定して評価できる (= state は単一 daemon で正本管理されているため race なし)。

### 10. snapshot — state-based 観察 / dump

[[DR-0013]] §9 で `ScreenDumpRequest` / `StateSnapshotRequest` の 2 種類の control message を確定した。本 DR §10 で **CLI 露出形式** を確定する (= ROADMAP `優先` の snapshot 項目に直接対応)。

#### 10.1 2 subcommand に分割: `screen dump` と `screen snapshot`

```bash
hyoui screen dump <session>     [--format=...] [--layer=...] [--rect=...]
hyoui screen snapshot <session> [--include=...] [--format=...]
```

意図的に subcommand を 2 つに分けた:

| subcommand | 主用途 | 出力 |
|---|---|---|
| `screen dump` | **terminal 上で目視 / 再生する** dump | ANSI bytes (= terminal で `cat` すれば描画再生される) を主軸 |
| `screen snapshot` | **構造化された state を機械処理する** | JSON / CBOR で cell / cursor / mode 等を取得 |

`dump` = 「画面を見る」、`snapshot` = 「state を query する」と棲み分け。

#### 10.2 `hyoui screen dump <session>` (= visible bytes dump)

```
--format=ansi (default)   # state_formatted() 相当の ANSI bytes、terminal で再生可能
--format=binary           # raw bytes (= attach 復元と同じ、internal 用途)
--format=json             # 構造化 (= cells / cursor / mode の JSON、debug 用)
--format=cbor             # CBOR (= snapshot bundle 内部、§10.3 の snapshot と同じ wire format)

--layer=visible (default) # 現在 visible viewport のみ
--layer=scrollback        # 過去 scrollback rows のみ
--layer=both              # scrollback + visible 結合 (= 上から下に時系列順)

--rect=x,y,w,h            # 部分 dump (= x 列, y 行 始点、w 幅, h 高さ、全 layer に適用)
```

`--format=ansi` がデフォルト。例:

```bash
hyoui screen dump <session>                              # 現在 visible の ANSI dump (= terminal で cat 再生可)
hyoui screen dump <session> --format=ansi > screen.ans   # ファイル保存後 cat で再生
hyoui screen dump <session> --layer=both | less -R       # scrollback 含めて less で見る
hyoui screen dump <session> --rect=0,0,80,5              # 上 5 行のみ dump
```

protocol: [[DR-0013]] §9 の `ScreenDumpRequest { format, layer, rect }` を発行、`ScreenDumpResponse { payload: Vec<u8> }` を stdout に書く。

#### 10.3 `hyoui screen snapshot <session>` (= 構造化 state query)

```
--include=Cells,Cursor,Mode,Style,Scrollback,WindowSize,Buffer   # comma 区切り、default は Cells,Cursor,Mode
--format=json (default)   # JSON、jq で処理しやすい
--format=cbor             # CBOR、機械処理 / 圧縮重視
--format=ansi             # = screen dump 相当 (= 利便性のため alias、内部的に dump にフォールバック)
```

例:

```bash
hyoui screen snapshot <session>                                              # default = JSON で Cells/Cursor/Mode
hyoui screen snapshot <session> --include=Cursor                             # cursor 位置だけ
hyoui screen snapshot <session> --include=Mode,WindowSize                    # mode flag と size
hyoui screen snapshot <session> --format=cbor > state.cbor                   # binary 保存
hyoui screen snapshot <session> | jq '.cursor'                               # JSON で cursor 抽出
```

protocol: [[DR-0013]] §9 の `StateSnapshotRequest { include }` を発行、`StateSnapshotResponse` を `--format` に従って serialize して stdout。

#### 10.4 wait との関係

`wait` は内部で `StateSnapshotRequest` を発行して評価する (= §9.8)。`snapshot` は CLI ユーザが同じ primitive を直接叩く形 (= 自動 test の predicate / debug observability / post-mortem 再現)。

#### 10.5 圧縮 / 性能

[[DR-0013]] §11 で確定済の hybrid 戦略を採用:

- `--format=ansi` / `--format=binary` (= raw bytes 層) は `state_formatted()` をそのまま、TYPE_RAW_DATA で送信、CBOR には載せない
- `--format=json` / `--format=cbor` (= 構造化 snapshot) は CBOR 圧縮 wrapper (= 空 cell skip + 属性 bit pack + Color variant 整数化) を使う
- zstd 圧縮は Phase B の負荷測定後に導入検討 (= ROADMAP `追加予定`)

### 11. tail — bytes stream client

[[DR-0013]] で daemon = screen state 正本になったあとも、**raw bytes stream を生で取りたい用途**は残る (= log monitor / script grep / asciinema record の前段)。`tail` はその用途のための ad-hoc client として維持する。

```bash
hyoui tail <name> [--follow|--no-follow] [--since DUR [--since-strict]] [--last N] [--strip] [--newline-convert=preserve|lf|crlf] [--lock-token T]
```

#### 11.1 仕様 (= 旧 §11.5 を継承、state-based 時代の用途を明示)

- daemon の **vt100 内蔵 scrollback** + **現在の子 PTY bytes stream** を取得、stdout に出力 (= state を経由せず raw bytes 層から)
- `--follow` (default): live stream (`tail -f` 相当)
- `--no-follow`: 現 scrollback dump して exit
- `--since DUR`: 過去 DUR 秒以内の出力 (= scrollback 内フィルタ、取れた分だけ)
- `--since-strict`: scrollback 不足 (= since 範囲の一部が evict 済) を検知して exit 非 0 (= [[DR-0013]] §3 の `last_evicted_age` 補完 counter で判定)
- `--last N`: 末尾 N bytes
- `--strip`: ANSI escape 除去 (= script で grep する用、default は装飾あり)
- `--newline-convert=lf` で CRLF → LF 正規化 (= 旧仕様継承、cooked モード ONLCR 対策)

#### 11.2 用途 (= 明示)

- **log / script モニタ**: `grep` / `less -R` / `awk` で処理する
- **asciinema record の前段**: 子 bytes stream を timestamp 付きで保存する用途 (= 将来 `hyoui record` の前身)
- **debug**: daemon に届く生 bytes を確認したい (= state ではなく bytes layer を見たい)

#### 11.3 画面 mirror 用途には使えない

raw bytes は alternate screen 切替 / resize 不一致 / cursor 移動再演で描画崩壊する。**画面 mirror が欲しいときは `hyoui attach --read-only`** (= state-based redraw を経由する)。`tail` は意図的に raw bytes 層を露出するもの。

#### 11.4 state-based wait / snapshot との棲み分け

| 用途 | 推奨 |
|---|---|
| 「画面の現在 visible で X が出るまで待ちたい」 | `hyoui wait` (= §9、state-based) |
| 「画面の現在 visible を見たい」 | `hyoui screen dump` (= §10、state-based) |
| 「画面の現在 visible を構造化 query したい」 | `hyoui screen snapshot` (= §10、state-based) |
| 「daemon が受け取った生 bytes stream を見たい / grep したい」 | `hyoui tail` (= §11、bytes-based) |
| 「daemon が受け取った生 bytes stream を file に流したい」 | `hyoui tail --no-follow > out.log` |

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
- → input spec 内は `text:` / `key:` / `wait:` / `wait-idle:` / `paste:` / `file:` / `hex:` の 7 種 (= §8.2) に絞り、複雑な wait (= timeout / print / scope 制御) は `hyoui wait` に分離

### `input text` / `input keys` / `input paste` の 3 leaf 維持 (= [[DR-0010]] §1 旧案)

- [[DR-0010]] §1 では `input` family を 3 leaf (= `text` / `keys` / `paste`) に分けていた
- 却下理由 (= §8.1):
  - spec prefix で type 判別すれば leaf 不要 (= `text:` / `key:` / `paste:` で意味が一意)
  - leaf 跨ぎで spec の順序を保ちたい場合に leaf 構造が邪魔
  - subcommand 数の削減 (= 3 leaf → 1 leaf)
- → `hyoui input <session> <spec>...` に圧縮、leaf 廃止

### scrollback regex に対する wait match (= 旧 §11)

- 旧 v0.1.x で実装した `wait --pattern R --from=history` (= scrollback ring buffer 全体に対する regex)
- 却下理由 (= [[DR-0013]] 起票の根本原因):
  - claude TUI 等の alternate screen 常駐 app は bg/fg 切替 / redraw で全画面 ANSI を再送する
  - scrollback に過去描画分が大量混在し、`wait --pattern "Continue?"` が過去履歴に誤発火する
  - 「現在 visible state に対する match」が無いと自動化が安定しない
- → §9 で **state-based wait (= 現在 visible に対する match)** に再定義、scrollback regex は廃止 (= L2 で opt-in 復活可能性は残すが MVP では除外)

### bracketed paste を auto 判定 (= 旧 §10 paste API の `--bracketed-paste=auto`)

- 旧 §10 では daemon が子の `ESC[?2004h` を hook して自動 wrap する `auto` mode を default にしていた
- 却下理由 (= §8.3):
  - daemon が自動判定すると semantics が timing 依存 (= 子の `?2004h` 送信前に paste すると wrap されない、後だと wrap される)
  - 自動化 script の挙動が「daemon が今何を見ているか」に依存して再現性が落ちる
  - ユーザの意図 (= 「これは paste 扱いで送りたい」) を CLI 表現に乗せる方が筋
- → §8.3 で **spec prefix で明示** (= `text:` direct / `paste:` bracketed) に振った
- 子が bracketed paste 非対応な状態で `paste:` を送った場合は warn を出すが送信実行 (= ユーザの明示尊重)

### multi-modifier (= Ctrl-Shift-A) を MVP で full support

- 主張: 自動化用途で modifier 組合せは便利、当然 MVP で対応すべき
- 却下理由 (= §8.4):
  - terminal capability negotiation (= xterm `modifyOtherKeys` / kitty keyboard protocol) が必要
  - 子の termios / capability を読み取って escape を切り替える実装が要る
  - MVP で実装すると vt100 wrapper + protocol negotiation が必要で scope が膨らむ
- → MVP は正規系で表現できる伝統的 escape sequence のみ対応、対応していない子では期待動作にならない可能性を doc 明記
- ROADMAP `追加予定` に「multi-modifier 対応 (= xterm modifyOtherKeys / kitty keyboard protocol)」として登録済

### `hyoui screen` を `hyoui dump` / `hyoui snapshot` の flat subcommand に

- 主張: subcommand 数を更に減らせる、深い nest は学習コスト
- 却下理由 (= §10.1):
  - `screen dump` / `screen snapshot` は **screen state を対象とする操作群** で family を成す
  - 将来 `screen mode` / `screen resize` 等の state 操作を追加する余地を残せる
  - 「snapshot」は意味として広い (= session snapshot / state snapshot 等の混乱) ので `screen` で scope を限定
  - cli-design-preferences.md (= subcommand 階層、補完前提) と整合
- → `hyoui screen dump` / `hyoui screen snapshot` の 2 leaf を確定

### `--file -` への誘導 (stdin size 不明時)

- stdin と `--file -` は本質的に同じ (どちらも size 不明)、誘導しても解決しない
- 代わりに spool mode を統一 (`--spool=memory|tmpfile|<path>|none`)、`--max-size` の意味を spool mode 別に定義
- ユーザの size 不明承諾は `--spool=none` or `--max-size=0` で明示

### `--newline=...` を 1 フラグに統合

- paste API には改行の文脈が 2 つある: (a) 中身の改行コード正規化、(b) 末尾改行制御
- 両方とも `--newline` prefix だと混乱 (`--newline-at-end` を見て「改行コードを末尾基準で正規化?」と誤読リスク)
- → 別名で分離: **`--line-ending`** (中身) + **`--trailing-newline`** (末尾)
- `line-ending` は editorconfig/git の慣用語、`trailing-newline` は POSIX/Unix tool 慣用語

### CRLF→LF 正規化を装飾除去に含める (PoC 08 で見直し)

- 初稿では「装飾除去の一部として CRLF→LF も含める」と書いていたが、PoC 08 [[2026-05-26-ansi-strip]] で見直し
- ANSI escape (CSI/OSC/DCS/single char) と改行 (LF/CR/CRLF) は意味的に **別レイヤ**:
  - ANSI escape = 表示装飾の制御 (色、cursor 移動、bracketed paste 等)
  - 改行 = text の一部 (line terminator)
- 同じ flag で両方制御すると semantics 混乱 (= `--raw` で escape 残せても改行は変換される/されない?)
- → wait/tail に **`--newline-convert=preserve|lf|crlf`** 別 flag (default `preserve`)、装飾除去 (`--raw` opt-out) と独立
- これにより pty の ONLCR ([[2026-05-26-multi-attach]]) で発生する CRLF 問題に意図的対処可能

### `--newline=none` (旧案)

- `none` が「改行 0 個」(= 削除) と誤読されるリスク
- 「正規化しない」を `--newline=preserve` で表現、誤解の余地を消す

### leader CLI を MVP に露出

- ユーザ (kawaz) の主用途は 1 人運用、leader の手動操作は不要
- 将来 HTTP gateway 展開時 (ローカル端末 + ブラウザ端末の同時 attach) に解放する余地を残す形 (DR-0007)

## Consequences

### 実装への波及

- daemon は in-flight paste state を持つ必要 (= best-effort end 保証のため Drop impl / signal handler)
- protocol message: handshake / resize / signal forward / data / detach / leader change / status query / lock acquire / release / mode change (= broadcast) + [[DR-0013]] §9 の `ScreenDumpRequest` / `ScreenDumpResponse` / `StateSnapshotRequest` / `StateSnapshotResponse` を新規追加
- protocol design は transport (= Unix socket / TCP / WebSocket) から独立した wire format
- attach client を library として export (= `kawaz/hyoui-serve` が呼ぶ、[[DR-0010]] §2)
- **wait / snapshot / screen dump は [[DR-0013]] の VirtualScreen wrapper を直接 query する** (= raw bytes ring buffer に依存しない、state を正本として扱う)

### CLI parser

- `text:` / `key:` / `hex:` / `file:` / `paste:` / `wait:` / `wait-idle:` の 7 種 spec prefix 解析 — `:` の前後を分割するだけのシンプルな parser で済む
- 絶対パス・予約語の `--spool` 値判定 (= `file:` 入力時のみ)、3 種 timeout 同時指定、symbolic key alias 多数 — 自前 parser の責務は残るが、leaf を廃止したことで dispatch ロジックは単純化
- `hyoui input --dry-run` で spec 正規化結果確認の道具を提供 (= 旧 `hyoui keys --dry-run` 相当)

### 未確定事項 (= MVP 実装フェーズで詰める)

以下は本 DR の射程外:

- wait `--print=json` の schema (= match / row / col / captures / timing / state_seqno の正本構造)
- `screen snapshot` `--format=json` の schema (= [[DR-0013]] §11 の CBOR wrapper との 1:1 対応)
- exit code 伝搬 (= 子 exit → daemon exit → client exit の連鎖)
- daemon 寿命の細部
- `hyoui run` 起動と attach の race 解消手順
- stale socket 検出の細部
- list / status の出力 schema
- kill の流儀 (= 子に SIGTERM → grace → SIGKILL)
- `--detached` の stdout フォーマット
- `$TERM` 等 tty 属性の継承戦略
- multi-modifier key の terminal capability negotiation 実装 (= ROADMAP `追加予定`)
- scrollback 範囲指定 wait の L2 仕様 (= `--scope=scrollback`、ROADMAP `追加予定`)

## 関連

- [[DR-0001]] — bg/fg jobcontrol 2 軸 (透明性思想の起点)
- [[DR-0004]] — CLI サブコマンド方式採用 (本 DR で具体化)
- [[DR-0005]] — 外側自動操作主軸の思想
- [[DR-0007]] — MVP scope と段階リリース
- [[DR-0010]] — input family / lock family 整理 (= 本 DR §8 で spec syntax 確定)
- [[DR-0013]] — screen emulator + attach/detach 安定化 (= 本 DR §8 input / §9 wait / §10 snapshot / §11 tail の state-based 基盤)
- `docs/journal/2026-05-26-cli-design-discussion.md` — 旧仕様の議論の経緯
- `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md` — state-based 方針大転換の議論経緯

## Archive (= 2026-05-27 以前の旧仕様)

以下は本 DR が state-based に改訂される前 (= 2026-05-26 初版) の旧記述。historical reference 用として保全する。**現行仕様は本文 §8-§11 が正本**。

### 旧 §8: send / keys / paste 棲み分け

| | 用途 | 入力源 | 用例 |
|---|---|---|---|
| `send` | raw bytes (= binary 安全、制御文字込み) | stdin / `--file` / argv | binary stream, 古い shell への制御文字 |
| `keys` | symbolic + text mixing | argv (= key spec list) | 自動操作の核 |
| `paste` | bracketed paste で囲んだテキスト塊 | `--text` / `--file` / stdin | 大きなテキスト・コード貼り付け |

→ 現行: §8 で `hyoui input <session> <spec>...` の 1 leaf に統合、spec prefix で type 判別 (= `text:` / `hex:` / `file:` / `paste:` / `key:`)。leaf 廃止。

### 旧 §9: keys の spec syntax

```
text:TEXT            # raw 文字列
key:STRICT_KEY       # strict (= alias 不可、case-sensitive、正規系のみ)
wait:DUR             # 固定 sleep
wait-idle:DUR        # idle 待ち
(prefix なし)        # 緩い key 表記 (= alias OK、modifier case-insensitive)
```

→ 現行: §8.2 で 7 種 prefix カタログに拡張 (= `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:`)。`wait:` は固定 sleep から **regex pattern match** に semantics 変更 (= state-based)。prefix なしの緩い key 表記は廃止 (= 全 spec で prefix 必須に統一)。

### 旧 §10: paste API (= 独立 subcommand `hyoui paste`)

`hyoui paste <name> [入力源] [spool/size] [オプション]` で `--text` / `--file` / `--spool=memory|tmpfile|<path>|none` / `--max-size SIZE` / `--bracketed-paste=auto|on|off` / `--line-ending` / `--trailing-newline` / `--chunk-size` / `--chunk-delay` / `--lock-token` / `--spool-file-overwrite` / `--spool-file-keep` を提供していた。

→ 現行: `hyoui paste` subcommand 廃止、`hyoui input <session> "paste:..."` に統合。spool / size / atomicity 仕様は `file:` 入力時のみ継承 (= §8.6)。bracketed paste の `auto` mode は廃止、`paste:` prefix で明示 (= §8.3)。`--line-ending` / `--trailing-newline` は `paste:` spec 全体に適用される flag として継承 (= §8.7)。

### 旧 §11: wait L0 (= scrollback bytes regex)

`hyoui wait <name> [--idle DUR | --text S | --pattern R | --text S --then-idle DUR | --pattern R --then-idle DUR] [--from=now|history] [--timeout DUR] [--print=none|match|line|json] [--raw] [--newline-convert=preserve|lf|crlf] [--lock-token T] [--strip-escapes]` で **scrollback ring buffer の bytes に対する正規表現 match** を実装していた。

→ 現行: §9 で **state-based** に再定義。マッチ対象を「daemon の現在 visible state (= vt100 wrapper の cells)」に変更。`--strip-escapes` / `--raw` / `--newline-convert` は廃止 (= state は cell 化後の text で escape / 改行混入なし)。`--from=now|history` は `--scope=visible|scrollback|both` に rename (= L2 は ROADMAP `追加予定`)。`--text S` は `wait:\Qstring\E` で代替できるため廃止、`--pattern R` 一本に統一。

主な変更点を表で:

| 旧仕様 (= scrollback bytes regex) | 新仕様 (= state-based) |
|---|---|
| `wait --pattern R --from=history` で過去 redraw に誤マッチ | 現在 visible state に対する match、過去 redraw は state 上書きで自然除外 |
| `--strip-escapes` で ANSI strip | state は cell 化後、ANSI 不在 |
| `--newline-convert` で CRLF 正規化 | state 上は row separator `\n` 統一、CRLF / CR の区別不在 |
| `--text S` / `--pattern R` の 2 形 | `wait:<regex>` 1 形 (= substring は `\Q...\E` で表現) |
| L0 / L1 / L2 の段階拡張 | L1 (= visible) が MVP、L2 (= scrollback / cursor / mode) は ROADMAP `追加予定` |

### 旧 §11.5: tail (= ad-hoc bytes stream client)

`hyoui tail <name> [--follow|--no-follow] [--since DUR [--since-strict]] [--last N] [--strip] [--newline-convert=preserve|lf|crlf] [--lock-token T]` で daemon の ring buffer から bytes stream を取得していた。

→ 現行: §11 でほぼ仕様維持。state-based の `wait` / `screen dump` / `screen snapshot` との棲み分けを明示 (= §11.4)。tail は raw bytes 層を露出する意図的選択肢として残し、画面 mirror 用途には使えない旨を強調。

### 旧 §11.6: scrollback ring buffer (= daemon 側 timestamped chunks)

`OutputChunk { timestamp, bytes }` の `VecDeque` で ring buffer を持ち、`--scrollback-size=4MB` (default) で size 制御、`hyoui status` で `oldest_age` / `last_evicted_age` / `chunks` を表示していた。

→ 現行: **vt100 内蔵 scrollback に統合** ([[DR-0013]] §8)。`crates/hyoui/src/scrollback.rs` は vt100 wrapper に置換予定 (= ROADMAP `優先` Phase B)。`last_evicted_age` 補完 counter は vt100 に public API がないため wrapper 自前実装 ([[DR-0013]] §3)。`--scrollback-size` の起動時指定は維持 (= `Parser::new(rows, cols, scrollback_len)` に渡る)。alternate screen の限界 (= primary / alternate 混在で履歴押し出し) は vt100 が primary / alternate を別 buffer 管理するため自然解消。
