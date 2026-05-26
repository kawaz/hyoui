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

## Archived

(なし)

## Moved to research/

(なし)
