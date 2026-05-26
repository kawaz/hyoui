# v0.x release deployment checklist

> Status: Active
> Date: 2026-05-27
> Related: [[R5-H13]] (cargo audit/deny)、[[R5-H14]] (SLSA attestation/SHA256SUMS)

## 症状 (= 適用タイミング)

- v0.x の release tag を打つ直前 (= `pkf run bump-version` 実行後)
- brew tap (`kawaz/homebrew-tap`) の Formula が release artifact を
  参照するため、artifact の integrity / 配布形が壊れていると brew install
  が壊れる
- 既存ユーザの自動更新 (= `brew upgrade`) が爆発するのを防ぐ目的

## 切り分け (= release 前の事前確認)

1. **VERSION ファイルが bump 済か**:
   ```bash
   cat VERSION
   git/jj log -- VERSION | head -3   # 直近で bump commit があるか
   ```
2. **CHANGELOG.md が最新か**:
   - 新規 commit (= 前 tag からの差分) が CHANGELOG に反映されているか
   - `## [Unreleased]` セクションが当該 version に rename されているか
3. **CI が main で green か**:
   ```bash
   gh run list --branch main --limit 5
   ```
4. **cargo audit / cargo deny がパスしているか** (= R5-H13 対応後):
   ```bash
   cargo audit
   cargo deny check
   ```
5. **README / DESIGN の翻訳ペアが揃っているか**:
   ```bash
   pkf run docs:check-translations    # ja vs en の commit lag を見る
   ```

## 対処 (= release flow 本体)

1. **bump-version** (= 通常 patch、breaking なら minor):
   ```bash
   pkf run bump-version            # default level=patch
   # or
   pkf run bump-version -- minor
   ```
2. **push して release workflow を発火**:
   ```bash
   pkf run push                    # 翻訳ペア / version gate / lint test を deps で通す
   ```
3. **release.yml の進捗を watch**:
   ```bash
   run_id=$(gh run list --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')
   gh run watch "$run_id"
   ```
4. **artifact の integrity を検証** (= R5-H14 対応後):
   ```bash
   # SHA256SUMS が公開されている場合
   gh release download v$(cat VERSION) -p 'SHA256SUMS*' -p '*.tar.gz'
   shasum -a 256 -c SHA256SUMS
   # SLSA attestation が添付されている場合
   gh attestation verify hyoui-*.tar.gz --repo kawaz/hyoui
   ```
5. **brew tap への自動反映を確認**:
   ```bash
   gh run list --repo kawaz/homebrew-tap --limit 5
   # Formula が更新されているか
   curl -fsSL https://raw.githubusercontent.com/kawaz/homebrew-tap/main/Formula/hyoui.rb | grep version
   ```
6. **smoke test**:
   ```bash
   brew update && brew upgrade kawaz/tap/hyoui
   hyoui --version              # 新 version が出るか
   hyoui run smoke-test bash -c 'echo hello'
   hyoui list
   ```

## 予防 (= release を壊さないための日常運用)

- **bump-version は CI/CD に任せる** ([[release-flow-awareness]] ルール):
  - 人 / agent は `git tag` / `jj tag` / `gh release create` を直接叩かない
  - VERSION 更新 commit を main に push したら release workflow が自動で
    tag + GH Release を作成する
- **cargo audit / deny は PR gate に入れる** (R5-H13 対応):
  - main に merge する前に yanked / RUSTSEC を捕捉
- **SHA256SUMS + SLSA attestation を release artifact に必ず添付** (R5-H14):
  - MITM 検知 / supply chain attack 検知に必須
- **brew tap の deploy key が有効か**:
  - `gh repo deploy-key list --repo kawaz/homebrew-tap | grep 'hyoui release'`
  - kawaz の personal rules `homebrew-tap-deploy-key.md` 参照
- **release notes の draft を事前に書く**:
  - `gh release create` が auto-generate するが、breaking change / migration
    手順は手動追記する

## 関連

- [[R5-H13]] — cargo audit / cargo deny を CI に追加
- [[R5-H14]] — SLSA attestation + SHA256SUMS の release artifact 添付
- [[R5-H17]] — brew install path (= primary install) の完成
- kawaz personal rules `release-flow-awareness.md` — tag/release は CI の仕事
- kawaz personal rules `homebrew-tap-deploy-key.md` — tap への自動 push 経路
- `Taskfile.pkl` — `pkf run push` / `pkf run bump-version` の定義
