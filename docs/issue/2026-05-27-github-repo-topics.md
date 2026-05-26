# GitHub repo Topics タグ設定 (R5-M34)

出典: R5-M34 (Sales R5-SAL-M7)、`docs/REVIEW-BACKLOG.md`

## 状況

GitHub repo の Topics タグが未設定で、リポ単体の SEO + 「他に類似ツールを探している人」
への発見性が低い。tmux / expect / pty まわりを探索しているユーザの導線が無い。

## やること

`gh` CLI で以下の topics を設定する (要 `kawaz` アカウント、`gh auth status` で確認):

```sh
gh repo edit kawaz/hyoui \
  --add-topic pty \
  --add-topic terminal \
  --add-topic automation \
  --add-topic cli \
  --add-topic rust \
  --add-topic daemon \
  --add-topic tmux \
  --add-topic expect \
  --add-topic claude \
  --add-topic repl \
  --add-topic interactive \
  --add-topic session-manager
```

## 確認

```sh
gh repo view kawaz/hyoui --json repositoryTopics -q '.repositoryTopics[].name'
# 上記 12 個が全部表示されれば OK
```

## なぜ手元 commit に閉じないか

repo metadata (Topics) は git の管理外で、`gh repo edit` が走らないと反映されない。
逆に言うと、コミットしても効果が無い項目なので、issue/ に残して必要なタイミングで
kawaz 本人が実行する。

## Done 条件

- [ ] 上記 12 topics が `gh repo view --json repositoryTopics` に出る
- [ ] `docs/REVIEW-BACKLOG.md` の R5-M34 を `[done]` にする
- [ ] 本 issue ファイルを削除 (jj 履歴で追える)
