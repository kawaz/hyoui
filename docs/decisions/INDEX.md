# Decision Records (DR) Index

hyoui の設計判断記録一覧。ファイル名は `DR-NNNN-title.md`（4 桁ゼロパディング）。
`docs-structure.md` ルールに従い `## Active` / `## Archived` / `## Moved to research/` で区分する。

## Active

- [DR-0001](./DR-0001-bgfg-jobcontrol-two-axis.md) — bg/fg ジョブ制御の 2 軸設計と invariant「親 fg ⇒ 子 fg」
- [DR-0002](./DR-0002-project-naming.md) — プロジェクト名 "hyoui"（憑依）の決定
- [DR-0003](./DR-0003-rust-only-and-forkpty-login_tty.md) — Rust 一本化 (MoonBit 却下) と forkpty + login_tty 採用
- [DR-0004](./DR-0004-cli-subcommand-design.md) — CLI サブコマンド設計 (run / completion / 将来枠 send/attach/status)
- [DR-0005](./DR-0005-design-philosophy-external-automation.md) — hyoui の思想再定義 (外側自動操作主軸、TUI multiplexer ではない、透明性最優先)
- [DR-0006](./DR-0006-cli-ground-rules.md) — CLI 設計の地盤ルール (動作モデル、自動操作 API send/keys/paste/wait、排他 lock/tx)
- [DR-0007](./DR-0007-mvp-scope-and-staged-release.md) — MVP scope と段階リリース (v0.1.0 / v0.2.0 serve / v0.3.0 leader CLI)
- [DR-0008](./DR-0008-protocol-design.md) — protocol 設計 (CBOR ハイブリッド framing、cap flags ベース schema evolution、gateway 戦略で PtyMux 将来互換)
- [DR-0009](./DR-0009-session-module-split.md) — `daemon/session.rs` 責務分割 (= R4-H6 解消方針、R4-M2 を Phase B で解消、pty/accept/broadcast/control/lock/wait/tail に module 分離、Phase A-E 段階移行)
- [DR-0010](./DR-0010-v020-scope-and-serve-placement.md) — v0.2.0 scope re-scope + serve gateway 配置判断 (= R5-H4 / R5-H5、subcommand 11→7 への統合、serve を別 repo `kawaz/hyoui-serve` 切り出し、snapshot を v0.3.0 押下げ、DR-0007 部分上書き)
- [DR-0011](./DR-0011-observability-strategy.md) — observability 戦略 (= R5-H1 解消方針、tracing 採用、Phase A: log instrument / Phase B: status --metrics / Phase C: detached child log file + hyoui logs、v0.2.0 serve gateway 前提)
- [DR-0012](./DR-0012-signal-wire-name-not-number.md) — signal wire を u8 number から signal name string に変更 (= R5-POSIX-C1 / R5-C4 解消、DR-0008 §protocol 部分上書き、v0.2.0 breaking change、cross-OS serve gateway 対応)
- [DR-0013](./DR-0013-screen-emulator-and-attach-stability.md) — screen emulator + attach/detach 安定化 + データモデル統一 (= vt100 採用、daemon = screen state 正本化、Phase A push 型 redraw + Phase B pull 型 SequenceNo、reflow truncate を input bytes log 再 feed で吸収、debug/snapshot protocol 追加、DR-0008 連動)
- [DR-0014](./DR-0014-transparency-and-empirical-verification.md) — 透過原則の徹底と検証主義 (= 介入判断 self-check リスト、マトリクス検証主義、ドッグフーディング、anti-pattern「監視 + 新 protocol 発明」「サンプル 1 断定」「マトリクス先送り」を明示禁止、CLAUDE.md 経由で常時参照)

## Archived

(なし)

## Moved to research/

(なし)
