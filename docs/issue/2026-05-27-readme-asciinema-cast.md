---
title: "README に asciinema cast を録画・配置する"
status: open
category: task
created: 2026-05-27T00:00:00+09:00
last_read:
open_entered: 2026-05-27T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: R5 marketing review (Sales ペルソナ) で「README に動いてる感がゼロ」と指摘
---

# README に asciinema cast を録画・配置する

- Priority: High (営業面、R5-SAL-C1 / R5-H15)

## 背景

README L1-L20 を読み終えても「実際に何が画面上で起きるのか」が分からない。
初見ユーザの離脱率を下げるために、tagline 直下に **30 秒前後の asciinema cast** を
1 本配置したい。録画は v0.1.x の `run` / `attach` / `detach` / `list` で十分。

README 内の placeholder コメントは既に配置済 (`<!-- TODO(R5-H15): ... -->`)。
asciinema URL が確定したら埋め込みコメントを有効化するだけ。

## やること

### 1. シナリオを決める (= 5 秒で差別化が伝わる動作)

R5-SAL-H5 の提案を踏襲する案 (約 30 秒):

```bash
# Terminal A: claude セッションを detached で起動 (= demo の主役)
hyoui run --detached --session=work -- claude

# Terminal B (or 別 ssh / 別 device):
hyoui list
hyoui attach work
# claude と数往復、Ctrl-A D で detach
# detach 後も claude は生き続ける (= hyoui list で確認)
hyoui list
```

ポイント:
- "long-running な claude セッションに、外側から後で attach できる" を見せる
- bash を wrap するだけの地味な demo にしない (= 差別化点を 5 秒で実感させる)
- claude の応答待ち時間が長いと退屈なので、軽い prompt 1 つに絞る

### 2. 録画

```bash
brew install asciinema  # 未導入なら
asciinema rec hyoui-demo.cast
# 上記シナリオを実演
# Ctrl-D で録画終了
asciinema play hyoui-demo.cast  # 確認
```

録画は等幅・80 cols 程度・terminal は light/dark どちらでも可だが、
README で見栄えが良い方を選ぶ (= 後で実際に embed して確認)。

### 3. asciinema.org にアップロード

```bash
asciinema upload hyoui-demo.cast
# 認証 (初回のみ): asciinema auth で出る URL を browser で開いて github 連携
# 公開設定: public
# 出力された https://asciinema.org/a/<ID> を控える
```

または **自前 host** したい場合は `*.cast` ファイルを repo 内 (e.g.
`docs/assets/hyoui-demo.cast`) に置き、asciinema-player を README で読み込む形も可。
営業面では asciinema.org embed のほうが「外部 service に投稿している」signal が
出て信頼度が上がるのでそちらを優先。

### 4. README に埋め込み

README.md / README-ja.md の `<!-- TODO(R5-H15): ... -->` placeholder コメントを
削除し、その位置に以下を有効化:

```markdown
[![asciicast](https://asciinema.org/a/<ID>.svg)](https://asciinema.org/a/<ID>)
```

`<ID>` を控えた cast ID で置換。両言語版で同じ ID を使う (= 翻訳作業ではなく
共通 asset)。

### 5. 完了後の後処理

- [ ] 本 issue を delete (= placeholder 用 issue は役目を終える)
- [ ] `docs/journal/YYYY-MM-DD-<slug>.md` に「cast を録画した、シナリオはこう、
      ID はこれ、撮り直し手順」を残す (= 後で撮り直したくなった時のため)

## 注意

- asciinema cast に **API key / token / 個人情報** が映り込まないように注意
  (= `hyoui list` の出力に socket path が出るが、`/tmp/hyoui-<user>-...` 程度なので
  問題ないはず。念のため `$HOME` が見えていないか確認)
- 録画中に間違えたら撮り直し OK。1 本完璧な録画を目指す
- GIF 化 (= terminalizer / asciicast2gif) は README で **embed sizing が制御
  しづらい** ので、asciinema.org の SVG link 形式を優先

## ロゴ (optional)

R5-SAL-C1 ではロゴ画像も「あれば良い」とされているが、本 issue では対象外。
cast 配置を優先し、ロゴは別 issue として後で立てる。
