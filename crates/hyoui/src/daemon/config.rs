//! daemon の起動 config (DR-0008 §Consequences)。

use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::{OnChildSuspend, OnParentSuspend};

/// suspend / resume の前後 hook (DR-0001 jobcontrol 2 軸 + Issue #1: termios 復元)。
///
/// daemon thread が `raise(SIGSTOP)` する直前 (= [`DaemonConfig::on_suspend`]) /
/// 復帰直後 (= [`DaemonConfig::on_resume`]) に呼ぶ callback。CLI process が
/// 保持する [`crate::sys::TtyGuard`] の `suspend()` / `resume()` を thread 越しに
/// 触らせるための薄い橋渡し。
pub type SuspendHook = Arc<dyn Fn() + Send + Sync>;

/// daemon 1 つ分の起動設定。
///
/// `cmd` で指定した process を子 PTY として spawn し、`socket_path` で
/// Unix socket を bind して client 接続を受け付ける。
#[derive(Clone)]
pub struct DaemonConfig {
    /// session 名 (= status.response や `hyoui list` で識別される)。
    pub session_id: String,

    /// Unix socket の bind 先 (= `~/.hyoui/sock/<name>.sock` 慣例)。
    pub socket_path: PathBuf,

    /// 子 PTY として spawn する command + args (argv[0] が command 本体)。
    pub cmd: Vec<String>,

    /// 子 PTY の初期サイズ (columns)。
    pub cols: u16,

    /// 子 PTY の初期サイズ (rows)。
    pub rows: u16,

    /// scrollback ring buffer の上限 byte 数 (= `src/scrollback.rs` 使用)。
    ///
    /// DR-0013 §8 の方針整理: scrollback は **byte-base** (= `Scrollback`) と
    /// **rows-base** (= vt100 内蔵 ring) の 2 層に分けて並行運用する。byte-base 層は
    /// tail コマンドの `since_ms` / `last_bytes` 等 timestamp-base API の前提を担い、
    /// rows-base 層は cell 単位の構造化 access に使う。byte-base 層を rows-base に
    /// 置換すると tail の意味論が壊れるため、置換ではなく **責務分離** を採用。
    /// `scrollback_bytes` はこの byte-base 層の上限を設定する。
    pub scrollback_bytes: usize,

    /// primary buffer 用 input bytes log (= resize 救済策) の上限 byte 数。
    /// DR-0013 §7。0 を渡すと log を無効化 (= resize 時の replay は no-op、
    /// vt100 `set_size` の truncate だけが効く挙動になる)。
    pub screen_input_log_bytes: usize,

    /// vt100 内蔵 scrollback ring の **行数上限** (DR-0013 §8 + §8 Update)。
    ///
    /// rows-base 層 (= cell 単位アクセス用) のみに影響する。`scrollback_bytes`
    /// (= byte-base 層、tail timestamp filter 用) とは責務分離されており、両者は
    /// 別 layer として並行運用する (§8 Update)。
    ///
    /// `screen.dump --layer=scrollback` / `--layer=both` で過去 row を取り出す際の
    /// 上限がこの値。`0` を渡すと scrollback は無効 (= 過去 row は保存されない、
    /// `screen.dump --layer=scrollback` は空配列を返す)。
    ///
    /// 既定 1000 行。典型 TUI app (例: 80×24 で 60 行応答が visible からスクロール
    /// アウト) を救うのに十分なサイズ。過大設定は cell grid メモリを増やすため、
    /// rows × cols 比例の消費に注意 (= 200 cols × 10000 行 ≒ 2M cells)。
    ///
    /// 設計判断: bytes ベース換算は **採用しない** (= cell byte 数は UTF-8 + style
    /// overhead で大きく揺れる、`scrollback_bytes / (cols * 4)` 等の換算は根拠が
    /// 脆い、§8 Update)。rows ベース直接指定で vt100 API と整合させる。
    pub screen_vt100_scrollback_rows: usize,

    /// 1 client への broadcast queue の上限 byte 数 (DR-0008 §8.2 backpressure)。
    /// 既定 8 MiB。超過時はその client を `error` kind=`backpressure.disconnect` で
    /// notify → close する。
    pub client_buffer_bytes: usize,

    /// handshake 時に client が提示する必須 token (= `HandshakeRequest.token`)。
    ///
    /// - `None`: 認可 token は要求しない (= default、同 UID 信頼境界のみ)
    /// - `Some(s)`: client は handshake で同一 token を提示する必要あり。不一致 /
    ///   未提示なら daemon は `error` kind=`auth.token-mismatch` で reject して
    ///   接続を切る (DR-0006 §6 lock token、`HYOUI_LOCK_TOKEN` 経路)
    ///
    /// MVP では `None` 既定で運用 (= 同 UID + socket perm 0600 で十分とする)。
    /// 将来 TCP / WebSocket transport を加えるときに必須化する想定。
    pub expected_token: Option<String>,

    /// R5-FB1: `hyoui run --until PATTERN`。子 PTY 出力に substring `PATTERN`
    /// が現れた瞬間、子 process group に SIGTERM を送って session を畳む。
    ///
    /// - `None`: 監視なし (= default)
    /// - `Some(needle)`: serve_loop が master byte stream に対し sliding window
    ///   scan を行い、match した瞬間 `kill_pgrp(child, SIGTERM)` で終了させる
    ///
    /// scan は raw byte (= ANSI escape を含む) に対して行う。strip 済 stream で
    /// 一致させたい場合は v0.2.0 で `wait --pattern` 経路を使う想定 (= 本機能は
    /// `run` から手早く使うための簡易 needle match)。
    pub until: Option<String>,

    /// DR-0001 軸 1: 子が STOPPED 状態になったときの親 daemon の挙動。
    ///
    /// - [`OnChildSuspend::Follow`]: 親自身に `SIGSTOP` を `raise` し、外側 shell に
    ///   制御を返す (= invariant「親 fg なら子 fg」を維持しながら、ユーザの
    ///   `fg` を待つ形で両者停止)。
    /// - [`OnChildSuspend::AutoResume`]: 子 pgrp に即 `SIGCONT` を送って復帰させる
    ///   (= 子の suspend を一切許さない、poc3 `nosuspend` 相当)。
    pub on_child_suspend: OnChildSuspend,

    /// DR-0001 軸 2: 親 daemon が外部から SIGTSTP を受信したときの子の挙動。
    ///
    /// - [`OnParentSuspend::Transparent`]: 子 pgrp に `SIGSTOP` を送ってから、親も
    ///   `SIGSTOP` を `raise` (= 親子ペアで停止)。
    /// - [`OnParentSuspend::Decouple`]: 親だけ `SIGSTOP` を `raise`、子はそのまま
    ///   走らせる (= headless バッチで親を止めても子のジョブを進めたいとき)。
    pub on_parent_suspend: OnParentSuspend,

    /// daemon が `raise(SIGSTOP)` する **直前** に呼ぶ hook。
    ///
    /// 典型用途: CLI process が保持する [`crate::sys::TtyGuard::suspend`] を呼んで
    /// 外側 TTY を pre-raw 状態に戻す (= 外側 cmux / tmux / libghostty が STOPPED 中の
    /// raw mode TTY に talk して freeze する事故を防ぐ、Issue #1 修正)。
    ///
    /// `None` なら no-op。
    pub on_suspend: Option<SuspendHook>,

    /// daemon が `raise(SIGSTOP)` から **復帰した直後** に呼ぶ hook。
    ///
    /// 典型用途: CLI process が保持する [`crate::sys::TtyGuard::resume`] を呼んで
    /// 外側 TTY を再 raw 化する。`on_suspend` と対になる。
    ///
    /// `None` なら no-op。
    pub on_resume: Option<SuspendHook>,
}

impl std::fmt::Debug for DaemonConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonConfig")
            .field("session_id", &self.session_id)
            .field("socket_path", &self.socket_path)
            .field("cmd", &self.cmd)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("scrollback_bytes", &self.scrollback_bytes)
            .field("screen_input_log_bytes", &self.screen_input_log_bytes)
            .field(
                "screen_vt100_scrollback_rows",
                &self.screen_vt100_scrollback_rows,
            )
            .field("client_buffer_bytes", &self.client_buffer_bytes)
            .field(
                "expected_token",
                &self.expected_token.as_ref().map(|_| "<redacted>"),
            )
            .field("until", &self.until)
            .field("on_child_suspend", &self.on_child_suspend)
            .field("on_parent_suspend", &self.on_parent_suspend)
            .field("on_suspend", &self.on_suspend.as_ref().map(|_| "<hook>"))
            .field("on_resume", &self.on_resume.as_ref().map(|_| "<hook>"))
            .finish()
    }
}

impl DaemonConfig {
    /// 既定値で `DaemonConfig` を組み立てる helper。
    ///
    /// `scrollback_bytes = 1 MiB`、`screen_input_log_bytes = 1 MiB`、
    /// `screen_vt100_scrollback_rows = 1000` 行、`client_buffer_bytes = 8 MiB`、
    /// `cols × rows = 80 × 24`。
    pub fn new(session_id: impl Into<String>, socket_path: PathBuf, cmd: Vec<String>) -> Self {
        Self {
            session_id: session_id.into(),
            socket_path,
            cmd,
            cols: 80,
            rows: 24,
            scrollback_bytes: 1024 * 1024,
            screen_input_log_bytes: 1024 * 1024,
            screen_vt100_scrollback_rows: 1000,
            client_buffer_bytes: 8 * 1024 * 1024,
            expected_token: None,
            until: None,
            // 既定は CLI 層の `Mode::Interactive` preset と揃える (DR-0001 §デフォルト)。
            // `hyoui-cli` の `run_command` / `__daemonize-run` 経由なら `RunConfig` から
            // 上書きされる。直接 `DaemonConfig::new` を使う test 経路にも妥当な既定を
            // 与えるため、ここで明示する。
            on_child_suspend: OnChildSuspend::Follow,
            on_parent_suspend: OnParentSuspend::Transparent,
            on_suspend: None,
            on_resume: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_dr_0008() {
        let cfg = DaemonConfig::new(
            "demo",
            PathBuf::from("/tmp/hyoui-demo.sock"),
            vec!["/bin/sh".into()],
        );
        assert_eq!(cfg.session_id, "demo");
        assert_eq!(cfg.cols, 80);
        assert_eq!(cfg.rows, 24);
        assert_eq!(cfg.scrollback_bytes, 1024 * 1024);
        assert_eq!(cfg.screen_input_log_bytes, 1024 * 1024); // DR-0013 §7 既定 1 MiB
        assert_eq!(cfg.screen_vt100_scrollback_rows, 1000); // DR-0013 §8 既定 1000 行
        assert_eq!(cfg.client_buffer_bytes, 8 * 1024 * 1024); // DR-0008 §8.2 既定
    }

    #[test]
    fn config_is_clonable_for_thread_handoff() {
        let cfg = DaemonConfig::new("demo", PathBuf::from("/tmp/x.sock"), vec!["/bin/sh".into()]);
        let cloned = cfg.clone();
        assert_eq!(cfg.session_id, cloned.session_id);
        assert_eq!(cfg.socket_path, cloned.socket_path);
    }
}
