# PoC: vt100 crate の評価 (DR-0013 用)

DR-0013 (screen state 正本化) で vt100 crate を採用できるかを検証した独立 crate (hyoui の cargo workspace からは `exclude` で外している)。結果と判断はレポート `docs/research/2026-05-27-vt100-poc-report.md`。`tests/v01_*.rs` 〜 `v10_perf.rs` が検証項目 1〜10 に対応し、このディレクトリで `cargo test --release -- --nocapture` で実行する。
