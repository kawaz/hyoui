# hyoui

> [English](./README.md) | 日本語

**hyoui** `/ˈhjoʊi/`（ヒョーイ・憑依）— 子プロセスに「憑依」して一体で動く、透過的な PTY ラッパー CLI。

任意のコマンドを PTY の中で起動し、内側からは透過的に振る舞いながら、
**外側から監視・自動操作するための足場**を提供する。

## 何ができる

`hyoui run -- <cmd>` でコマンドを PTY 内で起動して daemon 化し、別プロセスから
`hyoui attach` / `hyoui list` / `hyoui kill` で操作する。`hyoui` 自身は子に対して
何も介入せず（in-band escape 一切なし）、入出力は完全透過。代わりに **CLI / 将来の HTTP
gateway 経由で外側から制御** する。

主な用途:

- long-running な対話プロセス（例: `claude` / REPL / `ssh` / TUI app）を起動して、
  あとから何度でも attach / detach する
- CI / スクリプトから入力注入と出力待ちで自動操作する（`send` / `wait`、v0.2.0+ で本格化）
- 複数 client で同じセッションを共有する（pair-programming、観戦、人手介入）

## Installation

### Homebrew (planned)

```bash
brew install kawaz/tap/hyoui
```

> v0.1.0 時点では tap への formula 公開は未実施。GitHub Release から binary 取得 or ソースビルドを使う。

### Cargo

```bash
cargo install --git https://github.com/kawaz/hyoui hyoui-cli
```

### ソースから

```bash
git clone https://github.com/kawaz/hyoui.git
cd hyoui
cargo build --release
# binary は target/release/hyoui
```

対応 platform: Linux / macOS（Rust 1.86+、PTY と Unix socket を使うため Windows は未対応）。

## Quickstart

### 1 セッションを起動

```bash
# foreground (= 自動 attach)
hyoui run -- bash

# detached (= daemon だけ起動、stdout に session 名が出る)
SESS=$(hyoui run --detached -- bash)
echo "started: $SESS"
```

### 別のターミナルから attach / list / kill

```bash
# session 一覧 (session 名 と socket path)
hyoui list

# 既存 session に attach (= 入出力を中継、Ctrl-A D で detach)
hyoui attach "$SESS"

# read-only で覗き見
hyoui attach "$SESS" --mode=ro

# 終了 (子に SIGTERM)
hyoui kill "$SESS"
```

### 主な subcommand (v0.1.0)

| コマンド | 用途 |
|---|---|
| `hyoui run [--detached] [--session=ID] [--size=COLSxROWS] -- cmd args...` | PTY 起動・daemon 化 |
| `hyoui attach <session> \| --socket=PATH [--mode=rw\|ro\|rw-no-leader] [--exclusive] [--detach-others]` | 入出力中継 |
| `hyoui list` | アクティブ session を列挙 |
| `hyoui kill <session> [--signum=N]` | 子に signal 送出（default SIGTERM） |

詳細仕様は [`docs/DESIGN.md`](./docs/DESIGN.md) と [`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) を参照。

### Detach key

attach 中の `Ctrl-A D` でクライアントだけ detach（screen 互換、子は生き続ける）。
`Ctrl-A Ctrl-A` で literal Ctrl-A を子に送る。

## tmux / screen / Pexpect との違い

hyoui は **terminal multiplexer ではない**。

| | hyoui | tmux / screen | Pexpect / Expect |
|---|---|---|---|
| in-band prefix キー | **無し**（透過） | 必須（C-b / C-a） | 無し |
| window / pane | 無し（1 session = 1 PTY） | 中心機能 | 無し |
| 外側 CLI からの入力注入 | **first-class**（`send` / `keys` / `paste`、v0.2.0+） | `send-keys` あり | library から call |
| 外側からの出力待ち | **first-class**（`wait` / `tail`） | `pipe-pane` で間接的 | `expect()` |
| daemon ライフサイクル | 起動と同時、子 exit で終了 | server 常駐、複数 session | 子と心中 |
| 主用途 | スクリプト/外部 driver から触る long-running プロセス | 人が中で生活する | テスト自動化 |

要するに「TUI multiplexer の代わりではなく、tmux の中で hyoui を動かす」想定。
スクリプトから shell や REPL を **外側から駆動** する layer として位置づく。

詳しい思想は [DR-0005](./docs/decisions/DR-0005-design-philosophy-external-automation.md) を参照。

## 名前について

「憑依」— 何かが宿って一体化し、宿主は一見ふつうに見えるが内側から動かしうる。
子プロセスに付き添い、一蓮托生で生き死にし、外側からは操縦ハンドルにもなる、
というこのツールの性格を表す（[DR-0002](./docs/decisions/DR-0002-project-naming.md)）。

## Status

v0.1.0 = MVP。`run` / `attach` / `list` / `kill` + multi-attach + protocol cap
negotiation までが安定動作。`send` / `keys` / `paste` / `wait` / `tail` / `lock` /
`tx` などの自動操作 API は v0.2.0 以降で順次提供（[`docs/ROADMAP.md`](./docs/ROADMAP.md)）。

## ドキュメント

- [`docs/DESIGN.md`](./docs/DESIGN.md) — 現実装の説明（ドメイン + アーキテクチャ）
- [`docs/ROADMAP.md`](./docs/ROADMAP.md) — 将来検討項目
- [`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) — 設計判断記録 (DR)
- [`docs/journal/`](./docs/journal/) — 開発ジャーナル
- [`docs/findings/`](./docs/findings/) — PoC 知見

## License

MIT License — Yoshiaki Kawazu (@kawaz)
