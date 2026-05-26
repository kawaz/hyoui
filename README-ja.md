# hyoui

> [English](./README.md) | 日本語

**hyoui** `/ˈhjoʊi/`（ヒョーイ・憑依）— `claude` / REPL / TUI を **外側から** CLI で
駆動する。prefix キーも in-band escape も無い、PTY 透過ラッパー。

<!-- TODO(R5-H15): asciinema cast / GIF 配置予定 -->
<!-- 録画 / 配置手順は docs/issue/2026-05-27-readme-asciinema-cast.md を参照 -->
<!--
[![asciicast](https://asciinema.org/a/PLACEHOLDER.svg)](https://asciinema.org/a/PLACEHOLDER)
-->

任意のコマンドを PTY の中で起動し、内側からは透過的に振る舞いながら、
**外側から監視・自動操作するための足場**を提供する。

## Who is hyoui for?

「ターミナルの中で生活する」ツールではなく、「外側から駆動する」ツール。
以下に当てはまるなら多分使い所がある:

- **claude / Claude Code を CI やスクリプトから操りたい**:
  `tmux send-keys` の quoting 地獄に疲れた、`expect` script を書きたくない
- **長時間走る LLM / REPL / TUI セッションに attach し直したい**:
  夜走らせた `claude` セッションに朝スマホから ssh で繋ぎ直す、など
- **テスト・運用スクリプトから interactive command を expect 的に駆動したい**:
  入力注入 (`send`) と出力待ち (`wait`) を CLI 一本でやりたい

逆に「`Ctrl-b` を押して画面を分割して人間が生活する」用途は tmux / zellij の仕事。
hyoui は **tmux の中で動かす** 想定。

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

### Pre-built binaries (GitHub Releases)

```bash
# 最新 release から自分の platform 用 binary を取得
# https://github.com/kawaz/hyoui/releases/latest
```

Linux x86_64 / aarch64, macOS Intel / Apple Silicon の binary を提供。

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

### Homebrew (planned)

```bash
brew install kawaz/tap/hyoui
```

> tap への formula 公開は準備中（[`docs/issue/2026-05-27-homebrew-tap-deploy-key.md`](./docs/issue/2026-05-27-homebrew-tap-deploy-key.md)）。
> それまでは上記 3 経路を使う。

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

## 既存ツールとの違い

hyoui は **terminal multiplexer ではない**。「人が中で生活する系」と「外側から
制御する系」で 2 段に整理する。

### 人が中で生活する系（hyoui の競合ではない、組み合わせて使える）

| | hyoui | tmux / screen | zellij |
|---|---|---|---|
| in-band prefix キー | **無し**（透過） | 必須（C-b / C-a） | 必須（C-p / C-q 等） |
| window / pane | 無し（1 session = 1 PTY） | 中心機能 | 中心機能 |
| 主用途 | 外部から駆動 | 人間が中で生活 | 人間が中で生活 |

→ 「tmux の中で hyoui を動かす」「zellij の pane で hyoui run する」が想定。

### 外側から制御する系（ここが hyoui の領域）

| | hyoui | abduco / dtach | shpool | Pexpect / Expect | ttyd / gotty | asciinema |
|---|---|---|---|---|---|---|
| 1 daemon 1 session モデル | ◯ | ◯ | ◯ | × | × | × |
| 外側 CLI からの入力注入 | **first-class**（v0.2.0+） | × | × | library から call | ブラウザ経由 | 録画のみ |
| 外側からの出力待ち | **first-class**（`wait` / `tail`） | × | × | `expect()` | × | × |
| 録画 / replay | v0.2.0+ で計画 | × | × | × | × | 中心機能 |
| HTTP / ブラウザ gateway | v0.2.0+ で計画（`serve`） | × | × | × | 中心機能 | replay のみ |
| daemon ライフサイクル | 起動と同時、子 exit で終了 | session manager 型 | 永続 server | 子と心中 | server 型 | N/A |

要するに「**1 daemon 1 session の透過 PTY ラッパー** に対して、**外側から自動操作するための
CLI / HTTP API** を first-class で乗せる」のが hyoui の独自ポジション。
abduco / dtach は外側操作 API が無く、shpool は server 永続型、ttyd はブラウザ前提、
expect は library。「`expect` の使いやすさ」「`abduco` の attach 体験」「`ttyd` の遠隔操作」
の 3 つを CLI 一本で揃えるのが目標。

詳しい思想は [DR-0005](./docs/decisions/DR-0005-design-philosophy-external-automation.md) を参照。

## 名前について

「憑依」— 何かが宿って一体化し、宿主は一見ふつうに見えるが内側から動かしうる。
子プロセスに付き添い、一蓮托生で生き死にし、外側からは操縦ハンドルにもなる、
というこのツールの性格を表す（[DR-0002](./docs/decisions/DR-0002-project-naming.md)）。

## Status

v0.1.x = **外側 API 確立期**。`run` / `attach` / `list` / `kill` + multi-attach +
protocol cap negotiation までが安定動作。

**production readiness**:

- 動作確認済 platform: Linux x86_64 / aarch64, macOS Intel / Apple Silicon
- breaking change policy: **v0.x の間は minor bump で breaking 可**
  （API が固まるまで snake oil を売らない方針）
- production 利用: **v0.1.x はまだ非推奨**。kawaz 自身が `claude` 駆動の
  daily-driver として使用 (= eat your own dogfood) しているが、業務運用には
  self-test 推奨
- **production stable は v0.2.0+ 予定**: `serve` gateway 公開と自動操作 API
  (`send` / `keys` / `paste` / `wait` / `tail` / `lock` / `tx`) の確立が条件

ロードマップ詳細: [`docs/ROADMAP.md`](./docs/ROADMAP.md)。

## ドキュメント

- [`docs/DESIGN.md`](./docs/DESIGN.md) — 現実装の説明（ドメイン + アーキテクチャ）
- [`docs/ROADMAP.md`](./docs/ROADMAP.md) — 将来検討項目
- [`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) — 設計判断記録 (DR)
- [`docs/journal/`](./docs/journal/) — 開発ジャーナル
- [`docs/findings/`](./docs/findings/) — PoC 知見

## License

MIT License — Yoshiaki Kawazu (@kawaz)
