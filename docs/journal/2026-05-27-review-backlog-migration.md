# 2026-05-27 レビューバックログの再構築とリポ内移管

Round 4 の集約結果が失われかけた経緯と、backlog を `/tmp` からリポ内へ移管した記録。
以降の backlog 本体は `docs/REVIEW-BACKLOG.md` が正本。

## Round 4 — 集約結果の喪失と再構築

Round 4 は前セッション (b368f29e、2026-05-27 01:40〜02:09 JST) で
8 personas + Codex + Gemini Pro 並列レビューとして実施した。集約結果を
`/tmp` に書き戻そうとした際に `Prompt is too long` で Write が落ち、backlog が
空のままセッションが終了した。次セッション (c7988b6b) で csa の thinking ログから
集約内容を抽出し、再構築した。

外部レビュアー 2 系統はいずれも結果を得られなかった:

- **Codex**: jj リポを git として認識できず失敗
- **Gemini Pro**: RATE_LIMIT_EXCEEDED

## Round 5

同セッションで 8 ペルソナ (SRE / Kernel / Formal / Audit / Perf / POSIX / Sales /
Classic) 並列レビュー → dedup 後 95 件集約 → CRITICAL/HIGH をバッチで消化。
R5-FRM-C1 (`Session::into_parts` の ManuallyDrop) は誤指摘 (= v0.1.6 で
`Option<SessionInner>` 化済) として除外した。

## リポ内移管

backlog を `/tmp/itumono-backlog-hyoui.md` から `docs/REVIEW-BACKLOG.md` へ移管し、
リポ内で永続化した。`itumono-full-review` / `itumono-nonstop` スキルは
`/tmp/itumono-backlog-{repo}.md` 規約で参照するため、当初は `/tmp` 側を symlink で
互換維持していたが、`/tmp` は再起動で揮発するため恒久的な参照経路にはならない。
スキル側の規約をリポ内パスに寄せる改修は他リポへ波及するため別途扱う。
