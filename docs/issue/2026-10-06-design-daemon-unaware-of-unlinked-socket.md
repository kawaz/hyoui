---
title: session の socket file が消えても daemon は気付かず、誰も到達できないまま子と一緒に残り続ける
status: open
category: design
created: 2026-10-06T12:30:00+09:00
last_read: 2026-10-06T12:30:00+09:00
open_entered: 2026-10-06T12:30:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# session の socket file が消えても daemon は気付かず、誰も到達できないまま子と一緒に残り続ける

## 観測 (2026-10-06)

テストが畳み損ねた detached session が 49 個、PPID 1 で残っていた。どれも socket を置いた一時 dir (TempDir) ごと socket file が消えていたが、daemon は listen fd を握ったまま生きており、子 (bash / cat 等) も動いていた。socket file が無いので `hyoui list` / `hyoui kill --socket` のどちらからも到達できず、止める手段は pid への signal だけだった。

テスト側の漏れは `crates/hyoui-cli/tests/common/session_dir.rs` (SessionDir の drop で配下の daemon を畳む) で直した。本 issue は daemon 本体の振る舞いの方。

## 論点 (決めない)

- 到達手段を失った daemon をどう扱うか。候補: socket file を作り直す / 警告をログに出すだけ / 子ごと終わる
- 「消えた」の判定 (定期的に stat して inode を比べる、dir の変更通知を受ける等) と、false-positive の扱い。CLAUDE.md の「partial state を扱う実装の規律」により、自動で畳む方向は default にしない (子は正常に動いている)
- DR-0041 決定 3 / 4 (socket の置き場、古い socket と重複の判定) との関係
- 実害の範囲: 人が使う session の socket が消えるのは、置き場を手で消した時・tmp の掃除・DR-0041 の移行くらいで、テスト以外では稀
