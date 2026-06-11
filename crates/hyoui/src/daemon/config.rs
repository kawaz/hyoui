//! daemon の起動 config (DR-0008 §Consequences)。

use std::path::PathBuf;

/// 子 process が STOPPED に入ったときの daemon の挙動 (DR-0017 §柱2 / DR-0019)。
///
/// CLI 層の `crate::cli::OnChildSuspend` と 1:1 対応するが、daemon 層が CLI 層に
/// 依存しないよう config 側に独立した型を持つ (= 依存方向を CLI → daemon に保つ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ChildSuspendPolicy {
    /// leader client に `SessionChildStoppedNotify` を送るだけ (= 子を起こさない)。
    /// DR-0017 §柱2 の default 挙動。
    #[default]
    Notify,
    /// daemon が即座に子 process group へ SIGCONT を送って復帰させる
    /// (= notify は送らない、`session.rs::notify_child_stopped` 参照)。
    AutoResume,
}

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

    /// `--debug-dump=<path>`: 子 PTY から daemon が受け取った raw bytes を append-only で
    /// 書き出す debug 用 dump file path。
    ///
    /// - format: 純 raw bytes (= ANSI escape 込み)、`cat <path> > /dev/tty` で原寸再生可
    /// - 用途: SIGTSTP / SIGCONT 等の境界で何 bytes が流れているか post-mortem する
    ///   観測道具 (DR-0014 §道具揃った段階の運用、Issue #1 検証)
    /// - daemon process 自身が file open / write_all。失敗時は stderr に 1 行 warn を
    ///   出して dump を諦める (= session 自体は止めない)。
    /// - `None` なら dump 無効 (= 既定)。
    pub debug_dump_path: Option<PathBuf>,

    /// daemon 起動時の cwd (= `hyoui run` を叩いた起点 dir)。
    ///
    /// `hyoui list` の表示で「この session は何処で起動された session か」を一目で
    /// 把握できるようにするための情報源。`status.response` の `cwd` field に乗せて
    /// client に渡す。
    ///
    /// daemonize の経路では `run_daemon_child` の **`chdir("/")` 直前**に
    /// `std::env::current_dir()` を読み取ってここに格納する (= 親 process の
    /// cwd を inherit している瞬間の値)。test 経路や cwd 取得失敗時は `None`。
    pub cwd: Option<PathBuf>,

    /// daemon プロセス起動を識別する一意 ID (DR-0016 §3 jsonl header)。
    ///
    /// pid は OS が再利用するため、**daemon restart を跨いだ真の識別子**として
    /// UUID v4 を保持する。`hyoui record` の jsonl header に乗せて、複数 record
    /// file を時系列でつなぐときの「同一 daemon プロセス由来か」判定に使う。
    ///
    /// `DaemonConfig::new` で起動毎に新規生成。clone 時は同 ID を保持
    /// (= 同一 daemon の中で config を clone する経路は restart ではない)。
    pub daemon_boot_id: String,

    /// 子が STOPPED に入ったときの daemon の挙動 (DR-0017 §柱2 / DR-0019)。
    ///
    /// `hyoui run --on-child-suspend=notify|auto-resume` から配線される
    /// (= DaemonizeInit → run_daemon_child → ここ)。default は `Notify`。
    pub on_child_suspend: ChildSuspendPolicy,
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
            .field("debug_dump_path", &self.debug_dump_path)
            .field("cwd", &self.cwd)
            .field("daemon_boot_id", &self.daemon_boot_id)
            .field("on_child_suspend", &self.on_child_suspend)
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
            // DR-0015: daemon は jobcontrol policy を持たない (= client 側で発動)。
            // 子 stopped 観測時は notify_child_stopped が cap-aware に leader へ通知する。
            debug_dump_path: None,
            cwd: None,
            // DR-0016 §3: daemon プロセス起動毎に UUID v4 を生成し、jsonl header の
            // `daemon_boot_id` に乗せる。pid 再利用に強い識別子。
            daemon_boot_id: uuid::Uuid::new_v4().to_string(),
            // DR-0017 §柱2: default は notify-only (= 子を勝手に起こさない)。
            on_child_suspend: ChildSuspendPolicy::Notify,
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
        // DR-0017 §柱2: default は notify-only。
        assert_eq!(cfg.on_child_suspend, ChildSuspendPolicy::Notify);
    }

    #[test]
    fn config_is_clonable_for_thread_handoff() {
        let cfg = DaemonConfig::new("demo", PathBuf::from("/tmp/x.sock"), vec!["/bin/sh".into()]);
        let cloned = cfg.clone();
        assert_eq!(cfg.session_id, cloned.session_id);
        assert_eq!(cfg.socket_path, cloned.socket_path);
    }

    #[test]
    fn cwd_default_is_none_and_settable() {
        let mut cfg =
            DaemonConfig::new("demo", PathBuf::from("/tmp/x.sock"), vec!["/bin/sh".into()]);
        assert!(cfg.cwd.is_none(), "default cwd must be None");
        cfg.cwd = Some(PathBuf::from("/work"));
        assert_eq!(cfg.cwd.as_deref(), Some(std::path::Path::new("/work")));
    }

    #[test]
    fn daemon_boot_id_is_generated_and_unique_per_new() {
        // DR-0016 §3: 起動毎に新規 UUID。`new()` を 2 回呼べば異なる ID。
        let a = DaemonConfig::new("a", PathBuf::from("/tmp/a.sock"), vec!["/bin/sh".into()]);
        let b = DaemonConfig::new("b", PathBuf::from("/tmp/b.sock"), vec!["/bin/sh".into()]);
        assert_ne!(a.daemon_boot_id, b.daemon_boot_id);
        assert_eq!(
            a.daemon_boot_id.len(),
            36,
            "uuid v4 hyphenated form expected"
        );
    }

    #[test]
    fn daemon_boot_id_is_stable_under_clone() {
        // clone は同一 daemon プロセス内の handoff (= restart ではない)。
        let cfg = DaemonConfig::new("demo", PathBuf::from("/tmp/x.sock"), vec!["/bin/sh".into()]);
        let cloned = cfg.clone();
        assert_eq!(cfg.daemon_boot_id, cloned.daemon_boot_id);
    }
}
