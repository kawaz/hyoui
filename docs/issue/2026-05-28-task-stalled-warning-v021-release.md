# task: stalled warning fix を v0.2.1 として release publish

- Date: 2026-05-28
- Priority: 低-中 (= bug fix だが緊急性は低い、Issue 1/2 と合わせ release する判断もあり)
- Status: commit 済 (= 6780e13)、VERSION bump + release publish 未着手

## 現状

- commit `6780e13` で stalled warning silent 化 (= `eprintln!` 削除) 済
- CI success 確認済 (= run id 26562623652)
- ただし **VERSION bump してない** = v0.2.0 のまま
- release workflow 未 trigger = brew tap formula 古いまま (= v0.2.0)
- 実機 (= kawaz の `brew install hyoui`) は v0.2.0 のまま warning 出る

## 修正方針

```bash
cd ~/.local/share/repos/github.com/kawaz/hyoui/main
pkf run bump-version --level=patch    # v0.2.0 → v0.2.1
# → VERSION + Cargo.toml が更新される、自動 commit される (= release-flow-awareness.md 参照)

pkf run push
# → check + test + 翻訳ペア確認 + push
# → main に push されて release workflow trigger
# → v0.2.1 release publish + brew tap Formula 更新
```

kawaz 手元で反映確認:

```bash
brew update
brew upgrade kawaz/tap/hyoui   # or brew reinstall
hyoui --version   # → 0.2.1 確認
```

## 注意 — 順序判断

**Issue 1 (= termios) / Issue 2 (= SIGCONT) と合わせて v0.2.1 にする判断もあり**:

| 選択 | pros | cons |
|---|---|---|
| 単独 v0.2.1 (= stalled warning fix のみ) | 早く release、kawaz の warning ノイズ解消 | bug 込みの release が増える (= Issue 1/2 はそのまま) |
| Issue 1/2 と合わせ v0.2.1 | 重大 bug fix も含む | Issue 1/2 修正完了待ち、時間かかる |

→ 新 session で kawaz と判断。kawaz の優先度 (= warning ノイズ vs cmux freeze 修正完了) で決める。

## 関連 commit / file

- commit `6780e13` (= stalled warning silent 化)
- CI run id 26562623652 (= success 確認済)
- `~/.claude-personal/rules/release-flow-awareness.md` (= release フローの正本)
- `release.yml` (= update-homebrew job 含む、本 session で整備済)

## 検証

release 後:
```bash
gh release list --repo kawaz/hyoui --limit 5    # v0.2.1 確認
gh api repos/kawaz/homebrew-tap/contents/Formula/hyoui.rb | jq -r '.content' | base64 -d | grep -E "version|url"
brew install kawaz/tap/hyoui
hyoui --version   # 0.2.1
```
