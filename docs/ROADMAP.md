# hyoui ROADMAP

hyoui の将来検討項目。確定した段階リリース計画は [[DR-0007]]、本ファイルは
**未実装機能の振り分け** と **検討中アイデア** を扱う。実装着手時には個別に DR を立てる。

形式:

- バージョン括りで「いつ頃やりたいか」を表現（厳密な commitment ではない）
- 各項目に出典（DR / journal / レビュー指摘）を併記
- 完了した項目は `docs/journal/` または `docs/decisions/` に移して本ファイルから削除

## v0.1.0 残（次の minor release の前に解消したい）

| 項目 | 出典 | 備考 |
|---|---|---|
| Phase 10 完了 (lock + leader cascade + mode change) | journal 2026-05-26-night | 一部 lock state machine は v0.1.0 入り、leader cascade 完了 |
| detach key sequence の customize | journal 2026-05-26-night | 現状 `Ctrl-A D` 固定、`--detach-prefix` env / option で変更可能に |
| attach subcommand 専用 `--help` | handoff 2026-05-26 night | detach key 動作・mode 説明を専用 help に書く |
| `wait` の chunk boundary 跨ぎ needle miss 修正 | R4-H3 | 現状 best-effort、状態保持型 strip + match に置換 |

## v0.2.0（外側自動操作 API + serve gateway）

[[DR-0007]] v0.2.0 セクションが詳細。要約:

### 自動操作 CLI の本実装

- `hyoui send <session> [--file PATH | --text T]`
- `hyoui keys <session> <spec>...`（`text:` / `key:` / `wait:` / `wait-idle:` prefix）
- `hyoui paste <session> [--text|--file|--spool|--max-size|...]`（bracketed paste auto detect、in-flight state best-effort end 保証）
- `hyoui detach <session> [--all | --others]`
- `hyoui status <session> [--format text|json]`
- `hyoui tail <session> [--follow]`（daemon handler は v0.1.0 で実装済、CLI を proper に）
- `hyoui wait <session> [--idle|--text|--pattern|--then-idle] [--timeout]`（同上）
- `hyoui lock <session> [--timeout-* ...] [--mode wait|fail]`
- `hyoui unlock <session> [--token T | --force]`
- `hyoui tx <session> [--timeout-* ...] -- cmd args...`
- `hyoui completion <shell>`

### wait queue (lock の wait=true 対応)

v0.1.0 の lock は wait=true でも即 Denied 返却。v0.2.0 で proper な wait queue を実装。
出典: journal 2026-05-26-night、R4-H10

### tail / wait の正規化規約

- tail.data の chunk 境界保持（現状は buffer dump を 1 個に潰す）
- TailEnd(ChildExited) を child exit 時に tail subscriber へ broadcast
- ANSI escape strip の chunk 境界跨ぎ正規化
- 出典: journal 2026-05-26-night、R4-M27

### serve gateway

```
hyoui serve [--bind 127.0.0.1] [--port 6978] [--auth none|token|file:PATH]
            [--tls cert,key] [--static-dir PATH] [--allow-spawn]
```

- 別 crate (`crates/hyoui-serve` + `crates/hyoui-serve-cli`)
- xterm.js + WebSocket binary、protocol は v0.1.0 wire format をそのまま流す
- default port `6978`（QWERTY 物理配置で h y o u i ≒ 6 9 7 8）
- 出典: [[DR-0007]]、ユーザ確認 2026-05-26

### bounded queue / backpressure の measurement

v0.1.0 default は queue cap 8 MiB の見積もり値。実 measurement で調整。
出典: R4-M28

### 環境変数の自動継承を proper 実装

- `HYOUI_NAME` / `HYOUI_SOCK`（nest 起動検知用、子 env に注入）
- `HYOUI_LOCK_TOKEN`（lock 取得 token、tx の子に注入、全自動操作系コマンドが自動継承）
- 出典: R4-M10、[[DR-0006]] §12

## v0.2.0 候補（仕様再検討中）

着手時に DR を立てるか、本セクションから外して採用見送りにするか判断する。

### 画面 emulator + snapshot

```
hyoui snapshot <session> [--rect X,Y,W,H] [--format text|ansi|json]
hyoui wait <session> --rect X,Y,W,H --pattern R
hyoui wait <session> --cursor X,Y
```

- daemon 内に `vte` crate 等で screen grid を保持
- primary / alternate screen の別 grid 管理 + `tail --screen=primary` で分離
- 実装重 (vt100 ステート、escape sequence 完全解釈)、L0 (stream regex) で 8 割カバーできているので慎重に評価
- 出典: [[DR-0007]] v0.2.0、R4-H12 / R4-M21

### `wait --child-exit` / `--regex-on-screen`

子 process exit を待つ shortcut、screen grid 上の regex 検索。
出典: R4-M24

### record / replay

```
hyoui record <session> --output FILE                # cast format
hyoui play <session> --input FILE [--speed] [--input-only|--output-only]
```

asciinema 互換 cast format を検討。sink 概念の前段。
出典: R4-M22、[[DR-0007]] v0.3.0 sink（前倒し候補）

### Python / Node bindings

`hyoui::client::AttachClient` を pyo3 / napi-rs 経由で expose。Pexpect 代替の library
API を提供。
出典: R4-M23

## v0.3.0+

### 高度な TUI 自動化

```
hyoui wait <session> --area input-line --pattern R     # L2: named area
hyoui wait <session> --predicate-file PATH             # JSON 述語
```

config-driven area alias (`input-line`, `status-bar` 等の semantic 名)。
画面 emulator (v0.2.0 候補) に依存。
出典: [[DR-0007]] v0.3.0

### leader CLI 露出

```
hyoui leader show <session>
hyoui leader take <session>
hyoui leader give <session> <client-id>
hyoui attach <session> --as-leader
```

内部実装は v0.1.0 で完了。CLI 追加だけで動く。
出典: [[DR-0007]] v0.3.0

### tx buffered mode

```
hyoui tx <session> --buffered
```

他 client の入力を蓄積、tx 後 flush。複雑度高。
出典: [[DR-0007]] v0.3.0

### sink concept

```
hyoui sink add <session> --output FILE [--format=raw|cast] [--rotate=size:10MB,age:1h]
hyoui sink remove <session> <sink-id>
hyoui sink list <session>
```

daemon 内永続出力先。tail (ad-hoc) と区別。詳細は
`docs/issue/2026-05-26-feature-recording-and-dump.md`。

## 横断的な改善項目

時期未定だが必要。バージョン括りに上げる判断は別途。

### Architecture / refactoring

- `session.rs` 3879 行責務集約の分割（PTY / control / writer / backpressure / lock）— R4-H6
- `Transport` abstraction を daemon 側にも徹底（現状 UnixStream 前提のコードが散在）— R4-H7
- `Session::run` (Phase 8 legacy) の撤去（`Session::serve` 一本化）— R4-M1
- `handle_control_message` 311 行の分割 — R4-M2
- `Session::serve` cleanup の Drop 化 — R4-M3
- `cli.rs` 2200+ 行手書き parser の整理（clap 移行検討は別 DR）— R4-M20

### API / safety

- `token` field の `Debug` derive 漏れ（security: ログに token 漏出）— R4-H8
- 全 public enum に `#[non_exhaustive]`（前方互換）— R4-H9
- `Error` enum の `&'static str` sub-discriminator を構造化エラーに置換 — R4-H13
- struct field の `pub` → builder pattern（invariant 保護）— R4-M18
- `Transport::split` の `Send + 'static` 緩和（embed 利用向け）— R4-M19
- `Session` に `Drop` 実装（test panic 時 orphan child 防止）— R4-H4

### UX / docs

- error message に next-action hint — R4-H2
- duration format の bare 数字 reject 時の hint — R4-M5
- `hyoui run --help` option 順序の一貫性 — R4-M6
- detach key と bash readline (`Ctrl-A`) 衝突の docs 警告 — R4-M4
- session id 自動採番ルールの docs 化 — R4-M7
- DR-0008 に error code 一覧追記 — R4-M11
- error code naming の階層化 — R4-M9

### Multi-platform

- linux / macOS / WSL サポート明文化 — R4-M29
- packaging 計画（brew tap + cargo install 以外、Linux 配布 path）— R4-M25
- migration guide（v0.1.x → v0.2.0 移行手順）— R4-M30

### Test 戦略

- signal handler test の process-global state leak 対処（serialize / lazy_static lock）— R4-C4
- timing tight な threshold の relax（CI flaky 解消）— R4-H5
- `parse_duration_ms` overflow path のテスト追加 — R4-M12
- regex DoS / `size_limit` 超過の test — R4-M13
- `hyoui-cli` の `main.rs` / `daemonize.rs` に test — R4-M14
- `sys/raw.rs` の test 拡充 — R4-L6

## 関連

- [[DR-0005]] — 思想
- [[DR-0006]] — CLI ground rules（v0.2.0+ 自動操作 API の正本）
- [[DR-0007]] — MVP scope と段階リリース（本 ROADMAP の骨子）
- [[DR-0008]] — protocol（cap flags ベース schema evolution）
- `docs/issue/2026-05-26-feature-recording-and-dump.md` — sink / record / dump の発想元
