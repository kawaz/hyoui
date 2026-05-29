//! test 用 env 操作 helper。
//!
//! Rust 2024 edition で `std::env::set_var` / `std::env::remove_var` が
//! `unsafe` になった (= 並列スレッドで env を触ると UB の可能性) ため、
//! test 内で必要な箇所をここに集約して `unsafe` を sys/ 配下に封じ込める。
//!
//! 呼び出し側は test 用 Mutex 等で並列 test との直列化を担保すること。

/// `std::env::set_var` の test 用 thin wrapper。
///
/// # Safety (caller 契約)
/// caller 側で test 用 Mutex / `serial_test` 等で並列 test と直列化される前提。
/// production code からは呼ばないこと (= `#[cfg(test)]` 配下のみ想定)。
#[cfg(test)]
pub fn set_var(key: &str, value: &str) {
    // SAFETY: caller 側で並列 test と直列化される前提。
    // production code からは呼ばれない (`#[cfg(test)]` 配下のみ)。
    unsafe { std::env::set_var(key, value) }
}

/// `std::env::remove_var` の test 用 thin wrapper。
///
/// # Safety (caller 契約)
/// caller 側で test 用 Mutex / `serial_test` 等で並列 test と直列化される前提。
/// production code からは呼ばないこと (= `#[cfg(test)]` 配下のみ想定)。
#[cfg(test)]
pub fn remove_var(key: &str) {
    // SAFETY: caller 側で並列 test と直列化される前提。
    // production code からは呼ばれない (`#[cfg(test)]` 配下のみ)。
    unsafe { std::env::remove_var(key) }
}

/// `std::env::remove_var` の production 用 wrapper (= 起動初期 single-threaded のみ)。
///
/// # Safety (caller 契約)
/// **process 起動初期 + single-threaded** でのみ呼ぶこと。Rust 2024 edition で
/// `std::env::remove_var` が unsafe 化された理由は「並列 thread で env を触ると
/// glibc の env table に race condition が起きる」ため。`fn main` の冒頭 (= thread
/// spawn 前) で呼ぶならその risk は無い。
///
/// 本 wrapper は `hyoui-cli` の main entry で `HYOUI_DAEMONIZE_INTERNAL` 等の
/// internal env を **mode 確定後すぐに unset** する用途で使う (= 子 process への
/// 漏れ防止、kawaz 指示 2026-05-29)。
pub fn remove_var_at_startup(key: &str) {
    // SAFETY: caller が「main entry 冒頭、thread spawn 前」を保証する前提。
    unsafe { std::env::remove_var(key) }
}
