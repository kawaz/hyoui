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

## 👺RESUME-Q1: `hyoui run` の子が self-stop した時、attach client が起こしてよいか

`notify_default_does_not_resume_self_stopped_child` (macOS Ignored job で 7/7 失敗、
ローカルでも 6/6 再現) の真因が **DR-0019 と DR-0029 の規定衝突**だったため裁定を求めます。

- **DR-0019 §3**: `on-child-suspend` の default は `notify` = 「daemon は勝手に起こさない」
- **DR-0029 §5**: `[attach] resume_on_reattach = true` (default) で、rw attach 時に
  stopped child へ resume 要求を送る
- `hyoui run` は DR-0015 で「fork daemon + **attach client**」の合成なので、
  **run した瞬間に attach 経路が発火して子を起こす**。daemon は notify を守っているが、
  同居する client が起こすので、外から見た挙動は auto-resume と区別できない

DR-0029 は自身を「DR-0019 の配置は不変、config default を足すだけ」と書いていますが、
`run` 経路では **観測可能な挙動が変わっています** (= 自己申告と実態の齟齬)。

選択肢:

- **a (推奨): product を直す** — `run` が内部生成する attach では `resume_on_reattach` を
  適用しない (= 明示的な `hyoui attach` でのみ resume する)。
  根拠: DR-0019 の「勝手に起こさない」は子の self-stop (= アプリの意図的な停止、
  `less` の SIGSTOP 等) を尊重する透過原則そのもので、`run` は「起動」であって
  「復帰意思の表明」ではない。DR-0029 §5 の意図 (= 人間が再 attach した時の UX) とも
  矛盾しない
- b: test を現状に合わせる — DR-0019 の default が実質 auto-resume になったと認め、
  test の期待値を反転する。**非推奨**: 透過原則 (DR-0005/0014) を CLI の都合で曲げる
- c: `resume_on_reattach` の default を `false` にする。
  影響範囲が a より広い (= 明示 attach の UX も変わる)

参照: `crates/hyoui-cli/src/main.rs:820-823`、`crates/hyoui/src/config/mod.rs:190`、
`docs/decisions/DR-0019-*.md` §3、`docs/decisions/DR-0029-*.md` §5、
`crates/hyoui-cli/tests/jobcontrol_auto_resume.rs:77`
