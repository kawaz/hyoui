# Issue INDEX

active な issue の一覧。close 済みは archive/ にあり、ここには載せない。

| date | category | status | slug | 概要 |
|---|---|---|---|---|
| 2026-07-21 | bug | open | [bug-screen-dump-empty-while-tail-has-output](./2026-07-21-bug-screen-dump-empty-while-tail-has-output.md) | screen dump/snapshot が空 (seqno=0) を返す — idle 15s の stalled auto-reset が false-positive で cells 全消し |
| 2026-07-21 | request | idea | [screen-region-watch-api](./2026-07-21-screen-region-watch-api.md) | screen 仮想スクリーンの部分切り出し API + 監視エリアのマッチング検出インターフェース (DR-0025 母体、web ターミナル完了後着手) |
| 2026-07-20 | bug | open | [socket-dir-tmp-fallback-macos-cleanup](./2026-07-20-socket-dir-tmp-fallback-macos-cleanup.md) | socket dir が /tmp 固定 fallback のため macOS 定期掃除で daemon 生存中に socket file が消える |
| 2026-07-20 | bug | open | [detach-key-not-firing-keyboard-protocol](./2026-07-20-detach-key-not-firing-keyboard-protocol.md) | detach key (Ctrl-A d) が実端末で発火しない疑い — 端末 keyboard protocol (kitty CSI-u 等) 起因の可能性 |
| 2026-07-04 | bug | open | [bug-flaky-outer-token-e2e-deadline](./2026-07-04-bug-flaky-outer-token-e2e-deadline.md) | outer_token_inheritance_skips_auto_acquire が単独実行でも稀に 30s deadline fail (macOS CI flaky と同一 test) |
| 2026-07-04 | task | open | [dr0025-phase2b-raw-data-reducer](./2026-07-04-dr0025-phase2b-raw-data-reducer.md) | DR-0025 Phase 2-β — raw_data hot path の reducer→Effect→execute 化 |
| 2026-07-03 | bug | open | [bug-flaky-serve-tail-follow-tail-end](./2026-07-03-bug-flaky-serve-tail-follow-tail-end.md) | serve_tail_follow_receives_tail_end_on_child_exit が ubuntu CI で flaky fail (並走 CI では pass) |
| 2026-07-03 | bug | open | [bug-macos-ci-flaky-pty-tests](./2026-07-03-bug-macos-ci-flaky-pty-tests.md) | macos CI で PTY 系 e2e が flaky fail (outer_token_inheritance / child_inherits_session_id_env、main red の実原因) |
| 2026-07-03 | bug | open | [bug-main-unittest-hang-ubuntu-ci](./2026-07-03-bug-main-unittest-hang-ubuntu-ci.md) | hyoui-cli main.rs unit tests が ubuntu CI で hang (send_raw_bytes_partial_byte_race_regression / list_marks_stale_socket、flaky) |
| 2026-06-22 | bug | blocked | [backpressure-writer-pump-drop-sequence-deadlock](./2026-06-22-backpressure-writer-pump-drop-sequence-deadlock.md) | serve_backpressure_disconnects_slow_client が CI で 30s deadline hang する (真因未観測・調査継続) |
| 2026-06-22 | bug | blocked | [wait-scrollback-snapshot-coverage](./2026-06-22-wait-scrollback-snapshot-coverage.md) | hyoui wait の StateSnapshotRequest が scrollback を含まず viewport 外の出力を見逃す (DR-0013 Phase B 未完) |
| 2026-05-28 | design | idea | [feature-cli-restructure-discussion](./2026-05-28-feature-cli-restructure-discussion.md) | CLI 設計大改修議論 (screen view 改名 / dump top-level 化 / screen write overlay / format 整理) |
| 2026-06-16 | request | open | [feature-icanon-large-input-chunking](./2026-06-16-feature-icanon-large-input-chunking.md) | ICANON apps への大量 byte 送信時の chunk 化 helper / timeout 調整 |
| 2026-06-16 | task | open | [feature-ack-test-coverage-expansion](./2026-06-16-feature-ack-test-coverage-expansion.md) | DR-0021 ack 機構の test cover 拡張 |
| 2026-06-12 | task | open | [tcsaflush-input-discard-in-suspend-resume](./2026-06-12-tcsaflush-input-discard-in-suspend-resume.md) | TtyGuard suspend/resume/Drop の TCSAFLUSH による入力破棄の検討 |
| 2026-06-12 | bug | open | [child-spawn-sigttou-stop-race](./2026-06-12-child-spawn-sigttou-stop-race.md) | daemon 子プロセスが fork〜exec 間で停止する race (SIGTTOU/SIGTTIN 系) |
| 2026-06-12 | bug | blocked | [bug-child-stopped-flag-not-cleared](./2026-06-12-bug-child-stopped-flag-not-cleared.md) | auto-resume / 外部 SIGCONT 後も `child-state: stopped` が恒久的に下りない |
| 2026-06-11 | bug | open | [bug-vt100-zero-size-pty-panic](./2026-06-11-bug-vt100-zero-size-pty-panic.md) | PTY サイズ 0 のとき vt100 grid が subtract overflow で panic する |
| 2026-06-10 | task | blocked | [refactor-large-file-decomposition](./2026-06-10-refactor-large-file-decomposition.md) | 巨大ファイル解体 (session.rs serve_loop / main.rs / cli.rs) |
| 2026-06-10 | request | open | [feature-signal-ack](./2026-06-10-feature-signal-ack.md) | ControlMessage::Signal に成功 ack を追加する |
| 2026-06-10 | task | open | [feature-record-redaction-phase5](./2026-06-10-feature-record-redaction-phase5.md) | record secret redaction Phase 5 本実装 (DR-0016 §6) |
| 2026-06-10 | bug | open | [bug-wait-fullwidth-padding](./2026-06-10-bug-wait-fullwidth-padding.md) | wait の全角文字 padding でマッチが崩れる (screen→text 変換) |
| 2026-06-10 | bug | blocked | [bug-anchor-startup-sigttin-transient](./2026-06-10-bug-anchor-startup-sigttin-transient.md) | anchor 起動直後に子が一過性の T+ (SIGTTIN) になる瞬間がある |
| 2026-06-02 | bug | blocked | [bug-flaky-serve-propagates-child-exit-code](./2026-06-02-bug-flaky-serve-propagates-child-exit-code.md) | `serve_propagates_child_exit_code` が full workspace 並列実行時に flaky fail |
| 2026-06-01 | tech-memo | idea | [advanced-feature-jsonl-zstd-domain-dict](./2026-06-01-advanced-feature-jsonl-zstd-domain-dict.md) | hyoui dump jsonl の自分ドメイン辞書付き zstd 圧縮 (`jsonl.zst`) |
| 2026-05-27 | task | open | [readme-asciinema-cast](./2026-05-27-readme-asciinema-cast.md) | README に asciinema cast を録画・配置する |
| 2026-05-26 | request | blocked | [feature-claude-tui-automation](./2026-05-26-feature-claude-tui-automation.md) | claude code TUI 自動操作 (A/B/C 判定 + L1/L2 必須要件) |
| 2026-05-27 | task | wip | [tx-lock-unlock-cli-subcommands](./2026-05-27-tx-lock-unlock-cli-subcommands.md) | tx / lock / unlock CLI subcommand 実装 (DR-0006 §7) |
