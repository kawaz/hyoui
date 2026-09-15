//! 監督者の制御 socket に流れる JSON (= DR-0034 決定 4)。
//!
//! 1 要求 1 行、1 応答 1 行。`log --follow` だけは応答のあとに行が続く (JSONL)。
//! hyoui の CBOR protocol (DR-0008) とは無関係な別経路で、cap flags も持たない。
//!
//! field 名は llm-gateway から引き写さず hyoui として決めている (決定 4): 識別子は
//! `name`、時刻は ISO 8601 の絶対時刻、単位を名前に埋めない。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use hyoui::version::VersionInfo;
use serde::{Deserialize, Serialize};

/// 操作の対象。`name` が無ければ登録簿の全 unit。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Target {
    /// 対象の unit 名。`None` は `--all`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Target {
    /// 単体指定。
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
        }
    }

    /// 全 unit。
    pub fn all() -> Self {
        Self { name: None }
    }
}

/// CLI から監督者への要求。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 登録の面を聞く (= 版は聞かない)。
    ///
    /// `status` と分けているのは、版を聞くと unit ごとに `--version` の短命な
    /// プロセスが 1 つ走るため (決定 7a)。`list` は障害時に最初に打つ口なので、
    /// 生死と登録内容だけを返して軽く保つ。
    List(Target),
    /// 状態を聞く (= 版まで含む)。
    Status(Target),
    /// 起こす (= `enabled` を立てる)。
    Start(Target),
    /// 止める (= `enabled` を下ろす)。
    Stop(Target),
    /// 入れ替える。`--all` は 1 台ずつ (決定 5)。
    Restart(Target),
    /// ログを読む。`follow` なら応答のあとに行が続く。
    Log {
        /// 対象 unit。
        #[serde(flatten)]
        target: Target,
        /// 追従するか。
        #[serde(default)]
        follow: bool,
    },
    /// 登録簿を読み直して望みとの差を埋める。
    ///
    /// **CLI の verb としては出さない** (決定 4)。`add` / `remove` が使う内部 op で、
    /// 人が打つ場面が無く、出すと「`add` の後に `reload` が必要か」の迷いを生む。
    Reload,
}

/// unit 1 台の状態 (= `daemon status` の 1 行)。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UnitStatus {
    /// unit 名。
    pub name: String,
    /// 居てほしいか (= 登録簿の desired state)。
    pub enabled: bool,
    /// 監督者が今この子を抱えているか。
    pub running: bool,
    /// 今の子の pid。
    pub pid: Option<u32>,
    /// 今の子が起きた時刻 (ISO 8601)。
    pub started_at: Option<String>,
    /// bind 先。
    pub listen: String,
    /// この unit が起動する実行ファイル。
    pub binary: PathBuf,
    /// その実行ファイルが今あるか (= `running: false` の原因が読める)。
    pub binary_exists: bool,
    /// 走っている版と置いてある版。
    pub version: VersionPair,
    /// 監督者が起こし直した回数。
    pub restarts: u32,
    /// 最後に終わった時の様子。
    pub last_exit: Option<String>,
}

/// 走っている版と置いてある版の組 (= 決定 7a)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct VersionPair {
    /// 動いているプロセス自身が答えた版。答えない版なら `None`。
    pub running: Option<VersionInfo>,
    /// `binary` を実行して `--version` を聞いた版。次に上がる時の版。
    pub on_disk: Option<VersionInfo>,
    /// 両方分かって食い違う時だけ `true`。
    pub restart_needed: bool,
}

impl VersionPair {
    /// 2 つの版から組を作る。
    ///
    /// 比較は `(version, build_id)` の組で行う — crate version は tag を打つまで
    /// 動かないので、version だけでは unstable の入れ替えを検出できない。片方が
    /// `None` なのは「食い違っていない」ではなく「比べられない」なので `false`。
    pub fn new(running: Option<VersionInfo>, on_disk: Option<VersionInfo>) -> Self {
        let restart_needed = match (&running, &on_disk) {
            (Some(running), Some(on_disk)) => running != on_disk,
            _ => false,
        };
        Self {
            running,
            on_disk,
            restart_needed,
        }
    }
}

/// 監督者自身の状態。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SupervisorInfo {
    /// 監督者の pid。
    pub pid: u32,
    /// 監督者が走らせている実行ファイル。
    pub binary: PathBuf,
    /// 監督者がメモリに載せている版 (= 本人しか言えない)。
    pub version: VersionInfo,
}

/// 監督者からの応答。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// 状態の一覧。
    Units {
        /// unit ごとの状態 (名前の昇順)。
        units: Vec<UnitStatus>,
        /// 答えた監督者。
        supervisor: SupervisorInfo,
        /// 操作の過程で伝えることがあれば添える (= skip した unit 等)。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    /// ログの先頭。`follow` ならこの後に [`LogLine`] が続く。
    Log {
        /// これまでに書かれた行。
        lines: Vec<LogLine>,
        /// この後も行が続くか。
        follow: bool,
    },
    /// 要求を果たせなかった。
    Error(ErrorBody),
}

/// ログ 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LogLine {
    /// どの unit の行か。
    pub name: String,
    /// 行の中身 (改行は含まない)。
    pub line: String,
}

/// エラー 1 つ。原因と、次に打つ手を添える。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ErrorBody {
    /// 機械が分岐するための種別。
    pub kind: String,
    /// 人が読む説明。
    pub message: String,
    /// 次に打つ手。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ErrorBody {
    /// 監督者に届かなかった時の答え (= 決定 4)。
    pub fn supervisor_not_running() -> Self {
        Self {
            kind: "supervisor_not_running".to_string(),
            message: "the supervisor is not running, so no unit can be started, stopped, or inspected"
                .to_string(),
            hint: Some(
                "run `hyoui web service start` to load it, or `hyoui web daemon supervise` to run it in the foreground"
                    .to_string(),
            ),
        }
    }

    /// 登録簿にない unit を指された時の答え。
    pub fn unknown_unit(name: &str) -> Self {
        Self {
            kind: "unknown_unit".to_string(),
            message: format!("there is no web gateway unit named `{name}`"),
            hint: Some("run `hyoui web daemon list` to see the registered units".to_string()),
        }
    }

    /// 要求そのものが読めなかった / 果たせなかった時の答え。
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: "request_failed".to_string(),
            message: message.into(),
            hint: None,
        }
    }
}

/// 1 行 JSON を書いて flush する。
pub fn write_line<T: Serialize>(stream: &mut impl Write, value: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()
}

/// 1 行 JSON を読む。相手が何も送らずに閉じたら `None`。
pub fn read_line<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(line.trim_end())
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// 監督者へ 1 要求を送り、1 応答を読む。
///
/// 監督者が居なければ [`ErrorBody::supervisor_not_running`] を `Err` で返す。
/// 応答を読んだ後の stream は、`log --follow` が続きを読むために返す。
pub fn request(
    socket: &std::path::Path,
    request: &Request,
) -> Result<(Response, BufReader<UnixStream>), ErrorBody> {
    let mut stream =
        UnixStream::connect(socket).map_err(|_| ErrorBody::supervisor_not_running())?;
    write_line(&mut stream, request)
        .map_err(|error| ErrorBody::failed(format!("could not send the request: {error}")))?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| ErrorBody::failed(format!("could not read the reply: {error}")))?,
    );
    let response = read_line::<Response>(&mut reader)
        .map_err(|error| ErrorBody::failed(format!("could not read the reply: {error}")))?
        .ok_or_else(|| {
            ErrorBody::failed("the supervisor closed the connection without replying")
        })?;
    Ok((response, reader))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(version: &str, build_id: Option<&str>) -> VersionInfo {
        VersionInfo {
            version: version.to_string(),
            build_id: build_id.map(str::to_string),
        }
    }

    #[test]
    fn requests_round_trip_as_one_line() {
        let cases = [
            Request::Status(Target::all()),
            Request::Status(Target::named("unstable")),
            Request::Start(Target::named("stable")),
            Request::Stop(Target::all()),
            Request::Restart(Target::named("unstable")),
            Request::Log {
                target: Target::named("stable"),
                follow: true,
            },
            Request::Reload,
        ];
        for case in cases {
            let line = serde_json::to_string(&case).unwrap();
            assert!(!line.contains('\n'), "{line}");
            assert_eq!(
                serde_json::from_str::<Request>(&line).unwrap(),
                case,
                "{line}"
            );
        }
    }

    /// 対象の指定は `name` の有無だけで表す (= `--all` に別 field を持たせない)。
    #[test]
    fn a_missing_name_means_every_unit() {
        assert_eq!(
            serde_json::to_string(&Request::Status(Target::all())).unwrap(),
            r#"{"op":"status"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Status(Target::named("x"))).unwrap(),
            r#"{"op":"status","name":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Log {
                target: Target::named("x"),
                follow: true,
            })
            .unwrap(),
            r#"{"op":"log","name":"x","follow":true}"#
        );
    }

    /// `restart_needed` は両方分かって食い違う時だけ true (= 決定 7a)。
    #[test]
    fn restart_needed_only_when_both_versions_are_known_and_differ() {
        let running = version("0.9.44", Some("9f0e1d2"));
        let on_disk = version("0.9.44", Some("7c8b9a0-dirty"));

        assert!(VersionPair::new(Some(running.clone()), Some(on_disk)).restart_needed);
        // version が同じで build_id も同じなら入れ替えは要らない。
        assert!(!VersionPair::new(Some(running.clone()), Some(running.clone())).restart_needed);
        // 片方が分からないのは「比べられない」であって食い違いではない。
        assert!(!VersionPair::new(Some(running.clone()), None).restart_needed);
        assert!(!VersionPair::new(None, Some(running.clone())).restart_needed);
        assert!(!VersionPair::new(None, None).restart_needed);
        // version が同じでも build_id が違えば入れ替えが要る (= crate version は
        // tag まで動かないので、ここが hyoui の判別の中心)。
        assert!(
            VersionPair::new(
                Some(version("0.9.44", None)),
                Some(version("0.9.44", Some("abc")))
            )
            .restart_needed
        );
    }

    /// 監督者に届かなければ `supervisor_not_running` を返す (= socket の有無で
    /// 判定せず、実際に繋いで確かめる)。
    #[test]
    fn an_absent_socket_is_reported_as_a_stopped_supervisor() {
        let home = tempfile::tempdir().unwrap();
        let error = request(
            &home.path().join("supervisor.sock"),
            &Request::Status(Target::all()),
        )
        .expect_err("there is no supervisor to answer");
        assert_eq!(error.kind, "supervisor_not_running");

        // socket ファイルが残っていても、誰も listen していなければ同じ答え。
        let stale = home.path().join("stale.sock");
        std::fs::write(&stale, "").unwrap();
        assert_eq!(
            request(&stale, &Request::Status(Target::all()))
                .expect_err("a stale socket file is not a supervisor")
                .kind,
            "supervisor_not_running"
        );
    }

    #[test]
    fn supervisor_absence_carries_a_hint() {
        let error = ErrorBody::supervisor_not_running();
        assert_eq!(error.kind, "supervisor_not_running");
        let hint = error.hint.expect("hint");
        assert!(hint.contains("hyoui web service start"), "{hint}");
        assert!(hint.contains("hyoui web daemon supervise"), "{hint}");
    }

    #[test]
    fn lines_are_framed_one_per_message() {
        let mut buffer = Vec::new();
        write_line(&mut buffer, &Request::Reload).unwrap();
        write_line(&mut buffer, &Request::Status(Target::all())).unwrap();

        let mut reader = std::io::BufReader::new(buffer.as_slice());
        assert_eq!(
            read_line::<Request>(&mut reader).unwrap(),
            Some(Request::Reload)
        );
        assert_eq!(
            read_line::<Request>(&mut reader).unwrap(),
            Some(Request::Status(Target::all()))
        );
        assert_eq!(read_line::<Request>(&mut reader).unwrap(), None);
    }

    #[test]
    fn responses_round_trip() {
        let response = Response::Units {
            units: vec![UnitStatus {
                name: "unstable".to_string(),
                enabled: true,
                running: true,
                pid: Some(4242),
                started_at: Some("2026-09-15T16:02:31Z".to_string()),
                listen: "127.0.0.1:43691".to_string(),
                binary: PathBuf::from("/tmp/hyoui"),
                binary_exists: true,
                version: VersionPair::new(Some(version("0.9.44", Some("9f0e1d2"))), None),
                restarts: 0,
                last_exit: None,
            }],
            supervisor: SupervisorInfo {
                pid: 4211,
                binary: PathBuf::from("/opt/homebrew/bin/hyoui"),
                version: version("0.9.44", None),
            },
            notes: vec!["skipped `stable` because it is stopped".to_string()],
        };
        let line = serde_json::to_string(&response).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), response);
    }
}
