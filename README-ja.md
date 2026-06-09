# hyoui

> [English](./README.md) | 日本語

[![CI](https://github.com/kawaz/hyoui/actions/workflows/ci.yml/badge.svg)](https://github.com/kawaz/hyoui/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/kawaz/hyoui?include_prereleases&sort=semver)](https://github.com/kawaz/hyoui/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

**hyoui** `/ˈhjoʊi/`（ヒョーイ・憑依）— `claude` / REPL / TUI を **外側から** CLI で
駆動する。prefix キーも in-band escape も無い、PTY 透過ラッパー。

<!-- TODO(R5-H15): asciinema cast / GIF 配置予定 -->
<!-- 録画 / 配置手順は docs/issue/2026-05-27-readme-asciinema-cast.md を参照 -->
<!--
[![asciicast](https://asciinema.org/a/PLACEHOLDER.svg)](https://asciinema.org/a/PLACEHOLDER)
-->

任意のコマンドを PTY の中で起動し、内側からは透過的に振る舞いながら、
**外側から監視・自動操作するための足場**を提供する。

daemon は子 PTY を [`vt100`](https://docs.rs/vt100) ベースの screen emulator で
解釈し、**screen state の正本を持つ**。これによって attach 復元 (= alt screen
常駐 TUI の観戦も含む)、wait の現在 visible state に対する match、structured
snapshot などが安定して動作する ([DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md))。

## Who is hyoui for?

「ターミナルの中で生活する」ツールではなく、「外側から駆動する」ツール。
以下に当てはまるなら多分使い所がある:

- **claude / Claude Code を CI やスクリプトから操りたい**:
  `tmux send-keys` の quoting 地獄に疲れた、`expect` script を書きたくない
- **長時間走る LLM / REPL / TUI セッションに attach し直したい**:
  夜走らせた `claude` セッションに朝スマホから ssh で繋ぎ直す、など
- **テスト・運用スクリプトから interactive command を expect 的に駆動したい**:
  入力注入と現在画面に対する出力待ちを CLI 一本でやりたい

逆に「`Ctrl-b` を押して画面を分割して人間が生活する」用途は tmux / zellij の仕事。
hyoui は **tmux の中で動かす** 想定。

## 何ができる

`hyoui run -- <cmd>` でコマンドを PTY 内で起動して daemon 化し、別プロセスから
`hyoui attach` / `hyoui list` / `hyoui kill` で操作する。`hyoui` 自身は子に対して
何も介入せず（in-band escape 一切なし）、入出力は完全透過。代わりに **CLI / 将来の HTTP
gateway 経由で外側から制御** する。

主な用途:

- long-running な対話プロセス（例: `claude` / REPL / `ssh` / TUI app）を起動して、
  あとから何度でも attach / detach する。attach 時は daemon が screen state から
  画面を再描画するので、alt screen 常駐の TUI も画面崩壊なしで再現される
- CI / スクリプトから入力注入と出力待ちで自動操作する（`hyoui input` の
  `text:` / `key:` / `paste:` / `wait:` / `wait-idle:` spec、`hyoui wait`、
  `hyoui screen dump` / `screen snapshot`、`hyoui record`、`hyoui lock`）
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

### Homebrew

```bash
brew install kawaz/tap/hyoui
```

> formula は release のたびに `release.yml` が
> [`kawaz/homebrew-tap`](https://github.com/kawaz/homebrew-tap) へ自動公開する。

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

attach は daemon の screen state から 1 frame で画面を再描画する
([DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md) §4 Phase A) ので、claude TUI のような alt screen 常駐アプリも
detach 直前の状態が綺麗に復元される。

### 自動操作: input / wait / screen / lock

```bash
# direct text と key の組み合わせ
hyoui input "$SESS" "text:ls -la" "key:Enter"

# 現在 visible state に対する match 待ち (= 過去 redraw の誤マッチが起きない)
hyoui input "$SESS" "wait:^Continue\\?" "key:Enter"

# binary 制御文字 (= ESC[A = Up arrow)
hyoui input "$SESS" "hex:1b5b41"

# multi-line script を bracketed paste で
hyoui input "$SESS" "paste:$(cat script.py)"

# 単独 wait (= 現在の visible state に対する regex マッチ、timeout つき)
hyoui wait "$SESS" "^\\$" --timeout=10s

# 画面 dump (= ANSI bytes、terminal で cat 再生可)
hyoui screen dump "$SESS"
hyoui screen dump "$SESS" --layer=both --rect=0,0,80,5

# 構造化 snapshot (= wire 上は CBOR encoded StateSnapshotResponse)
# 注: --format=json は forward-compat で未配線 (= 現状の出力は CBOR)。
#     jq に流す前に CBOR デコーダを通すこと。
hyoui screen snapshot "$SESS" --include=Cursor
hyoui screen snapshot "$SESS" --include=Cells,Cursor,Mode

# 排他取得 (= 他 client が強制 ro、自分は leader 強制昇格)
hyoui lock acquire "$SESS" --timeout=30s
hyoui lock release "$SESS"

# tty I/O timeline をファイルに録画 (= jsonl)
# ⚠ stdin の redaction はまだ未配線 — --input-secrecy の値に関わらず stdin は
#   素通しで記録される。secret を打つ可能性があるなら --stdout のみに限定する。
hyoui record start "$SESS" --output session.jsonl --both
hyoui record list "$SESS"
hyoui record stop "$SESS" --all

# raw bytes を grep / 保存したい時は tail
hyoui tail "$SESS" --last-bytes=4096
# strict variant: 要求した window が scrollback から evict 済なら fail
hyoui tail "$SESS" --since=10s --since-strict
```

### 主な subcommand

| コマンド | 用途 |
|---|---|
| `hyoui run [--detached] [--session=ID] [--size=COLSxROWS] -- cmd args...` | PTY 起動・daemon 化 |
| `hyoui attach <session> [--mode=rw\|ro\|rw-no-leader]` | 入出力中継 (= screen state から画面復元) |
| `hyoui list` | アクティブ session を列挙 |
| `hyoui kill <session> [--signal=NUM_OR_NAME]` | 子に signal 送出（default SIGTERM、name / number 両対応。例 `--signal KILL` / `--signal 9`） |
| `hyoui status <session>` | session 状態表示 (= clients / leader / lock / scrollback) |
| `hyoui input <session> <spec>...` | 入力注入 (= `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:` spec) |
| `hyoui wait <session> <pattern>` | 現在 visible state に対する regex match 待ち |
| `hyoui screen dump <session>` | 画面 ANSI dump (= terminal で cat 再生可) |
| `hyoui screen snapshot <session>` | 構造化 state snapshot (= JSON / CBOR) |
| `hyoui lock acquire\|release <session>` | 排他制御 (= 自動操作の atomic 性。`unlock` は `lock release` の alias、`tx` は未実装) |
| `hyoui record start\|stop\|list <session>` | tty I/O timeline を永続録画 (= jsonl / raw)。**⚠ stdin redaction は未配線** |
| `hyoui tail <session>` | raw bytes stream (= log / grep / asciinema 前段) |

詳細仕様は [`docs/DESIGN.md`](./docs/DESIGN.md) と
[`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) を参照。

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
| daemon が screen state 正本 | **◯** (vt100 base) | × | × | × | × | × |
| 外側 CLI からの入力注入 | **first-class** (`input` family) | × | × | library から call | ブラウザ経由 | 録画のみ |
| 現在 visible state に対する待ち合わせ | **first-class** (`wait` state-based) | × | × | `expect()` (= 子 PTY stream regex) | × | × |
| 構造化 snapshot / dump | **first-class** (`screen dump` / `screen snapshot`) | × | × | × | × | × |
| 録画 / replay | **record 出荷済**（`record start/stop/list`、jsonl/raw timeline）/ replay 追加予定 | × | × | × | × | 中心機能 |
| HTTP / ブラウザ gateway | 追加予定 (= `kawaz/hyoui-serve`) | × | × | × | 中心機能 | replay のみ |
| daemon ライフサイクル | 起動と同時、子 exit で終了 | session manager 型 | 永続 server | 子と心中 | server 型 | N/A |

要するに「**1 daemon 1 session の透過 PTY ラッパー** に対して、**daemon 側で
screen state を正本管理**し、**外側から自動操作するための CLI / HTTP API** を
first-class で乗せる」のが hyoui の独自ポジション。
abduco / dtach は外側操作 API が無く、shpool は server 永続型、ttyd はブラウザ前提、
expect は library。「`expect` の使いやすさ」「`abduco` の attach 体験」「`ttyd` の遠隔操作」
の 3 つを CLI 一本で揃えるのが目標。

詳しい思想は [DR-0005](./docs/decisions/DR-0005-design-philosophy-external-automation.md)、
screen state 正本化と attach 復元の仕組みは
[DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md) を参照。

## 名前について

「憑依」— 何かが宿って一体化し、宿主は一見ふつうに見えるが内側から動かしうる。
子プロセスに付き添い、一蓮托生で生き死にし、外側からは操縦ハンドルにもなる、
というこのツールの性格を表す（[DR-0002](./docs/decisions/DR-0002-project-naming.md)）。

## Status

v0.1.x = **外側 API 確立期**。

- `run` / `attach` / `list` / `kill` + multi-attach + protocol cap negotiation:
  v0.1.0 で安定動作
- screen emulator 採用 + attach handshake redraw + state-based wait /
  snapshot / dump: [DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md) Phase A/B で完了 (= **claude TUI 観戦 / 自動操作
  の核となる機能が動作する状態**)
- input family (= `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` /
  `wait-idle:` spec) と `lock` / `unlock` の本実装も完了済（`tx` は未実装）
- `hyoui record start/stop/list` (= tty I/O timeline、jsonl/raw) は v0.2.2 で出荷。
  ただし **stdin redaction (`--input-secrecy`) はまだ未配線**

**production readiness**:

- 動作確認済 platform: Linux x86_64 / aarch64, macOS Intel / Apple Silicon
- breaking change policy: **v0.x の間は minor bump で breaking 可**
  （API が固まるまで snake oil を売らない方針）
- production 利用: kawaz 自身が `claude` 駆動の daily-driver として使用
  (= eat your own dogfood)。業務運用にはまだ self-test 推奨
- **production stable の目安**: `serve` gateway (= 別 repo `kawaz/hyoui-serve`)
  公開と remaining 機能 (= record redaction 配線、replay、observability、L2 wait 等) の確立後

ロードマップ詳細: [`docs/ROADMAP.md`](./docs/ROADMAP.md)。

## ドキュメント

- [`docs/DESIGN.md`](./docs/DESIGN.md) — 現実装の説明（ドメイン + アーキテクチャ）
- [`docs/MANUAL-ja.md`](./docs/MANUAL-ja.md) — エンドユーザ向けレシピ集
- [`docs/ROADMAP.md`](./docs/ROADMAP.md) — 将来検討項目
- [`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) — 設計判断記録 (DR)
- [`docs/journal/`](./docs/journal/) — 開発ジャーナル
- [`docs/findings/`](./docs/findings/) — PoC 知見

## 質問 / Issue

- バグ・想定外挙動の報告:
  [bug report テンプレート](./.github/ISSUE_TEMPLATE/bug_report.md) を使って issue 起票。
- 機能要望:
  [feature request テンプレート](./.github/ISSUE_TEMPLATE/feature_request.md) を使って issue 起票。
- 純粋な質問・相談:
  まず [Discussion](https://github.com/kawaz/hyoui/discussions) で。

## License

MIT License — Yoshiaki Kawazu (@kawaz)
