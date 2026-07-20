# QUESTIONS — 裁定待ちキュー

> 運用規約 (self-contained):
> - 現在ユーザ裁定を待っている質問だけを索引として並べる (経緯は git log と各記録先が担う)
> - ラベルはバッチ毎に一意な prefix (例: `LIST-Q1`, `TSTP-Q1`)。Qn の使い回し禁止
> - 各 Q は「質問 1〜2 行 + 選択肢 + AI の推し + 根拠 1 文 + 参照 (相対パス / 節)」
> - 提示と同一ターンで本 file を path 指定 commit、push はリリース窓に同乗
> - 裁定が下りた Q は本 file から削除、裁定内容は DR / issue / journal / close_reason へ反映
> - チャットは「LIST-Q1 待ち」等のラベル参照だけで済ませ、質問の正本は本 file が持つ
> - 説明要求 (「詳しく」「それ何」) が来たら本 file 内に「### 背景説明」等を追記して再提示、
>   TL に長文を流さない

---

## 👺 UPG-Q1: graceful upgrade の state 一時ファイル format

- a) **CBOR** (AI の推し) — protocol と同じ ciborium 基盤で依存追加なし、bytes 埋め込みが自然
- b) JSON — 人間可読だが bytes 表現が base64 になり、serde_json 依存追加
- 参照: docs/decisions/DR-0028-daemon-graceful-upgrade-self-exec.md §state 引き継ぎ

## 👺 UPG-Q2: exec 中の新規 attach 接続の扱い

- a) **listener backlog に任せる** (AI の推し) — exec の空白は数十 ms、backlog が自然に吸収、介入最小
- b) exec 前に accept 停止 — 明示的だが実装が増える割に得るものが薄い
- 参照: 同 DR §attach client の扱い

## 👺 UPG-Q3: upgrade 受理時の pending input の扱い

- a) **upgrade 受理後は新規 input を reject し、送信中 (ack 待ち) の drain を待ってから exec** (AI の推し) — DR-0021 の「PTY drain が完了点」の意味論を upgrade 跨ぎでも保存
- b) lock 運用で回避 (upgrade 前に外部から lock 取得を推奨するだけ) — 機構なしだが保証もなし
- 参照: 同 DR §Open Questions
