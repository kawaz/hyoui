//! 走っている版と置いてある版の取り方 (= DR-0034 決定 7 / 7a)。
//!
//! 走っているプロセスがメモリに載せている版は本人にしか言えないので gateway の
//! `/version` に聞き、置いてある版は実行ファイルに `--version` を聞く。ファイルの
//! 中身から版を推し量る経路は持たない (= 中の文字列が何を指すかはその binary の
//! 作りしだいで、こちらから決められない)。
//!
//! 口を trait にしてあるのは、実際のプロセスを走らせずに組み立てを検証するため
//! (= 決定 7a の「ディスクを読む口の注入」)。

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use hyoui::version::{VersionInfo, parse_version_line};

/// 版を答えられる相手に聞く口。
pub trait VersionProbe: Send + Sync {
    /// `binary` を実行して `--version` を聞く (= 次に上がる時の版)。
    fn on_disk(&self, binary: &Path) -> Option<VersionInfo>;

    /// gateway の `/version` に聞く (= 今処理している版)。
    fn running(&self, listen: &str) -> Option<VersionInfo>;
}

/// 実機に聞く実装。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl VersionProbe for SystemProbe {
    fn on_disk(&self, binary: &Path) -> Option<VersionInfo> {
        let output = Command::new(binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // 名乗りが `hyoui ` で始まらない出力は「分からない」にする — `--binary` で
        // 別の何かが焼かれていることもあり、その出力を版として読むと嘘になる。
        parse_version_line(stdout.lines().next().unwrap_or_default().trim())
    }

    fn running(&self, listen: &str) -> Option<VersionInfo> {
        let body = http_get_body(&probe_target(listen), "/version")?;
        let parsed: VersionInfo = serde_json::from_str(body.trim()).ok()?;
        Some(parsed)
    }
}

/// 問い合わせ先の読み替え (= 決定 7)。
///
/// listen が wildcard の unit には、そのアドレスのままでは繋がらない環境がある。
/// loopback に読み替えて聞く。
pub fn probe_target(listen: &str) -> String {
    match listen.rsplit_once(':') {
        Some((host, port)) => {
            let host = match host.trim() {
                "0.0.0.0" => "127.0.0.1",
                "[::]" => "[::1]",
                other => other,
            };
            format!("{host}:{port}")
        }
        None => listen.to_string(),
    }
}

/// `GET <path>` を 1 回だけ投げて body を返す。
///
/// HTTP client の依存を足さずに済ませる。応答が 200 でなければ `None`。
pub fn http_get_body(target: &str, path: &str) -> Option<String> {
    let mut stream = std::net::TcpStream::connect(target).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    let status = head.lines().next()?;
    status.contains(" 200 ").then(|| body.to_string())
}

/// `GET /healthz` が 200 を返すか (= 決定 5 の待ち合わせ条件)。
pub fn healthz_is_ok(listen: &str) -> bool {
    http_get_body(&probe_target(listen), "/healthz").is_some_and(|body| body.trim() == "ok")
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 実プロセスを走らせずに版の組み立てを検証する口。
    #[derive(Debug, Default)]
    pub struct FakeProbe {
        on_disk: Mutex<HashMap<std::path::PathBuf, VersionInfo>>,
        running: Mutex<HashMap<String, VersionInfo>>,
    }

    impl FakeProbe {
        pub fn with_on_disk(
            self,
            binary: impl Into<std::path::PathBuf>,
            info: VersionInfo,
        ) -> Self {
            self.on_disk.lock().unwrap().insert(binary.into(), info);
            self
        }

        pub fn with_running(self, listen: impl Into<String>, info: VersionInfo) -> Self {
            self.running.lock().unwrap().insert(listen.into(), info);
            self
        }
    }

    impl VersionProbe for FakeProbe {
        fn on_disk(&self, binary: &Path) -> Option<VersionInfo> {
            self.on_disk.lock().unwrap().get(binary).cloned()
        }

        fn running(&self, listen: &str) -> Option<VersionInfo> {
            self.running.lock().unwrap().get(listen).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_addresses_are_asked_over_loopback() {
        assert_eq!(probe_target("0.0.0.0:43690"), "127.0.0.1:43690");
        assert_eq!(probe_target("[::]:43690"), "[::1]:43690");
        // 具体的な宛先はそのまま聞く。
        assert_eq!(probe_target("127.0.0.1:43690"), "127.0.0.1:43690");
        assert_eq!(probe_target("localhost:43690"), "localhost:43690");
        assert_eq!(probe_target("[::1]:43690"), "[::1]:43690");
        // 宛先として成り立たない綴りは読み替えの対象にしない (= `add` が
        // `SocketAddr` に解決できた値しか登録簿に入らないので、ここで救う相手が
        // 居ない)。port が無い値も同じ。
        assert_eq!(probe_target("::43690"), "::43690");
        assert_eq!(probe_target("weird"), "weird");
    }

    /// 走っているプロセスに聞く口は、繋がらなければ `None` を返して黙る。
    #[test]
    fn an_unreachable_gateway_has_no_running_version() {
        // 誰も listen していない port (= 予約範囲外の高位 port を即席で確保して閉じる)。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);

        assert_eq!(SystemProbe.running(&address), None);
        assert!(!healthz_is_ok(&address));
    }

    /// 名乗りが違う実行ファイルの出力は版として読まない (= 決定 7a)。
    #[test]
    fn a_binary_that_does_not_identify_as_hyoui_has_no_version() {
        // `true` は何も出さずに成功する = 1 行目が空で parse できない。
        for candidate in ["/usr/bin/true", "/bin/true"] {
            let path = Path::new(candidate);
            if path.exists() {
                assert_eq!(SystemProbe.on_disk(path), None);
                return;
            }
        }
    }

    #[test]
    fn a_missing_binary_has_no_version() {
        assert_eq!(
            SystemProbe.on_disk(Path::new("/nonexistent/hyoui-does-not-exist")),
            None
        );
    }

    /// 自分自身に聞けば、自分が名乗る版が返る (= 経路が実際に通っていることの確認)。
    #[test]
    fn asking_this_binary_returns_the_version_it_prints() {
        let own = std::env::current_exe().unwrap();
        // test binary は `--version` を解さないので、CLI の bin に聞く。
        let cli = own
            .parent()
            .and_then(|dir| dir.parent())
            .map(|dir| dir.join("hyoui"));
        let Some(cli) = cli.filter(|path| path.exists()) else {
            return;
        };
        let reported = SystemProbe
            .on_disk(&cli)
            .expect("hyoui reports its version");
        assert_eq!(reported.version, hyoui::VERSION);
    }
}
