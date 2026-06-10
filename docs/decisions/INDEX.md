# Decision Records (DR) Index

hyoui の設計判断記録一覧。ファイル名は `DR-NNNN-title.md`（4 桁ゼロパディング）。
`docs-structure.md` ルールに従い `## Active` / `## Archived` / `## Moved to research/` で区分する。

## Active

各 DR の Status 列ラベル基準:

- **✅ 実装済**: DR の Decision / Implementation phases が完了している
- **🟡 部分実装**: 一部のみ実装、他は ROADMAP / 別 task に
- **⬜ 未実装**: 設計のみ、実装エビデンスなし
- **N/A**: 実装対象でない (= 命名 / 思想 / ROADMAP / プロセス)
- **❌ 撤退**: 撤退判断済

| DR | Status | 説明 |
|---|---|---|
| [DR-0001](./DR-0001-bgfg-jobcontrol-two-axis.md) | 🟡 軸 2 廃止 (DR-0015、2026-05-28) | bg/fg ジョブ制御 (= 軸 1 follow/auto-resume のみ維持、軸 2 transparent/decouple は DR-0015 で廃止。invariant は「子が死ねば全部 exit」の片方向に縮小) |
| [DR-0002](./DR-0002-project-naming.md) | N/A (= 命名) | プロジェクト名 "hyoui"（憑依）の決定 |
| [DR-0003](./DR-0003-rust-only-and-forkpty-login_tty.md) | ✅ 実装済 | Rust 一本化 (MoonBit 却下) と forkpty + login_tty 採用 |
| [DR-0004](./DR-0004-cli-subcommand-design.md) | ✅ 実装済 | CLI サブコマンド設計 (run / attach / list / kill / status / tail / wait / screen / input / lock / unlock / record / completion 実装済。send / detach / tx は予約 = parse 成功で廃止/予約エラーを返す) |
| [DR-0005](./DR-0005-design-philosophy-external-automation.md) | N/A (= 思想) | hyoui の思想再定義 (外側自動操作主軸、TUI multiplexer ではない、透明性最優先) |
| [DR-0006](./DR-0006-cli-ground-rules.md) | 🟡 部分実装 | CLI 設計の地盤ルール。自動操作 API は `input` family (text/hex/file/paste/key/wait/wait-idle spec) に統合実装済 (= 旧構想の send/keys/paste は廃止、独立 subcommand 化せず)。`wait` / `lock` / `unlock` 実装済。`tx` (= lock + 子 process wrapper) は未実装 (docs/issue/2026-05-27-tx-lock-unlock-cli-subcommands.md) |
| [DR-0007](./DR-0007-mvp-scope-and-staged-release.md) | N/A (= ROADMAP) | MVP scope と段階リリース (v0.1.0 / v0.2.0 serve / v0.3.0 leader CLI) |
| [DR-0008](./DR-0008-protocol-design.md) | ✅ 実装済 | protocol 設計 (CBOR ハイブリッド framing、cap flags、gateway 戦略で PtyMux 将来互換) |
| [DR-0009](./DR-0009-session-module-split.md) | ✅ 実装済 | `daemon/session.rs` 責務分割 (pty/accept/broadcast/control/lock/wait/tail) |
| [DR-0010](./DR-0010-v020-scope-and-serve-placement.md) | N/A (= ROADMAP) | v0.2.0 scope re-scope + serve gateway 配置判断 (= subcommand 11→7、serve を別 repo 切り出し、DR-0007 部分上書き) |
| [DR-0011](./DR-0011-observability-strategy.md) | ⬜ 未実装 | observability 戦略 (= tracing 採用、Phase A 以降、v0.2.0 serve gateway 前提) |
| [DR-0012](./DR-0012-signal-wire-name-not-number.md) | ✅ 実装済 | signal wire を u8 number から signal name string に変更 (v0.2.0 breaking change)。`normalize_signal_spec` で NUM/NAME 両受け → SIG-prefix 大文字 name に正規化、`--signum` は廃止エラーで `--signal` へ誘導 |
| [DR-0013](./DR-0013-screen-emulator-and-attach-stability.md) | 🟡 部分実装 (Phase A/B + scrollback layer 完了、Phase C 残: observe mode / multi-client resize モード / reflow / zstd 等) | screen emulator + attach/detach 安定化 + データモデル統一 (vt100 採用、daemon = screen state 正本化) |
| [DR-0014](./DR-0014-transparency-and-empirical-verification.md) | N/A (= プロセス) | 透過原則の徹底と検証主義 (= self-check リスト、マトリクス検証主義、ドッグフーディング、CLAUDE.md 経由で常時参照) |
| [DR-0015](./DR-0015-run-as-fork-plus-attach.md) | ✅ 実装済 (2026-05-28) | `hyoui run` を fork daemon + attach client の合成に再定義、client/server 同居廃止 (= Phase A-D 全完了、新 protocol message 3 個 + linger pattern + attach SIGTSTP handler + Issue #1 / 派生 issue 解消) |
| [DR-0016](./DR-0016-tty-io-record.md) | 🟡 record core 実装済 (v0.2.x 出荷、Phase 4 hot path 配線完了。⚠ redaction の state machine = Phase 5 は未配線、`--input-secrecy` 値に関わらず stdin は素通し記録) | `hyoui record` — tty I/O timeline の永続録画 subcommand (= bug 解析の観測道具、jsonl format with header + bytes/lifecycle event + seq monotonic + 4 段階 SIGTSTP/SIGCONT lifecycle 分離、broadcast 経路と独立 I/O sink、bounded queue + writer task で観測対象を歪めない設計、redact-after-prompt default で secret 防護、record-v1 optional cap で旧 client 互換、`hyoui screen dump` 静止画とは命名分離) |
| [DR-0017](./DR-0017-session-anchor-and-suspend-policy.md) | ⬜ 未実装 | session anchor 化 + suspend policy 改訂 — TUI の Ctrl-Z を本来のセマンティクスで動かす (= 2 本柱。柱 1: `forkpty` 廃止し `openpty` + 手動 fork、daemon が `TIOCSCTTY` で controlling tty を取り child を同 session・別 pgrp・foreground で起動 → orphan pgrp の SIGTSTP discard を解消 (層 1)。柱 2: leader 不在時の無条件 auto-resume fallback を廃止、ユーザ/端末起因 stop を尊重 (層 2、DR-0001 軸 1 改訂)。DR-0003 の「子を session leader にする」部分を部分 supersede、3 platform PoC 済) |

## Archived

(なし)

## Moved to research/

(なし)
