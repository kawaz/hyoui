//! `hyoui web daemon supervise` — 子 gateway を抱える監督者 (= DR-0034 決定 3)。
//!
//! 監督者は「登録簿の `enabled` を見て起こし、落ちたら上げ直す」だけの役に閉じる。
//! unit の `listen` / `web_assets_dir` を argv に展開せず、渡すのは unit 名だけで、
//! 値を読むのは子 (`hyoui web daemon run <name>`) 。監督者が読んだ値と子が使う値の
//! 2 系統を作らないため (決定 3)。
//!
//! ## 待ち方
//!
//! signal / 制御 socket / 子の終了 / 子の出力を 1 つの channel に集約し、backoff と
//! grace の満了は `recv_timeout` の期限として待つ。間隔に根拠のない sleep ループは
//! 持たない (= 満了時刻そのものが期限になる)。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use hyoui::version::VersionInfo;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use super::probe::{VersionProbe, healthz_is_ok};
use super::protocol::{
    ErrorBody, LogLine, Request, Response, SupervisorInfo, Target, UnitStatus, VersionPair,
    read_line, write_line,
};
use super::registry::{Registry, Unit};

/// 時間に関する定数。test が実時間を待たないよう注入できる (= DR-0034 P3 の gate)。
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// 落ちた子を次に起こすまでの初回待ち。以後は倍々。
    pub initial_backoff: Duration,
    /// backoff の上限。
    pub max_backoff: Duration,
    /// SIGTERM から SIGKILL までの猶予。
    pub grace: Duration,
    /// `restart --all` が `/healthz` を叩く間隔。
    pub health_interval: Duration,
    /// 1 unit あたりの `/healthz` 待ちの上限。
    pub health_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            grace: Duration::from_secs(10),
            health_interval: Duration::from_millis(200),
            health_timeout: Duration::from_secs(30),
        }
    }
}

/// 子の今の居方。
#[derive(Debug)]
enum State {
    /// 子が居ない。
    Down,
    /// 子が走っている。
    Running {
        /// 子の pid。
        pid: u32,
        /// 起きた時刻 (ISO 8601)。
        started_at: String,
    },
    /// SIGTERM を送って降りるのを待っている。
    Stopping {
        /// 子の pid。
        pid: u32,
        /// この時刻を過ぎたら SIGKILL。
        deadline: Instant,
        /// SIGKILL まで送ったか。
        killed: bool,
    },
}

/// 1 unit の走行状態。
#[derive(Debug)]
struct Runtime {
    unit: Unit,
    state: State,
    /// 次に起こしてよい時刻 (= backoff 満了)。`None` なら起こさない。
    next_start: Option<Instant>,
    /// 次の backoff 幅。
    backoff: Duration,
    restarts: u32,
    last_exit: Option<String>,
}

impl Runtime {
    fn new(unit: Unit, backoff: Duration) -> Self {
        Self {
            unit,
            state: State::Down,
            next_start: None,
            backoff,
            restarts: 0,
            last_exit: None,
        }
    }

    fn pid(&self) -> Option<u32> {
        match &self.state {
            State::Running { pid, .. } => Some(*pid),
            State::Stopping { pid, .. } => Some(*pid),
            State::Down => None,
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.state, State::Running { .. })
    }

    fn started_at(&self) -> Option<String> {
        match &self.state {
            State::Running { started_at, .. } => Some(started_at.clone()),
            _ => None,
        }
    }
}

/// メインループが待つ出来事。
enum Event {
    /// 制御 socket に接続が来た。
    Request(UnixStream),
    /// 子が終わった。
    ChildExited {
        name: String,
        pid: u32,
        summary: String,
    },
    /// 子が 1 行書いた。
    LogLine { name: String, line: String },
    /// signal が届いた。
    Signal(i32),
}

/// 監督者。
pub struct Supervisor {
    registry: Registry,
    root: PathBuf,
    timings: Timings,
    probe: Box<dyn VersionProbe>,
    units: BTreeMap<String, Runtime>,
    events: Receiver<Event>,
    sender: Sender<Event>,
    /// `log --follow` で繋がっている相手 (= 名前で絞っている場合はその名前)。
    followers: Vec<(Option<String>, UnixStream)>,
    shutting_down: bool,
}

impl Supervisor {
    /// 登録簿と root を指定して組み立てる。
    pub fn new(
        registry: Registry,
        root: PathBuf,
        timings: Timings,
        probe: Box<dyn VersionProbe>,
    ) -> Self {
        let (sender, events) = channel();
        Self {
            registry,
            root,
            timings,
            probe,
            units: BTreeMap::new(),
            events,
            sender,
            followers: Vec::new(),
            shutting_down: false,
        }
    }

    /// 制御 socket を開き、登録簿どおりに子を起こしてから待ち続ける。
    ///
    /// SIGTERM / SIGINT を受けたら子を全部止めてから戻る (決定 3)。
    pub fn run(&mut self, socket: &Path) -> std::io::Result<()> {
        let listener = bind_control_socket(socket)?;
        self.spawn_accept_thread(listener);
        self.spawn_signal_thread()?;
        self.reload();

        while !self.finished() {
            let timeout = self.next_deadline();
            match timeout {
                Some(timeout) => match self.events.recv_timeout(timeout) {
                    Ok(event) => self.handle(event),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match self.events.recv() {
                    Ok(event) => self.handle(event),
                    Err(_) => break,
                },
            }
            self.tick();
        }

        let _ = std::fs::remove_file(socket);
        Ok(())
    }

    /// 停止しきったか (= SIGTERM を受けて子が全部降りたか)。
    fn finished(&self) -> bool {
        self.shutting_down
            && self
                .units
                .values()
                .all(|runtime| matches!(runtime.state, State::Down))
    }

    /// 次に何かをしなければならない時刻までの残り。
    fn next_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut earliest: Option<Instant> = None;
        for runtime in self.units.values() {
            let candidate = match &runtime.state {
                State::Stopping { deadline, .. } => Some(*deadline),
                State::Down if !self.shutting_down && runtime.unit.enabled => runtime.next_start,
                _ => None,
            };
            if let Some(candidate) = candidate
                && earliest.is_none_or(|current| candidate < current)
            {
                earliest = Some(candidate);
            }
        }
        earliest.map(|deadline| deadline.saturating_duration_since(now))
    }

    /// 期限が来たものを進める。
    fn tick(&mut self) {
        let now = Instant::now();
        let names: Vec<String> = self.units.keys().cloned().collect();
        for name in names {
            let Some(runtime) = self.units.get_mut(&name) else {
                continue;
            };
            match &mut runtime.state {
                // grace を過ぎたら SIGKILL に上げる。
                State::Stopping {
                    pid,
                    deadline,
                    killed,
                } if *deadline <= now && !*killed => {
                    let pid = *pid;
                    *killed = true;
                    *deadline = now + self.timings.grace;
                    send_signal(pid, Signal::SIGKILL);
                }
                State::Down
                    if !self.shutting_down
                        && runtime.unit.enabled
                        && runtime.next_start.is_some_and(|at| at <= now) =>
                {
                    self.start(&name);
                }
                _ => {}
            }
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Request(stream) => self.serve_request(stream),
            Event::ChildExited { name, pid, summary } => self.child_exited(&name, pid, summary),
            Event::LogLine { name, line } => self.fan_out_log(&name, &line),
            Event::Signal(signal) => self.begin_shutdown(signal),
        }
    }

    /// 子が終わった。stop の結果なら据え置き、自然死なら backoff を置いて起こし直す。
    fn child_exited(&mut self, name: &str, pid: u32, summary: String) {
        let Some(runtime) = self.units.get_mut(name) else {
            return;
        };
        // 入れ替わった後の古い子の報告は捨てる (= pid が今の子と違う)。
        if runtime.pid().is_some_and(|current| current != pid) {
            return;
        }
        let was_stopping = matches!(runtime.state, State::Stopping { .. });
        runtime.state = State::Down;
        runtime.last_exit = Some(summary);

        if was_stopping || !runtime.unit.enabled || self.shutting_down {
            runtime.next_start = None;
            return;
        }
        runtime.restarts += 1;
        runtime.next_start = Some(Instant::now() + runtime.backoff);
        runtime.backoff = (runtime.backoff * 2).min(self.timings.max_backoff);
    }

    /// 子を起こす。
    fn start(&mut self, name: &str) {
        let Some(runtime) = self.units.get(name) else {
            return;
        };
        if runtime.is_running() {
            return;
        }
        let binary = runtime.unit.binary.clone();
        let log_path = self.log_path(name);

        let mut command = Command::new(&binary);
        // 監督者が渡すのは unit 名だけ。値を読むのは子 (決定 3)。
        command
            .args(["web", "daemon", "run", name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let Some(runtime) = self.units.get_mut(name) else {
                    return;
                };
                runtime.last_exit =
                    Some(format!("could not start `{}`: {error}", binary.display()));
                runtime.restarts += 1;
                runtime.next_start = Some(Instant::now() + runtime.backoff);
                runtime.backoff = (runtime.backoff * 2).min(self.timings.max_backoff);
                return;
            }
        };

        let pid = child.id();
        for stream in [
            child.stdout.take().map(Pump::Stdout),
            child.stderr.take().map(Pump::Stderr),
        ]
        .into_iter()
        .flatten()
        {
            self.spawn_log_pump(name.to_string(), log_path.clone(), stream);
        }

        let sender = self.sender.clone();
        let reaped = name.to_string();
        // 終了は待つ thread が教える。定期的に生存を確かめる形は取らない。
        std::thread::spawn(move || {
            let summary = match child.wait() {
                Ok(status) => describe_exit(&status),
                Err(error) => format!("could not wait for the child: {error}"),
            };
            let _ = sender.send(Event::ChildExited {
                name: reaped,
                pid,
                summary,
            });
        });

        if let Some(runtime) = self.units.get_mut(name) {
            runtime.state = State::Running {
                pid,
                started_at: super::registry::now_iso8601(),
            };
            runtime.next_start = None;
        }
    }

    /// 子に降りてもらう (SIGTERM → grace → SIGKILL)。
    fn stop(&mut self, name: &str) {
        let grace = self.timings.grace;
        let Some(runtime) = self.units.get_mut(name) else {
            return;
        };
        runtime.next_start = None;
        if let State::Running { pid, .. } = runtime.state {
            runtime.state = State::Stopping {
                pid,
                deadline: Instant::now() + grace,
                killed: false,
            };
            send_signal(pid, Signal::SIGTERM);
        }
    }

    /// SIGTERM / SIGINT を受けた。抱えている子を全部止めてから終わる (決定 3 / 6)。
    ///
    /// 監督者を止めると子も止まるので、何を受けて降りたのかを残す (= `service log`
    /// から「落ちた」のか「止められた」のかが読める)。
    fn begin_shutdown(&mut self, signal: i32) {
        if self.shutting_down {
            return;
        }
        eprintln!(
            "hyoui web daemon supervise: got signal {signal}; stopping {} unit(s)",
            self.units
                .values()
                .filter(|runtime| runtime.is_running())
                .count()
        );
        self.shutting_down = true;
        let names: Vec<String> = self.units.keys().cloned().collect();
        for name in names {
            self.stop(&name);
        }
    }

    /// 登録簿を読み直し、望みとの差を埋める (= 内部 op `reload`)。
    fn reload(&mut self) {
        let listed = match self.registry.list() {
            Ok(listed) => listed,
            Err(_) => return,
        };
        let initial = self.timings.initial_backoff;

        // 登録簿から消えた unit は抱えたままにしない。
        let known: Vec<String> = listed.iter().map(|(name, _)| name.clone()).collect();
        let dropped: Vec<String> = self
            .units
            .keys()
            .filter(|name| !known.contains(name))
            .cloned()
            .collect();
        for name in dropped {
            self.stop(&name);
            if let Some(runtime) = self.units.get(&name)
                && matches!(runtime.state, State::Down)
            {
                self.units.remove(&name);
            }
        }

        for (name, unit) in listed {
            let enabled = unit.enabled;
            match self.units.get_mut(&name) {
                Some(runtime) => runtime.unit = unit,
                None => {
                    self.units.insert(name.clone(), Runtime::new(unit, initial));
                }
            }
            if self.shutting_down {
                continue;
            }
            let running = self.units.get(&name).is_some_and(Runtime::is_running);
            if enabled && !running {
                let Some(runtime) = self.units.get_mut(&name) else {
                    continue;
                };
                if runtime.next_start.is_none() {
                    runtime.next_start = Some(Instant::now());
                }
            } else if !enabled && running {
                self.stop(&name);
            }
        }
    }

    // -------------------------------------------------------------------------
    // 制御 socket
    // -------------------------------------------------------------------------

    fn serve_request(&mut self, mut stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let peer = match stream.try_clone() {
            Ok(peer) => peer,
            Err(_) => return,
        };
        let mut reader = BufReader::new(peer);
        let request: Option<Request> = read_line(&mut reader).ok().flatten();
        let Some(request) = request else {
            let _ = write_line(
                &mut stream,
                &Response::Error(ErrorBody::failed("the request was not one line of JSON")),
            );
            return;
        };

        match request {
            Request::List(target) => {
                let response = self.units_response(&target, Vec::new(), Versions::Skip);
                let _ = write_line(&mut stream, &response);
            }
            Request::Status(target) => {
                let response = self.status_response(&target, Vec::new());
                let _ = write_line(&mut stream, &response);
            }
            Request::Start(target) => {
                let response = self.apply_start(&target);
                let _ = write_line(&mut stream, &response);
            }
            Request::Stop(target) => {
                let response = self.apply_stop(&target);
                let _ = write_line(&mut stream, &response);
            }
            Request::Restart(target) => {
                let response = self.apply_restart(&target);
                let _ = write_line(&mut stream, &response);
            }
            Request::Reload => {
                self.reload();
                let response = self.status_response(&Target::all(), Vec::new());
                let _ = write_line(&mut stream, &response);
            }
            Request::Log { target, follow } => self.serve_log(stream, &target, follow),
        }
    }

    /// 名前が指定されていれば解決する。登録簿に無ければエラー。
    fn resolve(&self, target: &Target) -> Result<Vec<String>, ErrorBody> {
        match &target.name {
            None => Ok(self.units.keys().cloned().collect()),
            Some(name) if self.units.contains_key(name) => Ok(vec![name.clone()]),
            Some(name) => Err(ErrorBody::unknown_unit(name)),
        }
    }

    /// `start` は desired state を書き換える (= 「上げてほしい」という意思表示)。
    fn apply_start(&mut self, target: &Target) -> Response {
        let names = match self.resolve(target) {
            Ok(names) => names,
            Err(error) => return Response::Error(error),
        };
        let mut notes = Vec::new();
        for name in names {
            if let Err(error) = self.set_enabled(&name, true) {
                notes.push(error);
                continue;
            }
            if let Some(runtime) = self.units.get_mut(&name) {
                // 人が明示的に起こす時は backoff を待たせない。
                runtime.backoff = self.timings.initial_backoff;
                if !runtime.is_running() {
                    runtime.next_start = Some(Instant::now());
                }
            }
        }
        self.tick();
        self.status_response(target, notes)
    }

    /// `stop` も desired state を書き換える。止めた unit は登録簿に残る。
    fn apply_stop(&mut self, target: &Target) -> Response {
        let names = match self.resolve(target) {
            Ok(names) => names,
            Err(error) => return Response::Error(error),
        };
        let mut notes = Vec::new();
        for name in names {
            if let Err(error) = self.set_enabled(&name, false) {
                notes.push(error);
                continue;
            }
            self.stop(&name);
        }
        self.status_response(target, notes)
    }

    /// `restart <name>` は start と同義、`--all` は enabled のみ 1 台ずつ (決定 5)。
    fn apply_restart(&mut self, target: &Target) -> Response {
        let names = match self.resolve(target) {
            Ok(names) => names,
            Err(error) => return Response::Error(error),
        };
        let mut notes = Vec::new();

        if target.name.is_some() {
            // 名前を明示した時だけ desired state を書き換える。
            return self.apply_start(target);
        }

        // 名前の昇順で 1 台ずつ。前段が優先順で振り分けるので、順に上げ直せば
        // 外から見た断が出ない。全台を同時に落とす経路は持たない。
        for name in names {
            let enabled = self
                .units
                .get(&name)
                .is_some_and(|runtime| runtime.unit.enabled);
            if !enabled {
                // `stop` が書いた desired state を `restart --all` が覆さない。
                notes.push(format!("skipped `{name}` because it is stopped"));
                continue;
            }
            self.stop(&name);
            if !self.pump_until(self.timings.grace * 2, |this| {
                this.units
                    .get(&name)
                    .is_some_and(|runtime| matches!(runtime.state, State::Down))
            }) {
                notes.push(format!(
                    "`{name}` did not stop; stopped going through the units"
                ));
                break;
            }
            if let Some(runtime) = self.units.get_mut(&name) {
                runtime.backoff = self.timings.initial_backoff;
                runtime.next_start = Some(Instant::now());
            }
            self.tick();

            let listen = self
                .units
                .get(&name)
                .map(|runtime| runtime.unit.listen.clone())
                .unwrap_or_default();
            if !self.wait_for_healthz(&listen) {
                notes.push(format!(
                    "`{name}` did not answer /healthz within {} seconds; stopped going through the units",
                    self.timings.health_timeout.as_secs()
                ));
                return match self.status_response(target, notes) {
                    Response::Units {
                        units,
                        supervisor,
                        notes,
                    } => Response::Units {
                        units,
                        supervisor,
                        notes,
                    },
                    other => other,
                };
            }
        }
        self.status_response(target, notes)
    }

    /// 条件が成立するまで event を処理し続ける。成立したら `true`。
    ///
    /// 待っている間も channel を読む — 読まずに待つと、その間に届いた子の終了を
    /// 取りこぼして状態がずれる。
    fn pump_until(&mut self, limit: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while !done(self) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return done(self);
            }
            // grace の満了 (= SIGKILL への切り替え) も待つ必要があるので、
            // 自分の期限と条件待ちの期限の近い方で起きる。
            let wait = self
                .next_deadline()
                .map_or(remaining, |next| next.min(remaining));
            match self.events.recv_timeout(wait) {
                Ok(event) => self.handle(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return done(self),
            }
            self.tick();
        }
        true
    }

    /// `/healthz` が 200 を返すまで待つ (= 決定 5、200ms 間隔・30 秒上限)。
    fn wait_for_healthz(&mut self, listen: &str) -> bool {
        if listen.is_empty() {
            return false;
        }
        let deadline = Instant::now() + self.timings.health_timeout;
        loop {
            if healthz_is_ok(listen) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            // 叩く間隔の分だけ event を処理しながら待つ。
            self.pump_until(self.timings.health_interval, |_| false);
        }
    }

    fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), String> {
        match self.registry.set_enabled(name, enabled) {
            Ok(unit) => {
                if let Some(runtime) = self.units.get_mut(name) {
                    runtime.unit = unit;
                }
                Ok(())
            }
            Err(error) => Err(format!(
                "could not record the new state of `{name}`: {error}"
            )),
        }
    }

    fn status_response(&self, target: &Target, notes: Vec<String>) -> Response {
        self.units_response(target, notes, Versions::Ask)
    }

    fn units_response(&self, target: &Target, notes: Vec<String>, versions: Versions) -> Response {
        let names = match self.resolve(target) {
            Ok(names) => names,
            Err(error) => return Response::Error(error),
        };
        let units = names
            .iter()
            .filter_map(|name| {
                self.units
                    .get(name)
                    .map(|runtime| self.describe(name, runtime, versions))
            })
            .collect();
        Response::Units {
            units,
            supervisor: SupervisorInfo {
                pid: std::process::id(),
                binary: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("hyoui")),
                version: VersionInfo::current(),
            },
            notes,
        }
    }

    fn describe(&self, name: &str, runtime: &Runtime, versions: Versions) -> UnitStatus {
        let (running_version, on_disk) = match versions {
            Versions::Skip => (None, None),
            Versions::Ask => (
                runtime
                    .is_running()
                    .then(|| self.probe.running(&runtime.unit.listen))
                    .flatten(),
                self.probe.on_disk(&runtime.unit.binary),
            ),
        };
        UnitStatus {
            name: name.to_string(),
            enabled: runtime.unit.enabled,
            running: runtime.is_running(),
            pid: runtime.pid(),
            started_at: runtime.started_at(),
            listen: runtime.unit.listen.clone(),
            binary: runtime.unit.binary.clone(),
            binary_exists: runtime.unit.binary.exists(),
            version: VersionPair::new(running_version, on_disk),
            restarts: runtime.restarts,
            last_exit: runtime.last_exit.clone(),
        }
    }

    // -------------------------------------------------------------------------
    // ログ
    // -------------------------------------------------------------------------

    fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    fn log_path(&self, name: &str) -> PathBuf {
        self.log_dir().join(format!("{name}.log"))
    }

    fn serve_log(&mut self, mut stream: UnixStream, target: &Target, follow: bool) {
        let names = match self.resolve(target) {
            Ok(names) => names,
            Err(error) => {
                let _ = write_line(&mut stream, &Response::Error(error));
                return;
            }
        };
        let mut lines = Vec::new();
        for name in &names {
            // 置いてある分は末尾だけ返す。全部返すと回転前のファイル 1 つで応答が
            // 膨れるので、続きが要る場合は `--follow` で受ける。
            for line in tail_lines(&self.log_path(name), LOG_TAIL_LINES) {
                lines.push(LogLine {
                    name: name.clone(),
                    line,
                });
            }
        }
        if write_line(&mut stream, &Response::Log { lines, follow }).is_err() {
            return;
        }
        if follow {
            self.followers.push((target.name.clone(), stream));
        }
    }

    /// 子が書いた 1 行をログに足し、追従している相手へ配る (= 決定 9)。
    fn fan_out_log(&mut self, name: &str, line: &str) {
        let message = LogLine {
            name: name.to_string(),
            line: line.to_string(),
        };
        self.followers.retain_mut(|(filter, stream)| {
            if filter.as_deref().is_some_and(|wanted| wanted != name) {
                return true;
            }
            write_line(stream, &message).is_ok()
        });
    }

    fn spawn_log_pump(&self, name: String, log_path: PathBuf, pump: Pump) {
        let sender = self.sender.clone();
        let _ = std::fs::create_dir_all(self.log_dir());
        std::thread::spawn(move || {
            let reader: Box<dyn std::io::Read + Send> = match pump {
                Pump::Stdout(stdout) => Box::new(stdout),
                Pump::Stderr(stderr) => Box::new(stderr),
            };
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                let text = line.trim_end_matches(['\n', '\r']).to_string();
                append_log_line(&log_path, &text);
                if sender
                    .send(Event::LogLine {
                        name: name.clone(),
                        line: text,
                    })
                    .is_err()
                {
                    break;
                }
                line.clear();
            }
        });
    }

    // -------------------------------------------------------------------------
    // 入力経路
    // -------------------------------------------------------------------------

    fn spawn_accept_thread(&self, listener: UnixListener) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        if sender.send(Event::Request(stream)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    /// signal を self-pipe 経由で channel に流す。
    ///
    /// handler の中で出来ることは限られる (async-signal-safe) ので、pipe に 1 byte
    /// 書いてもらい、それを読む thread が event に変える。
    fn spawn_signal_thread(&self) -> std::io::Result<()> {
        use std::os::fd::AsFd;

        let pipe = hyoui::sys::install_self_pipe()
            .map_err(|error| std::io::Error::other(format!("could not arm signals: {error}")))?;
        for signal in [Signal::SIGTERM, Signal::SIGINT] {
            hyoui::sys::register_self_pipe(signal).map_err(|error| {
                std::io::Error::other(format!("could not arm {signal}: {error}"))
            })?;
        }

        let sender = self.sender.clone();
        std::thread::spawn(move || {
            use nix::poll::{PollFd, PollFlags, poll};
            loop {
                let fd = pipe.read.as_fd();
                let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
                match poll(&mut fds, nix::poll::PollTimeout::NONE) {
                    Ok(_) => {}
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => break,
                }
                let Ok(signals) = pipe.drain() else { break };
                for signal in signals {
                    if sender.send(Event::Signal(i32::from(signal))).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(())
    }
}

/// ログに返す末尾行数。
const LOG_TAIL_LINES: usize = 200;

/// 版を聞くか。聞くと unit ごとに短命のプロセスが 1 つ走る (= 決定 7a)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Versions {
    /// 走っている版と置いてある版を聞く (`status` / `version`)。
    Ask,
    /// 聞かない (`list`)。
    Skip,
}

enum Pump {
    Stdout(std::process::ChildStdout),
    Stderr(std::process::ChildStderr),
}

/// 制御 socket を開く。死んだ監督者の socket が残っていれば外して開き直す。
fn bind_control_socket(socket: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        // 繋がるなら本物が走っている。繋がらないなら残骸なので外す。
        if UnixStream::connect(socket).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!(
                    "another supervisor is already listening on {}",
                    socket.display()
                ),
            ));
        }
        std::fs::remove_file(socket)?;
    }
    UnixListener::bind(socket)
}

fn send_signal(pid: u32, signal: Signal) {
    if let Ok(pid) = i32::try_from(pid) {
        let _ = kill(Pid::from_raw(pid), signal);
    }
}

fn describe_exit(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        format!("exited with status {code}")
    } else if let Some(signal) = status.signal() {
        format!("killed by signal {signal}")
    } else {
        "ended for an unknown reason".to_string()
    }
}

fn append_log_line(path: &Path, line: &str) {
    // 追記だけを行い、回転は OS の仕組みに任せる (決定 9)。
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn tail_lines(path: &Path, limit: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(limit)..]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_daemon::probe::fake::FakeProbe;

    fn timings() -> Timings {
        // 実時間を待たない値。60 秒の上限に達することを実時間で確かめる test は
        // 書かない (= 定数を短くして同じ形を見る)。
        Timings {
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(80),
            grace: Duration::from_millis(200),
            health_interval: Duration::from_millis(20),
            health_timeout: Duration::from_millis(600),
        }
    }

    fn unit(listen: &str, binary: &Path, enabled: bool) -> Unit {
        Unit {
            listen: listen.to_string(),
            binary: binary.to_path_buf(),
            web_assets_dir: None,
            enabled,
            added_at: super::super::registry::now_iso8601(),
        }
    }

    /// backoff は初回幅から倍々で伸び、上限で止まる (= 決定 3)。
    #[test]
    fn backoff_doubles_up_to_the_ceiling() {
        let timings = timings();
        let mut runtime = Runtime::new(
            unit("127.0.0.1:1", Path::new("/nonexistent"), true),
            timings.initial_backoff,
        );

        let mut widths = Vec::new();
        for _ in 0..5 {
            widths.push(runtime.backoff);
            runtime.backoff = (runtime.backoff * 2).min(timings.max_backoff);
        }
        assert_eq!(
            widths,
            [
                Duration::from_millis(20),
                Duration::from_millis(40),
                Duration::from_millis(80),
                Duration::from_millis(80),
                Duration::from_millis(80),
            ]
        );
    }

    /// 起動できない binary の unit は、回数と理由を残して backoff に入る。
    #[test]
    fn a_unit_whose_binary_is_missing_records_why_it_is_not_running() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let missing = home.path().join("nonexistent-hyoui");
        registry
            .add("broken", &unit("127.0.0.1:1", &missing, true))
            .unwrap();

        let mut supervisor = Supervisor::new(
            registry,
            home.path().to_path_buf(),
            timings(),
            Box::new(FakeProbe::default()),
        );
        supervisor.reload();
        supervisor.start("broken");

        let runtime = &supervisor.units["broken"];
        assert!(matches!(runtime.state, State::Down));
        assert_eq!(runtime.restarts, 1);
        assert!(
            runtime
                .last_exit
                .as_deref()
                .is_some_and(|reason| reason.contains("could not start")),
            "{:?}",
            runtime.last_exit
        );
        // すぐには叩き直さず、backoff の満了を待つ。
        assert!(runtime.next_start.is_some_and(|at| at > Instant::now()));
        assert_eq!(runtime.backoff, Duration::from_millis(40));
    }

    /// 起こし直しを繰り返すと backoff は上限で止まり、無限に叩き続けない (= 決定 3)。
    ///
    /// 実時間で 60 秒を待つ代わりに定数を注入して同じ形を見る。bind に失敗し続ける
    /// unit (= 同 port が既に埋まっている) もこの経路を通る。
    #[test]
    fn repeated_failures_reach_the_backoff_ceiling_and_stop_widening() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let missing = home.path().join("nonexistent-hyoui");
        registry
            .add("broken", &unit("127.0.0.1:1", &missing, true))
            .unwrap();

        let timings = timings();
        let mut supervisor = Supervisor::new(
            registry,
            home.path().to_path_buf(),
            timings,
            Box::new(FakeProbe::default()),
        );
        supervisor.reload();

        let mut widths = Vec::new();
        for _ in 0..5 {
            supervisor.start("broken");
            widths.push(supervisor.units["broken"].backoff);
        }

        // 20ms → 40 → 80 で上限に達し、以後は伸びない。
        assert_eq!(
            widths,
            [
                Duration::from_millis(40),
                Duration::from_millis(80),
                timings.max_backoff,
                timings.max_backoff,
                timings.max_backoff,
            ]
        );
        // 失敗の回数と理由が `status` から読める (= 「上がらない」ことの手がかり)。
        let runtime = &supervisor.units["broken"];
        assert_eq!(runtime.restarts, 5);
        assert!(runtime.last_exit.is_some());
        assert!(!runtime.is_running());
    }

    /// `reload` は登録簿の enabled を望みとして読み、消えた unit を抱えない。
    #[test]
    fn reload_follows_the_registry() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let binary = home.path().join("hyoui");
        registry
            .add("on", &unit("127.0.0.1:1", &binary, true))
            .unwrap();
        registry
            .add("off", &unit("127.0.0.1:2", &binary, false))
            .unwrap();

        let mut supervisor = Supervisor::new(
            registry.clone(),
            home.path().to_path_buf(),
            timings(),
            Box::new(FakeProbe::default()),
        );
        supervisor.reload();

        // enabled な unit だけが「起こす」対象になる。
        assert!(supervisor.units["on"].next_start.is_some());
        assert!(supervisor.units["off"].next_start.is_none());

        // 登録簿から消えたら抱えない。
        registry.remove("off").unwrap();
        supervisor.reload();
        assert!(!supervisor.units.contains_key("off"));
        assert!(supervisor.units.contains_key("on"));
    }

    /// `status` は監督者自身と unit の版を並べ、聞けない版は `null` のままにする。
    #[test]
    fn status_pairs_the_running_and_on_disk_versions() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let binary = home.path().join("hyoui");
        std::fs::write(&binary, "").unwrap();
        registry
            .add("stable", &unit("127.0.0.1:43690", &binary, true))
            .unwrap();

        let on_disk = VersionInfo {
            version: "0.9.44".to_string(),
            build_id: Some("7c8b9a0".to_string()),
        };
        let probe = FakeProbe::default().with_on_disk(&binary, on_disk.clone());
        let mut supervisor = Supervisor::new(
            registry,
            home.path().to_path_buf(),
            timings(),
            Box::new(probe),
        );
        supervisor.reload();

        let Response::Units {
            units,
            supervisor: info,
            ..
        } = supervisor.status_response(&Target::all(), Vec::new())
        else {
            panic!("expected a unit listing");
        };
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].name, "stable");
        assert!(units[0].binary_exists);
        assert_eq!(units[0].version.on_disk, Some(on_disk));
        // 走っていないので「走っている版」は聞けない = null、比べられないので false。
        assert_eq!(units[0].version.running, None);
        assert!(!units[0].version.restart_needed);
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.version, VersionInfo::current());
    }

    /// 走っている版と置いてある版が食い違えば `restart_needed` が立つ (= 決定 7a)。
    ///
    /// 実際のプロセスを走らせず、版を答える口を差し替えて組み立てだけを見る。
    #[test]
    fn a_rebuilt_binary_makes_the_running_unit_need_a_restart() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let binary = home.path().join("hyoui");
        std::fs::write(&binary, "").unwrap();
        registry
            .add("unstable", &unit("127.0.0.1:43691", &binary, true))
            .unwrap();

        let running = VersionInfo {
            version: "0.9.44".to_string(),
            build_id: Some("9f0e1d2".to_string()),
        };
        // 置いてある方だけ入れ替わった状態 (= ビルドし直したが上げ直していない)。
        let on_disk = VersionInfo {
            version: "0.9.44".to_string(),
            build_id: Some("7c8b9a0-dirty".to_string()),
        };
        let probe = FakeProbe::default()
            .with_running("127.0.0.1:43691", running.clone())
            .with_on_disk(&binary, on_disk.clone());
        let mut supervisor = Supervisor::new(
            registry,
            home.path().to_path_buf(),
            timings(),
            Box::new(probe),
        );
        supervisor.reload();
        // 子を走らせる代わりに、抱えている状態にして版を聞かせる。
        supervisor.units.get_mut("unstable").unwrap().state = State::Running {
            pid: std::process::id(),
            started_at: super::super::registry::now_iso8601(),
        };

        let Response::Units { units, .. } =
            supervisor.status_response(&Target::named("unstable"), Vec::new())
        else {
            panic!("expected a unit listing");
        };
        assert_eq!(units[0].version.running, Some(running));
        assert_eq!(units[0].version.on_disk, Some(on_disk));
        assert!(units[0].version.restart_needed);

        // `list` は版を聞かないので、同じ状態でも組は空のまま (= 短命プロセスを
        // 走らせない軽い口)。
        let Response::Units { units, .. } =
            supervisor.units_response(&Target::all(), Vec::new(), Versions::Skip)
        else {
            panic!("expected a unit listing");
        };
        assert_eq!(units[0].version, VersionPair::default());
        assert!(units[0].running);
    }

    /// 登録簿にない名前を指した要求は `unknown_unit` で断る。
    #[test]
    fn an_unknown_name_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let supervisor = Supervisor::new(
            Registry::at(home.path().join("units")),
            home.path().to_path_buf(),
            timings(),
            Box::new(FakeProbe::default()),
        );
        let error = supervisor.resolve(&Target::named("ghost")).unwrap_err();
        assert_eq!(error.kind, "unknown_unit");
        assert!(error.hint.is_some_and(|hint| hint.contains("daemon list")));
    }

    /// `stop` は登録簿に `enabled = false` を書き、unit 自体は残す (= 決定 4)。
    #[test]
    fn stop_records_the_intent_and_keeps_the_unit() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let binary = home.path().join("hyoui");
        registry
            .add("stable", &unit("127.0.0.1:43690", &binary, true))
            .unwrap();

        let mut supervisor = Supervisor::new(
            registry.clone(),
            home.path().to_path_buf(),
            timings(),
            Box::new(FakeProbe::default()),
        );
        supervisor.reload();
        supervisor.apply_stop(&Target::named("stable"));

        assert!(!registry.get("stable").unwrap().enabled);
        assert!(supervisor.units["stable"].next_start.is_none());

        // `start` は逆に enabled を立てて起こす対象に戻す。
        supervisor.apply_start(&Target::named("stable"));
        assert!(registry.get("stable").unwrap().enabled);
    }

    /// `restart --all` は停止中の unit を触らず、その旨を notes に載せる (= 決定 5)。
    #[test]
    fn restart_all_leaves_stopped_units_alone() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let binary = home.path().join("hyoui");
        registry
            .add("off", &unit("127.0.0.1:43690", &binary, false))
            .unwrap();

        let mut supervisor = Supervisor::new(
            registry.clone(),
            home.path().to_path_buf(),
            timings(),
            Box::new(FakeProbe::default()),
        );
        supervisor.reload();

        let Response::Units { notes, .. } = supervisor.apply_restart(&Target::all()) else {
            panic!("expected a unit listing");
        };
        assert!(
            notes.iter().any(|note| note.contains("skipped `off`")),
            "{notes:?}"
        );
        // desired state は書き換わらない。
        assert!(!registry.get("off").unwrap().enabled);
    }

    /// `restart <name>` は名前を明示した時だけ desired state を立て直す (= 決定 5)。
    #[test]
    fn restart_by_name_is_the_same_as_start() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path().join("units"));
        let binary = home.path().join("hyoui");
        registry
            .add("off", &unit("127.0.0.1:43690", &binary, false))
            .unwrap();

        let mut supervisor = Supervisor::new(
            registry.clone(),
            home.path().to_path_buf(),
            timings(),
            Box::new(FakeProbe::default()),
        );
        supervisor.reload();
        supervisor.apply_restart(&Target::named("off"));

        assert!(registry.get("off").unwrap().enabled);
    }

    #[test]
    fn exit_summaries_name_the_code_or_signal() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            describe_exit(&std::process::ExitStatus::from_raw(0)),
            "exited with status 0"
        );
        let killed = std::process::ExitStatus::from_raw(9);
        assert_eq!(describe_exit(&killed), "killed by signal 9");
    }

    #[test]
    fn log_tail_returns_the_last_lines_only() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("unit.log");
        for index in 0..(LOG_TAIL_LINES + 50) {
            append_log_line(&path, &format!("line {index}"));
        }
        let tail = tail_lines(&path, LOG_TAIL_LINES);
        assert_eq!(tail.len(), LOG_TAIL_LINES);
        assert_eq!(tail[0], "line 50");
        assert_eq!(
            tail[LOG_TAIL_LINES - 1],
            format!("line {}", LOG_TAIL_LINES + 49)
        );
        // 無いファイルは空 (= まだ一度も走っていない unit)。
        assert!(tail_lines(&home.path().join("absent.log"), 10).is_empty());
    }

    /// 生きている監督者の socket は奪わない。残骸なら外して開き直す。
    #[test]
    fn a_live_socket_is_not_taken_over_but_a_stale_one_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("supervisor.sock");

        let listener = bind_control_socket(&socket).expect("first bind");
        let error = bind_control_socket(&socket).expect_err("second bind must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);

        drop(listener);
        // 残骸 (= 誰も listen していない socket ファイル) は外して開ける。
        assert!(socket.exists());
        assert!(bind_control_socket(&socket).is_ok());
    }
}
