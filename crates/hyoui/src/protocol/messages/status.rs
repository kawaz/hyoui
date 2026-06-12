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

/// `on-child-suspend` policy の wire 表現 (= `status` / `list` で可視化、DR-0019 Update)。
///
/// daemon 側 `ChildSuspendPolicy` と 1:1 だが、protocol 層が daemon 層に依存しない
/// よう独立した型を持つ。`StatusResponse` 上は `Option<OnChildSuspendPolicy>` で運び、
/// field を送らない旧 daemon は `None` (= unknown) になる — 既定値に倒すと実際と
/// 逆の値を断定表示しうるため、Default は意図的に持たない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum OnChildSuspendPolicy {
    /// leader に通知のみ (= 子を起こさない、DR-0017 default)。
    Notify,
    /// daemon が即座に SIGCONT で子を復帰させる。
    AutoResume,
}

impl OnChildSuspendPolicy {
    /// wire / 表示用の文字列 (`notify` / `auto-resume`)。
    pub fn as_str(self) -> &'static str {
        match self {
            OnChildSuspendPolicy::Notify => "notify",
            OnChildSuspendPolicy::AutoResume => "auto-resume",
        }
    }
}

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
    /// この client が daemon に attach した時刻 (= unix epoch ミリ秒、DR-0020 §5)。
    ///
    /// `serde(default)` で後方互換 (= field を送らない旧 daemon は `0` に倒れ、CLI は
    /// 「接続時刻不明」として `-` 表示する)。`daemon_version` と同流儀の前方互換追加。
    #[serde(default)]
    pub connected_at_unix_ms: u64,
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
    /// 現在の `on-child-suspend` policy (= `notify` / `auto-resume`、DR-0019 Update)。
    ///
    /// `hyoui set` で runtime 変更されうるため、daemon は **現在の実効値** を
    /// `Some(...)` で載せる。field を送らない旧 daemon は `None` に倒れ、CLI は
    /// `-` (unknown) 表示にする。`Option` で「不明」を構造的に表現する理由:
    /// 既定値 `Notify` に倒すと、released 0.6.3 daemon (= boot 時 policy 対応済・
    /// 報告未対応) を `auto-resume` で起動した worker に対して **実際と逆の値を
    /// 断定表示**してしまい、本機能の決定根拠 (= 旧バイナリ事故の検出) と正面衝突する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_child_suspend: Option<OnChildSuspendPolicy>,
    /// daemon バイナリの version (= `env!("CARGO_PKG_VERSION")`、DR-0019 Update)。
    ///
    /// 人間の診断用 (= 「PATH の旧バイナリで worker を起動して policy が no-op」事故の
    /// 検出)。serde default は空文字で、field を送らない旧 daemon は CLI 側で `-` 表示
    /// に倒れる (= それ自体が「古い daemon」のシグナル)。
    #[serde(default)]
    pub daemon_version: String,
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

    /// `OnChildSuspendPolicy` の wire 文字列表現。
    #[test]
    fn on_child_suspend_policy_as_str() {
        assert_eq!(OnChildSuspendPolicy::Notify.as_str(), "notify");
        assert_eq!(OnChildSuspendPolicy::AutoResume.as_str(), "auto-resume");
    }

    /// DR-0019 Update: 旧 daemon の StatusResponse (= `on-child-suspend` /
    /// `daemon-version` field 無し) を decode しても壊れず、policy=None (= unknown、
    /// CLI は `-` 表示) / version="" に倒れる。既定値 `Notify` に倒さない (= 実際と
    /// 逆の値を断定表示しない、M3)。
    #[test]
    fn legacy_status_response_without_new_fields_decodes_with_defaults() {
        use ciborium::Value;
        // 新 2 field を含まない古い StatusResponse map を手で組み立てる。
        let map = Value::Map(vec![
            (Value::Text("session-id".into()), Value::Text("demo".into())),
            (Value::Text("daemon-pid".into()), Value::Integer(999.into())),
            (Value::Text("child-stopped".into()), Value::Bool(false)),
            (Value::Text("clients".into()), Value::Array(vec![])),
            (
                Value::Text("scrollback-bytes".into()),
                Value::Integer(0.into()),
            ),
            (Value::Text("cwd".into()), Value::Text("/".into())),
            (
                Value::Text("argv".into()),
                Value::Array(vec![Value::Text("sh".into())]),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&map, &mut bytes).expect("encode legacy");
        let decoded: StatusResponse = ciborium::de::from_reader(bytes.as_slice())
            .expect("legacy StatusResponse must decode with serde defaults");
        assert_eq!(decoded.on_child_suspend, None);
        assert_eq!(decoded.daemon_version, "");
    }

    /// DR-0020 §5: `ClientInfo.connected_at_unix_ms` は serde default で後方互換。
    /// 旧 daemon (= field 無し) の ClientInfo を decode しても 0 に倒れる。
    #[test]
    fn legacy_client_info_without_connected_at_decodes_to_zero() {
        use ciborium::Value;
        // connected-at-unix-ms を含まない古い ClientInfo map。
        let map = Value::Map(vec![
            (Value::Text("client-id".into()), Value::Integer(7.into())),
            (Value::Text("mode".into()), Value::Text("rw".into())),
            (Value::Text("leader".into()), Value::Bool(true)),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&map, &mut bytes).expect("encode legacy ClientInfo");
        let decoded: ClientInfo = ciborium::de::from_reader(bytes.as_slice())
            .expect("legacy ClientInfo must decode with serde default");
        assert_eq!(decoded.client_id, 7);
        assert!(decoded.leader);
        assert_eq!(decoded.connected_at_unix_ms, 0);
    }

    /// `ClientInfo` の CBOR roundtrip (= connected_at_unix_ms 込みの wire 固定)。
    #[test]
    fn client_info_cbor_roundtrip_with_connected_at() {
        let ci = ClientInfo {
            client_id: 3,
            mode: Mode::Ro,
            leader: false,
            connected_at_unix_ms: 1_700_000_000_000,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&ci, &mut buf).expect("encode");
        let back: ClientInfo = ciborium::de::from_reader(buf.as_slice()).expect("decode");
        assert_eq!(back, ci);
    }
}
