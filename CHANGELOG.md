# Changelog

hyoui のリリース履歴は **GitHub Release** と **git log** を正本とする。

- 各バージョンの release artifact / release notes: <https://github.com/kawaz/hyoui/releases>
- コミット粒度の履歴: `jj log` / `git log`
- 設計判断の経緯: `docs/decisions/` （DR-NNNN）
- 開発ジャーナル: `docs/journal/`

## 形式

本プロジェクトは [Keep a Changelog](https://keepachangelog.com/) 形式を採用していない。
バージョン bump は `just`（`justfile`）の push task が deps で走らせる version check が
強制し、tag 打ちと GitHub Release 作成は CI（`.github/workflows/release.yml`）が
**`Cargo.toml` の version 変更**を trigger に自動で行う（`release-flow-awareness` ルール参照）。
version 比較・tag 取得は [kawaz/bump-semver](https://github.com/kawaz/bump-semver) に委譲している。

そのため本ファイルは「リリース履歴の窓口」のみを記録し、詳細は GitHub Release / DR /
journal へ誘導する形にしている。

## バージョン履歴（概略）

### v0.2.3 (2026-06-02)

- `record stop` / `record stop --all` の永久 hang を修正（= 成功時の ACK 欠如が原因）

詳細: [GitHub Release v0.2.3](https://github.com/kawaz/hyoui/releases/tag/v0.2.3)。

### v0.2.2 (2026-06-02)

- `hyoui record start/stop/list` 実装（DR-0016、tty I/O timeline 録画。jsonl/raw sink、
  bounded queue + writer task で観測対象を歪めない設計、record-v1 cap）。
  **⚠ secret redaction（`--input-secrecy`）は未配線で stdin 素通し記録**
  （[DR-0016](docs/decisions/DR-0016-tty-io-record.md) の注記参照）
- `hyoui list` 表示改善（cwd / argv / clients 列追加、mtime sort、固定長 plain format、
  `--format=jsonl`、誤情報経路だった自前 timeout を撤去）
- `--index=N` を status / tail / wait / screen / lock / input family に共通展開、
  attach / kill の位置引数 index 対応
- attach 初期 redraw が外側 shell の画面 history を clear する bug を修正

詳細: [GitHub Release v0.2.2](https://github.com/kawaz/hyoui/releases/tag/v0.2.2)、
[DR-0016](docs/decisions/DR-0016-tty-io-record.md)。

### v0.2.1 (2026-05-29)

- `hyoui run` を fork daemon + attach client の合成に再定義（DR-0015、client/server 同居廃止、
  軸 2 transparent/decouple 廃止）。新 protocol message 3 個 + linger pattern +
  attach 独立 SIGTSTP/SIGCONT handler で短命子 attach race / Issue #1 系を解消
- VERSION ファイル廃止、`Cargo.toml` を version 正本化（release flow を bump-semver 系に統一）
- daemon 子 init を env JSON で渡し ps 表示をクリーン化、初期 PTY size を stdin pipe で継承
- termios を SIGTSTP/SIGCONT 跨ぎで復元して外側端末のフリーズを解消、stalled warning の
  stderr 出力（透過原則違反）を hotfix

詳細: [GitHub Release v0.2.1](https://github.com/kawaz/hyoui/releases/tag/v0.2.1)、
[DR-0015](docs/decisions/DR-0015-run-as-fork-plus-attach.md)。

### v0.2.0 (2026-05-28)

DR-0001 軸 1/2 (suspend policy) 実装、state-based wait/snapshot、input family、
lock state machine、screen state dump (scrollback / both layer)、homebrew tap
自動公開フローなど、大量の機能追加と整備。詳細:

- DR-0001 軸 1/2 wiring (ChildTransition + suspend policy fields)
- screen dump (--format=text/plain, scrollback rows config)
- state-based wait + input family
- lock state machine + token handling
- release.yml に update-homebrew job 追加 (brew tap 自動反映)
- Taskfile.pkl bump-version task で workspace 内 path 依存 version も同期

詳細: [GitHub Release v0.2.0](https://github.com/kawaz/hyoui/releases/tag/v0.2.0)、
`docs/decisions/INDEX.md`、`docs/journal/`。

### v0.1.0 (2026-05-27)

MVP: daemon ライフサイクル + multi-attach + protocol cap negotiation の土台が完成。

- `hyoui run` / `attach` / `list` / `kill` の 4 コマンド
- CBOR ハイブリッド framing protocol（[DR-0008](docs/decisions/DR-0008-protocol-design.md)）
- multi-attach + leader cascade + lock state machine
- `Ctrl-A D` detach prefix
- scrollback ring buffer

詳細: [GitHub Release v0.1.0](https://github.com/kawaz/hyoui/releases/tag/v0.1.0)、
`docs/journal/2026-05-26-night-phase10-11-release.md`、[DR-0007](docs/decisions/DR-0007-mvp-scope-and-staged-release.md)。

### v0.0.0 (2026-05-21)

Rust 一本化後の初回 release（PoC 卒業マーカー）。MoonBit + Rust FFI 二層構成から
Rust 単一実装への切り替え。

詳細: `docs/journal/2026-05-22-rust-rewrite.md`、[DR-0003](docs/decisions/DR-0003-rust-only-and-forkpty-login_tty.md)。
