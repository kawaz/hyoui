---
title: 旧構成から回収した patch 2 件の採否 (poll_until refactor / attach marker 再現 test)
status: open
category: task
created: 2026-10-14T09:20:00+09:00
last_read: 2026-10-14T09:20:00+09:00
open_entered: 2026-10-14T09:20:00+09:00
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

# 旧構成から回収した patch 2 件の採否 (poll_until refactor / attach marker 再現 test)

リポを jj colocate に作り直した際、旧構成 (git bare + jj workspace) の main に入っていなかった commit から差分を 2 件回収した。現行 main には取り込まれていない。

## 置き場

artifacts workspace の `salvage-old-bare-20261010/` (`jj diff -r <commit> --git` の出力と由来の README)。

- URL: https://artifacts-kawaz-hyoui.kawaz-mbp16-20211217.tmpspace.net/salvage-old-bare-20261010/

| patch | 旧 commit | 内容 | 現行 main との関係 |
|---|---|---|---|
| `cb538d5b-poll-until-test-refactor.patch` | `cb538d5b` (2026-06-29、abandon 済み) | e2e テストの固定 `thread::sleep` を、観測できる条件で待つ `poll_until` helper (`crates/hyoui-cli/tests/common/pty.rs`) に置き換える (11 ファイル)。module doc に fixed sleep の使用ガイドライン (固定 sleep は観測条件が無い wait だけ、why-not-poll のコメント必須) と runner 隔離規約を足す | `poll_until` は現行に無い。対象ファイルの一部 (`stdin_eof_pipe.rs`) は現行に存在しない |
| `02947d8e-attach-marker-regression-test.patch` | `02947d8e` (2026-07-29、flaky4b workspace の head、main に入らず) | daemon の unit test helper が handshake 直後の attach redraw を読み捨て、attach 前に出た子の marker を見落とす問題の修正と、決定的再現 test `attach_after_child_output_still_observes_marker` (`crates/hyoui/src/daemon/session.rs`) | helper 側の修正は現行に別途入っている (`do_client_handshake_keep_redraw` / `read_attach_redraw`、commit `8fdaa64`)。決定的再現 test は現行に無い |

どちらも `git apply --check` では現行 main に当たらない (2026-10-14 確認)。取り込むなら内容を読んで現行のコードに書き直す。

## 判断点

- poll_until: 現行の e2e テストに固定 sleep がどれだけ残っているかを見て、helper と sleep ガイドラインを入れる価値があるか (sloppy-ai-patterns / test-integrity の方針とは合う)。入れるなら patch の置き換え箇所は参考に留め、現行のテスト群で洗い直す
- attach marker 再現 test: helper の修正は現行にあるので、残る価値は「redraw を捨てると marker を見落とす」ことを決定的に固定する回帰 test だけ。現行 helper に合わせて書き直して足すか、現行の既存 test で同じ輪郭が覆われているなら不要
- 採否が決まったら artifacts の `salvage-old-bare-20261010/` を残すか消すかも決める
