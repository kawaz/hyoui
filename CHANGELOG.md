# Changelog

hyoui のリリース履歴は **GitHub Release** と **git log** を正本とする。

- 各バージョンの release artifact / release notes: <https://github.com/kawaz/hyoui/releases>
- コミット粒度の履歴: `jj log` / `git log`
- 設計判断の経緯: `docs/decisions/` （DR-NNNN）
- 開発ジャーナル: `docs/journal/`

## 形式

本プロジェクトは [Keep a Changelog](https://keepachangelog.com/) 形式を採用していない。
バージョン bump と tag 打ちは CI（`.github/workflows/release.yml`）が VERSION ファイル
変更を trigger に自動で行う（`release-flow-awareness` ルール参照）。

そのため本ファイルは「リリース履歴の窓口」のみを記録し、詳細は GitHub Release / DR /
journal へ誘導する形にしている。

## バージョン履歴（概略）

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
- CBOR ハイブリッド framing protocol（[[DR-0008]]）
- multi-attach + leader cascade + lock state machine
- `Ctrl-A D` detach prefix
- scrollback ring buffer
- 208 tests pass

詳細: [GitHub Release v0.1.0](https://github.com/kawaz/hyoui/releases/tag/v0.1.0)、
`docs/journal/2026-05-26-night-phase10-11-release.md`、[[DR-0007]]。

### v0.0.0 (2026-05-21)

Rust 一本化後の初回 release（PoC 卒業マーカー）。MoonBit + Rust FFI 二層構成から
Rust 単一実装への切り替え。

詳細: `docs/journal/2026-05-22-rust-rewrite.md`、[[DR-0003]]。
