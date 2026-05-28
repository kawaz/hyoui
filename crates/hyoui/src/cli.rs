//! Command-line argument parser for the `hyoui` binary.
//!
//! This module is a pure function over `&[String]` (argv excluding argv[0]).
//! It performs no I/O and spawns no processes, so it can be exhaustively
//! covered by unit tests.
//!
//! `input` subcommand の spec parser (= DR-0006 §8) は本ファイル末尾の
//! "Input spec" section にまとめてある (= [`InputSpec`] / [`parse_input_spec`]
//! / [`InputCommand`])。本タスクでは parser + dispatcher 骨格のみで、
//! 各 spec prefix の handler は別 task (#16/#17) で実装する。
//!
//! # Subcommand layout
//!
//! ```text
//! hyoui <subcommand> [options]
//! ```
//!
//! Initially supported subcommands:
//!
//! * `run` — execute a child command inside a PTY as a transparent proxy.
//!   Mirrors the original (single-command) bootstrap CLI; the child argv goes
//!   after a `--` separator: `hyoui run [opts] -- cmd [args...]`.
//! * `completion <shell>` — print a shell completion script.
//!
//! Reserved (not yet implemented): `send`, `attach`, `status` for socket-based
//! remote control.
//!
//! When no subcommand is given, or an unknown subcommand is supplied, or the
//! user passes `--help` / `-h`, the parser returns `Command::Help`. There is
//! intentionally **no** shortcut that treats `hyoui -- cmd` as `hyoui run --
//! cmd`; the subcommand must be explicit.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Operating mode for the `run` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// Pass the parent terminal through (default).
    Interactive,
    /// Drive the child with a virtual PTY of fixed size; no terminal needed.
    Headless,
}

/// Behavior when the child process is suspended (STOPPED).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnChildSuspend {
    /// Follow the child: the parent also stops (SIGSTOP raised on self).
    Follow,
    /// Resume the child immediately by sending SIGCONT.
    AutoResume,
}

// DR-0015 §2.3: `OnParentSuspend` enum / `--on-parent-suspend` flag 廃止。
// 新構成では attach client が外部 SIGTSTP を受けても daemon は無関係 (= 旧
// `decouple` 相当の動作のみ、policy 選択肢自体が不要)。

/// Shell whose completion script is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Shell {
    /// Bourne-Again SHell.
    Bash,
    /// Z Shell.
    Zsh,
    /// Friendly Interactive SHell.
    Fish,
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        })
    }
}

/// Topic for which to render help text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HelpTopic {
    /// Help for the top-level invocation (subcommand list + global options).
    Top,
    /// Help for the `run` subcommand.
    Run,
    /// Help for the `attach` subcommand (detach key bindings, modes 等)。
    Attach,
    /// Help for the `list` subcommand.
    List,
    /// Help for the `kill` subcommand.
    Kill,
    /// Help for the `status` subcommand.
    Status,
    /// Help for the `tail` subcommand.
    Tail,
    /// Help for the `wait` subcommand (predicate / timeout / exit code 一覧)。
    Wait,
    /// Help for the `input` subcommand (= DR-0006 §8、spec prefix カタログ等)。
    Input,
    /// Help for the `screen` parent subcommand (= 子一覧 / 共通オプション)。
    Screen,
    /// Help for the `screen dump` subcommand (= DR-0006 §10.2)。
    ScreenDump,
    /// Help for the `screen snapshot` subcommand (= DR-0006 §10.3 / DR-0013 §9)。
    ScreenSnapshot,
    /// Help for the `completion` subcommand.
    Completion,
    /// Help for the `lock` parent subcommand (= 子: `acquire` / `release`)。
    Lock,
    /// Help for the `lock acquire` subcommand (= DR-0006 §7)。
    LockAcquire,
    /// Help for the `lock release` subcommand (= DR-0006 §7)。
    LockRelease,
    /// Help for the `unlock` subcommand (= release の alias、DR-0006 §7)。
    Unlock,
    /// User invoked an unknown subcommand; render top-level help with note.
    UnknownSubcommand(String),
}

/// Fully parsed `run` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// Operating mode.
    pub mode: Mode,
    /// Virtual screen columns (used in headless mode; default 80).
    pub cols: i32,
    /// Virtual screen rows (used in headless mode; default 24).
    pub rows: i32,
    /// Overall timeout in milliseconds, or `None` if unset.
    /// 負値は意味を持たないので `u64` (WaitConfig.timeout_ms と整合)。
    pub timeout_ms: Option<u64>,
    /// Output idle timeout in milliseconds, or `None` if unset.
    pub idle_timeout_ms: Option<u64>,
    /// Substring pattern that, when seen in PTY output, terminates the child.
    pub until: Option<String>,
    /// Explicit socket path, or `None` to auto-generate.
    pub socket: Option<String>,
    /// `--detached`: daemon を別 process で起動して親はすぐ exit。socket path を
    /// stdout に 1 行 print してから親が終わる。attach は別 process から行う。
    pub detached: bool,
    /// `--session`: 自動採番 (`run-<pid>-<rand4hex>`) ではなく明示 session id を使う。
    /// socket path 自動解決にもこの値が入る。
    pub session: Option<String>,
    /// Action when the child is suspended (preset by mode unless overridden).
    /// DR-0015 §2.2: attach client が SessionChildStoppedNotify 受信時に発動する
    /// policy。daemon には伝わらない (= client local)。
    pub on_child_suspend: OnChildSuspend,
    /// vt100 内蔵 scrollback ring の **行数上限** (= DR-0013 §8 + §8 Update)。
    ///
    /// `screen dump --layer={scrollback,both}` / `screen snapshot` で過去 row を
    /// 取り出す際の最大行数。`None` (= 未指定) なら DaemonConfig の既定値 1000 行が
    /// 使われる。`--scrollback-rows=<N>` flag or `HYOUI_SCROLLBACK_ROWS=<N>` env で
    /// override 可能。`0` を渡すと scrollback を完全無効化する (= 過去 row は保存
    /// されない、低メモリ運用)。
    pub scrollback_rows: Option<usize>,
    /// `--debug-dump-server=<path>`: **server 側** (= 子 PTY → daemon) の経路を
    /// 通った raw bytes を file に append。`daemon` が観測した最初の形 (= state
    /// 正本化前) を残す。
    pub debug_dump_server: Option<String>,
    /// `--debug-dump-client=<path>`: **client 側** (= daemon → attach client) の
    /// 経路で client process が受信した bytes を file に append。attach 復元
    /// redraw / DR-0013 state-based 翻訳の結果を含む = 「ユーザの terminal が
    /// 見ている形」が残る。
    pub debug_dump_client: Option<String>,
    /// argv of the child command.
    pub command: Vec<String>,
}

/// `attach` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachConfig {
    /// Target socket path. `Some(p)` で explicit、`None` なら session_id から resolve。
    pub socket: Option<String>,
    /// Target session id (= socket path resolver の入力)。`socket` 指定時は無視。
    pub session_id: Option<String>,
    /// 動作 mode (rw / ro)。MVP は文字列のみ受理。
    pub mode_str: Option<String>,
    /// `--exclusive` (= 起動時占有要求)。
    pub exclusive: bool,
    /// `--detach-others` (= attach 時に他 client を奪取)。
    pub detach_others: bool,
    /// `--debug-dump-client=<path>`: client 側受信 bytes を file に append (debug)。
    /// `hyoui run` の同名 flag と同じ意味 (= attach 単体で使うときの名前統一)。
    pub debug_dump_client: Option<String>,
}

/// `list` subcommand configuration (R5-H3)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListConfig {
    /// `--prune-stale` (= 接続不能 socket を unlink して掃除)。
    ///
    /// daemon が panic / SIGKILL で異常終了すると `UnixSock::drop` が走らず
    /// socket file が残留し、`hyoui list` で live と区別できなくなる (R5-H3)。
    /// `--prune-stale` は connect 試行で死活確認し、`ECONNREFUSED` 等で
    /// 失敗した socket を unlink で除去する。
    pub prune_stale: bool,
}

/// `kill` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// 子 PTY に送る signal 名 (= default SIGTERM、DR-0012)。
    ///
    /// 正規表記は SIG-prefix 大文字 ("SIGTERM" / "SIGKILL" 等)。受信した name は
    /// daemon 側で OS native value に解決される。略名 ("TERM") / 小文字
    /// ("sigterm") / 数値 ("15") は CLI 段で reject される。
    pub signal: Option<String>,
}

/// `status` subcommand の出力形式 (= `--format=plain|json`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum StatusFormat {
    /// Plain text (= human readable、default)。`key: value` 1 行ごと
    #[default]
    Plain,
    /// JSON (= scripting / jq 用、1 行 JSON object) — H5
    Json,
}

/// `status` subcommand configuration (Phase 11)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--format=plain|json` (= default `Plain`、H5: scripting で grep/cut の罠回避)。
    pub format: StatusFormat,
}

/// `tail` subcommand configuration (DR-0006 §11)。
///
/// DR-0013 §8 Update (2026-05-27) の責務分離方針で、tail は **byte-base scrollback
/// layer (= `scrollback.rs`)** に対する raw bytes stream client として位置づけられた
/// (= state-based の `wait` / `screen dump` / `screen snapshot` とは別 layer)。
/// timestamp filter (= `--since` / `--since-strict`) も byte-base scrollback 上で動作する。
///
/// 用途は **log / script monitor**、**asciinema record の前段**、**daemon に届く生 bytes
/// の debug 確認** など。画面 mirror 用途は `hyoui attach --read-only` を使う (= DR §11.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--follow` で daemon が live stream を継続送信。
    pub follow: bool,
    /// `--strip-ansi` (alias: `--strip`) で daemon 側で escape を strip 済の TailData を流す。
    pub strip_ansi: bool,
    /// `--since=<DUR>` (= 過去 DUR 以内の chunk を bundle)、`None` なら全体。
    pub since_ms: Option<u64>,
    /// `--since-strict` (= since 範囲が scrollback ring buffer から evict 済の場合に
    /// `TailEnd(BufferTruncated)` で exit 非 0)、`since_ms` と組み合わせて使う。
    pub since_strict: bool,
    /// `--last-bytes=<n>` (alias: `--last`、= 末尾 n bytes に絞る)、`None` なら制限なし。
    pub last_bytes: Option<u64>,
}

/// `wait` subcommand configuration (DR-0006 §9 state-based)。
///
/// DR-0006 §9 改訂後 ([[DR-0013]] §9 連動) で wait は **visible state regex match**
/// に再定義された。旧 `text:` / `pattern:` / `wait-idle:` prefix と
/// `--strip-escapes` / `--newline-convert-lf` / `--raw` は **廃止**。subcommand
/// は **regex pattern を 1 つ** だけ受け取り、`hyoui wait <session> <pattern>` の
/// 形で起動する (= input family の `wait:<pattern>` と同じ意味)。
///
/// `wait-idle:<duration>` は input family 経由 (= `hyoui input <session>
/// wait-idle:500ms ...`) でのみ利用可能 (DR-0006 §9.2 表)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// regex pattern (= visible state に対する正規表現)。空文字列は parser 段で
    /// reject。multiline mode は実行側 (= `wait_core::wait_for_pattern`) で default
    /// ON にする。
    pub pattern: String,
    /// `--timeout=<dur>` (絶対 timeout)、`None` なら無限 wait。
    pub timeout_ms: Option<u64>,
    /// `--poll-interval=<dur>` (= snapshot polling 周期)、`None` なら default
    /// (100ms)。`HYOUI_WAIT_POLL_MS` 環境変数の override は CLI 引数指定が
    /// 無いときのみ効く (= 引数優先)。
    pub poll_interval_ms: Option<u64>,
}

/// `screen dump` subcommand の format 選択肢 (= DR-0006 §10.2)。
///
/// protocol 層の `ScreenDumpFormat` と 1:1 対応。CLI 段で `--format=ansi` /
/// `--format=binary` / `--format=cbor` / `--format=text/plain` を受理する。
/// `--format=json` は protocol 上は予約 variant だが daemon が
/// `format-not-implemented` を返す MVP scope 外なので CLI 段でも reject する
/// (= 早期 fail で誤入力を見つける)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ScreenDumpCliFormat {
    /// `state_formatted()` の raw ANSI sequence (= terminal で cat 再生可能)。
    #[default]
    Ansi,
    /// 空白除去 + 改行 plaintext (= grep / log 用途)。
    Binary,
    /// CBOR encode された structured `ScreenSnapshot` (= 機械処理 / debug)。
    Cbor,
    /// 装飾なし + cell 空白 / 行構造保持の plaintext (= TUI 自動処理用、
    /// claude TUI PoC feedback)。`Binary` と違い行末空白を trim せず、
    /// viewport の盤面状態を装飾だけ抜いた形で出力する。
    /// CLI 受理 alias: `text` / `text/plain` / `plain`。
    TextPlain,
}

/// `screen dump` subcommand の layer 選択肢 (= DR-0006 §10.2)。
///
/// MVP では `--layer=visible` のみが daemon で実装済。`scrollback` / `both` は
/// forward-compat な CLI 側 enum として用意するが、現状 daemon が `layer-not-implemented`
/// を返す。本タスクでは visible のみ送信する CLI とし、`scrollback` / `both` は
/// 別 task で配線する (cli-design-preferences の `--enable/--disable` パターンではなく、
/// 値選択型なので forward-compat variant として CLI 段で reject)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ScreenDumpCliLayer {
    /// 現在 visible な viewport のみ (= MVP 唯一の実装済)。
    #[default]
    Visible,
    /// scrollback (= 過去) のみ。Phase B/C で daemon 配線後に CLI 側も解放予定。
    Scrollback,
    /// scrollback + visible 連結。Phase B/C 同様。
    Both,
}

/// `screen dump` subcommand の rect 指定 (= DR-0006 §10.2)。
///
/// `--rect=x,y,w,h` (= u16 4 つを comma 区切り) を表す。Phase B では daemon が
/// rect を受信しても無視する仕様 (= 全画面のみ対応) だが、CLI 段で構文 validate
/// しておくことで forward-compat 配線時に CLI の改修が不要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenDumpCliRect {
    /// 矩形左上 col (= 0-origin)。
    pub x: u16,
    /// 矩形左上 row (= 0-origin)。
    pub y: u16,
    /// 矩形 width (cols 単位)。
    pub w: u16,
    /// 矩形 height (rows 単位)。
    pub h: u16,
}

/// `screen dump <session>` subcommand configuration (= DR-0013 §9 + DR-0006 §10.2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDumpConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--format=ansi|binary|cbor` (= default ansi)。
    pub format: ScreenDumpCliFormat,
    /// `--layer=visible|scrollback|both` (= default visible、MVP は visible のみ送信)。
    pub layer: ScreenDumpCliLayer,
    /// `--rect=x,y,w,h` (= 未指定なら full viewport、Phase B 段では daemon が
    /// 受信しても無視)。
    pub rect: Option<ScreenDumpCliRect>,
    /// `--output=<path>` (= 未指定なら stdout に書き出し)。
    pub output: Option<String>,
    /// `--timeout=<ms>` (= response 受信 timeout、default 5000ms)。
    pub timeout_ms: u64,
}

/// `screen snapshot` subcommand の include 選択肢 (= DR-0006 §10.3 / DR-0013 §9)。
///
/// protocol 層の [`SnapshotComponent`] と 1:1 対応。CLI 段では `--include` の
/// comma-separated 値として受理する (= `Cells,Cursor,Mode,...`)。case-insensitive。
///
/// [`SnapshotComponent`]: crate::protocol::messages::SnapshotComponent
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SnapshotCliComponent {
    /// cell grid (= sparse CBOR encoded `ScreenSnapshot.cells`)。
    Cells,
    /// cursor 位置 + visibility。
    Cursor,
    /// mode flag (= alt / app_keypad / cursor / bracketed_paste / hide_cursor)。
    Mode,
    /// style (= MVP scope 外、forward-compat slot)。protocol 層に variant が無い
    /// ため `--include=style` は CLI 段では受理するが、wire 送信時に他 component
    /// と並んで送ることはない (= 単独指定は警告対象、現状は noop 扱い)。
    Style,
    /// scrollback rows (= 未実装、daemon は `protocol-malformed` を返す)。
    Scrollback,
    /// viewport size (rows × cols)。
    WindowSize,
    /// 現在 buffer kind (primary or alternate)。
    Buffer,
    /// SequenceNo (= current_seqno、incremental sync 連携用)。
    SequenceNo,
}

/// `screen snapshot <session>` subcommand configuration (= DR-0013 §9 + DR-0006 §10.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshotConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--include=Cells,Cursor,...` (= comma-separated)、default は全 component。
    /// Vec はそのまま wire の `include: Vec<SnapshotComponent>` に流す。
    pub include: Vec<SnapshotCliComponent>,
    /// `--format=cbor|json` (= default cbor)。`json` は MVP scope 外で daemon は
    /// 無視するが、CLI は wire に格別の追加情報を送らずそのまま cbor を返す。
    pub format: ScreenSnapshotCliFormat,
    /// `--output=<path>` (= 未指定なら stdout)。
    pub output: Option<String>,
    /// `--timeout=<ms>` (= response 受信 timeout、default 5000ms)。
    pub timeout_ms: u64,
}

/// `screen snapshot` の format 選択肢 (= DR-0006 §10.3)。
///
/// 現状 MVP では `cbor` のみ実装。`json` は forward-compat 用 (= daemon 側
/// 未実装、CLI 段では受理するが wire 上は cbor として送る)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ScreenSnapshotCliFormat {
    /// CBOR encoded `StateSnapshotResponse` (= 機械処理、default)。
    #[default]
    Cbor,
    /// JSON encoded (= forward-compat、現状 daemon 未実装で wire には cbor を送る)。
    Json,
}

/// `screen` 親 subcommand の子 dispatch (= DR-0006 §10.1)。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScreenCommand {
    /// `screen dump <session>` (= visible bytes dump)。
    Dump(ScreenDumpConfig),
    /// `screen snapshot <session>` (= structured state query)。
    Snapshot(ScreenSnapshotConfig),
}

/// `lock acquire` subcommand の `--mode` 選択肢 (= DR-0006 §7)。
///
/// 取得時に他者保持中だった場合の挙動を切替える。default は `Wait` (= 取得できるまで
/// block)。DR-0006 §7 では default を明示していないが、wrapper として「待つ」が
/// 直感的かつ「失敗で即 exit」より誤動作が少ない (= fail mode は明示 opt-in)。
///
/// **MVP 注意**: daemon 側 wait queue は未実装 (`LockAcquire { wait: true }` でも
/// `Denied` が返る)。CLI 側で `Denied` を受けたら短時間 sleep + retry することで
/// "wait" semantics を擬似的に実現する (= polling 戦略、`--timeout` 内に成功するまで
/// retry を続ける)。`fail` mode は 1 回送って終わり。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum LockMode {
    /// 他者保持中なら polling で待ち続ける (default、`--timeout` を超えたら timeout 扱い)。
    #[default]
    Wait,
    /// 他者保持中なら即 fail で exit 1。
    Fail,
}

/// `lock acquire <session>` subcommand configuration (= DR-0006 §7)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockAcquireConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--mode=wait|fail` (= default Wait)。fail = 即時 fail、wait = polling retry。
    pub mode: LockMode,
    /// `--timeout=<dur>` (= acquire 全体 timeout、`None` なら無限 wait)。
    /// wait mode 時のみ意味あり (fail mode は単発 send なので即決)。
    pub timeout_ms: Option<u64>,
}

/// `lock release <session> --token=<T>` / `unlock <session> --token=<T>` の共通 configuration
/// (= DR-0006 §7)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockReleaseConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--token=<T>` の値 (`HYOUI_LOCK_TOKEN` env fallback あり、parser 段では None 可、
    /// CLI dispatcher 側で env を読む)。CLI flag で空文字列は parser 段で reject。
    pub token: Option<String>,
}

/// `lock` 親 subcommand の子 dispatch (= DR-0006 §7)。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LockCommand {
    /// `lock acquire <session>` — token を取得、stdout に出力 + connection を保持して
    /// SIGINT/SIGTERM/stdin EOF まで block。
    Acquire(LockAcquireConfig),
    /// `lock release <session> --token=<T>` — 既存 lock を release。
    Release(LockReleaseConfig),
}

/// Result of parsing argv (excluding argv[0]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// Display usage text and exit 0.
    Help {
        /// What help to show.
        topic: HelpTopic,
    },
    /// Display the library version and exit 0.
    Version,
    /// Execute `run` with the given configuration.
    Run(RunConfig),
    /// Attach to an existing daemon session.
    Attach(AttachConfig),
    /// List existing daemon sessions (= socket dir scan)。
    List(ListConfig),
    /// Kill (= SIGTERM 等を子に送る) a daemon session by session id / socket。
    Kill(KillConfig),
    /// Print session status (clients/leader/lock/scrollback) and exit。
    Status(StatusConfig),
    /// Tail scrollback (optional --follow for live stream)。
    Tail(TailConfig),
    /// Wait until predicate (text/pattern/idle) matches, then exit。
    Wait(WaitConfig),
    /// `screen` 親 subcommand (= 子: `dump` / 将来 `snapshot`)。
    Screen(ScreenCommand),
    /// `input` subcommand (= DR-0006 §8、spec sequence の順序保証送信)。
    ///
    /// 本タスク (= #15) で parser + dispatcher 骨格のみ追加。各 spec prefix の
    /// handler は task #16 (text/hex/file/paste/key) / #17 (wait/wait-idle) で
    /// 実装するため、CLI parse は成功するが [`InputSpec`] dispatcher は
    /// `bail!("... not yet implemented")` を返す。
    Input(InputCommand),
    /// `lock` 親 subcommand (= DR-0006 §7、自動操作排他の低レベル primitive)。
    ///
    /// 子: `acquire` / `release`。`tx` (= 子 process 起動と lock を組み合わせる
    /// wrapper) は別 task。
    Lock(LockCommand),
    /// `unlock <session> --token=<T>` — `lock release` の alias (= DR-0006 §7)。
    ///
    /// 完全に同じ意味の subcommand を別名でも露出する: 「取得は `lock acquire`、
    /// 解放は `unlock`」という命名が直感的なため。
    Unlock(LockReleaseConfig),
    /// Print a completion script for the given shell.
    Completion {
        /// Target shell.
        shell: Shell,
    },
    /// Parsing failed. Caller should print the message + top-level usage and
    /// exit with a non-zero status.
    Error(String),
}

// =============================================================================
// Public entry points
// =============================================================================

/// Parse the entire command line (argv excluding argv[0]).
pub fn parse_args(args: &[String]) -> Command {
    // Top-level: no args -> top help.
    if args.is_empty() {
        return Command::Help {
            topic: HelpTopic::Top,
        };
    }

    // Top-level flags allowed before any subcommand.
    let head = args[0].as_str();
    match head {
        "--help" | "-h" => {
            return Command::Help {
                topic: HelpTopic::Top,
            };
        }
        "--version" | "-V" => return Command::Version,
        _ => {}
    }

    let rest = &args[1..];
    match head {
        "run" => parse_run(rest),
        "attach" => parse_attach(rest),
        "list" => parse_list(rest),
        "kill" => parse_kill(rest),
        "status" => parse_status(rest),
        "tail" => parse_tail(rest),
        "wait" => parse_wait(rest),
        "screen" => parse_screen(rest),
        "input" => parse_input(rest),
        "lock" => parse_lock(rest),
        "unlock" => parse_unlock(rest),
        "completion" => parse_completion(rest),
        // Reserved for future stages.
        //
        // `send` / `detach` は旧 leaf 設計の名残として予約。
        //
        // `tx` は DR-0006 §7 の自動操作排他 wrapper (= 子 process 起動 + env 注入 +
        // 子 exit で自動 unlock)。`lock` / `unlock` は実装済 (= task #20)、tx 本体は
        // `--process-bound` 等の daemon-side 機能が要るので別 task に切り出し中。
        // 詳細は `docs/issue/2026-05-27-tx-lock-unlock-cli-subcommands.md` 参照。
        "send" | "detach" | "tx" => Command::Error(format!(
            "subcommand `{head}` is reserved but not yet implemented"
        )),
        other => Command::Help {
            topic: HelpTopic::UnknownSubcommand(other.to_string()),
        },
    }
}

fn parse_list(args: &[String]) -> Command {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Command::Help {
            topic: HelpTopic::List,
        };
    }
    let mut cfg = ListConfig::default();
    for a in args {
        match a.as_str() {
            "--prune-stale" => cfg.prune_stale = true,
            other => return Command::Error(format!("list: unexpected argument: {other}")),
        }
    }
    Command::List(cfg)
}

fn parse_kill(args: &[String]) -> Command {
    let mut cfg = KillConfig {
        socket: None,
        session_id: None,
        signal: None,
    };
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Kill,
                };
            }
            "--socket" => match value {
                Some(v) => cfg.socket = Some(v),
                None => return Command::Error("--socket requires a value".into()),
            },
            // DR-0012: 旧 `--signum N` は完全廃止。v0.2.0 breaking。
            "--signal" => match value.as_deref() {
                Some(v) => {
                    // CLI 段で正規表記 (SIG-prefix 大文字) を強制する。
                    // - 略名 ("TERM") は SIG-prefix 不在で reject
                    // - 小文字 ("sigterm") は ASCII 大文字以外を含むので reject
                    // - 数値 ("15") は SIG prefix 不在で reject
                    // - 完全未知 ("SIGBOGUS") は CLI を通過し、daemon 側で
                    //   signal.invalid として reject される
                    if !v.starts_with("SIG")
                        || v.len() <= 3
                        || !v
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                    {
                        return Command::Error(format!(
                            "invalid --signal value: {v} (expected SIG-prefix uppercase, e.g. SIGTERM)"
                        ));
                    }
                    cfg.signal = Some(v.to_string());
                }
                None => return Command::Error("--signal requires a value".into()),
            },
            "--signum" => {
                // 旧形式は v0.2.0 で廃止。明確な error メッセージで誘導する
                // (DR-0012, R5-C4)。
                return Command::Error(
                    "--signum is removed in v0.2.0 (DR-0012); use --signal NAME (e.g. --signal SIGTERM)".into(),
                );
            }
            other if other.starts_with('-') => {
                return Command::Error(format!("unknown kill option: {other}"));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }
    match positionals.len() {
        0 => {
            if cfg.socket.is_none() {
                return Command::Error(
                    "kill: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui kill <session-id>` / `hyoui list` で session 一覧を確認できます"
                        .into(),
                );
            }
        }
        1 => {
            let sid = positionals.into_iter().next().unwrap();
            // R5-AUD-C2: positional session_id を validate (= path traversal 早期 reject)
            if let Err(e) = validate_session_id(&sid) {
                return Command::Error(format!("kill: {e}"));
            }
            cfg.session_id = Some(sid);
        }
        _ => return Command::Error("kill: too many positional arguments".into()),
    }
    Command::Kill(cfg)
}

/// shared helper: --socket / --help / positional session_id を抜き出す。
/// 残ったオプションは caller がコールバックで処理する。
#[allow(clippy::result_large_err)] // Command 内 String/Vec の Err サイズは parse path のみで許容
fn parse_session_targeted<F>(
    name: &str,
    args: &[String],
    help_topic: HelpTopic,
    mut on_option: F,
) -> Result<(Option<String>, Option<String>), Command>
where
    F: FnMut(&str, Option<String>) -> Result<bool, Command>,
{
    let mut socket: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt_name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match opt_name.as_str() {
            "--help" | "-h" => {
                return Err(Command::Help { topic: help_topic });
            }
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Err(Command::Error(format!("{name}: --socket requires a value"))),
            },
            other if other.starts_with("--") => {
                let cb_consumed = on_option(other, value)?;
                if !cb_consumed {
                    consumed_extra = false;
                }
            }
            other if other.starts_with('-') => {
                return Err(Command::Error(format!("{name}: unknown option: {other}")));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }
    let session_id = match positionals.len() {
        0 => {
            if socket.is_none() {
                return Err(Command::Error(format!(
                    "{name}: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui {name} <session-id>` / `hyoui list` で session 一覧を確認できます"
                )));
            }
            None
        }
        1 => {
            let sid = positionals.into_iter().next().unwrap();
            // R5-AUD-C2: positional session_id を validate (= path traversal 早期 reject)
            if let Err(e) = validate_session_id(&sid) {
                return Err(Command::Error(format!("{name}: {e}")));
            }
            Some(sid)
        }
        _ => {
            return Err(Command::Error(format!(
                "{name}: too many positional arguments"
            )));
        }
    };
    Ok((socket, session_id))
}

#[allow(clippy::result_large_err)]
fn parse_status(args: &[String]) -> Command {
    let mut format = StatusFormat::Plain;
    let res = parse_session_targeted("status", args, HelpTopic::Status, |opt, value| match opt {
        "--format" => {
            let v =
                value.ok_or_else(|| Command::Error("status: --format requires a value".into()))?;
            match v.as_str() {
                "plain" => {
                    format = StatusFormat::Plain;
                    Ok(true)
                }
                "json" => {
                    format = StatusFormat::Json;
                    Ok(true)
                }
                other => Err(Command::Error(format!(
                    "status: --format must be `plain` or `json`, got {other:?}"
                ))),
            }
        }
        other => Err(Command::Error(format!("status: unknown option: {other}"))),
    });
    match res {
        Ok((socket, session_id)) => Command::Status(StatusConfig {
            socket,
            session_id,
            format,
        }),
        Err(c) => c,
    }
}

#[allow(clippy::result_large_err)]
fn parse_tail(args: &[String]) -> Command {
    let mut follow = false;
    let mut strip_ansi = false;
    let mut since_ms: Option<u64> = None;
    let mut since_strict = false;
    let mut last_bytes: Option<u64> = None;
    let res = parse_session_targeted("tail", args, HelpTopic::Tail, |opt, value| match opt {
        "--follow" => {
            follow = true;
            Ok(false)
        }
        // DR-0006 §11 では `--strip`、現状実装は `--strip-ansi`。両対応 (= 後方互換 + DR 整合)。
        "--strip" | "--strip-ansi" => {
            strip_ansi = true;
            Ok(false)
        }
        "--since" => {
            let v = value.ok_or_else(|| Command::Error("tail: --since requires a value".into()))?;
            let ms =
                parse_duration_ms(&v).map_err(|e| Command::Error(format!("tail: --since: {e}")))?;
            since_ms = Some(ms);
            Ok(true)
        }
        "--since-strict" => {
            since_strict = true;
            Ok(false)
        }
        // DR-0006 §11 では `--last`、現状実装は `--last-bytes`。両対応 (= 後方互換 + DR 整合)。
        "--last" | "--last-bytes" => {
            let v = value.ok_or_else(|| Command::Error(format!("tail: {opt} requires a value")))?;
            let n = v
                .parse::<u64>()
                .map_err(|_| Command::Error(format!("tail: {opt}: bad number: {v}")))?;
            last_bytes = Some(n);
            Ok(true)
        }
        other => Err(Command::Error(format!("tail: unknown option: {other}"))),
    });
    match res {
        Ok((socket, session_id)) => {
            if since_strict && since_ms.is_none() {
                return Command::Error("tail: --since-strict requires --since=<DUR>".into());
            }
            Command::Tail(TailConfig {
                socket,
                session_id,
                follow,
                strip_ansi,
                since_ms,
                since_strict,
                last_bytes,
            })
        }
        Err(c) => c,
    }
}

fn parse_wait(args: &[String]) -> Command {
    let mut timeout_ms: Option<u64> = None;
    let mut poll_interval_ms: Option<u64> = None;
    let mut socket: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt_name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                // 次 arg を value 候補にするのは `--key value` の形だけ。
                // regex pattern が `-` で始まることはほぼ無いが、念のため `--`
                // から始まらない次 arg だけ value 候補にする (= screen dump 等
                // 既存 pattern と整合)。
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match opt_name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Wait,
                };
            }
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Command::Error("wait: --socket requires a value".into()),
            },
            "--timeout" => match value {
                Some(v) => match parse_duration_ms(&v) {
                    Ok(ms) => timeout_ms = Some(ms),
                    Err(e) => return Command::Error(format!("wait: --timeout: {e}")),
                },
                None => return Command::Error("wait: --timeout requires a value".into()),
            },
            "--poll-interval" => match value {
                Some(v) => match parse_duration_ms(&v) {
                    Ok(ms) => poll_interval_ms = Some(ms),
                    Err(e) => return Command::Error(format!("wait: --poll-interval: {e}")),
                },
                None => return Command::Error("wait: --poll-interval requires a value".into()),
            },
            other if other.starts_with("--") => {
                return Command::Error(format!("wait: unknown option: {other}"));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }
    // positionals: --socket あり → 1 つだけ (= pattern)、--socket なし → 2 つ
    // (= session_id, pattern)。前者で 0 個は pattern 不在で error、後者で
    // 1 個だけ場合は session_id を validate して pattern 不在 error。
    let (session_id, pattern) = match (socket.is_some(), positionals.len()) {
        (true, 0) => {
            return Command::Error(
                "wait: pattern が必要です。例: `hyoui wait --socket=<path> 'Continue\\?'`".into(),
            );
        }
        (true, 1) => (None, positionals.pop().expect("non-empty")),
        (true, _) => {
            return Command::Error(format!(
                "wait: 余分な positional 引数: {:?}",
                &positionals[1..]
            ));
        }
        (false, 0) => {
            return Command::Error(
                "wait: session id と pattern が必要です。\
                 例: `hyoui wait <session-id> 'Continue\\?' --timeout=5s` / \
                 `hyoui list` で session 一覧を確認できます"
                    .into(),
            );
        }
        (false, 1) => {
            // session_id だけある状態 → pattern が無い
            return Command::Error(
                "wait: pattern が必要です。例: `hyoui wait <session-id> 'Continue\\?'`".into(),
            );
        }
        (false, 2) => {
            let pattern = positionals.pop().expect("non-empty");
            let sid = positionals.pop().expect("non-empty");
            // R5-AUD-C2: positional session_id を validate (= path traversal 早期 reject)
            if let Err(e) = validate_session_id(&sid) {
                return Command::Error(format!("wait: {e}"));
            }
            (Some(sid), pattern)
        }
        (false, _) => {
            return Command::Error(format!(
                "wait: 余分な positional 引数: {:?}",
                &positionals[2..]
            ));
        }
    };
    if pattern.is_empty() {
        return Command::Error("wait: pattern が空文字列です (= 正規表現が必要)".into());
    }
    Command::Wait(WaitConfig {
        socket,
        session_id,
        pattern,
        timeout_ms,
        poll_interval_ms,
    })
}

/// 期間文字列を ms に変換する (kawaz/timespec.mbt の duration parser を参考)。
///
/// ## 文法
///
/// ```text
/// duration := SP? component (SP? sign SP? component)* SP?
/// component := digits ('.' digits)? SP? unit
/// digits := DIGIT (DIGIT | '_')*
/// sign := '+' | '-'
/// unit := short_unit | long_unit
/// short_unit := "ns" | "us" | "μs" | "ms" | "s" | "m" | "h" | "d" | "w"
/// long_unit := "millisecond"|"milliseconds" | "second"|"seconds"|"sec"
///            | "minute"|"minutes"|"min" | "hour"|"hours"
///            | "day"|"days" | "week"|"weeks"
/// ```
///
/// ## 仕様
///
/// - **単位必須**: bare 数字 / 空文字列は error
/// - **decimal 対応**: `1.5h` = 90 分
/// - **underscore separator**: `1_000.5s` = 1000.5 秒 (= 1_000_500 ms)
/// - **連結加算**: `1h30m` = 1 時間 + 30 分。同 group 内 segment は加算
/// - **符号付き group**: `1d-4h` = 1 日 group - 4 時間 group = 20 時間
/// - **whitespace tolerant**: `1 h 2 m` / `1h 2m` も accept
/// - **sub-ms 精度 (ns / us / μs) も accept、集積後に ms へ floor**:
///   `1500us` = 1 ms (= 1500000ns / 1000000 を floor)、
///   `999us` = 0 ms (= 999000ns / 1000000 = 0)、
///   `500us 600us` = 1 ms (= 集積値 1100000ns で 1ms を超えた分は取り入れ)。
///   timespec.mbt は YAGNI で reject していたが、本実装は集積 floor 方針
/// - **`y` / `M` (年 / 月) は reject** (= 単位固定でないため)
/// - **最終 total が負なら error** (= hyoui の duration は正値前提)
fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let total_ns = parse_duration_ns_signed(s)?;
    if total_ns < 0 {
        return Err(format!(
            "duration resolved to negative value ({total_ns}ns) in {s:?}"
        ));
    }
    // ns → ms に floor (= 1ms 未満は切り捨て、集積値が 1ms を超えた分のみ取り入れ)
    let total_ms = total_ns / 1_000_000;
    u64::try_from(total_ms).map_err(|_| format!("duration overflows u64 ms: {s:?}"))
}

/// 符号付き ns で返す internal helper (= negative 許容、集積精度 ns)。
fn parse_duration_ns_signed(s: &str) -> Result<i128, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    let chars: Vec<char> = s.chars().collect();
    let mut pos = 0usize;

    let mut total_ns: i128 = 0;
    let mut group_ns: i128 = 0;
    let mut group_sign: i128 = 1;
    let mut parsed_any = false;

    while pos < chars.len() {
        pos = skip_spaces(&chars, pos);
        if pos >= chars.len() {
            break;
        }
        // group 区切り符号 (= D5: 先頭 group の前に sign は不可 = grammar 違反)
        let mut new_group = false;
        match chars[pos] {
            '+' | '-' => {
                if !parsed_any {
                    return Err(format!(
                        "leading sign before any component at position {pos} in {s:?}"
                    ));
                }
                total_ns = total_ns
                    .checked_add(group_sign.checked_mul(group_ns).ok_or("overflow")?)
                    .ok_or("overflow")?;
                group_ns = 0;
                group_sign = if chars[pos] == '-' { -1 } else { 1 };
                pos += 1;
                new_group = true;
            }
            _ => {}
        }
        pos = skip_spaces(&chars, pos);
        // 符号の後ろに digit が無いと invalid (例: `1m-` / `1m+`)
        if pos >= chars.len() {
            if new_group {
                return Err(format!("trailing sign without component in {s:?}"));
            }
            break;
        }
        if !chars[pos].is_ascii_digit() {
            if new_group {
                return Err(format!(
                    "expected digit after sign at position {pos} in {s:?}"
                ));
            }
            // grammar 上、component の前は `+` / `-` か whitespace か EOF のみ。
            // それ以外の文字 (= `_` / `.` / alpha など) が残っているのは trailing
            // junk または unit 直後の不正な char → 明示 error にする (D2/D3)。
            return Err(format!(
                "unexpected character {:?} at position {pos} in {s:?} \
                 (component separator must be '+' / '-' / whitespace)",
                chars[pos]
            ));
        }

        let (int_part, frac_billion, new_pos) = parse_number(&chars, pos)?;
        pos = skip_spaces(&chars, new_pos);
        let (ns_mul, unit_end) = parse_unit(&chars, pos)?;
        if unit_end == pos {
            return Err(format!("missing unit after number in {s:?}"));
        }
        pos = unit_end;

        let mut seg_ns: i128 = (int_part as i128)
            .checked_mul(ns_mul)
            .ok_or("duration component overflow")?;
        if frac_billion > 0 {
            // frac_billion は (frac × 1e9) の整数表現。ns_mul を掛けて 1e9 で割れば
            // 「frac × unit_ns」を整数精度で得られる (= D4: 旧 f64 経由を排除)。
            let frac_ns: i128 = (frac_billion as i128)
                .checked_mul(ns_mul)
                .ok_or("frac component overflow")?
                / 1_000_000_000;
            seg_ns = seg_ns.checked_add(frac_ns).ok_or("frac add overflow")?;
        }
        group_ns = group_ns.checked_add(seg_ns).ok_or("group accum overflow")?;
        parsed_any = true;
    }
    if !parsed_any {
        return Err(format!("no duration segments parsed from {s:?}"));
    }
    total_ns = total_ns
        .checked_add(group_sign.checked_mul(group_ns).ok_or("overflow")?)
        .ok_or("overflow")?;
    Ok(total_ns)
}

fn skip_spaces(chars: &[char], start: usize) -> usize {
    let mut pos = start;
    while pos < chars.len() && (chars[pos] == ' ' || chars[pos] == '\t') {
        pos += 1;
    }
    pos
}

/// `123.456` / `1_000_000.5_0` 形式の数値を読む。
///
/// 文法 (= timespec.mbt 相当):
/// - `digits := DIGIT (DIGIT | '_')*` (= 必ず DIGIT で始まり、以降 `_` を separator として許容)
/// - `'_' を先頭` / `'_' 連続` / `数字なしの '_' のみ` は **error**
/// - `('.' digits)?` (= 小数点を入れたら必ず 1 桁以上の digits が続く)
/// - 旧実装 (Round3 まで) は `_5s` / `1.s` / `1h_2m` を silently 通していた。grammar
///   通りに厳格化 (= レビュー指摘 D2/D3)
///
/// 戻り値: `(int_part_i64, frac_part_in_per_billion_i64, new_pos)`。
/// frac は分母 1_000_000_000 (= 9 桁) で整数化することで f64 経由の overflow を回避
/// (= レビュー指摘 D4)。それ以上の精度は floor で切り捨て。
fn parse_number(chars: &[char], start: usize) -> Result<(i64, i64, usize), String> {
    let mut pos = start;
    // 1. 最初の 1 文字は必ず digit (= leading `_` 禁止)
    let first = chars
        .get(pos)
        .copied()
        .ok_or_else(|| "expected digit".to_string())?;
    let first_d = first
        .to_digit(10)
        .ok_or_else(|| format!("expected digit at position {pos}"))?;
    let mut int_part: i64 = first_d as i64;
    pos += 1;
    // 2. 以降は DIGIT または `_`、ただし `_` 連続不可 + 末尾 `_` 不可
    let mut last_was_underscore = false;
    while pos < chars.len() {
        let c = chars[pos];
        if c == '_' {
            if last_was_underscore {
                return Err(format!("consecutive '_' at position {pos}"));
            }
            last_was_underscore = true;
            pos += 1;
            continue;
        }
        if let Some(d) = c.to_digit(10) {
            int_part = int_part
                .checked_mul(10)
                .and_then(|v| v.checked_add(d as i64))
                .ok_or("integer part overflow")?;
            last_was_underscore = false;
            pos += 1;
        } else {
            break;
        }
    }
    if last_was_underscore {
        return Err(format!("trailing '_' in number at position {pos}"));
    }

    // 3. 小数部 (= `.` の後ろに必ず 1 桁以上の digit が必要)
    let mut frac_billion: i64 = 0; // frac × 1_000_000_000 を整数で蓄える
    if pos < chars.len() && chars[pos] == '.' {
        pos += 1;
        // 小数点直後の 1 文字目も必ず digit
        let first = chars
            .get(pos)
            .copied()
            .ok_or_else(|| format!("expected fractional digit after '.' at position {pos}"))?;
        let first_d = first
            .to_digit(10)
            .ok_or_else(|| format!("expected fractional digit after '.' at position {pos}"))?;
        let mut frac_digits: u32 = 1;
        frac_billion = frac_billion
            .checked_add((first_d as i64) * 10i64.pow(9 - frac_digits))
            .ok_or("frac overflow")?;
        pos += 1;
        let mut last_was_underscore = false;
        while pos < chars.len() {
            let c = chars[pos];
            if c == '_' {
                if last_was_underscore {
                    return Err(format!("consecutive '_' in fractional at position {pos}"));
                }
                last_was_underscore = true;
                pos += 1;
                continue;
            }
            if let Some(d) = c.to_digit(10) {
                if frac_digits < 9 {
                    frac_digits += 1;
                    frac_billion = frac_billion
                        .checked_add((d as i64) * 10i64.pow(9 - frac_digits))
                        .ok_or("frac overflow")?;
                }
                // 9 桁を超えた小数は精度を捨てる (= ns 単位 timer なので 9 桁で十分)
                last_was_underscore = false;
                pos += 1;
            } else {
                break;
            }
        }
        if last_was_underscore {
            return Err(format!("trailing '_' in fractional at position {pos}"));
        }
    }
    Ok((int_part, frac_billion, pos))
}

/// `parse_unit` は (ns_multiplier, end_pos) を返す。未知単位 / 拒否単位は Err。
///
/// **case-insensitive** (= レビュー指摘 H2): `1H` / `1Min` / `1MS` 等は lowercase
/// 化してから match する。`μ` (Greek mu) は ASCII 範囲外なのでそのまま保持。
/// 例外: 月の慣習表記 `M` (= Java) は単独単位として **error 候補** に乗せたいが、
/// case-insensitive 化すると `m`/`M` を区別できなくなる。そこで `m`/`M` は同等
/// に minute 扱いとし、月は長形 `month` / `months` のみで明示 reject する
/// (= 「単位は文脈で明確」優先、minute の頻度 >> month の頻度なので m を取る)。
fn parse_unit(chars: &[char], start: usize) -> Result<(i128, usize), String> {
    if start >= chars.len() {
        return Err("missing unit".into());
    }
    let mut end = start;
    while end < chars.len() && (chars[end].is_ascii_alphabetic() || chars[end] == 'μ') {
        end += 1;
    }
    if end == start {
        return Ok((0, start)); // no unit chars
    }
    // word 全体を lowercase 化、ただし `μ` (= U+03BC) は ASCII 外なので保存
    let word: String = chars[start..end]
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    const NS: i128 = 1;
    const US: i128 = 1_000;
    const MS: i128 = 1_000_000;
    const SEC: i128 = 1_000_000_000;
    let ns = match word.as_str() {
        "ns" => NS,
        "us" | "μs" => US,
        "ms" | "millisecond" | "milliseconds" => MS,
        "s" | "sec" | "second" | "seconds" => SEC,
        "m" | "min" | "minute" | "minutes" => 60 * SEC,
        "h" | "hour" | "hours" => 3600 * SEC,
        "d" | "day" | "days" => 86_400 * SEC,
        "w" | "week" | "weeks" => 604_800 * SEC,
        // explicit rejects: 年/月 (= 単位固定でない)
        // 注: 短形 `M` は minute と被るため month 用 reject に含めない。
        // ユーザが `1M` と書くと 1 分扱い (慣習衝突を minute 優先で解消)。
        "y" | "year" | "years" | "month" | "months" => {
            return Err(format!(
                "calendar unit {word:?} not supported (lengths vary; \
                 use d/days for fixed-length day counts)"
            ));
        }
        _ => return Err(format!("unknown unit {word:?}")),
    };
    Ok((ns, end))
}

fn parse_attach(args: &[String]) -> Command {
    let mut cfg = AttachConfig {
        socket: None,
        session_id: None,
        mode_str: None,
        exclusive: false,
        detach_others: false,
        debug_dump_client: None,
    };

    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Attach,
                };
            }
            "--socket" => match value {
                Some(v) => cfg.socket = Some(v),
                None => return Command::Error("--socket requires a value".into()),
            },
            "--mode" => match value {
                Some(v) => cfg.mode_str = Some(v),
                None => return Command::Error("--mode requires a value".into()),
            },
            "--exclusive" => {
                cfg.exclusive = true;
                consumed_extra = false; // bool flag は次 arg 食わない
            }
            "--detach-others" => {
                cfg.detach_others = true;
                consumed_extra = false;
            }
            "--debug-dump-client" => match value {
                Some(v) if !v.is_empty() => cfg.debug_dump_client = Some(v),
                Some(_) => return Command::Error("--debug-dump-client: path が空です".into()),
                None => {
                    return Command::Error("--debug-dump-client requires a value".into());
                }
            },
            other if other.starts_with('-') => {
                return Command::Error(format!("unknown attach option: {other}"));
            }
            _ => {
                // positional (= session id)
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }

    match positionals.len() {
        0 => {
            if cfg.socket.is_none() {
                return Command::Error(
                    "attach: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui attach <session-id>` / `hyoui list` で session 一覧を確認できます"
                        .into(),
                );
            }
        }
        1 => cfg.session_id = Some(positionals.into_iter().next().unwrap()),
        _ => return Command::Error("attach: too many positional arguments".into()),
    }

    Command::Attach(cfg)
}

/// Render the usage text for the given help topic.
pub fn usage(topic: &HelpTopic) -> String {
    match topic {
        HelpTopic::Top => usage_top(None),
        HelpTopic::UnknownSubcommand(name) => usage_top(Some(name.as_str())),
        HelpTopic::Run => usage_run(),
        HelpTopic::Attach => usage_attach(),
        HelpTopic::List => usage_list(),
        HelpTopic::Kill => usage_kill(),
        HelpTopic::Status => usage_status(),
        HelpTopic::Tail => usage_tail(),
        HelpTopic::Wait => usage_wait(),
        HelpTopic::Screen => usage_screen(),
        HelpTopic::ScreenDump => usage_screen_dump(),
        HelpTopic::ScreenSnapshot => usage_screen_snapshot(),
        HelpTopic::Input => usage_input(),
        HelpTopic::Lock => usage_lock(),
        HelpTopic::LockAcquire => usage_lock_acquire(),
        HelpTopic::LockRelease => usage_lock_release(),
        HelpTopic::Unlock => usage_unlock(),
        HelpTopic::Completion => usage_completion(),
    }
}

// =============================================================================
// Subcommand parsers
// =============================================================================

fn parse_run(args: &[String]) -> Command {
    // `run` accepts options, then optional `--`, then the child argv.
    if args.is_empty() {
        return Command::Error("no command given (use `hyoui run [opts] -- cmd [args...]`)".into());
    }

    // Recognise `run --help`.
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help" || a == "-h")
    {
        return Command::Help {
            topic: HelpTopic::Run,
        };
    }

    let mut mode = Mode::Interactive;
    let mut explicit_cols: Option<i32> = None;
    let mut explicit_rows: Option<i32> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut idle_timeout_ms: Option<u64> = None;
    let mut until: Option<String> = None;
    let mut socket: Option<String> = None;
    let mut on_child_suspend: Option<OnChildSuspend> = None;
    let mut command: Vec<String> = Vec::new();
    let mut detached = false;
    let mut session: Option<String> = None;
    let mut scrollback_rows: Option<usize> = None;
    let mut debug_dump_server: Option<String> = None;
    let mut debug_dump_client: Option<String> = None;

    let mut i = 0usize;
    let mut in_command = false;

    while i < args.len() {
        let arg = args[i].as_str();
        if in_command {
            command.push(args[i].clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            in_command = true;
            i += 1;
            continue;
        }

        let (name, inline_value) = split_eq(arg);

        // Take the value for an option: inline (`--x=v`) or following arg.
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                if i + 1 < args.len() {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };

        // Process the option. On success, advance past the value too.
        match name.as_str() {
            "--mode" => match value.as_deref() {
                Some("interactive") => mode = Mode::Interactive,
                Some("headless") => mode = Mode::Headless,
                Some(other) => return Command::Error(format!("invalid --mode value: {other}")),
                None => return Command::Error("--mode requires a value".into()),
            },
            "--size" => match value.as_deref() {
                Some(v) => match parse_size(v) {
                    Some((c, r)) => {
                        explicit_cols = Some(c);
                        explicit_rows = Some(r);
                    }
                    None => {
                        return Command::Error(format!(
                            "invalid --size value (expected COLSxROWS): {v}"
                        ));
                    }
                },
                None => return Command::Error("--size requires a value".into()),
            },
            "--cols" => match value.as_deref() {
                Some(v) => match parse_int(v) {
                    Some(c) => explicit_cols = Some(c),
                    None => return Command::Error(format!("invalid --cols value: {v}")),
                },
                None => return Command::Error("--cols requires a value".into()),
            },
            "--rows" => match value.as_deref() {
                Some(v) => match parse_int(v) {
                    Some(r) => explicit_rows = Some(r),
                    None => return Command::Error(format!("invalid --rows value: {v}")),
                },
                None => return Command::Error("--rows requires a value".into()),
            },
            "--timeout" => match value.as_deref() {
                Some(v) => match parse_duration_ms(v) {
                    Ok(ms) => timeout_ms = Some(ms),
                    Err(e) => return Command::Error(format!("--timeout: {e}")),
                },
                None => return Command::Error("--timeout requires a value".into()),
            },
            "--idle-timeout" => match value.as_deref() {
                Some(v) => match parse_duration_ms(v) {
                    Ok(ms) => idle_timeout_ms = Some(ms),
                    Err(e) => return Command::Error(format!("--idle-timeout: {e}")),
                },
                None => return Command::Error("--idle-timeout requires a value".into()),
            },
            "--until" => match value {
                Some(v) => until = Some(v),
                None => return Command::Error("--until requires a value".into()),
            },
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Command::Error("--socket requires a value".into()),
            },
            "--on-child-suspend" => match value.as_deref() {
                Some("follow") => on_child_suspend = Some(OnChildSuspend::Follow),
                Some("auto-resume") => on_child_suspend = Some(OnChildSuspend::AutoResume),
                Some(other) => {
                    return Command::Error(format!("invalid --on-child-suspend value: {other}"));
                }
                None => return Command::Error("--on-child-suspend requires a value".into()),
            },
            // DR-0015 §2.3: `--on-parent-suspend` 廃止 (= 軸 2 廃止)。
            "--detached" => {
                detached = true;
                consumed_extra = false; // bool flag は次 arg を食わない
            }
            "--session" => match value {
                Some(v) => {
                    if let Err(e) = validate_session_id(&v) {
                        return Command::Error(format!("--session: {e}"));
                    }
                    session = Some(v);
                }
                None => return Command::Error("--session requires a value".into()),
            },
            "--scrollback-rows" => match value.as_deref() {
                Some(v) => match v.parse::<usize>() {
                    Ok(n) => scrollback_rows = Some(n),
                    Err(e) => {
                        return Command::Error(format!(
                            "--scrollback-rows: 非負整数を指定してください: {e} (got {v:?})"
                        ));
                    }
                },
                None => return Command::Error("--scrollback-rows requires a value".into()),
            },
            "--debug-dump-server" => match value {
                Some(v) if !v.is_empty() => debug_dump_server = Some(v),
                Some(_) => return Command::Error("--debug-dump-server: path が空です".into()),
                None => return Command::Error("--debug-dump-server requires a value".into()),
            },
            "--debug-dump-client" => match value {
                Some(v) if !v.is_empty() => debug_dump_client = Some(v),
                Some(_) => return Command::Error("--debug-dump-client: path が空です".into()),
                None => return Command::Error("--debug-dump-client requires a value".into()),
            },
            other => return Command::Error(format!("unknown option: {other}")),
        }

        // Advance past option name (and value if it was a separate arg).
        if consumed_extra {
            i += 1;
        }
        i += 1;
    }

    if command.is_empty() {
        return Command::Error("no command given (use `-- cmd [args...]`)".into());
    }

    // Mode-driven preset defaults for suspend behavior, unless overridden.
    let final_child_suspend = on_child_suspend.unwrap_or(match mode {
        Mode::Headless => OnChildSuspend::AutoResume,
        Mode::Interactive => OnChildSuspend::Follow,
    });

    // Virtual size: default to 80x24 when unspecified.
    let cols = explicit_cols.unwrap_or(80);
    let rows = explicit_rows.unwrap_or(24);

    Command::Run(RunConfig {
        mode,
        cols,
        rows,
        timeout_ms,
        idle_timeout_ms,
        until,
        socket,
        detached,
        session,
        on_child_suspend: final_child_suspend,
        scrollback_rows,
        debug_dump_server,
        debug_dump_client,
        command,
    })
}

/// `screen` 親 subcommand の dispatcher (= DR-0006 §10.1)。
///
/// 引数なし / `--help` / `-h` は親 help を出す (= cli-design-preferences の
/// 「引数なし実行時は --help を表示」「子・孫ネスト可」)。最初の positional を
/// 子 subcommand 名として扱い、`dump` 以外は UnknownSubcommand 扱い。
fn parse_screen(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Help {
            topic: HelpTopic::Screen,
        };
    }
    let head = args[0].as_str();
    match head {
        "--help" | "-h" => Command::Help {
            topic: HelpTopic::Screen,
        },
        "dump" => parse_screen_dump(&args[1..]),
        "snapshot" => parse_screen_snapshot(&args[1..]),
        other if other.starts_with('-') => {
            Command::Error(format!("screen: unknown option: {other}"))
        }
        other => {
            // task #22: dump/snapshot に対する edit distance 1 以下の suggest。
            let base = format!("screen: unknown subcommand `{other}` (supported: dump, snapshot)");
            match suggest_closest(other, ["dump", "snapshot"]) {
                Some(s) => Command::Error(format!("{base} (did you mean `screen {s}`?)")),
                None => Command::Error(base),
            }
        }
    }
}

/// `screen dump <session>` parser (= DR-0013 §9 + DR-0006 §10.2)。
///
/// 受理する options:
/// - `--socket=<path>` — session_id の代替 (= shared helper)
/// - `--format=ansi|binary|cbor` (= default ansi)。`json` は MVP scope 外
/// - `--layer=visible|scrollback|both` (= default visible、MVP は visible のみ)
/// - `--rect=x,y,w,h` (= u16 4 つ、forward-compat: daemon は現状無視)
/// - `--output=<path>` — stdout の代わりに file へ書き出し
/// - `--timeout=<ms>` — response 受信 timeout (= default 5000ms)
#[allow(clippy::result_large_err)]
fn parse_screen_dump(args: &[String]) -> Command {
    let mut format = ScreenDumpCliFormat::default();
    let mut layer = ScreenDumpCliLayer::default();
    let mut rect: Option<ScreenDumpCliRect> = None;
    let mut output: Option<String> = None;
    let mut timeout_ms: u64 = 5_000;
    let res = parse_session_targeted("screen dump", args, HelpTopic::ScreenDump, |opt, value| {
        match opt {
            "--format" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen dump: --format requires a value".into())
                })?;
                match v.as_str() {
                    "ansi" => {
                        format = ScreenDumpCliFormat::Ansi;
                        Ok(true)
                    }
                    "binary" => {
                        format = ScreenDumpCliFormat::Binary;
                        Ok(true)
                    }
                    "cbor" => {
                        format = ScreenDumpCliFormat::Cbor;
                        Ok(true)
                    }
                    // TextPlain は 3 alias を受理 (= primary name は MIME 風の
                    // "text/plain"、短縮形 "text" / "plain" も同義)。
                    "text" | "text/plain" | "plain" => {
                        format = ScreenDumpCliFormat::TextPlain;
                        Ok(true)
                    }
                    "json" => Err(Command::Error(
                        "screen dump: --format=json は MVP scope 外 (= 別 task)。\
                         ansi / binary / cbor / text/plain を使ってください"
                            .into(),
                    )),
                    other => Err(Command::Error(format!(
                        "screen dump: --format must be `ansi`|`binary`|`cbor`|`text/plain`, got {other:?}"
                    ))),
                }
            }
            "--layer" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen dump: --layer requires a value".into())
                })?;
                match v.as_str() {
                    "visible" => {
                        layer = ScreenDumpCliLayer::Visible;
                        Ok(true)
                    }
                    "scrollback" => {
                        layer = ScreenDumpCliLayer::Scrollback;
                        Ok(true)
                    }
                    "both" => {
                        layer = ScreenDumpCliLayer::Both;
                        Ok(true)
                    }
                    other => Err(Command::Error(format!(
                        "screen dump: --layer must be `visible`|`scrollback`|`both`, got {other:?}"
                    ))),
                }
            }
            "--rect" => {
                let v = value
                    .ok_or_else(|| Command::Error("screen dump: --rect requires a value".into()))?;
                let parsed = parse_screen_dump_rect(&v).map_err(|e| {
                    Command::Error(format!("screen dump: --rect: {e} (expected x,y,w,h)"))
                })?;
                rect = Some(parsed);
                Ok(true)
            }
            "--output" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen dump: --output requires a value".into())
                })?;
                output = Some(v);
                Ok(true)
            }
            "--timeout" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen dump: --timeout requires a value".into())
                })?;
                let ms = parse_duration_ms(&v)
                    .map_err(|e| Command::Error(format!("screen dump: --timeout: {e}")))?;
                timeout_ms = ms;
                Ok(true)
            }
            other => Err(Command::Error(format!(
                "screen dump: unknown option: {other}"
            ))),
        }
    });
    match res {
        Ok((socket, session_id)) => Command::Screen(ScreenCommand::Dump(ScreenDumpConfig {
            socket,
            session_id,
            format,
            layer,
            rect,
            output,
            timeout_ms,
        })),
        Err(c) => c,
    }
}

/// `--rect=x,y,w,h` を `ScreenDumpCliRect` に変換する。各 component は u16 範囲。
fn parse_screen_dump_rect(s: &str) -> Result<ScreenDumpCliRect, String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        return Err(format!(
            "expected 4 comma-separated values (x,y,w,h), got {} part(s) in {s:?}",
            parts.len()
        ));
    }
    let mut vals = [0u16; 4];
    for (i, p) in parts.iter().enumerate() {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            return Err(format!("empty component at index {i} in {s:?}"));
        }
        vals[i] = trimmed
            .parse::<u16>()
            .map_err(|e| format!("invalid u16 at index {i} ({trimmed:?}): {e}"))?;
    }
    Ok(ScreenDumpCliRect {
        x: vals[0],
        y: vals[1],
        w: vals[2],
        h: vals[3],
    })
}

/// `screen snapshot <session>` parser (= DR-0013 §9 + DR-0006 §10.3)。
///
/// 受理する options:
/// - `--socket=<path>` — session_id の代替 (= shared helper)
/// - `--include=<set>` — comma-separated component 集合 (= default 全部)
/// - `--format=cbor|json` (= default cbor、json は MVP scope 外で wire 上は cbor)
/// - `--output=<path>` — stdout の代わりに file へ書き出し
/// - `--timeout=<ms>` — response 受信 timeout (= default 5000ms)
#[allow(clippy::result_large_err)]
fn parse_screen_snapshot(args: &[String]) -> Command {
    let mut include: Option<Vec<SnapshotCliComponent>> = None;
    let mut format = ScreenSnapshotCliFormat::default();
    let mut output: Option<String> = None;
    let mut timeout_ms: u64 = 5_000;
    let res = parse_session_targeted(
        "screen snapshot",
        args,
        HelpTopic::ScreenSnapshot,
        |opt, value| match opt {
            "--include" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen snapshot: --include requires a value".into())
                })?;
                let parsed = parse_snapshot_include(&v)
                    .map_err(|e| Command::Error(format!("screen snapshot: --include: {e}")))?;
                include = Some(parsed);
                Ok(true)
            }
            "--format" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen snapshot: --format requires a value".into())
                })?;
                match v.as_str() {
                    "cbor" => {
                        format = ScreenSnapshotCliFormat::Cbor;
                        Ok(true)
                    }
                    "json" => {
                        // MVP scope 外。CLI 段では受理するが daemon は cbor で返すため
                        // 実質 cbor と同じ wire 動作。後段 task で json encoder を入れる。
                        format = ScreenSnapshotCliFormat::Json;
                        Ok(true)
                    }
                    other => Err(Command::Error(format!(
                        "screen snapshot: --format must be `cbor`|`json`, got {other:?}"
                    ))),
                }
            }
            "--output" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen snapshot: --output requires a value".into())
                })?;
                output = Some(v);
                Ok(true)
            }
            "--timeout" => {
                let v = value.ok_or_else(|| {
                    Command::Error("screen snapshot: --timeout requires a value".into())
                })?;
                let ms = parse_duration_ms(&v)
                    .map_err(|e| Command::Error(format!("screen snapshot: --timeout: {e}")))?;
                timeout_ms = ms;
                Ok(true)
            }
            other => Err(Command::Error(format!(
                "screen snapshot: unknown option: {other}"
            ))),
        },
    );
    match res {
        Ok((socket, session_id)) => {
            let include = include.unwrap_or_else(default_snapshot_include);
            Command::Screen(ScreenCommand::Snapshot(ScreenSnapshotConfig {
                socket,
                session_id,
                include,
                format,
                output,
                timeout_ms,
            }))
        }
        Err(c) => c,
    }
}

/// `--include=<set>` の `<set>` を `Vec<SnapshotCliComponent>` に変換する。
///
/// 受理する形式: comma-separated (= `Cells,Cursor,Mode`)。前後 whitespace は trim、
/// 大文字小文字は case-insensitive (= `cells` / `CELLS` / `Cells` 全部 OK)。
/// 重複指定は dedupe する (= 最終 Vec で順序は維持しつつ unique 化)。
/// 不明な component 名は error。
///
/// 受理する名前 (= `SnapshotComponent` と 1:1):
/// `cells` / `cursor` / `mode` / `style` / `scrollback` / `window-size` / `windowsize`
/// / `buffer` / `sequence-no` / `sequenceno`。`-` は省略可 (= ハイフン無しの形も accept、
/// CBOR の kebab-case ↔ camelCase / lower 連結を吸収)。
fn parse_snapshot_include(s: &str) -> Result<Vec<SnapshotCliComponent>, String> {
    if s.trim().is_empty() {
        return Err("empty include set (= 最低 1 つ component を指定してください)".into());
    }
    let mut out: Vec<SnapshotCliComponent> = Vec::new();
    for raw in s.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(format!("empty component in {s:?}"));
        }
        let lower = trimmed.to_ascii_lowercase();
        let normalized: String = lower.chars().filter(|c| *c != '-' && *c != '_').collect();
        let comp = match normalized.as_str() {
            "cells" => SnapshotCliComponent::Cells,
            "cursor" => SnapshotCliComponent::Cursor,
            "mode" => SnapshotCliComponent::Mode,
            "style" => SnapshotCliComponent::Style,
            "scrollback" => SnapshotCliComponent::Scrollback,
            "windowsize" => SnapshotCliComponent::WindowSize,
            "buffer" => SnapshotCliComponent::Buffer,
            "sequenceno" => SnapshotCliComponent::SequenceNo,
            other => {
                return Err(format!(
                    "unknown component {other:?} (valid: Cells / Cursor / Mode / Style / Scrollback / WindowSize / Buffer / SequenceNo)"
                ));
            }
        };
        if !out.contains(&comp) {
            out.push(comp);
        }
    }
    Ok(out)
}

/// `--include` 未指定時の default (= `Scrollback` 以外を全て選択)。
///
/// `Scrollback` を default に含めない理由: daemon が `protocol-malformed` を返す
/// (= Phase B 時点で未実装、`handle_state_snapshot_request` 参照)。default で
/// 巻き込んで失敗させると UX が崩れるため、明示的に `--include=Scrollback` で
/// 指定したときだけ送る形にする (= 早期 fail で daemon の実装状況を露呈)。
fn default_snapshot_include() -> Vec<SnapshotCliComponent> {
    vec![
        SnapshotCliComponent::Cells,
        SnapshotCliComponent::Cursor,
        SnapshotCliComponent::Mode,
        SnapshotCliComponent::WindowSize,
        SnapshotCliComponent::Buffer,
        SnapshotCliComponent::SequenceNo,
    ]
}

/// `lock` 親 subcommand の dispatcher (= DR-0006 §7)。
///
/// 引数なし / `--help` / `-h` は親 help を出す (= cli-design-preferences の
/// 「引数なし実行時は --help を表示」「子・孫ネスト可」)。最初の positional を
/// 子 subcommand 名として扱い、`acquire` / `release` 以外は UnknownSubcommand 扱い。
fn parse_lock(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Help {
            topic: HelpTopic::Lock,
        };
    }
    let head = args[0].as_str();
    match head {
        "--help" | "-h" => Command::Help {
            topic: HelpTopic::Lock,
        },
        "acquire" => parse_lock_acquire(&args[1..]),
        "release" => parse_lock_release(&args[1..]),
        other if other.starts_with('-') => Command::Error(format!("lock: unknown option: {other}")),
        other => {
            // edit distance 1 以下で acquire / release を suggest する。
            let base = format!("lock: unknown subcommand `{other}` (supported: acquire, release)");
            match suggest_closest(other, ["acquire", "release"]) {
                Some(s) => Command::Error(format!("{base} (did you mean `lock {s}`?)")),
                None => Command::Error(base),
            }
        }
    }
}

/// `lock acquire <session> [--mode=wait|fail] [--timeout=<dur>]` parser。
///
/// 受理する options:
/// - `--socket=<path>` — session_id の代替 (= shared helper)
/// - `--mode=wait|fail` (= default wait)
/// - `--timeout=<dur>` — acquire 全体 timeout (= 未指定なら無限 wait、wait mode のみ意味)
#[allow(clippy::result_large_err)]
fn parse_lock_acquire(args: &[String]) -> Command {
    let mut mode = LockMode::default();
    let mut timeout_ms: Option<u64> = None;
    let res = parse_session_targeted(
        "lock acquire",
        args,
        HelpTopic::LockAcquire,
        |opt, value| match opt {
            "--mode" => {
                let v = value.ok_or_else(|| {
                    Command::Error("lock acquire: --mode requires a value".into())
                })?;
                match v.as_str() {
                    "wait" => {
                        mode = LockMode::Wait;
                        Ok(true)
                    }
                    "fail" => {
                        mode = LockMode::Fail;
                        Ok(true)
                    }
                    other => Err(Command::Error(format!(
                        "lock acquire: --mode must be `wait` or `fail`, got {other:?}"
                    ))),
                }
            }
            "--timeout" => {
                let v = value.ok_or_else(|| {
                    Command::Error("lock acquire: --timeout requires a value".into())
                })?;
                let ms = parse_duration_ms(&v)
                    .map_err(|e| Command::Error(format!("lock acquire: --timeout: {e}")))?;
                timeout_ms = Some(ms);
                Ok(true)
            }
            other => Err(Command::Error(format!(
                "lock acquire: unknown option: {other}"
            ))),
        },
    );
    match res {
        Ok((socket, session_id)) => Command::Lock(LockCommand::Acquire(LockAcquireConfig {
            socket,
            session_id,
            mode,
            timeout_ms,
        })),
        Err(c) => c,
    }
}

/// `lock release <session> --token=<T>` parser。
///
/// 受理する options:
/// - `--socket=<path>` — session_id の代替 (= shared helper)
/// - `--token=<T>` — release 対象の token (`HYOUI_LOCK_TOKEN` env fallback あり、CLI 段では
///   None 可 / dispatcher が env を読む)。flag 指定値が空文字なら parser 段で reject。
#[allow(clippy::result_large_err)]
fn parse_lock_release(args: &[String]) -> Command {
    let mut token: Option<String> = None;
    let res = parse_session_targeted(
        "lock release",
        args,
        HelpTopic::LockRelease,
        |opt, value| match opt {
            "--token" => {
                let v = value.ok_or_else(|| {
                    Command::Error("lock release: --token requires a value".into())
                })?;
                if v.is_empty() {
                    return Err(Command::Error(
                        "lock release: --token requires a non-empty value".into(),
                    ));
                }
                token = Some(v);
                Ok(true)
            }
            other => Err(Command::Error(format!(
                "lock release: unknown option: {other}"
            ))),
        },
    );
    match res {
        Ok((socket, session_id)) => Command::Lock(LockCommand::Release(LockReleaseConfig {
            socket,
            session_id,
            token,
        })),
        Err(c) => c,
    }
}

/// `unlock <session> --token=<T>` parser (= `lock release` の alias、DR-0006 §7)。
#[allow(clippy::result_large_err)]
fn parse_unlock(args: &[String]) -> Command {
    let mut token: Option<String> = None;
    let res = parse_session_targeted("unlock", args, HelpTopic::Unlock, |opt, value| match opt {
        "--token" => {
            let v =
                value.ok_or_else(|| Command::Error("unlock: --token requires a value".into()))?;
            if v.is_empty() {
                return Err(Command::Error(
                    "unlock: --token requires a non-empty value".into(),
                ));
            }
            token = Some(v);
            Ok(true)
        }
        other => Err(Command::Error(format!("unlock: unknown option: {other}"))),
    });
    match res {
        Ok((socket, session_id)) => Command::Unlock(LockReleaseConfig {
            socket,
            session_id,
            token,
        }),
        Err(c) => c,
    }
}

fn parse_completion(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Error("completion requires a shell name (bash|zsh|fish)".into());
    }
    if args[0] == "--help" || args[0] == "-h" {
        return Command::Help {
            topic: HelpTopic::Completion,
        };
    }
    if args.len() > 1 {
        return Command::Error(format!(
            "completion accepts exactly one argument, got {}",
            args.len()
        ));
    }
    match args[0].as_str() {
        "bash" => Command::Completion { shell: Shell::Bash },
        "zsh" => Command::Completion { shell: Shell::Zsh },
        "fish" => Command::Completion { shell: Shell::Fish },
        other => Command::Error(format!(
            "unknown shell: {other} (supported: bash, zsh, fish)"
        )),
    }
}

// =============================================================================
// Usage texts
// =============================================================================

fn usage_top(unknown: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(name) = unknown {
        // task #22: edit distance 1 以下で類似 subcommand を suggest する。
        // 候補は既存 + reserved を含めた `TOP_LEVEL_SUBCOMMANDS` の全項目。
        match suggest_closest(name, TOP_LEVEL_SUBCOMMANDS.iter().copied()) {
            Some(s) => out.push_str(&format!(
                "error: unknown subcommand `{name}` (did you mean `{s}`?)\n\n"
            )),
            None => out.push_str(&format!("error: unknown subcommand `{name}`\n\n")),
        }
    }
    out.push_str(
        "hyoui — terminal-aware process proxy\n\
        \n\
        USAGE:\n    \
            hyoui <subcommand> [options]\n\
        \n\
        SUBCOMMANDS:\n    \
            run         Run a command inside a PTY as a transparent proxy\n    \
            attach      Attach to an existing daemon session\n    \
            list        List daemon sessions (= socket dir scan)\n    \
            kill        Send signal to a daemon session and terminate it\n    \
            status      Print session status (clients/leader/lock/scrollback)\n    \
            tail        Stream scrollback / live output (--follow で継続)\n    \
            wait        Wait until predicate (text/pattern/idle) matches\n    \
            screen      Dump / inspect virtual screen state (subcommands: dump)\n    \
            input       Send input via spec list (DR-0006 §8; text:/key:/wait: ...)\n    \
            lock        Acquire / release a session lock (DR-0006 §7)\n    \
            unlock      Release a session lock (= `lock release` alias)\n    \
            completion  Print a shell completion script (bash|zsh|fish)\n\
        \n\
        RESERVED (not yet implemented):\n    \
            send, detach, tx   将来 protocol 拡張用に予約\n\
        \n\
        GLOBAL OPTIONS:\n    \
            -h, --help     Show this help and exit\n    \
            -V, --version  Show version and exit\n\
        \n\
        Run `hyoui <subcommand> --help` for per-subcommand help.\n",
    );
    out
}

fn usage_run() -> String {
    String::from(
        "hyoui run — run a command inside a PTY as a transparent proxy\n\
        \n\
        USAGE:\n    \
            hyoui run [options] -- cmd [args...]\n\
        \n\
        OPTIONS:\n    \
            --mode=interactive|headless   Operating mode (default: interactive)\n    \
            --size COLSxROWS              Virtual screen size, e.g. 80x24 (headless)\n    \
            --cols N                      Virtual screen columns (headless)\n    \
            --rows M                      Virtual screen rows (headless)\n    \
            --timeout DUR                 Overall timeout (DUR フォーマットは下記参照)\n    \
            --idle-timeout DUR            Output idle timeout (= 子 PTY 出力が止まったら exit)\n    \
            --until PATTERN               Terminate when PATTERN appears in output\n    \
            --socket PATH                 Unix socket path for input injection\n    \
            --on-child-suspend=follow|auto-resume\n                                  \
                Action when the child is stopped\n                                  \
                (default: follow; headless: auto-resume)\n    \
            --on-parent-suspend=transparent|decouple\n                                  \
                Action when the parent is stopped\n                                  \
                (default: transparent; headless: decouple)\n    \
            --scrollback-rows N           vt100 内蔵 scrollback ring 行数上限\n                                  \
                (= screen dump --layer=scrollback / --layer=both で\n                                  \
                取り出せる過去 row の最大数、default 1000、0 で無効)\n    \
            --debug-dump-server PATH      子 PTY → daemon の raw bytes を file に append\n                                  \
                (state 翻訳前の bytes、ANSI escape 込み)\n    \
            --debug-dump-client PATH      daemon → client の raw bytes を file に append\n                                  \
                (state-based redraw / attach 復元込み = user の terminal 表示)\n    \
            -h, --help                    Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            SHELL                  Fallback command when none is given (legacy)\n    \
            XDG_RUNTIME_DIR        Base directory for the auto-generated socket path\n    \
            TMPDIR                 Socket path base when XDG_RUNTIME_DIR is unset\n    \
            HYOUI_SCROLLBACK_ROWS  --scrollback-rows と同じ値を env で渡す\n                                   \
                (--scrollback-rows 指定時は flag 優先)\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。sub-ms (ns/us/μs) は accept、内部 ns 集積 → ms に floor\n    \
            (例: 500us 600us = 1.1ms → 1ms)。bare 数字 / 年 (y) / 月 (M) は **error**。\n    \
            case-insensitive。\n",
    )
}

fn usage_attach() -> String {
    String::from(
        "hyoui attach — attach to an existing daemon session\n\
        \n\
        USAGE:\n    \
            hyoui attach <session-id> [options]\n    \
            hyoui attach --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH         Explicit socket path (alternative to session-id)\n    \
            --mode rw|ro|rw-no-leader\n                          \
                Operating mode (default: rw)\n    \
            --exclusive           Demand exclusive session ownership at start\n    \
            --detach-others       Drop other attached clients on connect\n    \
            -h, --help            Show this help and exit\n\
        \n\
        DETACH KEY (= session を生かしたまま client だけ抜ける):\n    \
            Ctrl-A d              detach (session 維持 + 自分だけ Detach 送って終了)\n    \
            Ctrl-A Ctrl-A         escape — literal Ctrl-A を子 PTY に送る\n    \
            Ctrl-A <other>        prefix と当該キー両方を捨てる (= screen 慣例)\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_DETACH_PREFIX   detach prefix byte をカスタマイズ。値の形式:\n                                  \
                * `Ctrl-B` / `^B` (= 0x02)\n                                  \
                * `0x02` (hex)\n                                  \
                * `2` (decimal 0..=255)\n                                  \
                * `none` / `off` / `disable` (= detach key 無効化)\n                                  \
                未設定なら Ctrl-A (0x01)\n    \
            HYOUI_LOCK_TOKEN      lock token を env で渡す (= handshake.token)\n\
        \n\
        EXAMPLES:\n    \
            hyoui attach demo                       # session_id=demo に attach\n    \
            hyoui attach --socket=/tmp/x.sock       # 直接 socket 指定\n    \
            hyoui attach demo --mode=ro             # 読み取り専用 attach\n    \
            hyoui attach demo --detach-others       # 他 client を蹴って奪う\n    \
            HYOUI_DETACH_PREFIX=Ctrl-B hyoui attach demo  # prefix を Ctrl-B に変更\n\
        \n\
        RELATED:\n    \
            hyoui run --detached    daemon を background 起動\n    \
            hyoui list              attach 可能な session 一覧\n    \
            hyoui status <id>       session 状態を 1 度取得\n    \
            hyoui tail <id>         scrollback / live stream を流す\n    \
            hyoui wait <id> ...     条件達成まで block する\n    \
            hyoui kill <id>         daemon に SIGTERM を送って終了\n",
    )
}

fn usage_status() -> String {
    String::from(
        "hyoui status — print session status and exit\n\
        \n\
        USAGE:\n    \
            hyoui status <session-id>\n    \
            hyoui status --socket=<path>\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        OUTPUT (plaintext key:value 1 行ごと):\n    \
            session-id: <name>\n    \
            child-pid: <pid>  または  child-pid: (exited)\n    \
            scrollback-bytes: <N>\n    \
            lock-holder: client <id>  または  lock-holder: (none)\n    \
            clients:\n              \
                - id=<n> mode=<Rw|Ro|RwNoLeader>[ leader]\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了\n    \
            1   connect / I/O 失敗\n    \
            2   引数不足 (session-id も --socket も無し)\n",
    )
}

fn usage_tail() -> String {
    String::from(
        "hyoui tail — stream raw bytes from daemon (DR-0006 §11)\n\
        \n\
        daemon の byte-base scrollback layer + 現在の子 PTY bytes stream を生のまま流す\n\
        (= DR-0013 §8 責務分離。state-based の wait/screen dump/screen snapshot とは別 layer)。\n\
        用途: log/script monitor、asciinema record の前段、debug。\n\
        画面 mirror 用途には使わない (= ANSI 再演で崩壊する。代わりに `hyoui attach --read-only`)。\n\
        \n\
        USAGE:\n    \
            hyoui tail <session-id> [options]\n    \
            hyoui tail --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH        Explicit socket path (alternative to session-id)\n    \
            --follow             子 PTY exit / TailEnd まで stream を継続する\n    \
            --strip              ANSI escape を strip 済の bytes を受け取る (= `--strip-ansi` alias)\n    \
            --since DUR          過去 DUR 以内の chunk のみ流す。単位必須 (例: 500ms / 2s / 1m)\n    \
            --since-strict       --since の範囲が scrollback から evict 済なら exit 非 0\n    \
            --last N             末尾 N bytes に絞る (= `--last-bytes` alias)\n    \
            -h, --help           Show this help and exit\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            単位: ns / us / μs / ms / s / m / h / d / w (短形)、または\n    \
            millisecond(s) / second(s) / sec / minute(s) / min / hour(s) /\n    \
            day(s) / week(s) (長形)。\n    \
            decimal: 1.5h, underscore: 1_000ms, 連結: 1h30m, 加減: 1d-4h。\n    \
            sub-ms (ns/us/μs) も accept、内部 ns 集積後に ms へ floor:\n              \
                500us 600us → 1.1ms → 1ms (= 集積で 1ms 超過分のみ取り入れ)\n              \
                999us → 0.999ms → 0ms\n    \
            bare 数字 (= 単位なし) は **error**。年 (y) / 月 (M) は単位固定でない\n    \
            ため対応せず。\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= TailEnd Eof / ChildExited 受信 or socket close)\n    \
            1   connect / I/O 失敗、または --since-strict で since 範囲が evict 済\n\
        \n\
        EXAMPLES:\n    \
            hyoui tail demo                       # 全 scrollback 1 度だけ流して exit\n    \
            hyoui tail demo --follow              # live stream を継続\n    \
            hyoui tail demo --since=10s           # 過去 10 秒分\n    \
            hyoui tail demo --since=10s --since-strict   # 10 秒が evict 済なら exit 非 0\n    \
            hyoui tail demo --last=8192           # 末尾 8 KiB\n\
        \n\
        RELATED:\n    \
            hyoui wait <id> ...       条件達成まで block (= state-based、画面 visible match)\n    \
            hyoui screen dump <id>    現在 visible を 1 度 dump (= state-based)\n    \
            hyoui status <id>         clients / lock 状態を 1 度取得\n",
    )
}

fn usage_wait() -> String {
    String::from(
        "hyoui wait — wait until visible screen state matches a regex (DR-0006 §9)\n\
        \n\
        USAGE:\n    \
            hyoui wait <session-id> <pattern> [options]\n    \
            hyoui wait --socket=<path> <pattern> [options]\n\
        \n\
        PATTERN:\n    \
            正規表現 (regex crate、unicode-perl features)。multiline mode は default\n    \
            ON (= `^`/`$` は行頭/行末で効く)。case-insensitive にするなら `(?i)...`。\n    \
            substring が欲しいときは `\\Q...\\E` で literal にする。\n\
        \n\
        MATCH SCOPE:\n    \
            daemon の **現在 visible cells** を行 join した text に対して match。\n    \
            scrollback / 過去 redraw / ANSI escape は対象外 (= cell 化済の text のみ)。\n    \
            `wait-idle:<duration>` は本 subcommand では受け付けず、`hyoui input <id>\n    \
            wait-idle:500ms ...` のように **input family 経由** で利用する (DR-0006\n    \
            §9.2)。\n\
        \n\
        OPTIONS:\n    \
            --socket PATH         Explicit socket path (alternative to session-id)\n    \
            --timeout DUR         絶対 timeout。**指定なしは無限 wait**\n    \
            --poll-interval DUR   snapshot polling 周期 (default 100ms)。\n                      \
                                  環境変数 `HYOUI_WAIT_POLL_MS` でも override 可。\n    \
            -h, --help            Show this help and exit\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。sub-ms (ns/us/μs) は accept、内部 ns 集積 → ms に floor\n    \
            (例: 500us 600us = 1.1ms → 1ms)。bare 数字 / 年 (y) / 月 (M) は **error**。\n\
        \n\
        EXIT CODE:\n    \
            0   Matched\n    \
            1   Timeout / I/O error\n    \
            2   Cancelled / invalid usage\n    \
            3   regex compile / daemon error\n\
        \n\
        EXAMPLES:\n    \
            hyoui wait demo 'READY' --timeout=5s\n    \
            hyoui wait demo 'ITEM-\\d+' --timeout=30s\n    \
            hyoui wait demo '(?m)^Continue\\?' --poll-interval=50ms\n",
    )
}

fn usage_list() -> String {
    String::from(
        "hyoui list — list daemon sessions (= socket dir scan + liveness probe)\n\
        \n\
        USAGE:\n    \
            hyoui list [--prune-stale]\n\
        \n\
        OPTIONS:\n    \
            --prune-stale   stale socket (= connect 不能) を unlink で削除\n    \
            -h, --help      Show this help and exit\n\
        \n\
        OUTPUT (TAB separated, 1 line per session):\n    \
            <session-id>\\t<live|stale>\\t<socket-path>\n\
        \n\
        LIVENESS PROBE (R5-H3):\n    \
            各 socket に対し best-effort connect 試行 (= 100ms timeout)。\n    \
            成功なら `live`、ECONNREFUSED / timeout なら `stale` 表示。\n    \
            stale は daemon の panic / SIGKILL で socket が unlink されずに\n    \
            残留した状態。`hyoui list --prune-stale` で掃除可能。\n\
        \n\
        SCAN ORDER (= socket_path::resolve と同順、最初に見つかった dir のみ):\n    \
            1. $XDG_RUNTIME_DIR/hyoui/\n    \
            2. $TMPDIR/hyoui-<uid>/  (TMPDIR 未設定なら /tmp/hyoui-<uid>/)\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= 0 件でも成功扱い、stderr に `no sessions found` を 1 行)\n\
        \n\
        EXAMPLES:\n    \
            hyoui list                              # 全 session を一覧 (live/stale 表示)\n    \
            hyoui list --prune-stale                # stale socket を削除して live のみ残す\n    \
            hyoui list | awk '$2 == \"live\" {print $1}'  # live session id を抽出\n\
        \n\
        RELATED:\n    \
            hyoui status <id>   session 1 件の詳細\n    \
            hyoui attach <id>   session に接続\n    \
            hyoui kill <id>     session を終了\n",
    )
}

fn usage_kill() -> String {
    String::from(
        "hyoui kill — send signal to a daemon session and terminate it\n\
        \n\
        USAGE:\n    \
            hyoui kill <session-id> [options]\n    \
            hyoui kill --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --signal NAME   Signal name to send to the child PTY (default: SIGTERM)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        SIGNAL NAME (DR-0012):\n    \
            正規表記は SIG-prefix 大文字 (e.g. SIGTERM / SIGKILL / SIGINT / SIGHUP /\n    \
            SIGQUIT / SIGUSR1 / SIGUSR2 / SIGCONT / SIGTSTP / SIGCHLD)\n    \
            略名 (TERM) / 小文字 (sigterm) / 数値 (15) は受理されない\n    \
            POSIX が signal 数値を規定していないため wire 上は name で送る\n\
        \n\
        EXIT CODE:\n    \
            0   送信完了 (= daemon が close するのを待ってから exit)\n    \
            1   connect / send 失敗\n    \
            2   引数不足 (session-id も --socket も無し)\n\
        \n\
        EXAMPLES:\n    \
            hyoui kill demo                          # session_id=demo に SIGTERM\n    \
            hyoui kill demo --signal=SIGKILL         # SIGKILL を送る\n    \
            hyoui kill --socket=/tmp/x.sock          # socket 直指定で kill\n\
        \n\
        RELATED:\n    \
            hyoui list          attach 可能な session 一覧 (= 対象選び)\n    \
            hyoui status <id>   session の現在状態を確認\n",
    )
}

fn usage_screen() -> String {
    String::from(
        "hyoui screen — inspect / dump virtual screen state (DR-0006 §10)\n\
        \n\
        USAGE:\n    \
            hyoui screen <subcommand> [options]\n\
        \n\
        SUBCOMMANDS:\n    \
            dump        Dump visible (or scrollback) bytes (= ANSI / binary / CBOR)\n    \
            snapshot    Structured state query (= DR-0006 §10.3 / DR-0013 §9)\n\
        \n\
        OPTIONS:\n    \
            -h, --help      Show this help and exit\n\
        \n\
        Run `hyoui screen <subcommand> --help` for per-subcommand help.\n",
    )
}

fn usage_screen_dump() -> String {
    String::from(
        "hyoui screen dump — dump virtual screen state (DR-0013 §9 + DR-0006 §10.2)\n\
        \n\
        USAGE:\n    \
            hyoui screen dump <session-id> [options]\n    \
            hyoui screen dump --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --format FMT        Output format (default: ansi)\n                        \
                ansi       — raw ANSI bytes (= terminal で cat 再生可)\n                        \
                binary     — 空白除去 + 改行 plaintext (= grep 用)\n                        \
                cbor       — CBOR encoded ScreenSnapshot (= 機械処理)\n                        \
                text/plain — 装飾なし + cell 空白 / 行構造保持 (= TUI 自動処理用)\n                        \
                             (alias: text, plain)\n                        \
                (json は MVP scope 外 / 別 task)\n    \
            --layer LAYER       Layer (default: visible)\n                        \
                visible    — 現在 viewport\n                        \
                scrollback — 過去 rows のみ (= 古い → 新しい順、\n                                     \
                             format=ansi/binary/text-plain/cbor 対応)\n                        \
                both       — scrollback + visible 連結 (= 同順、Cbor は連結後\n                                     \
                             0-origin 座標に振り直し)\n                        \
                (scrollback / both は daemon の --scrollback-rows 設定 (default 1000)\n                         \
                 に従って rows が cap される。0 設定なら空 payload)\n    \
            --rect X,Y,W,H      矩形指定 (forward-compat: daemon 現状無視)\n    \
            --output PATH       書き出し先 (= 未指定なら stdout)\n    \
            --timeout DUR       response 受信 timeout (= default 5s。DUR 形式: 5s/500ms/...)\n    \
            -h, --help          Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_LOCK_TOKEN    lock token を env で渡す (= handshake.token)\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= response 受信、payload を出力済)\n    \
            1   connect / I/O / daemon error (= daemon が unsupported-capability\n        \
                を返す場合も含む)\n    \
            2   引数不足 / 未知 option\n\
        \n\
        EXAMPLES:\n    \
            hyoui screen dump demo                      # 現在 visible の ANSI dump (stdout)\n    \
            hyoui screen dump demo --format=ansi | cat  # terminal で再生\n    \
            hyoui screen dump demo --output=screen.ans  # ファイルに保存\n    \
            hyoui screen dump demo --format=cbor > s.cbor  # CBOR binary 保存\n    \
            hyoui screen dump demo --format=binary | grep ERROR\n\
        \n\
        RELATED:\n    \
            hyoui screen snapshot <id>   構造化 state query (= CBOR encoded StateSnapshotResponse)\n    \
            hyoui wait <id> ...          状態条件を待つ\n    \
            hyoui tail <id>              bytes stream を流す\n",
    )
}

fn usage_screen_snapshot() -> String {
    String::from(
        "hyoui screen snapshot — structured screen state snapshot (DR-0013 §9 + DR-0006 §10.3)\n\
        \n\
        USAGE:\n    \
            hyoui screen snapshot <session-id> [options]\n    \
            hyoui screen snapshot --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --include SET       Components (comma-separated, case-insensitive; default: all)\n                        \
                Cells, Cursor, Mode, Style, Scrollback, WindowSize, Buffer, SequenceNo\n                        \
                (Scrollback は daemon 側で未実装 → 明示指定すると error)\n    \
            --format FMT        Output format (default: cbor)\n                        \
                cbor — CBOR encoded StateSnapshotResponse\n                        \
                json — forward-compat (= 現状 daemon 未実装、wire 上は cbor)\n    \
            --output PATH       書き出し先 (= 未指定なら stdout)\n    \
            --timeout DUR       response 受信 timeout (= default 5s。DUR 形式: 5s/500ms/...)\n    \
            -h, --help          Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_LOCK_TOKEN    lock token を env で渡す (= handshake.token)\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= response 受信、payload を出力済)\n    \
            1   connect / I/O / daemon error (= daemon が unsupported-capability\n        \
                や protocol-malformed を返す場合も含む)\n    \
            2   引数不足 / 未知 option / 未知 component\n\
        \n\
        EXAMPLES:\n    \
            hyoui screen snapshot demo                                 # 全 component の CBOR snapshot\n    \
            hyoui screen snapshot demo --include=Cursor,Mode           # cursor + mode のみ\n    \
            hyoui screen snapshot demo --include=cells,window-size     # case-insensitive\n    \
            hyoui screen snapshot demo --output=snap.cbor              # ファイル保存\n    \
            hyoui screen snapshot demo --timeout=2s                    # response 待ち 2s\n\
        \n\
        RELATED:\n    \
            hyoui screen dump <id>       visible bytes dump (ANSI / binary / CBOR)\n    \
            hyoui wait <id> ...          状態条件を待つ\n    \
            hyoui tail <id>              bytes stream を流す\n",
    )
}

fn usage_lock() -> String {
    String::from(
        "hyoui lock — acquire / release a session lock (DR-0006 §7)\n\
        \n\
        USAGE:\n    \
            hyoui lock <subcommand> [options]\n\
        \n\
        SUBCOMMANDS:\n    \
            acquire     Acquire a lock, print token to stdout, hold connection\n    \
            release     Release a lock by token\n\
        \n\
        OPTIONS:\n    \
            -h, --help      Show this help and exit\n\
        \n\
        RELATED:\n    \
            hyoui unlock <id> --token=<T>   Same as `lock release`\n    \
            hyoui input <id> --lock-token=<T> ...   Use the token to run input under lock\n\
        \n\
        Run `hyoui lock <subcommand> --help` for per-subcommand help.\n",
    )
}

fn usage_lock_acquire() -> String {
    String::from(
        "hyoui lock acquire — acquire a session lock (DR-0006 §7)\n\
        \n\
        USAGE:\n    \
            hyoui lock acquire <session-id> [options]\n    \
            hyoui lock acquire --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --mode wait|fail    Behavior when another holder exists (default: wait)\n                        \
                wait — keep polling until acquired or --timeout expires\n                        \
                fail — exit 1 immediately when denied\n    \
            --timeout DUR       Overall acquire timeout (= 未指定なら無限 wait、wait mode のみ意味)\n    \
            -h, --help          Show this help and exit\n\
        \n\
        BEHAVIOR (= 重要):\n    \
            daemon 側は client が socket を保持している間だけ lock を維持する。\n    \
            `lock acquire` は token を **stdout に 1 行 print** した後、\n    \
            **connection を保持して block する** (SIGINT/SIGTERM/stdin EOF まで)。\n    \
            シグナル受信 / stdin EOF で `LockRelease` を送って exit 0 する。\n    \
            \n    \
            wrap した子 process が exit するまで lock を保持するパターンは:\n              \
                TOKEN=$(hyoui lock acquire demo & echo $! > /tmp/lockpid; \\\n                   \
                    wait $(cat /tmp/lockpid))      # ← stdin pipe + EOF 戦略\n              \
                hyoui input demo --lock-token=$TOKEN text:hello\n              \
                kill -TERM $(cat /tmp/lockpid)     # 解放\n    \
            \n    \
            将来 `hyoui tx <id> -- cmd...` (= 別 task) では子 process exit で\n    \
            自動 unlock + token 注入が完結する (= こちらの方が UX が良い)。\n\
        \n\
        OUTPUT:\n    \
            stdout: 取得 token (= 32 文字 hex、1 行)\n    \
            block 中は stderr のみに hint を 1 行出す (= ユーザに「block 中」と知らせる)\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。bare 数字 / 年 (y) / 月 (M) は **error**。\n\
        \n\
        EXIT CODE:\n    \
            0   取得 → release まで完走\n    \
            1   timeout / fail mode で denied / I/O / daemon error\n\
        \n\
        EXAMPLES:\n    \
            hyoui lock acquire demo                          # block で取得 (Ctrl-C で release)\n    \
            hyoui lock acquire demo --mode=fail              # 他者保持中なら即 fail\n    \
            hyoui lock acquire demo --timeout=10s            # 10 秒以内に取れなければ timeout\n\
        \n\
        RELATED:\n    \
            hyoui lock release <id> --token=<T>   Release the lock\n    \
            hyoui unlock <id> --token=<T>          Same as `lock release`\n",
    )
}

fn usage_lock_release() -> String {
    String::from(
        "hyoui lock release — release a session lock by token (DR-0006 §7)\n\
        \n\
        USAGE:\n    \
            hyoui lock release <session-id> --token=<T>\n    \
            hyoui lock release --socket=<path> --token=<T>\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --token TOKEN   Lock token to release (required; env HYOUI_LOCK_TOKEN fallback)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        BEHAVIOR (= 重要):\n    \
            daemon 側は **holder client (= 取得時の connection) からの release のみ** \n    \
            accept する (= token 一致だけでは release できない、holder 照合あり)。\n    \
            したがって本 subcommand は **同 process 内で acquire → release する** \n    \
            か、acquire の block を SIGTERM で起こす運用とは別経路。\n    \
            \n    \
            別 process から release を要求して daemon が `lock.not-held` を返した場合は\n    \
            stderr に hint を出して exit 1 する: 「acquire process を SIGTERM で起こす」\n    \
            ことを促す。\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_LOCK_TOKEN    --token 未指定時の fallback token\n\
        \n\
        EXIT CODE:\n    \
            0   release 成功\n    \
            1   token mismatch / not holder / I/O / daemon error\n    \
            2   引数不足 (token も env も無し / session id も socket も無し)\n\
        \n\
        EXAMPLES:\n    \
            hyoui lock release demo --token=abcd1234...      # explicit token\n    \
            HYOUI_LOCK_TOKEN=$TOKEN hyoui lock release demo  # env から token\n\
        \n\
        RELATED:\n    \
            hyoui lock acquire <id>                Acquire a lock\n    \
            hyoui unlock <id> --token=<T>          Same operation, alias\n",
    )
}

fn usage_unlock() -> String {
    String::from(
        "hyoui unlock — release a session lock (= `lock release` alias、DR-0006 §7)\n\
        \n\
        USAGE:\n    \
            hyoui unlock <session-id> --token=<T>\n    \
            hyoui unlock --socket=<path> --token=<T>\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --token TOKEN   Lock token to release (required; env HYOUI_LOCK_TOKEN fallback)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        本 subcommand は `hyoui lock release` と全く同じ意味で、命名の好みで使い分ける\n\
        (= 「取得は lock acquire、解放は unlock」の対称形が直感的なため別名で露出)。\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_LOCK_TOKEN    --token 未指定時の fallback token\n\
        \n\
        EXIT CODE:\n    \
            0   release 成功\n    \
            1   token mismatch / not holder / I/O / daemon error\n    \
            2   引数不足\n\
        \n\
        EXAMPLES:\n    \
            hyoui unlock demo --token=abcd1234...            # explicit token\n    \
            HYOUI_LOCK_TOKEN=$TOKEN hyoui unlock demo        # env から token\n\
        \n\
        RELATED:\n    \
            hyoui lock acquire <id>                Acquire a lock\n    \
            hyoui lock release <id> --token=<T>    Same operation\n",
    )
}

fn usage_completion() -> String {
    String::from(
        "hyoui completion — print a shell completion script\n\
        \n\
        USAGE:\n    \
            hyoui completion <shell>\n\
        \n\
        OPTIONS:\n    \
            -h, --help      Show this help and exit\n\
        \n\
        SHELLS:\n    \
            bash    Bourne-Again SHell。`source <(hyoui completion bash)` 等で読み込む\n    \
            zsh     Z Shell。`fpath` に置く or `eval` 経由で読み込む\n    \
            fish    Friendly Interactive SHell。`~/.config/fish/completions/` 配下に置く\n\
        \n\
        EXAMPLES:\n    \
            # bash: 現在の shell に直接読ませる\n    \
            source <(hyoui completion bash)\n\
        \n    \
            # zsh: fpath 配下に保存して再起動で有効化\n    \
            hyoui completion zsh > ~/.zsh/completions/_hyoui\n\
        \n    \
            # fish: 自動読み込みディレクトリへ配置\n    \
            hyoui completion fish > ~/.config/fish/completions/hyoui.fish\n\
        \n\
        EXIT CODE:\n    \
            0   script を stdout に出力して正常終了\n    \
            2   shell 名未指定 / 未知 shell / 引数過多\n\
        \n\
        RELATED:\n    \
            hyoui --help        全 subcommand 一覧\n",
    )
}

// =============================================================================
// Input spec (DR-0006 §8) — 本タスク #15 で追加した parser + dispatcher 骨格
// =============================================================================
//
// `hyoui input <session> <spec>...` の spec 単位の表現。各 spec は出現順で
// daemon に送信される (= 順序保証)。本タスクでは **parser のみ実装**、
// 各 prefix の実際の送信処理は別 task で配線する。

/// `hyoui input <session>` に渡される 1 spec の表現 (= DR-0006 §8.2 カタログ)。
///
/// CLI 文字列上では `<prefix>:<value>` の形 (= 例 `text:hello` / `wait-idle:500ms`)。
/// パースは [`parse_input_spec`] で行う。各 variant の `value` は parser 段で
/// 軽い validation を済ませてから保持する (= prefix-specific の重い validation は
/// handler 側 task で実施)。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputSpec {
    /// `text:<string>` — UTF-8 文字列を direct (= no bracket) で送る。
    ///
    /// 中身の改行 / escape 解釈は handler で行う (= DR-0006 §8.2: shell 任せ)。
    /// 本タスクの parser は受け取った文字列をそのまま保持するだけ。
    Text(String),
    /// `hex:<hex>` — even-length の hex string を bytes として送る。
    ///
    /// parser 段で hex string の形式 validation を行う (= `[0-9a-fA-F]+` かつ
    /// even length)。decode 結果の `Vec<u8>` を保持する。
    Hex(Vec<u8>),
    /// `file:<path>` — ファイル内容を bytes として送る (= 大規模 input 用)。
    ///
    /// parser 段では path 文字列のみ保持。ファイル存在確認 / size 制御 / spool
    /// 戦略は handler 側 task (= #16/#21) で実装する。
    File(PathBuf),
    /// `paste:<string>` — UTF-8 文字列を bracketed paste で wrap して送る。
    ///
    /// `ESC[200~` ... `ESC[201~` の wrap は handler 側で実施。
    Paste(String),
    /// `key:<name>` — symbolic key 名 (= `C-c` / `M-x` / `Enter` / `Tab` 等)。
    ///
    /// parser 段では文字列をそのまま保持。alias 解決 / modifier 順序正規化 /
    /// escape sequence への変換は handler 側 task (= #16) で実施する。
    Key(String),
    /// `wait:<pattern>` — visible state regex match まで block する pre-condition。
    ///
    /// regex の compile validation は handler 側 task (= #17) で実施。parser は
    /// 文字列をそのまま保持する。
    Wait(String),
    /// `wait-idle:<duration>` — 入力 idle 期間が指定時間経過するまで block。
    ///
    /// duration parse は parser 段で実施 (= `parse_duration_ms` 経由)。`Duration`
    /// に保持することで handler が単位変換せず使える。
    WaitIdle(Duration),
}

/// `hyoui input <session> <spec>...` の parsed configuration。
///
/// [`InputSpec`] の Vec を順序通り保持する。handler 側 task (= #16) では
/// `specs.iter()` を loop して dispatch する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputCommand {
    /// Explicit socket path (= `--socket`)。`session_id` の代替経路。
    pub socket: Option<String>,
    /// Target session id (= positional 第 1 引数)。`socket` 指定時は None でも OK。
    pub session_id: Option<String>,
    /// Spec list (= 出現順で送信、空 Vec は parser 段で reject)。
    pub specs: Vec<InputSpec>,
    /// Per-spec timeout (= default 5s、特に `wait:` / `wait-idle:` で意味を持つ)。
    pub timeout: Duration,
    /// `--lock-token=<T>` で明示指定された lock token (DR-0006 §6 / §8.5)。
    ///
    /// `Some` なら handshake.token としてそのまま流す (= env 値より優先)。
    /// `None` のときは実行段で `HYOUI_LOCK_TOKEN` 環境変数を読む (= 既存挙動)。
    /// flag 優先 / env fallback で「auto 継承」と「明示上書き」を両立する。
    pub lock_token: Option<String>,
    /// `file:` spec の 1 file あたり最大読み込み bytes (= task #21 セキュリティ視点)。
    ///
    /// 解決優先順 (= CLI 共通の flag > env > default):
    /// - `--max-file-bytes=<N>` (= 本フィールドに直接入る)
    /// - 環境変数 `HYOUI_MAX_FILE_BYTES` (= parser 段で読み込んで本フィールドに入れる)
    /// - default 16 MiB (= [`crate::cli::DEFAULT_INPUT_MAX_FILE_BYTES`])
    ///
    /// `0` は **無制限**扱い (= DR-0006 §8.6 の方針、巨大 file を許す代わりに
    /// memory 枯渇リスクは user 責任)。`file:` 以外の spec は size 制約なし
    /// (= argv 上限が implicit な上限) なので本値は無視される。
    pub max_file_bytes: u64,
}

/// [`InputSpec`] のパース結果 (= prefix で type 判別、payload validate)。
///
/// 戻り値の variant は [`InputSpec`] そのまま。spec 文字列 → InputSpec 変換は
/// CLI 段で完了させる方針 (= handler 側で 2 度 parse しない)。
///
/// # Errors
///
/// - 不明な prefix (= `text:` 等の 7 種以外)
/// - `:` を含まない (= prefix がない裸の文字列)
/// - `hex:` の中身が奇数長 / non-hex 文字を含む
/// - `wait-idle:` の中身が duration として parse できない
///
/// **path validation / regex compile / hex の semantics 検証** は handler 側 task
/// で実施する (= parser は構文 layer のみ担う)。
pub fn parse_input_spec(s: &str) -> Result<InputSpec, String> {
    // prefix と value を `:` で 1 回 split。`:` 自体は value に含まれていい
    // (= regex / paste 内に `:` がよく出る) ので、最初の `:` だけで分ける。
    let (prefix, value) = match s.split_once(':') {
        Some(pair) => pair,
        None => {
            return Err(format!(
                "missing prefix `:` in spec {s:?} \
                 (expected one of: text:, hex:, file:, paste:, key:, wait:, wait-idle:)"
            ));
        }
    };

    match prefix {
        "text" => Ok(InputSpec::Text(value.to_string())),
        "hex" => parse_hex_value(value).map(InputSpec::Hex),
        "file" => Ok(InputSpec::File(PathBuf::from(value))),
        "paste" => Ok(InputSpec::Paste(value.to_string())),
        "key" => {
            if value.is_empty() {
                return Err("key: spec requires a non-empty key name".into());
            }
            Ok(InputSpec::Key(value.to_string()))
        }
        "wait" => {
            // §8.2 によれば `wait:<pattern>` は regex (= state match)。
            // wait-idle は別 prefix。旧 `wait:<duration>` (= idle alias) は
            // 本 DR で廃止。空 pattern は意味を成さないので reject。
            if value.is_empty() {
                return Err("wait: spec requires a non-empty regex pattern".into());
            }
            Ok(InputSpec::Wait(value.to_string()))
        }
        "wait-idle" => {
            let ms = parse_duration_ms(value)
                .map_err(|e| format!("wait-idle: invalid duration {value:?}: {e}"))?;
            Ok(InputSpec::WaitIdle(Duration::from_millis(ms)))
        }
        // 不明な prefix。edit distance 1 以下の候補があれば suggest を添える
        // (= task #22、UX 改善)。
        other => {
            let base = format!(
                "unknown spec prefix {other:?}, expected one of: \
                 text, hex, file, paste, key, wait, wait-idle"
            );
            match suggest_closest(other, INPUT_SPEC_PREFIXES.iter().copied()) {
                Some(s) => Err(format!("{base} (did you mean `{s}:`?)")),
                None => Err(base),
            }
        }
    }
}

/// `hex:` の payload を decode。even-length + ascii hex のみ accept。
fn parse_hex_value(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Err("hex: spec requires non-empty hex string".into());
    }
    if s.len() % 2 != 0 {
        return Err(format!(
            "hex: payload must be even-length (got {} chars in {s:?})",
            s.len()
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])
            .ok_or_else(|| format!("hex: non-hex char {:?} at position {i}", bytes[i] as char))?;
        let lo = hex_nibble(bytes[i + 1]).ok_or_else(|| {
            format!(
                "hex: non-hex char {:?} at position {}",
                bytes[i + 1] as char,
                i + 1
            )
        })?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `hyoui input <session> <spec>... [options]` parser。
///
/// 受理する options:
/// - `--socket=<path>` — session_id の代替
/// - `--timeout=<dur>` — per-spec timeout (= default 5s)
///
/// 受理する positional:
/// - 第 1 引数 = `session_id` (= `--socket` 指定時は省略可)
/// - 残り = spec list (= 1 つ以上必須、空 spec list は error)
///
/// **本タスクでは parser のみ**。各 spec の handler は別 task (= #16/#17) で配線。
fn parse_input(args: &[String]) -> Command {
    let mut socket: Option<String> = None;
    let mut timeout_ms: u64 = 5_000;
    let mut lock_token: Option<String> = None;
    // task #21: `file:` spec の 1 file あたり最大 bytes。
    // 優先順: --max-file-bytes flag > HYOUI_MAX_FILE_BYTES env > default。
    // env は flag 未指定時のみ参照する (= flag 優先で env を上書きできる)。
    let mut max_file_bytes: Option<u64> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt_name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match inline_value {
            Some(v) => Some(v),
            None => {
                // 次 arg を value 候補にするのは `--key value` の形だけ。
                // spec list の途中 (= `text:hello` 等) は positional として
                // 扱いたいので、`--` で始まらない次 arg は value 扱いしない
                // (= screen dump 等の既存 pattern と整合)。
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    consumed_extra = true;
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
        };
        match opt_name.as_str() {
            "--help" | "-h" => {
                return Command::Help {
                    topic: HelpTopic::Input,
                };
            }
            "--socket" => match value {
                Some(v) => {
                    socket = Some(v);
                }
                None => return Command::Error("input: --socket requires a value".into()),
            },
            "--timeout" => match value {
                Some(v) => match parse_duration_ms(&v) {
                    Ok(ms) => timeout_ms = ms,
                    Err(e) => return Command::Error(format!("input: --timeout: {e}")),
                },
                None => return Command::Error("input: --timeout requires a value".into()),
            },
            // DR-0006 §6 / §8.5: 明示 lock token を CLI 引数で渡す。
            // env `HYOUI_LOCK_TOKEN` より優先 (= flag 指定で env を上書き)。
            // 値の検証は handshake で daemon が行うので CLI 段では空文字のみ reject。
            "--lock-token" => match value {
                Some(v) => {
                    if v.is_empty() {
                        return Command::Error(
                            "input: --lock-token requires a non-empty value".into(),
                        );
                    }
                    lock_token = Some(v);
                }
                None => return Command::Error("input: --lock-token requires a value".into()),
            },
            // task #21: `file:` spec の 1 file あたり最大 bytes (= 16 MiB default)。
            // 0 = 無制限、それ以外は u64。humanize 形式 ("16M" 等) は別 task。
            // 解決優先順は (1) flag > (2) HYOUI_MAX_FILE_BYTES env > (3) default。
            // env fallback は本 match 後に max_file_bytes が None なら適用する。
            "--max-file-bytes" => match value {
                Some(v) => {
                    let n = v.parse::<u64>().map_err(|_| {
                        Command::Error(format!("input: --max-file-bytes: invalid u64 value {v:?}"))
                    });
                    match n {
                        Ok(n) => max_file_bytes = Some(n),
                        Err(e) => return e,
                    }
                }
                None => {
                    return Command::Error("input: --max-file-bytes requires a value".into());
                }
            },
            other if other.starts_with("--") => {
                return Command::Error(format!("input: unknown option: {other}"));
            }
            other if other.starts_with('-') && other.len() > 1 => {
                // 単独 `-` は将来 stdin 入力源として予約しうるが、本タスクの
                // scope では明示 reject。`-h` は上で吸収済。
                return Command::Error(format!("input: unknown option: {other}"));
            }
            _ => {
                consumed_extra = false;
                positionals.push(args[i].clone());
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }

    // positional の最初は session_id。それ以降が spec list。
    // ただし `--socket` 指定時は session_id を省略でき、全 positional が spec。
    // 判別は positional 第 1 引数が「spec prefix を含むか」ではなく
    // session_id とみなしてから validate (= `text:` 等が session_id 形式の
    // validation に引っかかる)。
    //
    // 戦略:
    // 1. `--socket` 指定 → 全 positional を spec として parse
    // 2. それ以外 → 第 1 positional を session_id 候補とみなし、`validate_session_id`
    //    が通れば session_id、通らない場合は error (= 「最初の引数が session_id か
    //    spec か」を曖昧にしない、ユーザに spec を最初に書くなら `--socket` を
    //    使わせる)
    let (session_id, spec_strs): (Option<String>, &[String]) = if socket.is_some() {
        (None, positionals.as_slice())
    } else {
        match positionals.first() {
            None => {
                return Command::Error(
                    "input: session id (positional) または --socket=<path> が必要です。\
                     例: `hyoui input <session-id> text:hello key:Enter`"
                        .into(),
                );
            }
            Some(first) => {
                if let Err(e) = validate_session_id(first) {
                    return Command::Error(format!("input: {e}"));
                }
                (Some(first.clone()), &positionals[1..])
            }
        }
    };

    if spec_strs.is_empty() {
        return Command::Error(
            "input: spec list が空です (= 最低 1 つ <prefix>:<value> を指定してください)。\
             例: `hyoui input <session-id> text:hello key:Enter` / \
             prefix 一覧は `hyoui input --help` 参照"
                .into(),
        );
    }

    let mut specs: Vec<InputSpec> = Vec::with_capacity(spec_strs.len());
    for s in spec_strs {
        match parse_input_spec(s) {
            Ok(spec) => specs.push(spec),
            Err(e) => return Command::Error(format!("input: spec `{s}`: {e}")),
        }
    }

    // task #21: max_file_bytes 解決。flag > env > default の優先順。
    // env パース失敗時は CLI Error にせず、warning を stderr に出して default に
    // fallback する (= 既存 env で起動している他 session を巻き込まないため、
    // env の typo を fatal にしない方針)。CLI flag のパース失敗は fatal。
    let max_file_bytes = match max_file_bytes {
        Some(n) => n,
        None => match std::env::var("HYOUI_MAX_FILE_BYTES") {
            Ok(v) => match v.parse::<u64>() {
                Ok(n) => n,
                Err(_) => {
                    eprintln!(
                        "hyoui: warning: HYOUI_MAX_FILE_BYTES={v:?} is not a valid u64; \
                         falling back to default ({DEFAULT_INPUT_MAX_FILE_BYTES} bytes)"
                    );
                    DEFAULT_INPUT_MAX_FILE_BYTES
                }
            },
            Err(_) => DEFAULT_INPUT_MAX_FILE_BYTES,
        },
    };

    Command::Input(InputCommand {
        socket,
        session_id,
        specs,
        timeout: Duration::from_millis(timeout_ms),
        lock_token,
        max_file_bytes,
    })
}

fn usage_input() -> String {
    String::from(
        "hyoui input — send input via spec list (DR-0006 §8)\n\
        \n\
        USAGE:\n    \
            hyoui input <session-id> <spec>... [options]\n    \
            hyoui input --socket=<path> <spec>... [options]\n\
        \n\
        SPECS (= 出現順で送信、order-preserved):\n    \
            text:<string>      Direct UTF-8 text (no bracketed paste)\n    \
            hex:<hex>          Hex-encoded binary bytes (even-length)\n    \
            file:<path>        File content as bytes (大規模 input 用)\n    \
            paste:<string>     Bracketed paste で囲んで送信\n    \
            key:<name>         Symbolic key (= C-c / M-x / Enter / Tab / Up ...)\n    \
            wait:<pattern>     visible state regex match まで block (= state-based)\n    \
            wait-idle:<dur>    入力 idle <dur> 経過まで block (= 単位必須)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH      Explicit socket path (alternative to session-id)\n    \
            --timeout DUR      Per-spec timeout (default: 5s; DUR 形式は下記参照)\n    \
            --lock-token T     明示 lock token (= env HYOUI_LOCK_TOKEN より優先、DR-0006 §8.5)\n    \
            --max-file-bytes N file: spec の 1 file あたり最大 bytes (default 16777216 = 16 MiB、\n                       \
                               0 で無制限、env HYOUI_MAX_FILE_BYTES より優先、DR-0006 §8.6)\n    \
            -h, --help         Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_LOCK_TOKEN   lock token を env で渡す (= handshake.token)。\n                       \
                               --lock-token flag 指定時は無視される\n    \
            HYOUI_MAX_FILE_BYTES file: spec の 1 file あたり最大 bytes を env で渡す。\n                       \
                               --max-file-bytes flag 指定時は無視される。parse 失敗時は\n                       \
                               warning を出して default に fallback\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。bare 数字 / 年 (y) / 月 (M) は **error**。\n\
        \n\
        EXIT CODE:\n    \
            0   全 spec 送信完了\n    \
            1   connect / spec dispatch / daemon error\n    \
            2   引数不足 / 未知 prefix / 未知 option\n\
        \n\
        EXAMPLES:\n    \
            hyoui input demo text:hello key:Enter\n    \
            hyoui input demo \"text:ls -la\" key:Enter\n    \
            hyoui input demo \"paste:$(cat script.py)\"\n    \
            hyoui input demo hex:1b5b41                # = ESC[A (= Up arrow)\n    \
            hyoui input demo file:./payload.txt\n    \
            hyoui input demo \"wait:^\\\\$\" \"text:export FOO=bar\" key:Enter\n    \
            hyoui input demo key:C-c\n\
        \n\
        NOTE:\n    \
            本 subcommand は task #15 で parser + dispatcher 骨格のみ実装。\n    \
            各 spec prefix の実際の送信処理は task #16 (text/hex/file/paste/key) /\n    \
            #17 (wait/wait-idle) で配線される。本 binary では spec を 1 つでも\n    \
            含めると `not yet implemented` で exit 1 する。\n\
        \n\
        RELATED:\n    \
            hyoui screen snapshot <id>   入力後の state を確認\n    \
            hyoui wait <id> ...          条件達成まで block (= 単独 subcommand 形)\n",
    )
}

// =============================================================================
// Parsing helpers (mirror the bootstrap MoonBit implementation)
// =============================================================================

/// Parse a non-negative integer string. Returns `None` on invalid input.
fn parse_int(s: &str) -> Option<i32> {
    if s.is_empty() {
        return None;
    }
    let mut value: i32 = 0;
    for ch in s.chars() {
        if let Some(d) = ch.to_digit(10) {
            value = value.checked_mul(10)?.checked_add(d as i32)?;
        } else {
            return None;
        }
    }
    Some(value)
}

/// Parse a "COLSxROWS" size string. Returns `(cols, rows)` or `None`.
fn parse_size(s: &str) -> Option<(i32, i32)> {
    let bytes = s.as_bytes();
    let mut x_index: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'x' || b == b'X' {
            x_index = Some(i);
            break;
        }
    }
    let x_index = x_index?;
    if x_index == 0 || x_index + 1 >= bytes.len() {
        return None;
    }
    let cols_str = &s[..x_index];
    let rows_str = &s[x_index + 1..];
    match (parse_int(cols_str), parse_int(rows_str)) {
        (Some(c), Some(r)) => Some((c, r)),
        _ => None,
    }
}

/// Split `"--name=value"` into `("--name", Some("value"))`, or `"--name"` into
/// `("--name", None)`.
fn split_eq(arg: &str) -> (String, Option<String>) {
    if let Some(idx) = arg.find('=') {
        (arg[..idx].to_string(), Some(arg[idx + 1..].to_string()))
    } else {
        (arg.to_string(), None)
    }
}

/// `session_id` の最大長 (= 64 chars、POSIX `NAME_MAX` の半分以下に抑える)。
///
/// socket file 名は `<session_id>.sock` なので、parent dir + name で
/// `PATH_MAX` を割ることはまずないが、上限を切ることで CBOR / ANSI escape
/// 等の異常入力経路を早期 reject する (R5-AUD-C2 path traversal 対策)。
pub const MAX_SESSION_ID_LEN: usize = 64;

/// `hyoui input <session> file:<path>` の 1 file あたり default 上限 (= 16 MiB)。
///
/// DR-0006 §8.6 の「default 16MB」に従う (= MiB / MB を 1024^2 として扱う、
/// 厳密 IEC 表記)。CLI 側 (= [`InputCommand::max_file_bytes`]) は次の優先順で
/// この値を override する:
///
/// 1. `--max-file-bytes=<N>` (CLI flag)
/// 2. 環境変数 `HYOUI_MAX_FILE_BYTES`
/// 3. 本 default
///
/// `0` を渡すと無制限扱い。`hyoui-cli` の `input_handlers::handle_file` 側に
/// 同じ値の `DEFAULT_MAX_FILE_BYTES` がある (= handler 内 test 用)。両者は
/// 同期しているが parser 段ではこちらを使う。
pub const DEFAULT_INPUT_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// `session_id` を path traversal / 制御文字 / 過長から守る whitelist validator。
///
/// 許可: `[A-Za-z0-9._-]{1,64}`。さらに以下を明示 reject:
///
/// - 空 string (= "")
/// - `.` 単独、`..` 単独 (= path 構成要素として親 dir 参照になる)
/// - `/` / `\` を含む (= path separator、whitelist 外だが冗長 reject)
///
/// CLI argv parser 段階での早期 reject と、`socket_path::resolve` の前段
/// 防御の **双方** で呼ばれる (= R5-AUD-C2 defense-in-depth)。
///
/// # Errors
///
/// validator に反する場合、人間可読な reason 文字列を返す。
pub fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty() {
        return Err("session_id must not be empty".into());
    }
    if session_id.len() > MAX_SESSION_ID_LEN {
        return Err(format!(
            "session_id too long ({} bytes, max {MAX_SESSION_ID_LEN})",
            session_id.len()
        ));
    }
    if session_id == "." || session_id == ".." {
        return Err(format!(
            "session_id {session_id:?} is a path traversal component"
        ));
    }
    for (idx, ch) in session_id.char_indices() {
        let ok = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-';
        if !ok {
            return Err(format!(
                "session_id contains invalid character {ch:?} at byte {idx} \
                 (allowed: [A-Za-z0-9._-])"
            ));
        }
    }
    Ok(())
}

// =============================================================================
// Typo suggest helpers
// =============================================================================

/// 既知 候補のうち、`input` に最も近いものを Levenshtein 距離 1 以下で返す。
///
/// UX 視点で「edit distance 1 のみ」に絞る (= task #22 方針):
/// 距離 2 以上を suggest すると誤候補が増えてユーザがかえって混乱する。
/// 1 文字違い (typo / 大小ミス / 1 文字脱落 or 余分) のみを救う。
///
/// 比較は **ASCII 大小無視** で行う (= `Tex` → `text`、`Entr` → `Enter`)。
/// 距離 0 (= 大小無視で一致) は **suggest 対象外** (= 呼び出し側で完全一致を
/// 先に試した後の fallback 用)。
pub(crate) fn suggest_closest<'a, I>(input: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let input_lower = input.to_ascii_lowercase();
    let mut best: Option<(&str, usize)> = None;
    for cand in candidates {
        let dist = levenshtein_ascii_ci(&input_lower, cand);
        if dist == 0 {
            // 大小無視で一致するなら suggest としては弱い (= 呼び出し側が
            // 既に完全一致を試して落ちた後の fallback なので、ここに来た時点で
            // ASCII case-insensitive 比較で同一になる候補は事実上「自分自身」
            // か、handler 側で別途処理されるはずの値)。
            continue;
        }
        if dist > 1 {
            continue;
        }
        match best {
            None => best = Some((cand, dist)),
            Some((_, b)) if dist < b => best = Some((cand, dist)),
            _ => {}
        }
    }
    best.map(|(c, _)| c)
}

/// ASCII case-insensitive Levenshtein 距離 (= 早期 cutoff 2 で打ち切り)。
///
/// 短い文字列 (= subcommand / spec prefix / key 名) 専用の小さな実装。
/// 文字列長が `cutoff + 1` 以上離れていれば即 `usize::MAX` 相当で返す。
/// 内部は 1 次元 DP (= rolling) で空間 O(min(m,n))。
fn levenshtein_ascii_ci(a_lower: &str, b: &str) -> usize {
    let a = a_lower.as_bytes();
    // b は呼び出し側で固定 candidate なので、ここで lower-case 化のため to_owned。
    let b_lower = b.to_ascii_lowercase();
    let b = b_lower.as_bytes();
    let (m, n) = (a.len(), b.len());
    if m.abs_diff(n) > 2 {
        return usize::MAX;
    }
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    // 1 次元 DP。
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// `input` spec の prefix 一覧 (= edit distance 比較対象)。
const INPUT_SPEC_PREFIXES: &[&str] = &["text", "hex", "file", "paste", "key", "wait", "wait-idle"];

/// 既知の top-level subcommand 一覧 (= unknown subcommand 時の suggest 用)。
///
/// reserved (`send` / `detach` / `tx` / `lock` / `unlock`) も含める
/// (= 「予約済」と気づかせるほうが UX 改善になる)。
pub(crate) const TOP_LEVEL_SUBCOMMANDS: &[&str] = &[
    "run",
    "attach",
    "list",
    "kill",
    "status",
    "tail",
    "wait",
    "screen",
    "input",
    "completion",
    "send",
    "detach",
    "tx",
    "lock",
    "unlock",
];

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Vec<String>` from string literals — keeps tests terse.
    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    // -------- Ported from cli_wbtest.mbt (21 tests, all under `run`) --------

    #[test]
    fn no_args_shows_help() {
        match parse_args(&args(&[])) {
            Command::Help {
                topic: HelpTopic::Top,
            } => {}
            other => panic!("expected top Help, got {other:?}"),
        }
    }

    #[test]
    fn help_flag_shows_help() {
        match parse_args(&args(&["--help"])) {
            Command::Help {
                topic: HelpTopic::Top,
            } => {}
            other => panic!("expected top Help, got {other:?}"),
        }
    }

    #[test]
    fn run_missing_command_is_error() {
        match parse_args(&args(&["run", "--mode=headless"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_empty_command_after_dashdash_is_error() {
        match parse_args(&args(&["run", "--"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_simple_command() {
        match parse_args(&args(&["run", "--", "echo", "hello"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.command, vec!["echo".to_string(), "hello".to_string()]);
                assert_eq!(cfg.mode, Mode::Interactive);
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Follow);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_headless_preset_flips_suspend_defaults() {
        match parse_args(&args(&["run", "--mode=headless", "--", "cat"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.mode, Mode::Headless);
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::AutoResume);
                assert_eq!(cfg.cols, 80);
                assert_eq!(cfg.rows, 24);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_explicit_suspend_overrides_headless_preset() {
        // DR-0015 §2.3: --on-parent-suspend は廃止、--on-child-suspend のみ override 可
        match parse_args(&args(&[
            "run",
            "--mode=headless",
            "--on-child-suspend=follow",
            "--",
            "cat",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Follow);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_size_parses_cols_and_rows() {
        match parse_args(&args(&["run", "--size", "120x40", "--", "vim"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.cols, 120);
                assert_eq!(cfg.rows, 40);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_cols_and_rows_separately() {
        match parse_args(&args(&[
            "run", "--cols", "100", "--rows", "30", "--", "top",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.cols, 100);
                assert_eq!(cfg.rows, 30);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_invalid_size_is_error() {
        match parse_args(&args(&["run", "--size", "abc", "--", "cat"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_with_unit() {
        match parse_args(&args(&["run", "--timeout", "5s", "--", "sleep", "10"])) {
            Command::Run(cfg) => assert_eq!(cfg.timeout_ms, Some(5000)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_concatenated_units() {
        match parse_args(&args(&["run", "--timeout", "1m30s", "--", "sleep", "200"])) {
            Command::Run(cfg) => assert_eq!(cfg.timeout_ms, Some(90_000)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_timeout_bare_number_is_error() {
        match parse_args(&args(&["run", "--timeout", "5", "--", "sleep", "10"])) {
            Command::Error(_) => {}
            other => panic!("expected Error (bare numbers not allowed), got {other:?}"),
        }
    }

    #[test]
    fn run_idle_timeout_and_until() {
        match parse_args(&args(&[
            "run",
            "--idle-timeout=2s",
            "--until",
            "DONE",
            "--",
            "make",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.idle_timeout_ms, Some(2000));
                assert_eq!(cfg.until, Some("DONE".to_string()));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_socket_explicit_path() {
        match parse_args(&args(&["run", "--socket", "/tmp/x.sock", "--", "sh"])) {
            Command::Run(cfg) => assert_eq!(cfg.socket, Some("/tmp/x.sock".to_string())),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_unknown_option_is_error() {
        match parse_args(&args(&["run", "--bogus", "--", "cat"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_option_without_value_is_error() {
        match parse_args(&args(&["run", "--timeout"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_command_args_with_leading_dashes_preserved() {
        match parse_args(&args(&["run", "--", "ls", "-la", "--color"])) {
            Command::Run(cfg) => {
                assert_eq!(
                    cfg.command,
                    vec!["ls".to_string(), "-la".to_string(), "--color".to_string(),]
                );
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_invalid_mode_is_error() {
        match parse_args(&args(&["run", "--mode=weird", "--", "cat"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // parse_seconds_ms は撤去済 (= duration parser に統合)

    #[test]
    fn parse_size_edge_cases() {
        assert_eq!(parse_size("80x24"), Some((80, 24)));
        assert_eq!(parse_size("80X24"), Some((80, 24)));
        assert_eq!(parse_size("x24"), None);
        assert_eq!(parse_size("80x"), None);
        assert_eq!(parse_size("80"), None);
    }

    #[test]
    fn usage_top_non_empty() {
        let text = usage(&HelpTopic::Top);
        assert!(!text.is_empty());
        assert!(text.contains("SUBCOMMANDS"));
        assert!(text.contains("run"));
        assert!(text.contains("completion"));
    }

    // -------- New tests for subcommand-style CLI --------

    #[test]
    fn short_help_flag() {
        assert!(matches!(
            parse_args(&args(&["-h"])),
            Command::Help {
                topic: HelpTopic::Top
            }
        ));
    }

    #[test]
    fn version_flag_long() {
        assert!(matches!(
            parse_args(&args(&["--version"])),
            Command::Version
        ));
    }

    #[test]
    fn version_flag_short() {
        assert!(matches!(parse_args(&args(&["-V"])), Command::Version));
    }

    #[test]
    fn unknown_subcommand_returns_help_with_topic() {
        match parse_args(&args(&["foo"])) {
            Command::Help {
                topic: HelpTopic::UnknownSubcommand(name),
            } => {
                assert_eq!(name, "foo");
            }
            other => panic!("expected UnknownSubcommand Help, got {other:?}"),
        }
    }

    #[test]
    fn run_alone_is_error() {
        match parse_args(&args(&["run"])) {
            Command::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_help_shows_run_topic() {
        assert!(matches!(
            parse_args(&args(&["run", "--help"])),
            Command::Help {
                topic: HelpTopic::Run
            }
        ));
        assert!(matches!(
            parse_args(&args(&["run", "-h"])),
            Command::Help {
                topic: HelpTopic::Run
            }
        ));
    }

    #[test]
    fn run_help_after_dashdash_is_command_arg_not_help() {
        // `--help` after `--` is part of the child command, not hyoui's help.
        match parse_args(&args(&["run", "--", "cmd", "--help"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.command, vec!["cmd".to_string(), "--help".to_string()]);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn completion_bash() {
        assert!(matches!(
            parse_args(&args(&["completion", "bash"])),
            Command::Completion { shell: Shell::Bash }
        ));
    }

    #[test]
    fn completion_zsh() {
        assert!(matches!(
            parse_args(&args(&["completion", "zsh"])),
            Command::Completion { shell: Shell::Zsh }
        ));
    }

    #[test]
    fn completion_fish() {
        assert!(matches!(
            parse_args(&args(&["completion", "fish"])),
            Command::Completion { shell: Shell::Fish }
        ));
    }

    #[test]
    fn completion_no_shell_is_error() {
        assert!(matches!(
            parse_args(&args(&["completion"])),
            Command::Error(_)
        ));
    }

    #[test]
    fn completion_unknown_shell_is_error() {
        assert!(matches!(
            parse_args(&args(&["completion", "tcsh"])),
            Command::Error(_)
        ));
    }

    #[test]
    fn completion_too_many_args_is_error() {
        assert!(matches!(
            parse_args(&args(&["completion", "bash", "extra"])),
            Command::Error(_)
        ));
    }

    #[test]
    fn completion_help() {
        assert!(matches!(
            parse_args(&args(&["completion", "--help"])),
            Command::Help {
                topic: HelpTopic::Completion
            }
        ));
    }

    /// R5-FB6: `hyoui completion --help` の usage 出力が completion 専用 topic を
    /// 含んでいること (= R4-H1 で list/kill の help 配線を直したのと同じパターン
    /// を completion にも適用)。旧版は `usage_completion()` が骨組み 1 行だけで
    /// SHELLS / EXAMPLES / RELATED が欠落していた。
    #[test]
    fn completion_help_routes_to_completion_topic() {
        // -h でも --help でも completion topic に飛ぶ
        for flag in ["--help", "-h"] {
            match parse_args(&args(&["completion", flag])) {
                Command::Help {
                    topic: HelpTopic::Completion,
                } => {}
                other => {
                    panic!("expected HelpTopic::Completion for `completion {flag}`, got {other:?}")
                }
            }
        }
        // 中身は usage_completion() 由来 (= 上の usage_subcommand_help_routes_to_topic
        // と機能重複だが、SHELLS / EXAMPLES / RELATED 節の存在を明示確認する
        // regression guard)
        let text = usage(&HelpTopic::Completion);
        for needle in [
            "hyoui completion",
            "SHELLS:",
            "EXAMPLES:",
            "RELATED:",
            "bash",
            "zsh",
            "fish",
        ] {
            assert!(
                text.contains(needle),
                "usage_completion() must contain `{needle}`; got:\n{text}"
            );
        }
        // top-level help と混同していないこと (= R4-H1 regression guard)
        assert!(
            !text.contains("SUBCOMMANDS:\n"),
            "usage_completion() must not contain top-level SUBCOMMANDS; got:\n{text}"
        );
    }

    #[test]
    fn reserved_subcommands_return_error() {
        // attach / list / kill / status / tail / wait は実装済 (= 別 test)。
        // `send` / `detach` は旧 leaf 設計の reserved。
        // `tx` は DR-0006 §7 の wrapper、別 task で実装中。
        // `lock` / `unlock` は task #20 で実装済 (= `parse_lock_*` がある)、本テストでは
        // 「引数なしで Error にならない (= Help か Error)」を別 test で確認するため除外。
        for name in ["send", "detach", "tx"] {
            match parse_args(&args(&[name])) {
                Command::Error(msg) => assert!(msg.contains(name), "msg = {msg}"),
                other => panic!("expected Error for `{name}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_status_with_session_id() {
        match parse_args(&args(&["status", "demo"])) {
            Command::Status(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.socket.is_none());
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn parse_status_requires_session_or_socket() {
        match parse_args(&args(&["status"])) {
            Command::Error(msg) => assert!(msg.contains("session id") || msg.contains("socket")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_tail_with_follow_and_since() {
        match parse_args(&args(&["tail", "demo", "--follow", "--since=1s"])) {
            Command::Tail(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.follow);
                assert_eq!(cfg.since_ms, Some(1_000));
                assert!(!cfg.since_strict);
            }
            other => panic!("expected Tail, got {other:?}"),
        }
    }

    #[test]
    fn parse_tail_with_since_strict() {
        // DR-0006 §11: `--since-strict` で scrollback 不足を検知 → exit 非 0
        match parse_args(&args(&["tail", "demo", "--since=10s", "--since-strict"])) {
            Command::Tail(cfg) => {
                assert_eq!(cfg.since_ms, Some(10_000));
                assert!(cfg.since_strict);
            }
            other => panic!("expected Tail, got {other:?}"),
        }
    }

    #[test]
    fn parse_tail_since_strict_requires_since() {
        // `--since-strict` 単独は意味を成さない (= filter する範囲が無い)。error 推奨。
        match parse_args(&args(&["tail", "demo", "--since-strict"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--since-strict"), "got msg={msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_tail_strip_dr_alias() {
        // DR-0006 §11 では `--strip`。現状実装は `--strip-ansi`。両 alias 動作確認。
        match parse_args(&args(&["tail", "demo", "--strip"])) {
            Command::Tail(cfg) => assert!(cfg.strip_ansi),
            other => panic!("expected Tail, got {other:?}"),
        }
        match parse_args(&args(&["tail", "demo", "--strip-ansi"])) {
            Command::Tail(cfg) => assert!(cfg.strip_ansi),
            other => panic!("expected Tail, got {other:?}"),
        }
    }

    #[test]
    fn parse_tail_last_dr_alias() {
        // DR-0006 §11 では `--last N`。現状実装は `--last-bytes N`。両 alias 動作確認。
        match parse_args(&args(&["tail", "demo", "--last=4096"])) {
            Command::Tail(cfg) => assert_eq!(cfg.last_bytes, Some(4096)),
            other => panic!("expected Tail, got {other:?}"),
        }
        match parse_args(&args(&["tail", "demo", "--last-bytes=4096"])) {
            Command::Tail(cfg) => assert_eq!(cfg.last_bytes, Some(4096)),
            other => panic!("expected Tail, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_regex_pattern() {
        // DR-0006 §9 改訂後: subcommand は <pattern> を直接 regex として扱う
        // (= 旧 `text:` / `pattern:` / `wait-idle:` prefix は廃止)
        match parse_args(&args(&["wait", "demo", "READY", "--timeout=5s"])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.pattern, "READY");
                assert_eq!(cfg.timeout_ms, Some(5_000));
                assert_eq!(cfg.poll_interval_ms, None);
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_with_poll_interval() {
        match parse_args(&args(&[
            "wait",
            "demo",
            "ITEM-\\d+",
            "--timeout=30s",
            "--poll-interval=50ms",
        ])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.pattern, "ITEM-\\d+");
                assert_eq!(cfg.timeout_ms, Some(30_000));
                assert_eq!(cfg.poll_interval_ms, Some(50));
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_with_socket_only() {
        match parse_args(&args(&["wait", "--socket=/tmp/foo.sock", "Continue\\?"])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.session_id, None);
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/foo.sock"));
                assert_eq!(cfg.pattern, "Continue\\?");
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_rejects_empty_pattern() {
        match parse_args(&args(&["wait", "demo", ""])) {
            Command::Error(msg) => assert!(msg.contains("pattern"), "got msg={msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_rejects_legacy_strip_escapes_flag() {
        // DR-0006 §9 改訂で `--no-strip-escapes` は廃止 → unknown option として error
        match parse_args(&args(&["wait", "demo", "READY", "--no-strip-escapes"])) {
            Command::Error(msg) => assert!(
                msg.contains("--no-strip-escapes"),
                "expected unknown option message, got {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_duration_ms_basic_units() {
        assert_eq!(parse_duration_ms("500ms"), Ok(500));
        assert_eq!(parse_duration_ms("2s"), Ok(2_000));
        assert_eq!(parse_duration_ms("1m"), Ok(60_000));
        assert_eq!(parse_duration_ms("3h"), Ok(3 * 3_600_000));
        assert_eq!(parse_duration_ms("1d"), Ok(86_400_000));
        assert_eq!(parse_duration_ms("1w"), Ok(7 * 86_400_000));
    }

    #[test]
    fn parse_duration_ms_concatenation() {
        // 1m5s200ms = 60000 + 5000 + 200 = 65200
        assert_eq!(parse_duration_ms("1m5s200ms"), Ok(65_200));
        // 1m1s = 61000
        assert_eq!(parse_duration_ms("1m1s"), Ok(61_000));
        // 1ms (= ms 単位、minute 1 + second ではない)
        assert_eq!(parse_duration_ms("1ms"), Ok(1));
    }

    #[test]
    fn parse_duration_ms_signed_arithmetic() {
        // 1d-4h = 24h - 4h = 20h = 72_000_000
        assert_eq!(parse_duration_ms("1d-4h"), Ok(20 * 3_600_000));
        // 1d+4h = 28h
        assert_eq!(parse_duration_ms("1d+4h"), Ok(28 * 3_600_000));
        // 途中で負になっても最終が正なら OK
        assert_eq!(
            parse_duration_ms("2h-30m+15m"),
            Ok((120 - 30 + 15) * 60_000)
        );
    }

    #[test]
    fn parse_duration_ms_sub_ms_accepted_with_floor() {
        // ns / us / μs は accept、集積後に ms へ floor (= 1ms 超過分のみ取り入れ)。
        // timespec.mbt は YAGNI で reject していたが、hyoui は集積 floor 方針 (kawaz 確定)。
        assert_eq!(parse_duration_ms("999us"), Ok(0)); // 0.999 ms → floor → 0
        assert_eq!(parse_duration_ms("1500us"), Ok(1)); // 1.5 ms → floor → 1
        assert_eq!(parse_duration_ms("1000us"), Ok(1)); // 1.0 ms
        assert_eq!(parse_duration_ms("999999ns"), Ok(0)); // 0.999999 ms → 0
        assert_eq!(parse_duration_ms("1000000ns"), Ok(1)); // 1.0 ms
        assert_eq!(parse_duration_ms("2000μs"), Ok(2)); // 2.0 ms (multi-byte μ)
        // 集積で 1 ms を超えた分は取り入れ
        assert_eq!(parse_duration_ms("500us 600us"), Ok(1)); // 1.1 ms → floor → 1
        assert_eq!(parse_duration_ms("500us 500us"), Ok(1)); // 1.0 ms ぴったり
        assert_eq!(parse_duration_ms("999us 1us"), Ok(1)); // 1.0 ms 境界
        // 完全混在: 1ms + 1500us = 2.5 ms → 2 ms
        assert_eq!(parse_duration_ms("1ms 1500us"), Ok(2));
    }

    #[test]
    fn parse_duration_ms_long_unit_forms() {
        assert_eq!(parse_duration_ms("3minutes"), Ok(180_000));
        assert_eq!(parse_duration_ms("1hour"), Ok(3_600_000));
        assert_eq!(parse_duration_ms("2days"), Ok(172_800_000));
        assert_eq!(parse_duration_ms("1week"), Ok(604_800_000));
        assert_eq!(parse_duration_ms("500milliseconds"), Ok(500));
        assert_eq!(parse_duration_ms("30sec"), Ok(30_000));
        assert_eq!(parse_duration_ms("5min"), Ok(300_000));
    }

    #[test]
    fn parse_duration_ms_decimal_support() {
        // timespec.mbt 仕様: 1.5h = 5400000ms
        assert_eq!(parse_duration_ms("1.5h"), Ok(5_400_000));
        assert_eq!(parse_duration_ms("3.5s"), Ok(3_500));
        assert_eq!(parse_duration_ms("0.5m"), Ok(30_000));
    }

    #[test]
    fn parse_duration_ms_underscore_separator() {
        assert_eq!(parse_duration_ms("3_600_000ms"), Ok(3_600_000));
        assert_eq!(parse_duration_ms("1_000s"), Ok(1_000_000));
        assert_eq!(parse_duration_ms("1_000.5s"), Ok(1_000_500));
        assert_eq!(parse_duration_ms("1_000.5_0s"), Ok(1_000_500));
    }

    #[test]
    fn parse_duration_ms_whitespace_tolerant() {
        assert_eq!(parse_duration_ms("1h 2m"), Ok(3_720_000));
        assert_eq!(parse_duration_ms(" 1h 2m "), Ok(3_720_000));
        assert_eq!(parse_duration_ms(" 1 h 2 m "), Ok(3_720_000));
    }

    #[test]
    fn parse_duration_ms_duplicate_unit_merge() {
        // 1h5m1h = (1+1)h + 5m = 2h5m = 7_500_000
        assert_eq!(parse_duration_ms("1h5m1h"), Ok(7_500_000));
    }

    #[test]
    fn parse_duration_ms_mixed_long_short() {
        assert_eq!(parse_duration_ms("1hour 30m"), Ok(5_400_000));
        assert_eq!(parse_duration_ms("2 days 5h"), Ok(190_800_000));
    }

    #[test]
    fn parse_duration_ms_rejects_bare_number() {
        assert!(parse_duration_ms("0").is_err());
        assert!(parse_duration_ms("500").is_err());
        assert!(parse_duration_ms("1000").is_err());
    }

    #[test]
    fn parse_duration_ms_rejects_invalid() {
        assert!(parse_duration_ms("").is_err());
        assert!(parse_duration_ms("xs").is_err());
        assert!(parse_duration_ms("1y").is_err()); // y = year は不採用
        assert!(parse_duration_ms("1year").is_err());
        assert!(parse_duration_ms("1month").is_err()); // month 長形は reject
        assert!(parse_duration_ms("ms").is_err()); // 数字なし
        assert!(parse_duration_ms("1m-").is_err()); // 末尾 - 不完全
        assert!(parse_duration_ms("1m+").is_err()); // 末尾 + 不完全
    }

    #[test]
    fn parse_duration_ms_strict_grammar() {
        // D2: leading / consecutive / trailing '_' は error
        assert!(parse_duration_ms("_5s").is_err());
        assert!(parse_duration_ms("5__0s").is_err());
        assert!(parse_duration_ms("5_s").is_err());
        // segments 間の `_` も grammar 違反 (= sign で区切るべき)
        assert!(parse_duration_ms("1h_2m").is_err());
        // D3: trailing dot + 単位 (`1.s`) は error、`.5s` も error
        assert!(parse_duration_ms("1.s").is_err());
        assert!(parse_duration_ms(".5s").is_err());
        assert!(parse_duration_ms("1.").is_err());
        // D5: leading `+`/`-` は grammar で許されてない
        assert!(parse_duration_ms("+5m").is_err());
        // `-5m` も leading sign → error (= 別経路で「最終 < 0」も error だが、
        // 文法層で先に弾く)
        assert!(parse_duration_ms("-5m").is_err());
    }

    #[test]
    fn parse_duration_ms_case_insensitive() {
        // H2: 単位は case-insensitive
        assert_eq!(parse_duration_ms("1S"), Ok(1_000));
        assert_eq!(parse_duration_ms("1H"), Ok(3_600_000));
        assert_eq!(parse_duration_ms("1MIN"), Ok(60_000));
        assert_eq!(parse_duration_ms("1Min"), Ok(60_000));
        assert_eq!(parse_duration_ms("1MS"), Ok(1));
        // 短形 m は minute (case-insensitive)
        assert_eq!(parse_duration_ms("1M"), Ok(60_000));
        // month 長形は引き続き reject
        assert!(parse_duration_ms("1MONTH").is_err());
    }

    #[test]
    fn parse_duration_ms_negative_total_rejected() {
        // 最終 total が負なら error (= D5 で leading sign は文法層で先に弾くが、
        // 中間段階で負になる入力は最終 negative-check で弾く)
        assert!(parse_duration_ms("1h-2h").is_err());
    }

    #[test]
    fn status_tail_wait_help_routes_to_topic() {
        for (sub, expected) in [
            ("status", HelpTopic::Status),
            ("tail", HelpTopic::Tail),
            ("wait", HelpTopic::Wait),
        ] {
            let cmd = parse_args(&args(&[sub, "--help"]));
            match cmd {
                Command::Help { ref topic } if *topic == expected => {}
                other => panic!("expected Help({expected:?}) for {sub}, got {other:?}"),
            }
        }
    }

    #[test]
    fn top_help_lists_new_subcommands() {
        let text = usage(&HelpTopic::Top);
        for sub in ["run", "attach", "list", "kill", "status", "tail", "wait"] {
            assert!(text.contains(sub), "top help should list `{sub}`");
        }
        assert!(!text.contains("attach, status   Socket-based"));
    }

    #[test]
    fn parse_wait_idle_only_via_input_family() {
        // wait-idle は subcommand から取り除かれ、input family 経由のみ。
        // ここでは subcommand 側で "wait-idle:500ms" を pattern として regex 扱い
        // しても error にならない (= regex として有効な文字列なので pattern として
        // 受け取られる)。意味としては「文字列 'wait-idle:500ms' が画面に出るまで
        // 待つ」になり、ユーザの意図とずれる可能性はあるが parse 層で弾く方針は
        // 取らない (= regex は任意文字列を許容する)。
        match parse_args(&args(&["wait", "demo", "wait-idle:500ms"])) {
            Command::Wait(cfg) => {
                assert_eq!(cfg.pattern, "wait-idle:500ms");
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_missing_pattern_errors() {
        match parse_args(&args(&["wait", "demo"])) {
            Command::Error(msg) => assert!(msg.contains("pattern"), "got msg={msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn attach_with_session_id() {
        match parse_args(&args(&["attach", "demo"])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.socket, None);
                assert!(!cfg.exclusive);
                assert!(!cfg.detach_others);
                assert_eq!(cfg.mode_str, None);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_with_socket_option() {
        match parse_args(&args(&["attach", "--socket", "/tmp/x.sock"])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/x.sock"));
                assert_eq!(cfg.session_id, None);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_help_routes_to_attach_topic() {
        match parse_args(&args(&["attach", "--help"])) {
            Command::Help {
                topic: HelpTopic::Attach,
            } => {}
            other => panic!("expected Help(Attach), got {other:?}"),
        }
    }

    #[test]
    fn attach_help_text_mentions_detach_key() {
        let text = usage(&HelpTopic::Attach);
        assert!(text.contains("Ctrl-A d"), "help should document Ctrl-A d");
        assert!(text.contains("escape"), "help should document escape");
        assert!(text.contains("--mode"), "help should mention --mode option");
    }

    #[test]
    fn attach_with_mode_and_flags() {
        match parse_args(&args(&[
            "attach",
            "demo",
            "--mode",
            "ro",
            "--exclusive",
            "--detach-others",
        ])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.mode_str.as_deref(), Some("ro"));
                assert!(cfg.exclusive);
                assert!(cfg.detach_others);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_with_no_args_errors() {
        match parse_args(&args(&["attach"])) {
            Command::Error(msg) => assert!(msg.contains("attach")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_with_too_many_positionals_errors() {
        match parse_args(&args(&["attach", "a", "b"])) {
            Command::Error(msg) => assert!(msg.contains("attach")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_unknown_option_errors() {
        match parse_args(&args(&["attach", "demo", "--bogus"])) {
            Command::Error(msg) => assert!(msg.contains("bogus") || msg.contains("attach")),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn no_legacy_shortcut_for_dashdash_at_top() {
        // `hyoui -- cmd` must NOT be treated as `hyoui run -- cmd`.
        // `--` is an unknown subcommand here.
        match parse_args(&args(&["--", "echo", "hi"])) {
            Command::Help {
                topic: HelpTopic::UnknownSubcommand(name),
            } => assert_eq!(name, "--"),
            other => panic!("expected UnknownSubcommand Help, got {other:?}"),
        }
    }

    #[test]
    fn run_mode_separate_value() {
        // `--mode interactive` (space-separated) should work too.
        match parse_args(&args(&["run", "--mode", "headless", "--", "cat"])) {
            Command::Run(cfg) => assert_eq!(cfg.mode, Mode::Headless),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn usage_run_non_empty() {
        let text = usage(&HelpTopic::Run);
        assert!(text.contains("hyoui run"));
        assert!(text.contains("--mode"));
    }

    #[test]
    fn usage_unknown_subcommand_mentions_name() {
        let text = usage(&HelpTopic::UnknownSubcommand("frob".into()));
        assert!(text.contains("frob"));
        assert!(text.contains("SUBCOMMANDS"));
    }

    // R4-H1: each subcommand's `--help` must route to the subcommand-specific
    // HelpTopic (not Top). Regression: `hyoui kill --help` previously printed
    // top-level help, which gave users no info about --signal, exit codes, etc.

    #[test]
    fn list_help_routes_to_list_topic() {
        match parse_args(&args(&["list", "--help"])) {
            Command::Help {
                topic: HelpTopic::List,
            } => {}
            other => panic!("expected Help{{List}}, got {other:?}"),
        }
        match parse_args(&args(&["list", "-h"])) {
            Command::Help {
                topic: HelpTopic::List,
            } => {}
            other => panic!("expected Help{{List}}, got {other:?}"),
        }
    }

    /// R5-H3: `list` の引数なし呼び出しは `prune_stale = false` の
    /// `ListConfig` を返す (= default 動作: liveness 確認のみ、削除しない)。
    #[test]
    fn list_without_flag_returns_default_config() {
        match parse_args(&args(&["list"])) {
            Command::List(cfg) => {
                assert!(!cfg.prune_stale, "default should not prune");
            }
            other => panic!("expected List(default), got {other:?}"),
        }
    }

    /// R5-H3: `list --prune-stale` は `prune_stale = true` の `ListConfig` を返す。
    #[test]
    fn list_prune_stale_flag_sets_config() {
        match parse_args(&args(&["list", "--prune-stale"])) {
            Command::List(cfg) => {
                assert!(cfg.prune_stale, "--prune-stale should enable prune");
            }
            other => panic!("expected List(prune_stale=true), got {other:?}"),
        }
    }

    /// R5-H3: 未知の flag は `Command::Error` を返す (= 既存 list の挙動踏襲)。
    #[test]
    fn list_rejects_unknown_flag() {
        match parse_args(&args(&["list", "--bogus"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--bogus"), "error should mention the flag");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn kill_help_routes_to_kill_topic() {
        match parse_args(&args(&["kill", "--help"])) {
            Command::Help {
                topic: HelpTopic::Kill,
            } => {}
            other => panic!("expected Help{{Kill}}, got {other:?}"),
        }
        match parse_args(&args(&["kill", "-h"])) {
            Command::Help {
                topic: HelpTopic::Kill,
            } => {}
            other => panic!("expected Help{{Kill}}, got {other:?}"),
        }
    }

    // NOTE: status/tail/wait の help routing は status_tail_wait_help_routes_to_topic
    // (上記) で既にカバー済み。R4-H1 で新規追加した list/kill は上の専用 test を、
    // 全 subcommand の help text が subcommand-specific であることは下の
    // subcommand_help_text_is_subcommand_specific でまとめてチェックする。

    /// Each subcommand's usage text must contain the subcommand name and at least
    /// one subcommand-specific keyword, so `hyoui <sub> --help` does NOT look
    /// like the top-level help (= the original R4-H1 bug).
    #[test]
    fn subcommand_help_text_is_subcommand_specific() {
        let cases: &[(HelpTopic, &str, &[&str])] = &[
            (HelpTopic::Run, "hyoui run", &["--mode", "--timeout"]),
            (
                HelpTopic::Attach,
                "hyoui attach",
                &["DETACH KEY", "--exclusive"],
            ),
            (HelpTopic::List, "hyoui list", &["SCAN ORDER"]),
            (HelpTopic::Kill, "hyoui kill", &["--signal", "SIGTERM"]),
            (HelpTopic::Status, "hyoui status", &["OUTPUT", "child-pid"]),
            (HelpTopic::Tail, "hyoui tail", &["--follow", "--since"]),
            // DR-0006 §9 改訂後: PATTERN / --poll-interval が新ヘルプに含まれる
            (
                HelpTopic::Wait,
                "hyoui wait",
                &["PATTERN", "--poll-interval"],
            ),
            (
                HelpTopic::Completion,
                "hyoui completion",
                &["bash", "zsh", "fish", "EXAMPLES", "SHELLS"],
            ),
        ];
        for (topic, head, must_have) in cases {
            let text = usage(topic);
            assert!(
                text.contains(head),
                "topic {topic:?} usage must contain `{head}`; got:\n{text}"
            );
            for needle in must_have.iter() {
                assert!(
                    text.contains(needle),
                    "topic {topic:?} usage must contain `{needle}`; got:\n{text}"
                );
            }
            // Must NOT look like top-level help (= R4-H1 regression guard).
            assert!(
                !text.contains("SUBCOMMANDS:\n"),
                "topic {topic:?} usage must not contain top-level SUBCOMMANDS list; got:\n{text}"
            );
        }
    }

    // ------------------------------------------------------------------
    // R5-AUD-C2: session_id whitelist regression tests (CLI parser side)
    // ------------------------------------------------------------------

    #[test]
    fn parse_run_rejects_invalid_session_id() {
        // `hyoui run --session=<bad>` で path traversal / 制御文字 等を早期 reject。
        let bad = [
            "../../.ssh/control", // path traversal
            "../etc",
            "a/b",           // separator
            "a\\b",          // windows separator
            "..",            // dot-dot literal
            ".",             // dot literal
            "",              // empty (--session= 等で来る)
            "a\nb",          // newline (control char)
            "a\x1b[31mhack", // ANSI escape
            "name with space",
        ];
        for sid in bad {
            let arg = format!("--session={sid}");
            match parse_args(&args(&["run", &arg, "--", "true"])) {
                Command::Error(msg) => {
                    assert!(
                        msg.contains("--session") || msg.contains("session_id"),
                        "error for {sid:?} should mention --session/session_id, got: {msg}"
                    );
                }
                other => panic!("expected Error for invalid session_id {sid:?}, got {other:?}"),
            }
        }

        // 過長 (65 chars) も reject。
        let too_long = "a".repeat(MAX_SESSION_ID_LEN + 1);
        let arg = format!("--session={too_long}");
        match parse_args(&args(&["run", &arg, "--", "true"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("too long"),
                    "error for too-long should mention 'too long', got: {msg}"
                );
            }
            other => panic!("expected Error for too-long session_id, got {other:?}"),
        }
    }

    #[test]
    fn parse_run_accepts_normal_session_id() {
        // 正常系: 一般的な session 名は通る (= 回帰時に既存ユーザを巻き込まない確認)。
        for sid in ["demo", "run-12345", "session_01", "build.2025-05-27"] {
            let arg = format!("--session={sid}");
            match parse_args(&args(&["run", &arg, "--", "true"])) {
                Command::Run(cfg) => {
                    assert_eq!(cfg.session.as_deref(), Some(sid));
                }
                other => panic!("expected Run for valid session_id {sid:?}, got {other:?}"),
            }
        }
    }

    /// DR-0013 §8: `--scrollback-rows=<N>` 受理。`--scrollback-rows=0` も
    /// 受理 (= scrollback 完全無効化用)。
    #[test]
    fn parse_run_accepts_scrollback_rows() {
        match parse_args(&args(&["run", "--scrollback-rows=500", "--", "true"])) {
            Command::Run(cfg) => assert_eq!(cfg.scrollback_rows, Some(500)),
            other => panic!("expected Run, got {other:?}"),
        }
        match parse_args(&args(&["run", "--scrollback-rows=0", "--", "true"])) {
            Command::Run(cfg) => assert_eq!(cfg.scrollback_rows, Some(0)),
            other => panic!("expected Run for --scrollback-rows=0, got {other:?}"),
        }
        // 未指定なら None (= DaemonConfig 既定値を維持)
        match parse_args(&args(&["run", "--", "true"])) {
            Command::Run(cfg) => assert_eq!(cfg.scrollback_rows, None),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    /// 非数値 / 負数を渡すと Error。
    #[test]
    fn parse_run_rejects_invalid_scrollback_rows() {
        for bad in ["abc", "-1", "1.5"] {
            let arg = format!("--scrollback-rows={bad}");
            match parse_args(&args(&["run", &arg, "--", "true"])) {
                Command::Error(msg) => {
                    assert!(
                        msg.contains("scrollback-rows"),
                        "error should mention scrollback-rows: {msg}"
                    );
                }
                other => panic!("expected Error for --scrollback-rows={bad}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_kill_rejects_invalid_session_id() {
        // positional session_id 経由 (= `hyoui kill <bad>`) でも reject。
        match parse_args(&args(&["kill", "../../.ssh/control"])) {
            Command::Error(msg) => {
                assert!(msg.contains("kill"), "error should mention 'kill': {msg}");
                assert!(
                    msg.contains("invalid character") || msg.contains("path traversal"),
                    "error should explain why, got: {msg}"
                );
            }
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    /// DR-0012: `--signal=SIGTERM` が cfg.signal に正規表記文字列で格納される。
    #[test]
    fn parse_kill_signal_flag_accepts_sigterm() {
        match parse_args(&args(&["kill", "demo", "--signal=SIGTERM"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.signal.as_deref(), Some("SIGTERM"));
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Kill(signal=SIGTERM), got {other:?}"),
        }
        // 空白区切り形式 (= `--signal SIGKILL`) も同じ
        match parse_args(&args(&["kill", "demo", "--signal", "SIGKILL"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.signal.as_deref(), Some("SIGKILL"));
            }
            other => panic!("expected Kill(signal=SIGKILL), got {other:?}"),
        }
    }

    /// DR-0012: 旧 `--signum N` (= u8 数値) は v0.2.0 で removed。明示的 error で
    /// `--signal NAME` への誘導メッセージを返す。
    #[test]
    fn parse_kill_rejects_legacy_signum_flag() {
        match parse_args(&args(&["kill", "demo", "--signum=15"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("--signum"),
                    "error should mention removed flag: {msg}"
                );
                assert!(
                    msg.contains("--signal"),
                    "error should direct user to --signal: {msg}"
                );
                assert!(
                    msg.contains("DR-0012") || msg.contains("v0.2.0"),
                    "error should hint about the breaking change: {msg}"
                );
            }
            other => panic!("expected Error for legacy --signum, got {other:?}"),
        }
    }

    /// DR-0012: 略名 / 小文字 / 数値は CLI 段で reject される (= wire の正規化を
    /// CLI 入口で強制)。
    #[test]
    fn parse_kill_rejects_non_canonical_signal_names() {
        for bogus in &["TERM", "sigterm", "15", "SIG", "sig_term"] {
            match parse_args(&args(&["kill", "demo", "--signal", bogus])) {
                Command::Error(msg) => {
                    assert!(
                        msg.contains("invalid --signal"),
                        "error for {bogus} should mention invalid --signal: {msg}"
                    );
                }
                other => panic!("expected Error for `--signal {bogus}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_status_rejects_invalid_session_id() {
        // parse_session_targeted 経由 (status/attach/tail) も reject。
        match parse_args(&args(&["status", "a/b"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    #[test]
    fn parse_wait_rejects_invalid_session_id() {
        // parse_wait の positional path も reject (= predicate と紛らわしいので
        // session_id 側に落ちた値が validate されることを確認)。
        match parse_args(&args(&["wait", "../foo", "text:READY"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // DR-0013 §9 + DR-0006 §10.2: `screen dump` subcommand parser tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_screen_no_args_shows_parent_help() {
        match parse_args(&args(&["screen"])) {
            Command::Help { topic } => assert_eq!(topic, HelpTopic::Screen),
            other => panic!("expected Help::Screen, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_help_flag_shows_parent_help() {
        match parse_args(&args(&["screen", "--help"])) {
            Command::Help { topic } => assert_eq!(topic, HelpTopic::Screen),
            other => panic!("expected Help::Screen, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_unknown_subcommand_errors() {
        match parse_args(&args(&["screen", "bogus"])) {
            Command::Error(msg) => {
                assert!(msg.contains("bogus"), "msg should mention name: {msg}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // DR-0013 §9 + DR-0006 §10.3: `screen snapshot` subcommand parser tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_screen_snapshot_default_include_and_format() {
        match parse_args(&args(&["screen", "snapshot", "demo"])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.socket.is_none());
                // default include: Cells, Cursor, Mode, WindowSize, Buffer, SequenceNo
                // (Scrollback は意図的に除外)
                assert!(cfg.include.contains(&SnapshotCliComponent::Cells));
                assert!(cfg.include.contains(&SnapshotCliComponent::Cursor));
                assert!(cfg.include.contains(&SnapshotCliComponent::Mode));
                assert!(cfg.include.contains(&SnapshotCliComponent::WindowSize));
                assert!(cfg.include.contains(&SnapshotCliComponent::Buffer));
                assert!(cfg.include.contains(&SnapshotCliComponent::SequenceNo));
                assert!(!cfg.include.contains(&SnapshotCliComponent::Scrollback));
                assert_eq!(cfg.format, ScreenSnapshotCliFormat::Cbor);
                assert!(cfg.output.is_none());
                assert_eq!(cfg.timeout_ms, 5_000);
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_help_flag() {
        match parse_args(&args(&["screen", "snapshot", "--help"])) {
            Command::Help { topic } => assert_eq!(topic, HelpTopic::ScreenSnapshot),
            other => panic!("expected Help::ScreenSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_include_subset() {
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--include=Cursor,Mode",
        ])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(
                    cfg.include,
                    vec![SnapshotCliComponent::Cursor, SnapshotCliComponent::Mode]
                );
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_include_case_insensitive() {
        // 大小文字混在 / kebab / 連結を吸収
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--include=cells,WINDOW-SIZE,sequenceno",
        ])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(
                    cfg.include,
                    vec![
                        SnapshotCliComponent::Cells,
                        SnapshotCliComponent::WindowSize,
                        SnapshotCliComponent::SequenceNo,
                    ]
                );
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_include_dedupe() {
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--include=Cells,Cells,cursor,CELLS",
        ])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(
                    cfg.include,
                    vec![SnapshotCliComponent::Cells, SnapshotCliComponent::Cursor]
                );
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_include_unknown_errors() {
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--include=Cells,foobar",
        ])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("foobar") || msg.contains("unknown"),
                    "msg: {msg}"
                )
            }
            other => panic!("expected Error for unknown component, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_include_empty_errors() {
        match parse_args(&args(&["screen", "snapshot", "demo", "--include="])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("empty") || msg.contains("include"),
                    "msg: {msg}"
                )
            }
            other => panic!("expected Error for empty include, got {other:?}"),
        }
    }

    /// QA edge: comma 連続 (= `Cells,,Cursor`) は途中に空要素が混じる形。
    /// 既存 `parse_screen_snapshot_include_dedupe` は dedupe を扱うが、空要素
    /// 単体に関する明示的 reject は別系統 (= `empty component in ...`) なので
    /// 別 test として保護する。
    #[test]
    fn parse_screen_snapshot_include_consecutive_commas_errors() {
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--include=Cells,,Cursor",
        ])) {
            Command::Error(msg) => {
                assert!(msg.contains("empty"), "msg: {msg}");
            }
            other => panic!("expected Error for consecutive commas, got {other:?}"),
        }
    }

    /// QA edge: underscore 区切り (= `window_size`) も hyphen と等価に accept。
    /// `parse_snapshot_include` の `chars().filter(c != '-' && c != '_')` 経路を保護。
    #[test]
    fn parse_screen_snapshot_include_accepts_underscore_form() {
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--include=window_size,sequence_no",
        ])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(
                    cfg.include,
                    vec![
                        SnapshotCliComponent::WindowSize,
                        SnapshotCliComponent::SequenceNo,
                    ]
                );
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_format_cbor_default() {
        match parse_args(&args(&["screen", "snapshot", "demo", "--format=cbor"])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(cfg.format, ScreenSnapshotCliFormat::Cbor);
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_format_json_accepted() {
        match parse_args(&args(&["screen", "snapshot", "demo", "--format=json"])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(cfg.format, ScreenSnapshotCliFormat::Json);
            }
            other => panic!("expected Screen::Snapshot for --format=json, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_format_invalid_errors() {
        match parse_args(&args(&["screen", "snapshot", "demo", "--format=xml"])) {
            Command::Error(msg) => assert!(msg.contains("xml")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_output_option() {
        match parse_args(&args(&[
            "screen",
            "snapshot",
            "demo",
            "--output=/tmp/snap.cbor",
        ])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(cfg.output.as_deref(), Some("/tmp/snap.cbor"));
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_timeout_option() {
        match parse_args(&args(&["screen", "snapshot", "demo", "--timeout=2s"])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(cfg.timeout_ms, 2_000);
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_socket_alternative() {
        match parse_args(&args(&["screen", "snapshot", "--socket=/tmp/x.sock"])) {
            Command::Screen(ScreenCommand::Snapshot(cfg)) => {
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/x.sock"));
                assert!(cfg.session_id.is_none());
            }
            other => panic!("expected Screen::Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_requires_session_or_socket() {
        match parse_args(&args(&["screen", "snapshot"])) {
            Command::Error(msg) => {
                assert!(msg.contains("session") || msg.contains("socket"))
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_unknown_option_errors() {
        match parse_args(&args(&["screen", "snapshot", "demo", "--bogus"])) {
            Command::Error(msg) => {
                assert!(msg.contains("bogus") || msg.contains("screen snapshot"))
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_snapshot_rejects_invalid_session_id() {
        match parse_args(&args(&["screen", "snapshot", "../foo"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    #[test]
    fn usage_screen_lists_snapshot_subcommand() {
        let text = usage(&HelpTopic::Screen);
        assert!(text.contains("snapshot"));
    }

    #[test]
    fn usage_screen_snapshot_lists_options() {
        let text = usage(&HelpTopic::ScreenSnapshot);
        assert!(text.contains("hyoui screen snapshot"));
        assert!(text.contains("--include"));
        assert!(text.contains("--format"));
        assert!(text.contains("--output"));
        assert!(text.contains("--timeout"));
        assert!(text.contains("Cells"));
        assert!(text.contains("Cursor"));
        assert!(text.contains("Mode"));
        assert!(text.contains("WindowSize"));
        assert!(text.contains("Buffer"));
        assert!(text.contains("SequenceNo"));
    }

    #[test]
    fn parse_screen_dump_default_format_and_layer() {
        match parse_args(&args(&["screen", "dump", "demo"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.socket.is_none());
                assert_eq!(cfg.format, ScreenDumpCliFormat::Ansi);
                assert_eq!(cfg.layer, ScreenDumpCliLayer::Visible);
                assert!(cfg.rect.is_none());
                assert!(cfg.output.is_none());
                assert_eq!(cfg.timeout_ms, 5_000);
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_help_flag() {
        match parse_args(&args(&["screen", "dump", "--help"])) {
            Command::Help { topic } => assert_eq!(topic, HelpTopic::ScreenDump),
            other => panic!("expected Help::ScreenDump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_format_choices() {
        for (s, want) in &[
            ("ansi", ScreenDumpCliFormat::Ansi),
            ("binary", ScreenDumpCliFormat::Binary),
            ("cbor", ScreenDumpCliFormat::Cbor),
        ] {
            let arg = format!("--format={s}");
            match parse_args(&args(&["screen", "dump", "demo", &arg])) {
                Command::Screen(ScreenCommand::Dump(cfg)) => {
                    assert_eq!(&cfg.format, want, "format {s}");
                }
                other => panic!("expected Screen::Dump for format={s}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_screen_dump_format_text_plain() {
        // primary name = MIME 風の "text/plain"
        match parse_args(&args(&["screen", "dump", "demo", "--format=text/plain"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.format, ScreenDumpCliFormat::TextPlain);
            }
            other => panic!("expected Screen::Dump for --format=text/plain, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_format_text_alias() {
        // alias = "text" 短縮
        match parse_args(&args(&["screen", "dump", "demo", "--format=text"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.format, ScreenDumpCliFormat::TextPlain);
            }
            other => panic!("expected Screen::Dump for --format=text, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_format_plain_alias() {
        // alias = "plain" 短縮
        match parse_args(&args(&["screen", "dump", "demo", "--format=plain"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.format, ScreenDumpCliFormat::TextPlain);
            }
            other => panic!("expected Screen::Dump for --format=plain, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_format_json_rejected() {
        match parse_args(&args(&["screen", "dump", "demo", "--format=json"])) {
            Command::Error(msg) => {
                assert!(msg.contains("MVP") || msg.contains("json") || msg.contains("scope"))
            }
            other => panic!("expected Error for --format=json, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_format_invalid_errors() {
        match parse_args(&args(&["screen", "dump", "demo", "--format=xml"])) {
            Command::Error(msg) => assert!(msg.contains("xml")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_layer_choices() {
        for (s, want) in &[
            ("visible", ScreenDumpCliLayer::Visible),
            ("scrollback", ScreenDumpCliLayer::Scrollback),
            ("both", ScreenDumpCliLayer::Both),
        ] {
            let arg = format!("--layer={s}");
            match parse_args(&args(&["screen", "dump", "demo", &arg])) {
                Command::Screen(ScreenCommand::Dump(cfg)) => {
                    assert_eq!(&cfg.layer, want, "layer {s}");
                }
                other => panic!("expected Screen::Dump for layer={s}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_screen_dump_rect_ok() {
        match parse_args(&args(&["screen", "dump", "demo", "--rect=0,1,80,24"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                let r = cfg.rect.expect("rect should be set");
                assert_eq!(r.x, 0);
                assert_eq!(r.y, 1);
                assert_eq!(r.w, 80);
                assert_eq!(r.h, 24);
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_rect_with_spaces_ok() {
        // 余白 trim する
        match parse_args(&args(&[
            "screen",
            "dump",
            "demo",
            "--rect= 0 , 0 , 10 , 5 ",
        ])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                let r = cfg.rect.expect("rect should be set");
                assert_eq!((r.x, r.y, r.w, r.h), (0, 0, 10, 5));
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_rect_wrong_count_errors() {
        match parse_args(&args(&["screen", "dump", "demo", "--rect=0,1,80"])) {
            Command::Error(msg) => assert!(msg.contains("4")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_rect_invalid_int_errors() {
        match parse_args(&args(&["screen", "dump", "demo", "--rect=0,1,abc,24"])) {
            Command::Error(msg) => assert!(msg.contains("u16") || msg.contains("invalid")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// QA edge: 全ゼロ rect (= `0,0,0,0`) は構文上 valid。w=0/h=0 = 空 rect の
    /// forward-compat 動作確認 (= daemon 側 ignore が想定挙動、CLI 段では reject しない)。
    #[test]
    fn parse_screen_dump_rect_all_zero_ok() {
        match parse_args(&args(&["screen", "dump", "demo", "--rect=0,0,0,0"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                let r = cfg.rect.expect("rect should be set");
                assert_eq!((r.x, r.y, r.w, r.h), (0, 0, 0, 0));
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    /// QA edge: u16 上限 (= 65535) を超える値は明示的に error。overflow 経路を
    /// 確認しておく (= 黙って wrap-around しない安全網)。
    #[test]
    fn parse_screen_dump_rect_overflow_u16_errors() {
        match parse_args(&args(&["screen", "dump", "demo", "--rect=0,0,80,99999"])) {
            Command::Error(msg) => {
                assert!(msg.contains("u16") || msg.contains("invalid"), "msg: {msg}");
            }
            other => panic!("expected Error for overflow, got {other:?}"),
        }
    }

    /// QA edge: 負の値は u16 parse で reject される (= 符号付きにしていない確認)。
    #[test]
    fn parse_screen_dump_rect_negative_errors() {
        match parse_args(&args(&["screen", "dump", "demo", "--rect=-1,0,80,24"])) {
            Command::Error(msg) => {
                assert!(msg.contains("u16") || msg.contains("invalid"), "msg: {msg}");
            }
            other => panic!("expected Error for negative value, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_output_option() {
        match parse_args(&args(&[
            "screen",
            "dump",
            "demo",
            "--output=/tmp/screen.ans",
        ])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.output.as_deref(), Some("/tmp/screen.ans"));
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_timeout_option() {
        match parse_args(&args(&["screen", "dump", "demo", "--timeout=2s"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.timeout_ms, 2_000);
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_socket_alternative() {
        match parse_args(&args(&["screen", "dump", "--socket=/tmp/x.sock"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/x.sock"));
                assert!(cfg.session_id.is_none());
            }
            other => panic!("expected Screen::Dump, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_requires_session_or_socket() {
        match parse_args(&args(&["screen", "dump"])) {
            Command::Error(msg) => {
                assert!(msg.contains("session") || msg.contains("socket"))
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_unknown_option_errors() {
        match parse_args(&args(&["screen", "dump", "demo", "--bogus"])) {
            Command::Error(msg) => assert!(msg.contains("bogus") || msg.contains("screen dump")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_dump_rejects_invalid_session_id() {
        match parse_args(&args(&["screen", "dump", "../foo"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for invalid session_id, got {other:?}"),
        }
    }

    #[test]
    fn usage_screen_lists_dump_subcommand() {
        let text = usage(&HelpTopic::Screen);
        assert!(text.contains("hyoui screen"));
        assert!(text.contains("dump"));
    }

    #[test]
    fn usage_screen_dump_lists_options() {
        let text = usage(&HelpTopic::ScreenDump);
        assert!(text.contains("hyoui screen dump"));
        assert!(text.contains("--format"));
        assert!(text.contains("--layer"));
        assert!(text.contains("--rect"));
        assert!(text.contains("--output"));
        assert!(text.contains("--timeout"));
    }

    #[test]
    fn usage_top_lists_screen() {
        let text = usage(&HelpTopic::Top);
        assert!(text.contains("screen"));
    }

    // -------- input subcommand + spec parser (DR-0006 §8、task #15) --------

    // -------- parse_input_spec (= 各 prefix の成功 / 失敗ケース) --------

    #[test]
    fn parse_input_spec_text_ok() {
        assert_eq!(
            parse_input_spec("text:hello").unwrap(),
            InputSpec::Text("hello".into())
        );
        // 空文字列 text は許容 (= shell escape の都合で空 string を渡す pattern)
        assert_eq!(
            parse_input_spec("text:").unwrap(),
            InputSpec::Text(String::new())
        );
        // `:` を含む text 値も OK (= 最初の `:` で split、それ以降は value 内)
        assert_eq!(
            parse_input_spec("text:foo:bar:baz").unwrap(),
            InputSpec::Text("foo:bar:baz".into())
        );
    }

    #[test]
    fn parse_input_spec_hex_ok() {
        assert_eq!(
            parse_input_spec("hex:1b5b41").unwrap(),
            InputSpec::Hex(vec![0x1b, 0x5b, 0x41])
        );
        // 大文字 / 大小混在 OK
        assert_eq!(
            parse_input_spec("hex:DEadBEEF").unwrap(),
            InputSpec::Hex(vec![0xde, 0xad, 0xbe, 0xef])
        );
    }

    #[test]
    fn parse_input_spec_hex_invalid_errors() {
        // odd length
        let err = parse_input_spec("hex:abc").unwrap_err();
        assert!(err.contains("even-length"), "got: {err}");
        // empty
        let err = parse_input_spec("hex:").unwrap_err();
        assert!(err.contains("non-empty"), "got: {err}");
        // non-hex char
        let err = parse_input_spec("hex:zz").unwrap_err();
        assert!(err.contains("non-hex"), "got: {err}");
    }

    #[test]
    fn parse_input_spec_file_ok() {
        match parse_input_spec("file:./payload.txt").unwrap() {
            InputSpec::File(p) => assert_eq!(p, PathBuf::from("./payload.txt")),
            other => panic!("expected File, got {other:?}"),
        }
        // 絶対 path / `-` (= stdin) も parser は素通し (= handler 側 task で扱う)
        match parse_input_spec("file:/tmp/data").unwrap() {
            InputSpec::File(p) => assert_eq!(p, PathBuf::from("/tmp/data")),
            other => panic!("expected File, got {other:?}"),
        }
        match parse_input_spec("file:-").unwrap() {
            InputSpec::File(p) => assert_eq!(p, PathBuf::from("-")),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_spec_paste_ok() {
        assert_eq!(
            parse_input_spec("paste:line1\nline2").unwrap(),
            InputSpec::Paste("line1\nline2".into())
        );
    }

    #[test]
    fn parse_input_spec_key_ok() {
        assert_eq!(
            parse_input_spec("key:Enter").unwrap(),
            InputSpec::Key("Enter".into())
        );
        assert_eq!(
            parse_input_spec("key:C-c").unwrap(),
            InputSpec::Key("C-c".into())
        );
        assert_eq!(
            parse_input_spec("key:M-x").unwrap(),
            InputSpec::Key("M-x".into())
        );
        // 空 key は reject (= 「prefix だけ書いて value 空」は意味なし)
        assert!(parse_input_spec("key:").is_err());
    }

    #[test]
    fn parse_input_spec_wait_ok() {
        assert_eq!(
            parse_input_spec("wait:^Prompt>").unwrap(),
            InputSpec::Wait("^Prompt>".into())
        );
        // 空 pattern は reject
        let err = parse_input_spec("wait:").unwrap_err();
        assert!(err.contains("non-empty"), "got: {err}");
    }

    #[test]
    fn parse_input_spec_wait_idle_ok() {
        match parse_input_spec("wait-idle:500ms").unwrap() {
            InputSpec::WaitIdle(d) => assert_eq!(d, Duration::from_millis(500)),
            other => panic!("expected WaitIdle, got {other:?}"),
        }
        match parse_input_spec("wait-idle:2s").unwrap() {
            InputSpec::WaitIdle(d) => assert_eq!(d, Duration::from_secs(2)),
            other => panic!("expected WaitIdle, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_spec_wait_idle_invalid_errors() {
        // bare 数字 = 単位なし、parse_duration_ms で reject
        let err = parse_input_spec("wait-idle:500").unwrap_err();
        assert!(err.contains("wait-idle:"), "got: {err}");
        // empty
        let err = parse_input_spec("wait-idle:").unwrap_err();
        assert!(err.contains("wait-idle:"), "got: {err}");
        // garbage
        let err = parse_input_spec("wait-idle:abc").unwrap_err();
        assert!(err.contains("wait-idle:"), "got: {err}");
    }

    #[test]
    fn parse_input_spec_unknown_prefix_errors() {
        let err = parse_input_spec("bogus:value").unwrap_err();
        assert!(err.contains("unknown spec prefix"), "got: {err}");
        assert!(
            err.contains("text"),
            "should list known prefixes, got: {err}"
        );
        assert!(
            err.contains("wait-idle"),
            "should list known prefixes, got: {err}"
        );
    }

    #[test]
    fn parse_input_spec_missing_colon_errors() {
        let err = parse_input_spec("hello").unwrap_err();
        assert!(err.contains("missing prefix"), "got: {err}");
    }

    // -------- task #22: typo suggest (edit distance 1) --------

    #[test]
    fn parse_input_spec_typo_suggests_text() {
        // `tex:` → `text:`
        let err = parse_input_spec("tex:hello").unwrap_err();
        assert!(err.contains("did you mean `text:`"), "got: {err}");
    }

    #[test]
    fn parse_input_spec_typo_suggests_paste() {
        // `pase:` → `paste:`
        let err = parse_input_spec("pase:hello").unwrap_err();
        assert!(err.contains("did you mean `paste:`"), "got: {err}");
    }

    #[test]
    fn parse_input_spec_typo_suggests_wait_idle() {
        // `wait-idl:` → `wait-idle:`
        let err = parse_input_spec("wait-idl:500ms").unwrap_err();
        assert!(err.contains("did you mean `wait-idle:`"), "got: {err}");
    }

    #[test]
    fn parse_input_spec_far_typo_no_suggest() {
        // 距離 2 以上は suggest しない (= 誤候補を増やさない方針)
        let err = parse_input_spec("frobnicate:foo").unwrap_err();
        assert!(!err.contains("did you mean"), "got: {err}");
        // ただし base メッセージは出る
        assert!(err.contains("unknown spec prefix"), "got: {err}");
    }

    #[test]
    fn suggest_closest_returns_none_for_empty_candidates() {
        let v: Vec<&str> = vec![];
        assert_eq!(suggest_closest("foo", v), None);
    }

    #[test]
    fn suggest_closest_skips_exact_match() {
        // 距離 0 (= 大小無視で一致) は呼び出し側で既に試した前提なので skip。
        assert_eq!(
            suggest_closest("TEXT", ["text", "hex"]).map(str::to_string),
            None
        );
    }

    #[test]
    fn suggest_closest_handles_case_insensitive() {
        // `Entr` (= 4 chars) と `Enter` (= 5 chars) は 1 文字脱落の typo。
        assert_eq!(
            suggest_closest("Entr", ["Enter", "Esc", "Tab"]),
            Some("Enter")
        );
    }

    #[test]
    fn parse_args_unknown_subcommand_suggests_close_match() {
        // `snapsht` → `snapshot` ではなく、top-level subcommand 一覧で suggest。
        // top-level に `snapshot` は無いので、これは suggest 出ない (= screen の子)。
        // 代わりに「`statu` → `status`」で suggest が出るかを確認。
        match parse_args(&args(&["statu"])) {
            Command::Help {
                topic: HelpTopic::UnknownSubcommand(name),
            } => {
                assert_eq!(name, "statu");
                let text = usage(&HelpTopic::UnknownSubcommand(name));
                assert!(
                    text.contains("did you mean `status`"),
                    "expected suggest, got: {text}"
                );
            }
            other => panic!("expected UnknownSubcommand Help, got {other:?}"),
        }
    }

    #[test]
    fn parse_screen_unknown_subcommand_suggests_close_match() {
        // `snapsht` → `snapshot`
        match parse_args(&args(&["screen", "snapsht"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("did you mean `screen snapshot`"),
                    "expected suggest, got: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // -------- parse_args / parse_input (= subcommand integration) --------

    #[test]
    fn parse_input_basic_session_and_spec() {
        match parse_args(&args(&["input", "demo", "text:hello"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.session_id.as_deref(), Some("demo"));
                assert_eq!(cmd.socket, None);
                assert_eq!(cmd.specs, vec![InputSpec::Text("hello".into())]);
                assert_eq!(cmd.timeout, Duration::from_secs(5));
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_multiple_specs_preserve_order() {
        match parse_args(&args(&[
            "input",
            "demo",
            "text:ls -la",
            "key:Enter",
            "wait:^\\$",
            "wait-idle:200ms",
        ])) {
            Command::Input(cmd) => {
                assert_eq!(
                    cmd.specs,
                    vec![
                        InputSpec::Text("ls -la".into()),
                        InputSpec::Key("Enter".into()),
                        InputSpec::Wait("^\\$".into()),
                        InputSpec::WaitIdle(Duration::from_millis(200)),
                    ]
                );
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_help_flag() {
        match parse_args(&args(&["input", "--help"])) {
            Command::Help {
                topic: HelpTopic::Input,
            } => {}
            other => panic!("expected Help(Input), got {other:?}"),
        }
    }

    #[test]
    fn parse_input_no_session_no_socket_errors() {
        // session_id も --socket も指定なし
        match parse_args(&args(&["input"])) {
            Command::Error(msg) => {
                assert!(msg.contains("session id"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_empty_spec_list_errors() {
        match parse_args(&args(&["input", "demo"])) {
            Command::Error(msg) => {
                assert!(msg.contains("spec list が空"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_socket_alternative_no_session() {
        // --socket=path だけ、session_id 省略可、spec は必須
        match parse_args(&args(&["input", "--socket=/tmp/x.sock", "text:hi"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.socket.as_deref(), Some("/tmp/x.sock"));
                assert_eq!(cmd.session_id, None);
                assert_eq!(cmd.specs, vec![InputSpec::Text("hi".into())]);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_socket_with_empty_specs_errors() {
        // --socket だけで spec 0 個 → spec list 空 error
        match parse_args(&args(&["input", "--socket=/tmp/x.sock"])) {
            Command::Error(msg) => {
                assert!(msg.contains("spec list が空"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_timeout_option() {
        match parse_args(&args(&["input", "demo", "--timeout=2s", "text:x"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.timeout, Duration::from_secs(2));
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_timeout_bare_number_errors() {
        // 単位なしは parse_duration_ms で reject される
        match parse_args(&args(&["input", "demo", "--timeout=5", "text:x"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--timeout"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_unknown_option_errors() {
        match parse_args(&args(&["input", "demo", "--bogus=1", "text:x"])) {
            Command::Error(msg) => {
                assert!(msg.contains("unknown option"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // -------- --lock-token (DR-0006 §6 / §8.5) --------

    #[test]
    fn parse_input_lock_token_inline() {
        match parse_args(&args(&["input", "demo", "--lock-token=tok-abc", "text:x"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.lock_token.as_deref(), Some("tok-abc"));
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_lock_token_separated() {
        // `--lock-token VALUE` (= space-separated) も accept する
        match parse_args(&args(&[
            "input",
            "demo",
            "--lock-token",
            "tok-xyz",
            "text:x",
        ])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.lock_token.as_deref(), Some("tok-xyz"));
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_lock_token_default_is_none() {
        match parse_args(&args(&["input", "demo", "text:x"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.lock_token, None);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_lock_token_empty_value_errors() {
        match parse_args(&args(&["input", "demo", "--lock-token=", "text:x"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--lock-token"), "got: {msg}");
                assert!(msg.contains("non-empty"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_lock_token_missing_value_errors() {
        // 末尾に flag だけ置いて value 候補がない → error
        match parse_args(&args(&["input", "demo", "text:x", "--lock-token"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--lock-token"), "got: {msg}");
                assert!(msg.contains("requires a value"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // --- task #21: --max-file-bytes / HYOUI_MAX_FILE_BYTES ---

    /// flag 未指定 / env 未設定 → default 16 MiB が入る。
    /// env を確実に未設定にするため、test 内で remove_var しておく
    /// (= 並列 test との race を避けるため `MAX_FILE_BYTES_ENV_GUARD` で直列化)。
    #[test]
    fn parse_input_max_file_bytes_default() {
        let _g = MAX_FILE_BYTES_ENV_GUARD.lock().unwrap();
        // env 操作は test 内のみ、guard で並列 test と直列化
        crate::sys::env::remove_var("HYOUI_MAX_FILE_BYTES");
        match parse_args(&args(&["input", "demo", "text:x"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.max_file_bytes, DEFAULT_INPUT_MAX_FILE_BYTES);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    /// `--max-file-bytes=N` で override。
    #[test]
    fn parse_input_max_file_bytes_flag() {
        match parse_args(&args(&["input", "demo", "--max-file-bytes=4096", "text:x"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.max_file_bytes, 4096);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    /// `--max-file-bytes=0` は無制限扱い (= u64 値そのまま保持、handler 側で
    /// 0 を「無制限」として扱う)。
    #[test]
    fn parse_input_max_file_bytes_zero_unlimited() {
        match parse_args(&args(&["input", "demo", "--max-file-bytes=0", "text:x"])) {
            Command::Input(cmd) => {
                assert_eq!(cmd.max_file_bytes, 0);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    /// 非数値 / 負値は parse error (= u64 範囲外)。
    #[test]
    fn parse_input_max_file_bytes_invalid_errors() {
        match parse_args(&args(&["input", "demo", "--max-file-bytes=abc", "text:x"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--max-file-bytes"), "got: {msg}");
                assert!(msg.contains("invalid u64"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        match parse_args(&args(&["input", "demo", "--max-file-bytes=-1", "text:x"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--max-file-bytes"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// env `HYOUI_MAX_FILE_BYTES` で override (= flag 未指定時に有効)。
    /// 並列 test との race を避けるため `MAX_FILE_BYTES_ENV_GUARD` で直列化。
    #[test]
    fn parse_input_max_file_bytes_env_fallback() {
        let _g = MAX_FILE_BYTES_ENV_GUARD.lock().unwrap();
        // env 操作は test 内のみ、guard で並列 test と直列化
        crate::sys::env::set_var("HYOUI_MAX_FILE_BYTES", "65536");
        let res = parse_args(&args(&["input", "demo", "text:x"]));
        // 後始末を確実に
        crate::sys::env::remove_var("HYOUI_MAX_FILE_BYTES");
        match res {
            Command::Input(cmd) => {
                assert_eq!(cmd.max_file_bytes, 65536);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    /// flag が env より優先 (= flag 指定で env を上書き)。
    #[test]
    fn parse_input_max_file_bytes_flag_overrides_env() {
        let _g = MAX_FILE_BYTES_ENV_GUARD.lock().unwrap();
        crate::sys::env::set_var("HYOUI_MAX_FILE_BYTES", "99999");
        let res = parse_args(&args(&["input", "demo", "--max-file-bytes=4096", "text:x"]));
        crate::sys::env::remove_var("HYOUI_MAX_FILE_BYTES");
        match res {
            Command::Input(cmd) => {
                assert_eq!(cmd.max_file_bytes, 4096);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    /// env が parse 不能 → warning を出して default に fallback (= fatal でない)。
    #[test]
    fn parse_input_max_file_bytes_env_invalid_falls_back_to_default() {
        let _g = MAX_FILE_BYTES_ENV_GUARD.lock().unwrap();
        crate::sys::env::set_var("HYOUI_MAX_FILE_BYTES", "not-a-number");
        let res = parse_args(&args(&["input", "demo", "text:x"]));
        crate::sys::env::remove_var("HYOUI_MAX_FILE_BYTES");
        match res {
            Command::Input(cmd) => {
                assert_eq!(cmd.max_file_bytes, DEFAULT_INPUT_MAX_FILE_BYTES);
            }
            other => panic!("expected Input, got {other:?}"),
        }
    }

    /// env を触る test を直列化するための Mutex。
    /// `HYOUI_MAX_FILE_BYTES` を set/remove する test 同士の race を防ぐ。
    static MAX_FILE_BYTES_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_input_unknown_spec_prefix_errors() {
        match parse_args(&args(&["input", "demo", "bogus:value"])) {
            Command::Error(msg) => {
                assert!(msg.contains("unknown spec prefix"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_invalid_session_id_errors() {
        // session_id に `..` や path separator が含まれていれば
        // validate_session_id で reject される
        match parse_args(&args(&["input", "..", "text:x"])) {
            Command::Error(msg) => {
                assert!(msg.contains("path traversal"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_input_hex_invalid_propagates_to_command_error() {
        match parse_args(&args(&["input", "demo", "hex:zz"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("non-hex") || msg.contains("hex:"),
                    "got: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // -------- usage / help integration --------

    #[test]
    fn usage_input_lists_prefixes() {
        let text = usage(&HelpTopic::Input);
        assert!(text.contains("hyoui input"));
        for prefix in [
            "text:",
            "hex:",
            "file:",
            "paste:",
            "key:",
            "wait:",
            "wait-idle:",
        ] {
            assert!(
                text.contains(prefix),
                "usage_input should mention {prefix}, got:\n{text}"
            );
        }
        // option section
        assert!(text.contains("--socket"));
        assert!(text.contains("--timeout"));
    }

    #[test]
    fn usage_top_lists_input() {
        let text = usage(&HelpTopic::Top);
        assert!(
            text.contains("input"),
            "top usage should list input subcommand"
        );
    }

    // -------- `lock` / `unlock` subcommand (= DR-0006 §7、task #20) --------

    #[test]
    fn parse_lock_no_args_shows_help() {
        // `hyoui lock` 単体は親 help を出す (= cli-design-preferences の
        // 「引数なし実行時は --help を表示」)。
        match parse_args(&args(&["lock"])) {
            Command::Help {
                topic: HelpTopic::Lock,
            } => {}
            other => panic!("expected Help(Lock), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_help_flag_shows_help() {
        match parse_args(&args(&["lock", "--help"])) {
            Command::Help {
                topic: HelpTopic::Lock,
            } => {}
            other => panic!("expected Help(Lock), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_basic() {
        match parse_args(&args(&["lock", "acquire", "demo"])) {
            Command::Lock(LockCommand::Acquire(cfg)) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert!(cfg.socket.is_none());
                assert_eq!(cfg.mode, LockMode::Wait); // default
                assert_eq!(cfg.timeout_ms, None);
            }
            other => panic!("expected Lock(Acquire), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_mode_fail() {
        match parse_args(&args(&["lock", "acquire", "demo", "--mode=fail"])) {
            Command::Lock(LockCommand::Acquire(cfg)) => {
                assert_eq!(cfg.mode, LockMode::Fail);
            }
            other => panic!("expected Lock(Acquire), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_mode_wait_explicit() {
        match parse_args(&args(&["lock", "acquire", "demo", "--mode=wait"])) {
            Command::Lock(LockCommand::Acquire(cfg)) => {
                assert_eq!(cfg.mode, LockMode::Wait);
            }
            other => panic!("expected Lock(Acquire), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_mode_invalid_errors() {
        match parse_args(&args(&["lock", "acquire", "demo", "--mode=block"])) {
            Command::Error(msg) => assert!(msg.contains("--mode"), "got: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_timeout() {
        match parse_args(&args(&["lock", "acquire", "demo", "--timeout=5s"])) {
            Command::Lock(LockCommand::Acquire(cfg)) => {
                assert_eq!(cfg.timeout_ms, Some(5_000));
            }
            other => panic!("expected Lock(Acquire), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_timeout_bare_number_errors() {
        // 単位なしは parse_duration_ms で reject される
        match parse_args(&args(&["lock", "acquire", "demo", "--timeout=5"])) {
            Command::Error(msg) => assert!(msg.contains("--timeout"), "got: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_socket_alternative() {
        match parse_args(&args(&["lock", "acquire", "--socket=/tmp/x.sock"])) {
            Command::Lock(LockCommand::Acquire(cfg)) => {
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/x.sock"));
                assert!(cfg.session_id.is_none());
            }
            other => panic!("expected Lock(Acquire), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_acquire_unknown_option_errors() {
        match parse_args(&args(&["lock", "acquire", "demo", "--bogus=1"])) {
            Command::Error(msg) => assert!(msg.contains("unknown option"), "got: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_release_basic() {
        match parse_args(&args(&["lock", "release", "demo", "--token=abc123"])) {
            Command::Lock(LockCommand::Release(cfg)) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.token.as_deref(), Some("abc123"));
            }
            other => panic!("expected Lock(Release), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_release_token_missing_is_allowed_at_parse() {
        // token は parser 段では optional (= env fallback あり)。dispatcher 側で
        // env を読んでもなお None なら exit 2 で reject する。
        match parse_args(&args(&["lock", "release", "demo"])) {
            Command::Lock(LockCommand::Release(cfg)) => {
                assert!(cfg.token.is_none());
            }
            other => panic!("expected Lock(Release), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_release_empty_token_rejected() {
        match parse_args(&args(&["lock", "release", "demo", "--token="])) {
            Command::Error(msg) => assert!(msg.contains("--token"), "got: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_release_separated_token() {
        // `--token VALUE` (= space-separated) も accept する
        match parse_args(&args(&["lock", "release", "demo", "--token", "tok-xyz"])) {
            Command::Lock(LockCommand::Release(cfg)) => {
                assert_eq!(cfg.token.as_deref(), Some("tok-xyz"));
            }
            other => panic!("expected Lock(Release), got {other:?}"),
        }
    }

    #[test]
    fn parse_lock_unknown_subcommand_suggests() {
        // `lock acqire` (= typo) を `lock acquire` に suggest する
        match parse_args(&args(&["lock", "acqire", "demo"])) {
            Command::Error(msg) => {
                assert!(msg.contains("acqire"), "got: {msg}");
                assert!(
                    msg.contains("did you mean") && msg.contains("acquire"),
                    "got: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_unlock_basic() {
        match parse_args(&args(&["unlock", "demo", "--token=tok"])) {
            Command::Unlock(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.token.as_deref(), Some("tok"));
            }
            other => panic!("expected Unlock, got {other:?}"),
        }
    }

    #[test]
    fn parse_unlock_token_missing_is_allowed_at_parse() {
        match parse_args(&args(&["unlock", "demo"])) {
            Command::Unlock(cfg) => {
                assert!(cfg.token.is_none());
            }
            other => panic!("expected Unlock, got {other:?}"),
        }
    }

    #[test]
    fn parse_unlock_empty_token_rejected() {
        match parse_args(&args(&["unlock", "demo", "--token="])) {
            Command::Error(msg) => assert!(msg.contains("--token"), "got: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_unlock_unknown_option_errors() {
        match parse_args(&args(&["unlock", "demo", "--bogus"])) {
            Command::Error(msg) => assert!(msg.contains("unknown option"), "got: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_unlock_no_session_or_socket_errors() {
        match parse_args(&args(&["unlock"])) {
            Command::Error(msg) => assert!(
                msg.contains("session id") || msg.contains("socket"),
                "got: {msg}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn usage_lock_topic_is_renderable() {
        let text = usage(&HelpTopic::Lock);
        assert!(text.contains("acquire"));
        assert!(text.contains("release"));
    }

    #[test]
    fn usage_lock_acquire_mentions_mode_and_timeout() {
        let text = usage(&HelpTopic::LockAcquire);
        assert!(text.contains("--mode"));
        assert!(text.contains("--timeout"));
    }

    #[test]
    fn usage_lock_release_mentions_token() {
        let text = usage(&HelpTopic::LockRelease);
        assert!(text.contains("--token"));
    }

    #[test]
    fn usage_unlock_mentions_token() {
        let text = usage(&HelpTopic::Unlock);
        assert!(text.contains("--token"));
    }

    #[test]
    fn usage_top_lists_lock_and_unlock() {
        let text = usage(&HelpTopic::Top);
        assert!(text.contains("lock"));
        assert!(text.contains("unlock"));
    }
}
