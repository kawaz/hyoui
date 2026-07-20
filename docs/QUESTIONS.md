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

## 👺 WEB-Q1: gateway の置き場 — DR-0010 の「別 repo 切り出し」を上書きするか

DR-0010 は serve gateway を別 repo `kawaz/hyoui-serve` に切り出す方針を確定済み
(docs/decisions/DR-0010-v020-scope-and-serve-placement.md:60-88。理由: release cycle /
security audit boundary の分離)。今回の web gateway をどこに置くか。

- a) **同 repo に `crates/hyoui-web` を追加、DR-0010 を supersede** (AI の推し)
  — protocol/client を path 依存で直接再利用でき、breaking 期 (v1.0 未満) の共進化が速い。
  audit boundary は「gateway は認証なし・tailnet 前提」の現状要件では過剰
- b) DR-0010 通り別 repo `hyoui-serve` — core の純度は保てるが、protocol が
  毎リリース breaking する現状では追従コストが高い

## 👺 WEB-Q2: HTTP/WS の実装スタック

現状 core は tokio 不使用 (blocking + thread + crossbeam)。gateway の実装方式:

- a) **`crates/hyoui-web` に axum + tokio + tokio-tungstenite** (AI の推し)
  — WS attach・複数クライアント・将来の HTTPS/認証まで一気通貫。tokio は gateway
  crate に閉じ、core は blocking のまま (bridge は spawn_blocking or 専用 thread)
- b) tiny-http + tungstenite (blocking 継続) — 依存最小だが WS 多重化を手組みする
  ことになり第二弾で作り直しになる懸念
- c) websocketd + `hyoui attach` (DR-0010 の最軽量案) — 実装ゼロだが「セッション
  一覧ページ + screenshot + input POST」の第一弾要件を満たせない

## 👺 WEB-Q3: 第一弾の endpoint 構成 (確認)

kawaz 裁定済み事項の反映確認: port 43690 / ANSI そのまま client 側描画 (mid18) /
認証なし / HTTPS は前段 proxy。endpoint 案:

- `GET /` — セッション一覧 (socket dir 走査 + status.query、hyoui list 相当)
- `GET /sessions/:id` — セッション情報ページ (xterm.js で screen 表示 + input 欄)
- `GET /api/sessions` — 一覧 JSON
- `GET /api/sessions/:id/screen` — ANSI bytes (screen.dump.request 転写)
- `POST /api/sessions/:id/input` — input 送信 (text/key spec、hyoui input 相当)
- `WS /api/sessions/:id/attach` — 第二弾: フルターミナル (frame を WS binary に転写)

このまま進めてよいか。変更・追加あれば指摘を。
