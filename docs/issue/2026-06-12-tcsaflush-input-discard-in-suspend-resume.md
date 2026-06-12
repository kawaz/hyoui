# TtyGuard suspend/resume/Drop の TCSAFLUSH による入力破棄の検討

- Date: 2026-06-12
- Status: open
- Priority: 低 (= 実害の観測例なし、構造的に同種の問題があることの記録)
- Origin: Fable review M4 の root cause 調査 (= `enter_raw` の TCSAFLUSH 入力破棄)

## 内容

M4 で `enter_raw` の `TCSAFLUSH` → `TCSANOW` 修正を行った
(= TCSAFLUSH は未読の入力 queue を破棄するため、attach 起動シーケンスの cooked 窓に
届いた detach key が消える回帰の root cause だった。`sys/tty.rs` の
`enter_raw_preserves_input_queued_during_cooked_mode` 参照)。

`TtyGuard` には同じ `TCSAFLUSH` を使う箇所が残っている:

- `suspend()` — SIGSTOP 前の cooked 復元 (= raw 中の未読入力を破棄して停止)
- `resume()` — SIGCONT 後の raw 再設定 (= **suspend 中に打たれた入力を破棄**)
- `Drop` — detach / exit 時の termios 復元

特に `resume()` は「follow で停止 → 外側 fg で復帰」の窓に打たれた入力が破棄される
構造で、`enter_raw` と同型 (= cooked 窓の入力消失)。ただし:

- suspend/resume の窓はユーザが意図的に停止している期間で、復帰前の入力破棄は
  「停止中に押したキーが復帰後に化けて流れる」事故を防ぐ意図とも解釈できる
  (= screen/tmux の挙動とどう揃えるべきか要調査)
- 実害の観測例はまだ無い

## 実装時の論点

- screen / tmux の SIGCONT 復帰時の input queue 取り扱いを調査して揃える
- 「停止中の入力を子に流すべきか捨てるべきか」は透過原則 (DR-0005/0014) で判断
- 変更するなら enter_raw と同じ regression test パターン (cooked 窓に bytes を
  queue して切替後に読めるか) で固定する

## 関連

- DR-0020 §5 / Fable review M4 (2026-06-12)
- `crates/hyoui/src/sys/tty.rs` — enter_raw の TCSANOW 化 + regression test
