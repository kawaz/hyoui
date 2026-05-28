//! `session.exit.notify` / `session.child.stopped.notify` /
//! `session.child.resume.request` payload (DR-0015 §2.1, §2.2)。
//!
//! 3 message が DR-0015 で追加。`session.exit.notify` は子 PTY exit を全 attached
//! client に伝搬する終了通知。`session.child.stopped.notify` /
//! `session.child.resume.request` は DR-0001 軸 1 (= 子 self-stop / follow,
//! auto-resume) を別プロセス構成で維持するための daemon ↔ leader 間 message。

use serde::{Deserialize, Serialize};

/// `session.exit.notify` payload (DR-0015 §2.1、cap `session-exit-v1`)。
///
/// 子 PTY が exit した時、daemon が **全 attached client へ broadcast** する。
/// buffer drain 完了後に送信 (= DR-0015 §2.3.5 採用パターン)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionExitNotify {
    /// 親 client がそのまま `process::exit(status)` する shell convention 値。
    ///
    /// - 通常 exit: 子の exit code (= 0..255)
    /// - signal 死亡: 128 + signum (= shell convention、bash 等と整合、
    ///   `crates/hyoui/src/daemon/session.rs::finalize_child` の慣習と一致)
    pub exit_status: i32,
    /// signal で死んだ場合の補足情報 (= "SIGTERM" 等、DR-0012 SIG-prefix 形式)。
    ///
    /// 通常 exit 時は `None`。client が「signal 死亡」を区別したい場合の optional info
    /// (= 多くの場合は `exit_status` だけで足りる)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// `session.child.stopped.notify` payload (DR-0015 §2.2、cap `child-state-v1`)。
///
/// daemon が `waitpid(WUNTRACED)` で子 stopped を観測した瞬間に **leader にのみ**
/// 送信 (= broadcast ではない)。leader が `on-child-suspend` policy
/// (= follow / auto-resume) を発動する。
///
/// 軸 2 廃止 (= DR-0015 §2.3) のため、stopped origin は 100% 子 self-stop。
/// daemon が能動的に子 pgrp を SIGSTOP する経路は本 DR では一切存在しない =
/// origin 衝突なし。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionChildStoppedNotify {
    /// 子プロセスの PID (= debug / 表示用、leader は policy 判断に使わなくて OK)。
    pub pid: u32,
    /// stopped 原因の signal 名 (= "SIGTSTP" / "SIGSTOP" / `null`=不明、
    /// DR-0012 SIG-prefix 形式)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// `session.child.resume.request` payload (DR-0015 §2.2、cap `child-state-v1`)。
///
/// leader (= attach client) が `session.child.stopped.notify` を受けて follow / auto-resume
/// policy を発動した後、子を復帰させる時に **daemon へ送信** する。
/// daemon は受信して `killpg(child_pgid, SIGCONT)` を実行。
///
/// payload は **空** (= 単一 session 1 子のみのため pid 指定不要)。将来 multi-pane 化
/// する場合は payload で対象 pid を指定する形に拡張可能。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionChildResumeRequest {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_exit_notify_roundtrip_normal() {
        // 通常 exit (= exit_status 0..255、signal=None)
        let msg = SessionExitNotify {
            exit_status: 0,
            signal: None,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SessionExitNotify = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
        assert_eq!(decoded.exit_status, 0);
        assert_eq!(decoded.signal, None);
    }

    #[test]
    fn session_exit_notify_roundtrip_signaled() {
        // signal 死亡 (= exit_status は 128+signum 慣習、signal は補足)
        let msg = SessionExitNotify {
            exit_status: 128 + 15, // SIGTERM
            signal: Some("SIGTERM".into()),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SessionExitNotify = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
        assert_eq!(decoded.exit_status, 143);
        assert_eq!(decoded.signal.as_deref(), Some("SIGTERM"));
    }

    #[test]
    fn session_child_stopped_notify_roundtrip() {
        let msg = SessionChildStoppedNotify {
            pid: 12345,
            signal: Some("SIGTSTP".into()),
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SessionChildStoppedNotify = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn session_child_stopped_notify_signal_optional() {
        // signal=None も round-trip 可
        let msg = SessionChildStoppedNotify {
            pid: 42,
            signal: None,
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SessionChildStoppedNotify = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn session_child_resume_request_roundtrip() {
        let msg = SessionChildResumeRequest::default();
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).expect("encode");
        let decoded: SessionChildResumeRequest = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(msg, decoded);
    }
}
