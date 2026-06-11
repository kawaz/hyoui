//! `status.query` / `status.response` payload (DR-0008 §2.3、basic schema)。
//!
//! detailed schema は実装フェーズで詰める。最小限 (= session 名、子 pid、
//! client 一覧、scrollback 情報、lock 状態) を最初に固める。

use serde::{Deserialize, Serialize};

use super::Mode;

/// `status.query` payload (引数なし)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct StatusQuery {}

/// 1 client の情報 (status.response の clients 配列要素)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ClientInfo {
    /// daemon が割り当てた client 番号。
    pub client_id: u64,
    /// 個別 mode。
    pub mode: Mode,
    /// leader かどうか。
    pub leader: bool,
}

/// 子 PTY process の実行時状態 (= `status` / `list` の状態表現)。
///
/// 既存の `StatusResponse::child_pid` (None = exit 済) + `child_stopped` (bool)
/// を包含する整理表現。新 field として serde default で追加し (= 後方互換)、
/// 旧 2 field も残す。daemon は両者を矛盾なく埋める。
///
/// wire tag は `state` discriminant + (exited のみ) `code`:
/// - `{"state":"running"}` — 子は生存中 + stopped でない
/// - `{"state":"stopped"}` — 子は生存中だが SIGTSTP/SIGSTOP で停止中
/// - `{"state":"exited","code":<i32|null>}` — 子は exit 済 (code 不明なら null)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ChildLiveState {
    /// 子は生存中で stopped でない。
    Running,
    /// 子は生存中だが stopped (= ^Z / SIGSTOP)。
    Stopped,
    /// 子は exit 済。`code` は判明していれば `Some`。
    Exited {
        /// exit code (= 判明していれば `Some`、不明なら `None`)。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
    },
}

/// `child_pid` (None=exited) + `child_stopped` (bool) から `ChildLiveState` を
/// 導出する default の役割を持つ helper (= 旧 field との整合を 1 箇所に集約)。
impl Default for ChildLiveState {
    fn default() -> Self {
        // 旧 client が `child_state` を送らない場合の fallback。`child_pid` が
        // 載っていれば caller (= CLI) 側で `from_legacy` を使うので、ここは
        // 「情報なし時の最も無難な値」= Running を返す。
        ChildLiveState::Running
    }
}

impl ChildLiveState {
    /// 旧 2 field (`child_pid` の有無 + `child_stopped`) から `ChildLiveState` を
    /// 導出する。`child_state` を送らない旧 daemon との互換に使う (= CLI 側)。
    pub fn from_legacy(child_pid: Option<u32>, child_stopped: bool) -> Self {
        match child_pid {
            None => ChildLiveState::Exited { code: None },
            Some(_) if child_stopped => ChildLiveState::Stopped,
            Some(_) => ChildLiveState::Running,
        }
    }
}

/// `status.response` payload (basic)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StatusResponse {
    /// session 名。
    pub session_id: String,
    /// 子 PTY の PID (= null なら子が exit 済)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    /// 子 PTY の process group id (= null なら子が exit 済)。
    ///
    /// 子は独立 session leader として起動するため (DR-0001 §実装ノート)、通常
    /// `child_pgid == child_pid`。`kill` が pgrp 単位で signal を送る対象を
    /// `ps -o pgid` と突き合わせる用途。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_pgid: Option<u32>,
    /// daemon process 自身の PID (= トラブル時に `ps` と突き合わせる起点)。
    ///
    /// 孤児 daemon の早期発見に使う (= socket は live だが child が無い等)。
    #[serde(default)]
    pub daemon_pid: u32,
    /// DR-0017 §柱2: 子が現在 stopped (= SIGTSTP/SIGSTOP で停止中) と daemon が
    /// 観測しているか。auto-resume 廃止後は stopped のまま残り得るため、`list` /
    /// `status` で放置 stopped child を可観測にする。子 exit 済 (= `child_pid`
    /// が `None`) の場合は `false`。
    ///
    /// 後方互換 field。新 `child_state` (= `ChildLiveState`) が running/stopped/
    /// exited を包含するが、旧 client / 既存 jq script のために残す。
    #[serde(default)]
    pub child_stopped: bool,
    /// 子 PTY の実行時状態 (= running / stopped / exited(code))。
    ///
    /// 旧 `child_pid` (None=exited) + `child_stopped` (bool) を包含する整理表現。
    /// serde default なので旧 client は `from_legacy` 相当の Running を受け取るが、
    /// CLI 側は `child_pid` / `child_stopped` から導出するため実害なし。
    #[serde(default)]
    pub child_state: ChildLiveState,
    /// 現在 attach 中の client 一覧。
    pub clients: Vec<ClientInfo>,
    /// scrollback ring buffer 内の総 byte 数。
    pub scrollback_bytes: u64,
    /// lock 保持者の client-id (= null なら未保持)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_holder: Option<u64>,
    /// daemon 起動時の cwd (= `hyoui run` の起動 dir、`hyoui list` 表示用)。
    ///
    /// 必須 field。daemon は `current_dir()` 失敗時にも `/` を入れて必ず value を載せる
    /// (= v1.0 未満なので breaking change OK 方針、`memory: project_v1_0_breaking_change_ok`)。
    /// 「取得失敗」と「未指定」を区別したいケースは現状なく、空文字は invalid value。
    pub cwd: String,
    /// daemon の子 PTY として起動した argv (= `DaemonConfig::cmd`)。
    ///
    /// 必須 field。daemon は argv なしで起動しない (= 空 `Vec` は invalid value)。
    /// `hyoui list` で「何の process が動いているか」を識別する用途。
    pub argv: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_legacy`: 旧 2 field (child_pid 有無 + child_stopped) からの導出が
    /// 全組合せで正しい (= 旧 daemon 互換の正本)。
    #[test]
    fn child_live_state_from_legacy_matrix() {
        assert_eq!(
            ChildLiveState::from_legacy(Some(123), false),
            ChildLiveState::Running
        );
        assert_eq!(
            ChildLiveState::from_legacy(Some(123), true),
            ChildLiveState::Stopped
        );
        // exit 済は stopped flag に関わらず Exited (= stopped は意味を持たない)。
        assert_eq!(
            ChildLiveState::from_legacy(None, false),
            ChildLiveState::Exited { code: None }
        );
        assert_eq!(
            ChildLiveState::from_legacy(None, true),
            ChildLiveState::Exited { code: None }
        );
    }

    /// `ChildLiveState` の CBOR roundtrip (= wire 互換性の固定)。
    #[test]
    fn child_live_state_cbor_roundtrip() {
        for state in [
            ChildLiveState::Running,
            ChildLiveState::Stopped,
            ChildLiveState::Exited { code: None },
            ChildLiveState::Exited { code: Some(143) },
        ] {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&state, &mut buf).expect("encode");
            let back: ChildLiveState = ciborium::de::from_reader(buf.as_slice()).expect("decode");
            assert_eq!(back, state);
        }
    }

    /// serde default (= 旧 daemon が field を送らない場合) は Running に倒れる。
    #[test]
    fn child_live_state_default_is_running() {
        assert_eq!(ChildLiveState::default(), ChildLiveState::Running);
    }
}
