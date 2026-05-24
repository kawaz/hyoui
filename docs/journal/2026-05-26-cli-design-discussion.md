# 2026-05-26 hyoui CLI 設計議論セッション

v0.0.0 リリース後の機能設計フェーズ。思想再定義から CLI 詳細 (送信系・排他系・paste API) まで詰めた。
結果は [[DR-0005]] [[DR-0006]] [[DR-0007]] に確定、本 journal は議論の経緯と残論点を残す。

## 開始時の状態

- hyoui v0.0.0 リリース済 (Rust 一本化 6 段階完了、97 件テスト pass)
- ユーザ (kawaz) が `./target/release/hyoui run -- zsh` で zsh 動作確認
- 次に何やるか相談から開始

選択肢: (a) 新機能 (send/attach/status)、(b) docs 整備、(c) DR-0001 未実装オプション
→ ユーザ「a の設計をしてきましょうか?」で機能設計フェーズに突入

## 議論の流れ (約 20 ラウンド)

### Phase 1: 思想再定義

最初に detach/attach の理解確認から始まったが、ユーザの「hyoui はどちらかというとその名の通り
主体は中の子プロセス内の生活ではなく外側からの、監視や自動操作が主軸」発言で大きく方向確定。

→ [[DR-0005]] (外側自動操作主軸、TUI multiplexer ではない、透明性最優先) 確定。

### Phase 2: アーキテクチャ確定

- daemon モデル: tmux 型 (1 server 多 session) vs screen 型 (1 daemon 1 socket 1 子)
  → screen 型採用 (思想と整合、シンプル、隔離性高)
- socket 自動配置: `$XDG_RUNTIME_DIR/hyoui/<name>.sock` (Linux) / `$TMPDIR/hyoui-$UID/<name>.sock` (macOS)
- name: default pid、`--name` で override、衝突 = error
- 起動 = daemon + 自動 attach、`--detached` で初期 detach
- nest 起動: 許可 + warn ($HYOUI_NAME/$HYOUI_SOCK env)

### Phase 3: 複数 attach + leader 概念

ユーザの「複数 attach 許可したい。rw/ro の区別をするしない?」から始まり議論深まる:

- rw/ro 区別 ⭕
- rw 複数可 (ユーザ責務)、`--exclusive` で起動時占有・`--detach-others` で奪取
- winsize: latest が直感的 (default)、smallest/largest は後付け、manual:WxH はテスト用で必須

ユーザの提案「leader 方式? 最初のプロセスがリーダーで...」で leader 概念導入が決定:

- leader は内部メカニズム (winsize 主体)、cascade policy `latest` default
- leader は rw 必須 (ro 共存可)
- MVP では leader CLI 露出なし (v0.3.0 で解放)

### Phase 4: HTTP gateway 発展シナリオ (port 6978)

ユーザ「`hyoui run claude` で起動して放置 → リモートからブラウザターミナルで attach」発展シナリオ提案。
→ leader 概念を残す確実な理由 (= ローカル端末 + ブラウザ端末の同時 attach で winsize 主体明示必須)。

`hyoui serve` の default port = `6978` (QWERTY 物理配置エンコード):
```
6 7 8 9
y u i o
h ← y と一緒に 6 ゾーンに吸収
```
→ hyoui = (h+y) o u i = 6 9 7 8

### Phase 5: 自動操作 API (send/keys/paste/wait)

ユーザ「中の子の生活ではなく外側からの監視/自動操作が主軸」確定後、CLI 一覧再整理:

- send (raw bytes、binary 安全)
- keys (symbolic + text mixing)
- paste (bracketed paste、大量テキスト)
- tail (ro 軽量、stream)
- wait (条件付き待機)
- lock / tx (排他)

### Phase 6: keys の prefix syntax

```
text:TEXT           # raw 文字列
key:STRICT_KEY      # strict 表記 (alias 不可)
wait:DUR            # 固定 sleep
wait-idle:DUR       # idle 待ち
(prefix なし)       # 緩い key 表記
```

ユーザの「wait-text/wait-pattern を keys に含めたい?」提案に対し、`:` エスケープの複雑さ + shell の
再発明回避から「keys 内は 4 prefix のみ、複雑 wait は専用コマンドへ (lock 環境変数で atomic 性継承)」
に集約。

### Phase 7: modifier alias 拡張

ユーザの「⎇ や ⌥ も?」から alias 表を拡張:

| modifier | alias |
|---|---|
| Ctrl | C, Ctrl, ctrl, ^, ⌃ |
| Alt | A, Alt, alt, M, Meta, meta, ⎇, ⌥ |
| Shift | S, Shift, shift, ⇧ |
| Super | Super, ⌘, Cmd, cmd, Win, ❖ |

Case-sensitivity: modifier 部 case-insensitive、modifier ありの key も case-insensitive (Shift 明示が必要なら `Ctrl+Shift+A`)、modifier なしの単キーは case-sensitive。

### Phase 8: Lock + tx 詳細

ユーザ「unlock 手動は避けたい、timeout 必須なら、間違えて長時間 lock 取ったままの救済で `unlock --force`」「毎回トークン指定面倒、環境変数経由で」「tx は subcmd 起動・exit で auto unlock、10 秒くらいの default」。

→ Timeout 3 種同時指定 (OR 評価):
- `--timeout-absolute DUR`
- `--timeout-idle DUR`
- `--process-bound`

ユーザの「ab 個別両方欲しい。プロセスも必要」で 3 種共通設計確定。
default: tx = process-bound + 5min absolute、lock = idle 30s。

idle timeout は wait にも採用 (`--idle DUR`、`--then-idle DUR`)。

### Phase 9: paste API (5 ラウンド)

最も時間かかった部分。論点が連鎖:

1. 入力源: `--text` / `--file` / stdin
2. bracketed paste 自動検出 + override
3. 改行正規化: 当初 `--newline=lf|crlf|none` だったが、`none` が「削除」と誤読されるリスクで `preserve` に修正
4. 改行自動変換 default: lf vs preserve → バイナリ判定不能なため preserve (bytes 透過) を default に
5. サイズ管理: `--max-size` (default 16MB)、`--unbounded` (= `--max-size=0`)、`--tmpfile`
6. ユーザ「サイズ不明な貼り付けは拒否、`--file -` 誘導は意味なし (stdin と同じ問題)」で大幅修正:
   - default は memory spool、`--max-size` まで OK、超過で error
   - 拒否時に選択肢提示 (`--max-size SIZE` 上げる / `--tmpfile` / `--unbounded`)
7. ユーザ「`--tmpfile` と memory が似た動作なのに CLI 異なるのが気になる、`--spool=memory|tmpfile|file|none` で統一」
   → `--spool=memory|tmpfile|<path>|none` の 4 値に統一、絶対パス強制 → 相対 path も OK (`./memory` で予約語衝突回避)
8. `--max-size` の意味は spool mode で異なる (確定モード = 拒否閾値、none = 切り捨て位置)
9. daemon は in-flight paste state 持ち、異常終了 path で `ESC[201~` を best-effort 送信 (子 hang 防止)
10. 「bracketed paste にキャンセル機構は原理的に存在しない」を確認、`--no-bracketed` で stream mode に切替えれば CLI 側 `^C` でクリーン中断可能、を回避策として明示

### Phase 10: kuu リポへの help カテゴリ提案起票

paste の CLI で「入力源 / spool / size / その他」のカテゴリ分けが見やすいと判明。
[[kawaz/kuu.mbt の docs/issue/2026-05-26-help-option-sections.md]] に提案として起票
(kuu-cli が既に merge 済み、その API に乗せる形が筋)。

## 残論点 (MVP 実装フェーズで詰める)

[[DR-0006]] Consequences 「未確定事項」セクションに集約済み:

### wait L0 詳細

- match スコープ: 新規出力のみ vs scrollback buffer 過去分
- daemon の scrollback buffer サイズ default
- `--text` vs `--pattern` 両方持つか
- `--idle` / `--then-idle` の組み合わせ semantics
- マッチ結果の stdout 印字 (regex captures)
- timeout 3 種共通フラグの wait での意味

### A 系 (コア動作) 残り

- exit code 伝搬 (子 exit → daemon exit → client exit)
- daemon 寿命の細部
- `hyoui run` 起動と attach の race 解消 (pipe? retry?)
- stale socket 検出 (ping 失敗 → 自動削除? `--force`?)
- 同名衝突 error 文言
- `$TERM` 等 tty 属性の継承 (起動時 env 固定 = tmux 流?)

### B 系 (UI 詳細) 残り

- `hyoui list` の出力 schema (name/pid/socket/uptime/子cmd/leader/client数/detached)
- `hyoui status [name]` の name 省略時挙動 ($HYOUI_NAME 検出?)
- `kill` の流儀 (SIGTERM → grace → SIGKILL?)
- `--detached` の stdout フォーマット
- `detach` の粒度 (`--all` / `--others`)
- resize broadcast 必要性

### Protocol 設計

- 議論意図的に後回し ([[DR-0006]] の wire format 独立性宣言だけ確定)
- 「実装でも替えが効く、テスト充実が先」(ユーザ方針)
- 着手は MVP 実装フェーズで

## kuu リポへの起票

[[kawaz/kuu.mbt]] の `docs/issue/2026-05-26-help-option-sections.md` に、CLI parser へ
「--help にカテゴリ/セクション分け表示」概念を追加するアイデアを起票。hyoui の paste API CLI 設計で
オプション数が増えた時にカテゴリ分けが圧倒的に読みやすくなったのが発見元。kuu-cli が既に merge 済みなのでその API に乗せる形で実装したい。

## 関連

- [[DR-0005]] — 思想再定義 (外側自動操作主軸)
- [[DR-0006]] — CLI ground rules (本セッションの主成果)
- [[DR-0007]] — MVP scope と段階リリース
- kawaz/kuu.mbt docs/issue/2026-05-26-help-option-sections.md — 派生提案

## ユーザの会話スタイル所感 (今後参照)

- 設計議論は手戻りを厭わず、思いついた論点をその都度投げてくる (paste API は 7 ラウンド超)
- 用語の細部にこだわる (`none` → `preserve` 修正、`--spool` 統一提案、絶対パス強制 → 相対 OK)
- 「中で生活する」「外から操作する」のメタ視点で方針を決めたがる
- protocol 等の実装詳細は意図的に後回し、動作モデルを先に固める
- AI の提案に対し賛同点と懸念点をすぐ返してくる、ヨイショは不要
