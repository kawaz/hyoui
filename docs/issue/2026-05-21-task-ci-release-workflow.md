# CI / release ワークフローの整備

- Status: Open
- Date: 2026-05-21 (2026-05-22 Rust 一本化を反映)
- Priority: Middle（リリースを出す段階になったら必須）

## 現状

hyoui には `.github/workflows/` が無い。`Taskfile.pkl`（push 経路）は段階 6 で
Rust 用に作り直す予定。CI（push 時の自動チェック）と release ワークフローが未整備。

`release-flow-awareness` ルール上、kawaz リポは「VERSION（hyoui の場合
`Cargo.toml` の `[workspace.package].version`）変更を trigger に release workflow
が起動し、workflow 自身が tag + GH Release を作成する」標準ループに乗るべき。

## やること

1. **CI workflow** (`.github/workflows/ci.yml`) — push / PR で `pkf run ci`
   (lint + test + build) を回す。canonical = `kawaz/template-rust/.github/workflows/ci.yml`。
   matrix は `ubuntu-latest` + `macos-latest`（PTY/termios の OS 差異があるので両方必須）。
   - `cargo fmt --check`
   - `cargo clippy --workspace -- -D warnings`
   - `cargo build --workspace`
   - `cargo test --workspace`
   - unsafe 封じ込め検証 grep (`sys/raw.rs` と `sys/signal.rs` を除外して `unsafe` が 0 件であること)
2. **release workflow** (`.github/workflows/release.yml`) — canonical は
   `kawaz/template-rust/.github/workflows/release.yml`。
   `on: push: branches:[main] paths:[Cargo.toml]` を trigger に、
   `bump-semver compare gt` でバージョン検証 → 各ターゲット build → `gh release create`。
   - matrix: `x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu` /
     `x86_64-apple-darwin` / `aarch64-apple-darwin` 等
   - artifact: `hyoui-<target>.tar.gz`
3. ネイティブビルド成果物（`hyoui`）の配布方法を決める。Homebrew tap 配布を
   するなら `homebrew-tap-deploy-key` ルールに従って deploy key をセットアップ。

## 関連

- canonical: `kawaz/template-rust/.github/workflows/{ci,release}.yml`、`kawaz/bump-semver/Taskfile.pkl`
- `release-flow-awareness` ルール、`homebrew-tap-deploy-key` ルール
- docs/journal/2026-05-22-rust-rewrite.md（Rust 一本化の経緯）
