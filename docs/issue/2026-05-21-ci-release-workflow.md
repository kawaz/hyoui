# CI / release ワークフローの整備

- Status: Open
- Date: 2026-05-21
- Priority: Middle（リリースを出す段階になったら必須）

## 現状

hyoui には `.github/workflows/` が無い。`Taskfile.pkl`（push 経路）は整備済みだが、
CI（push 時の自動チェック）と release ワークフローが未整備。

`release-flow-awareness` ルール上、kawaz リポは「VERSION（hyoui の場合 `moon.mod.json`
の `$.version`）変更を trigger に release workflow が起動し、workflow 自身が tag +
GH Release を作成する」標準ループに乗るべき。

## やること

1. **CI workflow** — push / PR で `pkf run ci`（lint + test + build）を回す。
   MoonBit toolchain（`moon`）と Rust toolchain のセットアップが必要。
2. **release workflow** — canonical は `kawaz/bump-semver/.github/workflows/release.yml`。
   `on: push: branches:[main] paths:[moon.mod.json]` 等を trigger に、
   `bump-semver compare gt` でバージョン検証 → build → `gh release create`。
3. ネイティブビルド成果物（`agent.exe`）の配布方法を決める。Homebrew tap 配布を
   するなら `homebrew-tap-deploy-key` ルールに従って deploy key をセットアップ。

## 関連

- canonical: `kawaz/bump-semver/.github/workflows/release.yml`
- `release-flow-awareness` ルール、`homebrew-tap-deploy-key` ルール
- docs/journal/2026-05-21-bootstrap.md（ブートストラップ時点では push タスクまでで停止）
