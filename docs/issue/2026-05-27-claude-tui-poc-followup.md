# claude TUI 自動操作 PoC からのフィードバック (dump plain-text format + scrollback layer)

- Status: Open
- Date: 2026-05-27
- Priority: Middle
- 発見元: claude-cmux-msg main 側で `hyoui input + wait + screen dump` を使って claude TUI を実機操作した PoC

## PoC サマリ

`hyoui input` family + `screen dump` + `wait` (state-based) が揃ったので、cmux-msg subscribe 経路を介さず **hyoui 単体で claude TUI を完全に自動操作できる**ことを実機で確認した:

```bash
# 1. spawn
nohup hyoui run --mode=headless --size=120x40 -- claude --session-id <uuid> > log 2>&1 &
disown

# 2. ready 待ち (= 入力欄プロンプト ❯ 出現)
hyoui wait <sid> '❯' --timeout=30s

# 3. プロンプト送信
hyoui input <sid> 'text:7 と 5 の最大公約数を 1 行で答えて' 'key:Enter'

# 4. 応答完了 marker
hyoui wait <sid> '✻.+for' --timeout=60s

# 5. visible state 取得
hyoui screen dump <sid> --format ansi    # 装飾込み (ターミナル再生用)
hyoui screen snapshot <sid> --include Cells  # CBOR 構造化
```

これで claude の回答 `⏺ 1` まで取れた。**hyoui keys subcommand 無しでも本命 use case が立つ** ([[2026-05-26-feature-claude-tui-automation]] の B 判定が input/wait/screen 経路で実用可能になった証拠)。

副次的判明: claude TUI は **primary screen 使用** (alt screen 切替なし) なので、現状の `screen dump --layer visible` で取れる。DR-0013 の alt screen 対応は当面 vim/less/top 用と切り分けて良い。

## 要望 1: `screen dump --format` に「装飾なし + 空白保持」option が欲しい

現状の `--format` 選択肢:

| format | 挙動 | 用途 |
|---|---|---|
| `ansi` | ANSI escape 込みの raw bytes | ターミナル再生 (cat <file>) |
| `binary` | 空白除去 + 改行 plaintext | grep 用 |
| `cbor` | CBOR encoded ScreenSnapshot | 機械処理 |

`binary` (空白除去) は **TUI app と相性が悪い**: claude TUI のように「ステータスバー + 入力欄」だけ描いて残り 30+ 行が空白の状態だと、空白除去で結果がほぼ空に見える (= 行構造が消える)。

ほしい挙動 = **「ANSI escape は strip するが、cell の空白 (padding) と行構造はそのまま保持」**:

```
追加候補: --format text  (or plain)
  - cell の文字部分のみ抽出 (= ANSI escape sequence は除去)
  - 行末空白は保持、改行で行分け (= rows 数の行が出る)
  - ターミナル独立で人間が `less` で読める形
```

実装イメージ: 既存 `binary` format から「空白除去」step を抜くだけで近い挙動になる (= `screen.cells_to_text(strip_trailing=false)` 的に)。

代替: `--format binary` に `--no-strip-trailing` flag を追加する形でも可。ただし命名が `binary` のまま「行構造保持」になると混乱しそうで、新 format 名 (`text`) の方が綺麗。

### 関連: 既存 `dump --format binary` の help 文言

現状の説明 `binary — 空白除去 + 改行 plaintext (= grep 用)` は明確だが、「grep 用」と言いつつ TUI 状態判定では結構痛い (= `wait` で `--pattern` 渡したい時に行マッチが効きにくい）。新 format 追加と合わせて、help の使い分け説明も整理したい。

## 要望 2: `screen dump --layer scrollback` の実装 (or 別経路)

現状:

```
hyoui screen dump <sid> --layer scrollback
→ ProtocolMalformed message=screen.dump layer not implemented in MVP (scrollback / both)
```

**use case**: claude TUI で長文応答 (50 行超) が出ると visible 40 行から **スクロールアウトして見えない**。例:

- claude に「DR-0010 を要約して詳細に説明して」と振ると 60+ 行の応答 → visible 40 行には末尾しか残らない
- 自動化スクリプトで応答全文を取りたいのに、上端から消えた部分が読めない

代替案 (現状の workaround):

- `hyoui tail <sid>` で raw byte stream を流して自前で vt100 emulator 通す (= csa の jsonl 経路でなく hyoui tail を vt100 で再構成) → 二重 emulator は不毛
- claude TUI 側で `--size=120x200` のように **rows を最初から大きく取る** (= スクロール発生させない) → 実用上はこれが回避策、ただし daemon 側 grid memory が rows × cols 比例で増える

実装は DR-0013 で API 設計済 (`--layer scrollback / both`) なので、優先度を上げて欲しい。少なくとも「visible より上に N 行の最近 scrollback だけ取れる」(= `--layer scrollback --last-rows 100` 的な) でも実用上助かる。

## 副次的要望 (小)

### 「`hyoui wait` で多 pattern alternation がほしい」

claude TUI の応答完了マーカーは `✻ <verb> for <duration>` の形式だが、verb が `Brewed / Sautéed / Simmered / Cooked / Cooked / Crunched ...` と多数の調理動詞を渡り歩く。正規表現 `'✻.+for'` で網羅できるが、もう少し型安全に「complete マーカー候補」を組み込み prefix で渡せると嬉しい (= `wait <sid> 'kind:claude-completion'` 的な、claude-specific helper)。

これは **hyoui のスコープ外** (= claude TUI 固有のヘルパは別 layer or 別 plugin) かもしれないが、もし「TUI 別 marker library」を `kawaz/hyoui-tui-markers` 的に切り出すなら、そこに claude / vim / fzf / less 等の典型マーカーを集約できそう。

## scope 外 / 別途検討

- snapshot Cells の **CBOR decode** の人間可読化: `hyoui screen snapshot --format json` の実装 (現状 forward-compat の wire 上 cbor only) があれば、scripted 処理での扱いがぐっと楽になる。ただしこれは [[2026-05-26-feature-recording-and-dump]] と関連しそうなので別 issue で。

## 関連

- [[2026-05-26-feature-claude-tui-automation]] — 本 PoC が実証した本命 use case の親 issue
- [[2026-05-26-feature-recording-and-dump]] — snapshot の JSON 化等の関連
- [[../decisions/DR-0013]] — screen emulator / alt screen / scrollback の API 設計 (本 issue で実装優先度を上げたい部分)
- [[2026-05-27-cmux-msg-experiment-feedback-v020-refresh]] — 前回の cmux-msg 検証フィードバック (B1-B8 系統)
- 発見元 repo: `kawaz/claude-cmux-msg`
