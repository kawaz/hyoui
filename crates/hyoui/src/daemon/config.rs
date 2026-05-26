//! daemon の起動 config (DR-0008 §Consequences)。

use std::path::PathBuf;

/// daemon 1 つ分の起動設定。
///
/// `cmd` で指定した process を子 PTY として spawn し、`socket_path` で
/// Unix socket を bind して client 接続を受け付ける。
#[derive(Debug, Clone)]
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
    pub scrollback_bytes: usize,

    /// 1 client への broadcast queue の上限 byte 数 (DR-0008 §8.2 backpressure)。
    /// 既定 8 MiB。超過時はその client を `error` kind=`backpressure.disconnect` で
    /// notify → close する。
    pub client_buffer_bytes: usize,
}

impl DaemonConfig {
    /// 既定値で `DaemonConfig` を組み立てる helper。
    ///
    /// `scrollback_bytes = 1 MiB`、`client_buffer_bytes = 8 MiB`、`cols × rows = 80 × 24`。
    pub fn new(session_id: impl Into<String>, socket_path: PathBuf, cmd: Vec<String>) -> Self {
        Self {
            session_id: session_id.into(),
            socket_path,
            cmd,
            cols: 80,
            rows: 24,
            scrollback_bytes: 1024 * 1024,
            client_buffer_bytes: 8 * 1024 * 1024,
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
