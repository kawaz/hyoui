//! DR-0014 マトリクス検証の harness 群。`tests/<name>.rs` から
//! `mod common;` で取り込み、`common::pty::HyouiTestRunner` 等を使う。
//!
//! 構成:
//! - `pty`: PTY 内で hyoui-cli を spawn し、bytes 送受信 + signal + screen dump
//!   を観測する `HyouiTestRunner` / `SpawnedHyoui`
//! - `normalize`: screen dump bytes から非決定的要素を regex で削る正規化 helper
//!   (= zellij `account_for_races_in_snapshot` pattern)

pub mod normalize;
pub mod pty;
