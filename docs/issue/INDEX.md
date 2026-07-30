# Issue INDEX

active な issue の一覧。close 済みは archive/ にあり、ここには載せない。

| date | category | status | slug | 概要 |
|---|---|---|---|---|
| 2026-07-30 | bug | wip | [web-terminal-font-load-fit-race](./2026-07-30-bug-web-terminal-font-load-fit-race.md) | webfont 読み込み前の fallback セル寸法で初回 fit が固定され、font load 完了だけでは再測定されない (font race)。加えて `embed=1` 縮小時は CSS でなく xterm.js reflow で複数行化する (embed shrink reflow)、また persistent WS bridge が resize leader を握り続けるため resize POST が 204 を返しても daemon に反映されない (resize 204 偽成功、`ws_attach.rs` / `lib.rs::resize_blocking` が原因)。3 者は独立原因、v0.9.25 と現行で同一再現し最近の regression ではない |
| 2026-07-29 | bug | wip | [ctrlz-guard-bypassed-by-keyboard-protocol](./2026-07-29-bug-ctrlz-guard-bypassed-by-keyboard-protocol.md) | DR-0029 §2 の Ctrl+Z ガードが keyboard protocol 有効端末 (Ghostty × claude) で完全不発。子が `\x1b[>1u` / `\x1b[>4;2m` を出すと外側端末が Ctrl+Z を CSI-u (`\x1b[122;5u`) で送るため、0x1a しか見ないガードを素通りしていた。**3 符号化対応に拡張して修正済 + ネスト実機でマトリクス再検証済**、残るは kawaz の実端末での最終確認 |
| 2026-07-29 | request | open | [web-narrow-symbol-fallback-font](./2026-07-29-request-web-narrow-symbol-fallback-font.md) | 記号グリフ幅対策の unicode-range fallback が macOS/iOS でしか効かない (Linux は DejaVu が全角)。narrow symbol subset webfont 同梱が本筋。① U+2460 は macOS にも narrow グリフが無く未解決 |
| 2026-07-26 | bug | open | [ignored-tests-job-permanently-red](./2026-07-26-bug-ignored-tests-job-permanently-red.md) | CI の ignored-tests job が continue-on-error で恒常 red を隠している (ubuntu は backpressure の 31s hang = 別 issue で blocked。macOS の notify_default_* は DR-0030 の test 置換で解消済みと 2026-07-29 調査で確定、現在の red は `daemon_sigcont_wakes_stopped_child` の leader 接続 timeout — 調査中) |
| 2026-07-26 | task | open | [web-ime-safari-ios-unverified](./2026-07-26-web-ime-safari-ios-unverified.md) | IME 変換位置ズレの原因 2 件 (textarea 溢れ / resize 後のズレ) を特定し session.js で修正済み、検証は Chromium のみ — 実機 macOS/iOS Safari が未検証 |
| 2026-07-25 | bug | open | [flaky-serve-ro-lock-acquire-rejected](./2026-07-25-bug-flaky-serve-ro-lock-acquire-rejected.md) | 高負荷時の flaky 2 系統: `serve_ro_client_lock_acquire_rejected` (= 32s 回に SessionExitNotify(143) を拾う、元凶は `/bin/sleep 30` を待つ token test) と `input_auto_lock_cli` の deadline fail (= 変更前 revision でも再現、DR-0029 起因でないことを確認済)。根に PTY 枯渇 (123/128 使用、`start: Errno(ENXIO)`) |
| 2026-07-25 | request | open | [request-attach-overlay-progress](./2026-07-25-request-attach-overlay-progress.md) | attach 画面最下行に detach 遅延の progress overlay (DR-0029 §5、`ctrlz_guard_overlay` は現在 no-op) |
| 2026-07-21 | request | open | [daemon-graceful-upgrade-self-exec](./2026-07-21-daemon-graceful-upgrade-self-exec.md) | daemon の graceful upgrade (self-exec で fd/pid 引き継ぎ、DR 起草必須) |
| 2026-07-21 | bug | open | [sigcont-alive-child-session-vanish](./2026-07-21-sigcont-alive-child-session-vanish.md) | SIGCONT を送るとセッションが消滅する疑い — 根本原因候補特定 (`hyoui kill --no-terminate` が `detach_others: true` で全 client を蹴る、2026-07-25 実測) |
| 2026-07-21 | request | idea | [screen-region-watch-api](./2026-07-21-screen-region-watch-api.md) | screen 仮想スクリーンの部分切り出し API + 監視エリアのマッチング検出インターフェース (DR-0025 母体、web ターミナル完了後着手) |
| 2026-07-21 | design | open | [screen-overlay-general-mechanism](./2026-07-21-screen-overlay-general-mechanism.md) | screen state への動的仮想オーバーレイ一般機構 (DR-0013 延長、DR-0029 detach 案内 / web ターミナル ダイアログ用、web ターミナル完了後着手) |
| 2026-07-20 | bug | open | [socket-dir-tmp-fallback-macos-cleanup](./2026-07-20-socket-dir-tmp-fallback-macos-cleanup.md) | socket dir が /tmp 固定 fallback のため macOS 定期掃除で daemon 生存中に socket file が消える |
| 2026-07-04 | task | open | [dr0025-phase2b-raw-data-reducer](./2026-07-04-dr0025-phase2b-raw-data-reducer.md) | DR-0025 Phase 2-β — raw_data hot path の reducer→Effect→execute 化 |
| 2026-07-03 | bug | wip | [bug-flaky-serve-tail-follow-tail-end](./2026-07-03-bug-flaky-serve-tail-follow-tail-end.md) | serve_tail_follow_receives_tail_end_on_child_exit の flaky — product バグを 2 つ特定・修正済 (子 exit 検出が tail.request を追い越す race / **anchor 経路の子が `tcsetpgrp` を background pgrp から呼び SIGTTOU で停止し exec 到達しない race**、後者は高負荷 A/B で 0/14 vs 6/14、p=0.016)。残るは legacy 経路 (`forkpty_then_exec_legacy`) 側 — lib test は anchor 化不可で legacy に落ちるため上記修正が効かず `daemon::session` 20 回中 3 失敗 |
| 2026-07-03 | bug | open | [bug-macos-ci-flaky-pty-tests](./2026-07-03-bug-macos-ci-flaky-pty-tests.md) | PTY 系 e2e の flaky (blocking failure の 57%) — 束ねた 2 test は別原因と判明。outer_token_* は WriterDead 起因で**修正済**、child_inherits_session_id_env は attach redraw が attach 前の子出力を落とす別問題で**未解決** |
| 2026-07-03 | bug | open | [bug-main-unittest-hang-ubuntu-ci](./2026-07-03-bug-main-unittest-hang-ubuntu-ci.md) | hyoui-cli main.rs unit tests が ubuntu CI で hang (send_raw_bytes_partial_byte_race_regression / list_marks_stale_socket、flaky) |
| 2026-06-22 | bug | blocked | [backpressure-writer-pump-drop-sequence-deadlock](./2026-06-22-backpressure-writer-pump-drop-sequence-deadlock.md) | serve_backpressure_disconnects_slow_client が CI で 30s deadline hang する (真因未観測・調査継続、ubuntu CI では 12/12 で恒常失敗 = [[2026-07-26-bug-ignored-tests-job-permanently-red]]) |
| 2026-06-22 | bug | blocked | [wait-scrollback-snapshot-coverage](./2026-06-22-wait-scrollback-snapshot-coverage.md) | hyoui wait の StateSnapshotRequest が scrollback を含まず viewport 外の出力を見逃す (DR-0013 Phase B 未完) |
| 2026-05-28 | design | idea | [feature-cli-restructure-discussion](./2026-05-28-feature-cli-restructure-discussion.md) | CLI 設計大改修議論 (screen view 改名 / dump top-level 化 / screen write overlay / format 整理) |
| 2026-06-16 | request | open | [feature-icanon-large-input-chunking](./2026-06-16-feature-icanon-large-input-chunking.md) | ICANON apps への大量 byte 送信時の chunk 化 helper / timeout 調整 |
| 2026-06-16 | task | open | [feature-ack-test-coverage-expansion](./2026-06-16-feature-ack-test-coverage-expansion.md) | DR-0021 ack 機構の test cover 拡張 |
| 2026-06-12 | task | open | [tcsaflush-input-discard-in-suspend-resume](./2026-06-12-tcsaflush-input-discard-in-suspend-resume.md) | TtyGuard suspend/resume/Drop の TCSAFLUSH による入力破棄の検討 |
| 2026-06-12 | bug | open | [child-spawn-sigttou-stop-race](./2026-06-12-child-spawn-sigttou-stop-race.md) | daemon 子プロセスが fork〜exec 間で停止する race (SIGTTOU/SIGTTIN 系) |
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
