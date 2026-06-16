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

/// Behavior when the child process is suspended (STOPPED).
///
/// daemon 視点の policy 名。daemon が子の stop を観測したときに何をするか:
/// - `Notify`: leader client に `SessionChildStoppedNotify` を送るだけ
///   (= 勝手に子を起こさない、DR-0017 §柱2)。client 側がそれを受けて follow する。
/// - `AutoResume`: daemon が即座に子 process group へ SIGCONT を送って復帰させる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnChildSuspend {
    /// Notify the leader client; do not resume the child automatically.
    Notify,
    /// Resume the child immediately by sending SIGCONT (daemon-driven).
    AutoResume,
}

// DR-0015 §2.3: `OnParentSuspend` enum / `--on-parent-suspend` flag 廃止。
// 新構成では attach client が外部 SIGTSTP を受けても daemon は無関係 (= 旧
// `decouple` 相当の動作のみ、policy 選択肢自体が不要)。

/// `--stdin-eof` flag の明示値 (DR-0019 §5)。attach / run 共通。
///
/// 未指定 (= `None`) のときの解決は呼出側 (= `attach_command`) が stdin の tty
/// 判定で行う: 非 tty なら `SendEof` (= pipe-through の透過性回復)、tty なら従来
/// 挙動 (= EOF が通常来ないので実質 `Detach`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StdinEofArg {
    /// EOF 観測時にそのまま切断 (= 現行挙動。子は daemon 配下に残る)。
    Detach,
    /// EOF 観測時に EOT (0x04) を子 PTY へ送出 (= canonical mode の子が自然 exit)。
    SendEof,
}

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
    /// Help for the `set` subcommand (= runtime 設定変更、DR-0019 Update)。
    Set,
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
    /// Help for the `detach` subcommand (= attach 引き剥がし、DR-0020 §4)。
    Detach,
    /// Help for the `record` parent subcommand (= 子: `start` / `stop` / `list`、DR-0016)。
    Record,
    /// Help for the `record start` subcommand (= DR-0016 §2)。
    RecordStart,
    /// Help for the `record stop` subcommand (= DR-0016 §2)。
    RecordStop,
    /// Help for the `record list` subcommand (= DR-0016 §2)。
    RecordList,
    /// User invoked an unknown subcommand; render top-level help with note.
    UnknownSubcommand(String),
}

/// Fully parsed `run` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// Virtual screen columns (explicit `--cols/--size` 指定時のみ Some)。
    /// `None` なら caller (= `run_command`) が外側 TTY size or 80 fallback で解決。
    pub cols: Option<i32>,
    /// Virtual screen rows (explicit `--rows/--size` 指定時のみ Some)。
    /// `None` なら caller (= `run_command`) が外側 TTY size or 24 fallback で解決。
    pub rows: Option<i32>,
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
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。socket 配置先 dir と、
    /// 子プロセスへ常時注入する `HYOUI_NAMESPACE` env の値を決める。
    pub namespace: Option<String>,
    /// `--stdin-eof=detach|send-eof` (DR-0019 §5)。`None` (= 未指定) なら exec attach
    /// 側の tty 判定で解決 (= 非 tty で `SendEof`、tty で従来挙動)。`Some` のときは
    /// 値をそのまま exec attach に伝搬する。
    pub stdin_eof: Option<StdinEofArg>,
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
    /// `--index=N` or 位置引数の数字 (= `hyoui attach 1` / `attach -1`)。
    ///
    /// `hyoui list` の mtime 昇順 sort 結果に対する index 指定:
    /// - `1` → 1 番古い session、`2` → 2 番古い、...
    /// - `-1` → 1 番新しい session、`-2` → 2 番新しい、...
    /// - `0` は不正 (= 1-based index、`Command::Error` で reject)
    ///
    /// `session_id` と同時指定はエラー。位置引数が数字のみで該当 session-id が
    /// 存在しない場合は index 解釈、数字以外の session-id を強制したい場合は
    /// `--` セパレータか `--index=N` を使う。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。session / index 解決を
    /// namespace スコープに絞る。
    pub namespace: Option<String>,
    /// `--stdin-eof=detach|send-eof` (DR-0019 §5)。`None` (= 未指定) なら stdin の
    /// tty 判定で解決 (= 非 tty で `SendEof`、tty で従来挙動の `Detach`)。
    pub stdin_eof: Option<StdinEofArg>,
    /// `--quiet` (DR-0020 §5)。attach 成立時の stderr ヒント (= detach/peek 案内) を
    /// 抑止する。非 tty stderr では flag に関わらずヒントを出さない。
    pub quiet: bool,
}

/// `detach` subcommand configuration (DR-0020 §4)。
///
/// CLI の detach は常に **all** (= この session の全 attach client を引き剥がす)。
/// `--target=self|others` は CLI から出さない (Fable review M1 2026-06-12):
/// detach CLI は一時接続で daemon に Detach を送る構造のため、self は「一時接続が
/// 自分を切る」no-op、others は「一時接続以外 ≒ 全部」となり all と実質同義で、
/// flag として嘘になる。中から自分の端末だけ抜けるのは attach の detach key
/// (Ctrl-A d) の役割。protocol の `DetachTarget::{Myself, Others}` は detach key /
/// `--detach-others` 用として内部に残る。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetachConfig {
    /// Target socket path. `Some(p)` で explicit、`None` なら session_id から resolve。
    pub socket: Option<String>,
    /// Target session id (= socket path resolver の入力)。
    pub session_id: Option<String>,
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
}

/// `list` subcommand の出力形式 (= `--format=plain|jsonl`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ListFormat {
    /// Plain text (= human readable、default)。固定長 columns で
    /// `SESSION / STATUS / DUR / SOCKET` を 1 行ごとに出力。
    #[default]
    Plain,
    /// JSON Lines (= scripting 用、1 session 1 行 JSON object)。
    /// field: `session` / `status` / `started_unix_ms` / `dur_ms` / `socket`。
    Jsonl,
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

    /// 出力 format (= default Plain、`--format=jsonl` で JSON Lines)。
    pub format: ListFormat,

    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。表示対象を当該
    /// namespace のみに絞る。`all_namespaces` 指定時は無視される。
    pub namespace: Option<String>,

    /// `--all-namespaces` (= DR-0018)。全 namespace を横断 scan し、出力に NS 列を
    /// 追加する。`--prune-stale` と併用すると全 namespace の stale socket を掃除する。
    pub all_namespaces: bool,
}

/// `kill` subcommand configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KillConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// 子 PTY に送る signal 名 (= default SIGTERM、DR-0012)。
    ///
    /// 入力は POSIX kill 慣習 (= 数字 `9` / 略名 `KILL` / `-N` flag) も受理し、
    /// CLI 段で正規 SIG-prefix 大文字 (= `SIGKILL`) に normalize してから wire
    /// に流す (= daemon 側 `signal_name_to_nix_signal` は SIG-prefix 大文字のみ
    /// 解釈、defense in depth)。OS 上で defined でない signal は CLI 段で reject。
    pub signal: Option<String>,
    /// `--index=N` または 位置引数 (正数) 由来の session selector index。
    ///
    /// `hyoui list` の mtime 昇順 sort に対する 1-based 指定 (= attach と同じ流儀)。
    /// `session_id` / `socket` / `all` と排他。
    ///
    /// kill 文脈では 位置引数の負数は **signal 番号 (POSIX `-N`)** として扱うため、
    /// 最新 session を選ぶには `--index=-1` を使う (= 位置引数 `-1` は SIGHUP)。
    pub index: Option<i32>,
    /// `--all`: 全 live session を kill する (= killall 相当)。
    ///
    /// `session_id` / `socket` / `index` と排他。`--signal` だけは併用可。
    pub all: bool,
    /// `--no-terminate` (DR-0017 §柱2): signal を送るだけで **session を畳まない**。
    ///
    /// 既定の `kill` は signal 送信後に session を terminate する (= master close →
    /// 子に SIGHUP)。stopped child を起こすだけ (= `--signal=CONT --no-terminate`)
    /// 等、session を残したまま signal を送りたい場合に使う。`ControlMessage::Signal`
    /// (= 非 terminate) 経路に切り替わる。`--all` とは併用不可 (= killall に
    /// 非 terminate は意味を成さない)。
    pub no_terminate: bool,
    /// `--wait`: 子 exit + session 終了まで見届けて返る (= 従来挙動)。
    ///
    /// **terminate するか / 待つか の 2 軸**のうち「待つか」軸を制御する
    /// (`no_terminate` が「terminate するか」軸)。既定 (= `wait=false`) は daemon
    /// が signal 受理時点で `KillAck` を返すので client は即時 return する
    /// (= `kill(1)` と同じ直感、子が 1 発で死なない app でも無応答にならない)。
    /// `--wait` 指定時は daemon は ack を送らず、session terminate 完了
    /// (= socket EOF) まで待つ (= kill 直後に同名 session を作り直すスクリプト等)。
    ///
    /// `--no-terminate` とは併用不可 (= terminate しない経路に「終了を待つ」は
    /// 意味を成さない)。terminate 経路 (= `ControlMessage::Kill`) 専用。
    pub wait: bool,
    /// `--wait` の timeout (ms)。`wait == true` のときのみ意味を持つ。
    ///
    /// - 裸 `--wait` (= 値なし): [`KILL_WAIT_DEFAULT_TIMEOUT_MS`] (= 10s)
    /// - `--wait=<DUR>`: `parse_duration_ms` で解釈した値
    ///
    /// timeout 超過時は `kill_on_timeout` に従う (= default はエラー終了、
    /// `--kill-on-timeout` 指定時は SIGKILL 昇格)。
    pub wait_timeout_ms: Option<u64>,
    /// `--kill-on-timeout`: `--wait` の timeout 超過時に SIGKILL 昇格して見届けるか。
    ///
    /// default `false` (= timeout 時はエラー終了、子は生かす)。`--wait` 無しでの
    /// 指定は parse エラー (= timeout 概念が無い経路に昇格指定は無意味)。
    pub kill_on_timeout: bool,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。session / index / --all の
    /// 解決を namespace スコープに絞る。
    pub namespace: Option<String>,
}

/// 裸 `--wait` (= 値なし) のデフォルト timeout (ms)。
///
/// 10s の根拠: 対話 app (claude / vim 等) が SIGTERM を受けて後始末 (= state flush /
/// 子 process 回収) してから exit するのに十分余裕がある一方、TERM を完全 ignore する
/// 子で client / daemon が無駄に待ち続けない閾値。CI / script では `--wait=<DUR>` で
/// 短く絞れる。
pub const KILL_WAIT_DEFAULT_TIMEOUT_MS: u64 = 10_000;

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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= 未指定なら None、実行時に env / default へ fallback)。
    /// DR-0018: session 解決を namespace スコープに絞る。
    pub namespace: Option<String>,
    /// `--format=plain|json` (= default `Plain`、H5: scripting で grep/cut の罠回避)。
    pub format: StatusFormat,
}

/// `set` subcommand configuration (DR-0019 Update)。
///
/// `hyoui set <session> <key>=<value>` で runtime 設定を変更する。汎用 key=value
/// 形式で、初期サポート key は `on-child-suspend` (値 `notify` / `auto-resume`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
    /// 変更する設定 key (= `on-child-suspend` 等、`key=value` の左辺)。
    pub key: String,
    /// 設定 value (= `notify` / `auto-resume` 等、`key=value` の右辺)。
    pub value: String,
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
    /// `--include=Cells,Cursor,...` (= comma-separated)、default は全 component。
    /// Vec はそのまま wire の `include: Vec<SnapshotComponent>` に流す。
    pub include: Vec<SnapshotCliComponent>,
    /// `--format=cbor|json` (= default cbor)。`json` は CLI 段で `serde_json` で
    /// human-readable JSON に変換 (= daemon は CBOR が正本、wire 変更なし)。
    pub format: ScreenSnapshotCliFormat,
    /// `--output=<path>` (= 未指定なら stdout)。
    pub output: Option<String>,
    /// `--timeout=<ms>` (= response 受信 timeout、default 5000ms)。
    pub timeout_ms: u64,
}

/// `screen snapshot` の format 選択肢 (= DR-0006 §10.3)。
///
/// daemon は CBOR が正本。`json` は CLI 段で `serde_json` で human-readable に
/// 変換した出力 (= wire 変更なし、`cells` / `scrollback` の bytes は serde
/// default で number array に展開される、量が多い場合は `--include` で skip 推奨)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ScreenSnapshotCliFormat {
    /// CBOR encoded `StateSnapshotResponse` (= 機械処理、default)。
    #[default]
    Cbor,
    /// JSON encoded `StateSnapshotResponse` (= CLI 段で `serde_json` 変換、
    /// jq 等の標準 JSON ツールに渡しやすい)。
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
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

// =============================================================================
// `record` subcommand (DR-0016)
// =============================================================================
//
// `protocol::messages::record` の wire enum と 1:1 対応する CLI 表現 enum を持つ
// (= 既存 `ScreenDumpCliFormat` / `SnapshotCliComponent` と同流儀。cli.rs は
// `protocol` を import しない pure module 方針)。main.rs 側で wire 型へ写像する。

/// `record start --stdin / --stdout / --both` の CLI 表現 (DR-0016 §2)。
///
/// protocol 層の [`crate::protocol::messages::RecordDirection`] と 1:1 対応。
/// CLI default は `Both` (= jsonl format との組合せで stdin/stdout 両方を記録)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecordDirectionArg {
    /// `--stdin` (= 子 PTY 向け input のみ、認可済 write 成功 bytes)。
    Stdin,
    /// `--stdout` (= 子 PTY からの出力のみ、screen 加工前の生 bytes)。
    Stdout,
    /// `--both` (= stdin + stdout 双方、default、jsonl format 限定)。
    #[default]
    Both,
}

/// `record start --format=jsonl|raw` の CLI 表現 (DR-0016 §3, §5)。
///
/// protocol 層の [`crate::protocol::messages::RecordFormat`] と 1:1 対応。
/// `Raw` は単一 direction 限定 (= `Both` との組合せは parse 段で reject)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecordFormatArg {
    /// `--format=jsonl` (= default、header + body 構造化、診断 timeline 用)。
    #[default]
    Jsonl,
    /// `--format=raw` (= 単一 direction の raw bytes、`cat` 互換、stream export 専用)。
    Raw,
}

/// `record start --input-secrecy` policy の CLI 表現 (DR-0016 §6)。
///
/// protocol 層の [`crate::protocol::messages::InputSecrecy`] と 1:1 対応。
/// default は `RedactAfterPrompt` (= password/OTP prompt 後の stdin を自動 redact)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecordInputSecrecyArg {
    /// `redact-after-prompt` (= default、prompt pattern 後の stdin を redact)。
    #[default]
    RedactAfterPrompt,
    /// `record-all` (= opt-in、redaction なし、全 stdin を hex 記録、loud warning 必須)。
    RecordAll,
    /// `never-record-stdin` (= opt-in、全 stdin を `in-redacted` 化、内容捨て)。
    NeverRecordStdin,
}

/// `record list --format=table|jsonl` の CLI 表現 (DR-0016 §2)。
///
/// `Table` は人間可読の固定長 column 1 行 1 record、`Jsonl` は `RecordInfo` を
/// そのまま 1 行 jsonl で出す (= scripting 用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecordListFormatArg {
    /// 固定長 column の人間可読出力 (= default)。
    #[default]
    Table,
    /// `RecordInfo` を 1 record 1 行の JSON Lines で出す (= scripting 用)。
    Jsonl,
}

/// `record start <session>` subcommand configuration (= DR-0016 §2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordStartConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
    /// 録画 direction (= default `Both`、`Raw` format との組合せでは parse 段で reject)。
    pub direction: RecordDirectionArg,
    /// 出力 format (= default `Jsonl`)。
    pub format: RecordFormatArg,
    /// 出力 file path (= **絶対 path 必須**、parse 段で `Path::is_absolute` reject)。
    pub output_path: PathBuf,
    /// `--max-bytes <N>` 解釈後の wire 値 (= `None` で disable / `Some(N)` で
    /// 自動 stop)。CLI default 100 MiB は parse 段で適用済、明示 `0` で `None`。
    /// 明示 `0` (= disable) は main.rs 側で loud warning を出す責務。
    pub max_bytes: Option<u64>,
    /// `--max-duration <DUR>` 解釈後の wire 値 (= `None` で disable / `Some(ms)`
    /// で自動 stop)。CLI default 1h は parse 段で適用済、明示 `0` で `None`。
    /// 明示 `0` は main.rs 側で loud warning を出す責務。
    pub max_duration_ms: Option<u64>,
    /// stdin redaction policy (= default `RedactAfterPrompt`、§6)。
    pub input_secrecy: RecordInputSecrecyArg,
    /// custom prompt detection regex (= `None` で daemon default 適用、§6)。
    pub prompt_pattern: Option<String>,
    /// `--max-bytes 0` で明示 disable された場合 `true` (= main.rs 側で loud warning)。
    pub max_bytes_disabled: bool,
    /// `--max-duration 0` で明示 disable された場合 `true` (= main.rs 側で loud warning)。
    pub max_duration_disabled: bool,
}

/// `record stop <session>` subcommand configuration (= DR-0016 §2)。
///
/// `--id <N>` と `--all` は排他。両省略時は CLI 側で `record list` を query して
/// single active 判定する経路 (= main.rs 側、Phase 1 message に手を入れず単一
/// active の autoselect を CLI 側で完結させるための設計選択)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordStopConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
    /// `--id <N>` で停止対象 record_id を明示。`None` の場合 main.rs 側で
    /// `record list` を先に query して single active のみ自動採用する
    /// (= multiple active なら error、none なら error)。
    pub record_id: Option<u32>,
    /// `--all` (= 同 session の全 active record を一括停止、`record_id` と排他)。
    pub all: bool,
}

/// `record list <session>` subcommand configuration (= DR-0016 §2)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordListConfig {
    /// Target socket path (explicit) または session_id から resolve。
    pub socket: Option<String>,
    /// Target session id。
    pub session_id: Option<String>,
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
    /// `--format=table|jsonl` (= default `Table`)。
    pub format: RecordListFormatArg,
}

/// `record` 親 subcommand の子 dispatch (= DR-0016 §2)。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordCommand {
    /// `record start <session>` — 録画開始、`record_id` を stdout に出す。
    Start(RecordStartConfig),
    /// `record stop <session>` — 特定 record / 全 record を停止。
    Stop(RecordStopConfig),
    /// `record list <session>` — active record 一覧を出す。
    List(RecordListConfig),
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
    /// Change a runtime setting (`set <session> <key>=<value>`、DR-0019 Update)。
    Set(SetConfig),
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
    /// `detach [session]` — この session の全 attach client を引き剥がす (DR-0020 §4)。
    ///
    /// 常に all (= 全 client 切断、daemon / 子は継続)。中から実行
    /// (= `$HYOUI_SESSION_ID`) で自セッションを TUI 直起動から脱出する用途、または
    /// 外から全 client を引き剥がす用途。target 指定は持たない (= [`DetachConfig`])。
    Detach(DetachConfig),
    /// `record` 親 subcommand (= DR-0016 §2、tty I/O timeline 録画)。
    ///
    /// 子: `start` / `stop` / `list`。protocol 層では `record-v1` cap 必須。
    /// 各 subcommand の executor は本 commit (Phase 7) では protocol message を
    /// 構築・送信できる構造のみ、daemon 側 hook 配線は Phase 4 で行う。
    Record(RecordCommand),
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
        "set" => parse_set(rest),
        "tail" => parse_tail(rest),
        "wait" => parse_wait(rest),
        "screen" => parse_screen(rest),
        "input" => parse_input(rest),
        "lock" => parse_lock(rest),
        "unlock" => parse_unlock(rest),
        "record" => parse_record(rest),
        "detach" => parse_detach(rest),
        "completion" => parse_completion(rest),
        // Reserved for future stages.
        //
        // `send` は旧 leaf 設計の名残として予約。
        //
        // `tx` は DR-0006 §7 の自動操作排他 wrapper (= 子 process 起動 + env 注入 +
        // 子 exit で自動 unlock)。`lock` / `unlock` は実装済 (= task #20)、tx 本体は
        // `--process-bound` 等の daemon-side 機能が要るので別 task に切り出し中。
        // 詳細は `docs/issue/2026-05-27-tx-lock-unlock-cli-subcommands.md` 参照。
        "send" | "tx" => Command::Error(format!(
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
        let (name, inline_value) = split_eq(a.as_str());
        match name.as_str() {
            "--prune-stale" => {
                if inline_value.is_some() {
                    return Command::Error("list: --prune-stale does not take a value".to_string());
                }
                cfg.prune_stale = true;
            }
            "--all-namespaces" => {
                if inline_value.is_some() {
                    return Command::Error(
                        "list: --all-namespaces does not take a value".to_string(),
                    );
                }
                cfg.all_namespaces = true;
            }
            "--namespace" => match inline_value.as_deref() {
                Some(v) => {
                    if let Err(e) = validate_namespace(v) {
                        return Command::Error(format!("list: --namespace: {e}"));
                    }
                    cfg.namespace = Some(v.to_string());
                }
                None => {
                    return Command::Error(
                        "list: --namespace requires a value (= `--namespace=<ns>`)".to_string(),
                    );
                }
            },
            "--format" => match inline_value.as_deref() {
                Some("plain") => cfg.format = ListFormat::Plain,
                Some("jsonl") => cfg.format = ListFormat::Jsonl,
                Some(other) => {
                    return Command::Error(format!(
                        "list: --format expects plain|jsonl, got: {other}"
                    ));
                }
                None => {
                    return Command::Error(
                        "list: --format requires a value (= plain | jsonl)".to_string(),
                    );
                }
            },
            other => return Command::Error(format!("list: unexpected argument: {other}")),
        }
    }
    if cfg.namespace.is_some() && cfg.all_namespaces {
        return Command::Error(
            "list: --namespace と --all-namespaces は同時に指定できません".to_string(),
        );
    }
    Command::List(cfg)
}

/// signal 表記 (= 数字 / 略名 / SIG-prefix 大文字) を正規 SIG-prefix 大文字に
/// normalize する。
///
/// 受理する表記:
/// - `"SIGTERM"` / `"SIGKILL"` — 正規表記、そのまま通る
/// - `"sigterm"` / `"sigkill"` — 大文字化して通す
/// - `"TERM"` / `"KILL"` — SIG-prefix を付加
/// - `"term"` / `"kill"` — 大文字化 + SIG-prefix
/// - `"9"` / `"15"` — 数字から OS の Signal variant 経由で名前解決 (= `"SIGKILL"` 等)
///
/// reject する表記:
/// - OS 上で defined されていない signal name / 番号 (= `nix::Signal::parse` / `try_from` で None)
/// - 空文字列
///
/// wire (= protocol message) には正規 SIG-prefix 大文字を流す (DR-0012、defense in
/// depth で daemon 側でも再 validate)。
pub fn normalize_signal_spec(spec: &str) -> Result<String, String> {
    use nix::sys::signal::Signal;
    if spec.is_empty() {
        return Err("signal spec is empty".into());
    }
    // (1) 数字 (= POSIX kill 慣習 `9` / `15`)。OS の Signal variant に解決できるか確認。
    if let Ok(num) = spec.parse::<i32>() {
        return match Signal::try_from(num) {
            Ok(sig) => Ok(sig.as_str().to_string()),
            Err(_) => Err(format!(
                "signal number {num} is not defined on this OS (use --help for valid names)"
            )),
        };
    }
    // (2) 大文字化して SIG-prefix を付加してから parse 試行。
    let upper = spec.to_ascii_uppercase();
    let candidate = if upper.starts_with("SIG") {
        upper
    } else {
        format!("SIG{upper}")
    };
    match candidate.parse::<Signal>() {
        Ok(sig) => Ok(sig.as_str().to_string()),
        Err(_) => Err(format!(
            "unknown signal: {spec:?} (use SIG-prefix uppercase, alias, or POSIX number)"
        )),
    }
}

fn parse_kill(args: &[String]) -> Command {
    let mut cfg = KillConfig::default();
    let mut positionals: Vec<String> = Vec::new();
    // `--` 以降は強制 session-id 扱い (= 数字 session-id を escape、attach と同流儀)
    let mut after_separator = false;
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();

        if after_separator {
            positionals.push(args[i].clone());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_separator = true;
            i += 1;
            continue;
        }

        let (name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        let value: Option<String> = match &inline_value {
            Some(v) => Some(v.clone()),
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
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Command::Error(format!("kill: --namespace: {e}"));
                    }
                    cfg.namespace = Some(v);
                }
                None => return Command::Error("--namespace requires a value".into()),
            },
            // DR-0012: 旧 `--signum N` は完全廃止 (= --signal で数字も受ける)。
            // 数字 / 略名 / SIG-prefix 大文字 全部 normalize 経由で wire 形式に揃える。
            "--signal" => match value.as_deref() {
                Some(v) => match normalize_signal_spec(v) {
                    Ok(name) => cfg.signal = Some(name),
                    Err(e) => return Command::Error(format!("invalid --signal value: {e}")),
                },
                None => return Command::Error("--signal requires a value".into()),
            },
            "--signum" => {
                // 旧形式は v0.2.0 で廃止 (= --signal で数字を受けるため不要)。
                return Command::Error(
                    "--signum is removed in v0.2.0 (DR-0012); use --signal NUM_OR_NAME (e.g. --signal 9 / --signal KILL / --signal SIGKILL)".into(),
                );
            }
            "--index" => match value {
                Some(v) => match v.parse::<i32>() {
                    Ok(0) => {
                        return Command::Error(
                            "kill: --index=0 は不正です (= 1-based、1 が最古、-1 が最新)".into(),
                        );
                    }
                    Ok(n) => cfg.index = Some(n),
                    Err(_) => {
                        return Command::Error(format!(
                            "kill: --index には整数を指定してください (got: {v:?})"
                        ));
                    }
                },
                None => return Command::Error("--index requires a value".into()),
            },
            "--all" => {
                cfg.all = true;
                consumed_extra = false;
            }
            "--no-terminate" => {
                cfg.no_terminate = true;
                consumed_extra = false;
            }
            "--wait" => {
                cfg.wait = true;
                // `--wait=<DUR>` の **inline 値のみ** timeout として解釈する。
                // `--wait demo` の `demo` は session-id 扱いにしたいので、次 arg は
                // 消費しない (= 裸 `--wait` は default timeout に倒す)。
                match inline_value {
                    Some(v) => match parse_duration_ms(&v) {
                        Ok(ms) => cfg.wait_timeout_ms = Some(ms),
                        Err(e) => {
                            return Command::Error(format!("kill: --wait: {e}"));
                        }
                    },
                    None => {
                        cfg.wait_timeout_ms = Some(KILL_WAIT_DEFAULT_TIMEOUT_MS);
                    }
                }
                consumed_extra = false;
            }
            "--kill-on-timeout" => {
                cfg.kill_on_timeout = true;
                consumed_extra = false;
            }
            // POSIX kill 慣習 + 略名拡張: `-X` short flag (= `--` で始まらない short
            // option) は signal spec として解釈する (kawaz 方針 2026-05-30):
            // - `-9`       = SIGKILL (= 番号)
            // - `-KILL`    = SIGKILL (= 略名)
            // - `-SIGKILL` = SIGKILL (= 正規表記)
            // - `-TERM`    = SIGTERM
            //
            // 既存 long option (`--socket` / `--index` / `--all` / `--signal` 等) は
            // 前 arm で match されるためここに来ない。short option は `-h` (`--help`
            // short) のみで、それも前 arm で先に処理。残りの `-X` は全て signal 扱い。
            //
            // 注: attach の `-1` は最新 index 解釈だが、kill 文脈では signal 慣習を
            // 優先する (POSIX kill の慣行)。最新 session を kill したい場合は
            // `--index=-1` を使う (= 位置引数の -1 は SIGHUP として解釈される)。
            other if other.starts_with('-') && !other.starts_with("--") && other.len() > 1 => {
                let sig_spec = &other[1..];
                match normalize_signal_spec(sig_spec) {
                    Ok(name) => {
                        if cfg.signal.is_some() {
                            return Command::Error("kill: signal を複数指定できません".into());
                        }
                        cfg.signal = Some(name);
                    }
                    Err(e) => {
                        return Command::Error(format!("kill: invalid signal {other}: {e}"));
                    }
                }
                consumed_extra = false;
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

    // --all との排他チェック
    if cfg.all
        && (cfg.session_id.is_some()
            || cfg.socket.is_some()
            || cfg.index.is_some()
            || !positionals.is_empty())
    {
        return Command::Error("kill: --all は session-id / --index / --socket と排他です".into());
    }

    // DR-0017: --no-terminate は単一 session 向け (= killall に「畳まない」は無意味)。
    if cfg.all && cfg.no_terminate {
        return Command::Error(
            "kill: --no-terminate は --all と併用できません (= 非 terminate な signal 送信は単一 session 向け)"
                .into(),
        );
    }

    // 2 軸の整理: --wait (= 終了を待つ) は terminate 経路専用。--no-terminate
    // (= terminate しない) と併用すると「畳まない session の終了を待つ」という
    // 矛盾になるため reject。
    if cfg.wait && cfg.no_terminate {
        return Command::Error(
            "kill: --wait は --no-terminate と併用できません (= terminate しない経路に「終了を待つ」は意味を成さない)"
                .into(),
        );
    }

    // --kill-on-timeout は --wait の timeout 超過時の escalation 指定なので、
    // --wait 無しでは意味を成さない (= timeout 概念が無い経路への SIGKILL 昇格指定)。
    if cfg.kill_on_timeout && !cfg.wait {
        return Command::Error(
            "kill: --kill-on-timeout は --wait と併用してください (= --wait の timeout 超過時の SIGKILL 昇格指定)"
                .into(),
        );
    }

    match positionals.len() {
        0 => {
            // DR-0020 §2/§3: env (= 中から実行) なら通す。kill の self default は許容
            // (= `exit` 相当)。値解決 / stale 検証は main.rs。
            if cfg.socket.is_none() && cfg.index.is_none() && !cfg.all && !has_self_session_env() {
                return Command::Error(
                    "kill: session id (positional) / --index=N / --socket=<path> / --all のいずれかが必要です。\
                     例: `hyoui kill <session-id>` / `hyoui kill 1` / `hyoui kill --all` / `hyoui list` で session 一覧を確認できます"
                        .into(),
                );
            }
        }
        1 => {
            // kawaz 方針: 位置引数は数字でも session-id 扱い (= index は --index=N 専用)。
            let pos = positionals.into_iter().next().unwrap();
            // R5-AUD-C2: session_id を validate (= path traversal 早期 reject)
            if let Err(e) = validate_session_id(&pos) {
                return Command::Error(format!("kill: {e}"));
            }
            cfg.session_id = Some(pos);
        }
        _ => return Command::Error("kill: too many positional arguments".into()),
    }

    if cfg.session_id.is_some() && cfg.index.is_some() {
        return Command::Error(
            "kill: session id (位置引数) と --index を同時に指定できません".into(),
        );
    }

    Command::Kill(cfg)
}

/// `$HYOUI_SESSION_ID` が set + 非空か (= 中から実行されているか、DR-0020 §2)。
///
/// parse 段では値の解決 / stale 検証はせず「中から実行か否か」の有無だけ見る
/// (= 値解決と socket liveness 検証は main.rs の resolve 層が担う)。env を読むのは
/// namespace 解決 (`HYOUI_NAMESPACE`) と同枠で、CLI parser が env を参照する既存の
/// 流儀に揃える。
fn has_self_session_env() -> bool {
    // Design rationale: lib ユニットテスト (= `cfg(test)`) では常に false を返す。
    // 多数の parse テストが「session 省略 = required エラー」を期待しており、
    // テスト実行環境 (= hyoui を `hyoui run` 配下で開発する等) に
    // `HYOUI_SESSION_ID` が漏れていると一斉に壊れる。env を read するだけの
    // 本関数を多数の parse テストが間接的に踏むため、個別 test での
    // `remove_var` は並列 read と race する (Rust 2024 で env mutation は unsafe)。
    // env 経路の検証は integration test (= `tests/self_session_resolve.rs`、
    // リリースバイナリを別プロセス起動 = `cfg(test)` でない) が担うので、lib
    // ユニットテストで env を無視しても検証カバレッジは落ちない。
    if cfg!(test) {
        return false;
    }
    matches!(std::env::var("HYOUI_SESSION_ID"), Ok(v) if !v.is_empty())
}

/// shared helper: `--socket` / `--index` / `--help` / positional session_id を抜き出す。
/// 残ったオプションは caller がコールバックで処理する。
///
/// 戻り値: `(socket, session_id, index)`。同時指定の排他チェックも本 helper で行う。
/// `--index=N` は session selector の共通形式 (= mtime 昇順 1-based、`1` 最古 / `-1` 最新)、
/// `0` は不正。session_id が無く index も socket も無い場合はエラー (= 全 selector 不在)。
#[allow(clippy::result_large_err)] // Command 内 String/Vec の Err サイズは parse path のみで許容
#[allow(clippy::type_complexity)] // 3-tuple は session selector の自然な戻り型 (= 別 alias は冗長)
fn parse_session_targeted<F>(
    name: &str,
    args: &[String],
    help_topic: HelpTopic,
    mut on_option: F,
) -> Result<SessionTarget, Command>
where
    F: FnMut(&str, Option<String>) -> Result<bool, Command>,
{
    let mut socket: Option<String> = None;
    let mut index: Option<i32> = None;
    let mut namespace: Option<String> = None;
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
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Err(Command::Error(format!("{name}: --namespace: {e}")));
                    }
                    namespace = Some(v);
                }
                None => {
                    return Err(Command::Error(format!(
                        "{name}: --namespace requires a value"
                    )));
                }
            },
            "--index" => match value {
                Some(v) => match v.parse::<i32>() {
                    Ok(0) => {
                        return Err(Command::Error(format!(
                            "{name}: --index=0 は不正です (= 1-based、1 が最古、-1 が最新)"
                        )));
                    }
                    Ok(n) => index = Some(n),
                    Err(_) => {
                        return Err(Command::Error(format!(
                            "{name}: --index には整数を指定してください (got: {v:?})"
                        )));
                    }
                },
                None => {
                    return Err(Command::Error(format!("{name}: --index requires a value")));
                }
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
            // DR-0020 §2: socket / index / 位置引数いずれも無くても、`$HYOUI_SESSION_ID`
            // が set (= 中から実行) なら通す (= 実行時に self-session へ解決)。env 未 set
            // のときだけ従来の必須エラー。値の解決と stale 検証は main.rs の resolve 層が担う
            // (= parse 段では「中から実行か否か」の有無判定のみ)。
            if socket.is_none() && index.is_none() && !has_self_session_env() {
                return Err(Command::Error(format!(
                    "{name}: session id (positional) / --index=N / --socket=<path> のいずれかが必要です。\
                     例: `hyoui {name} <session-id>` / `hyoui {name} --index=1` / `hyoui list` で session 一覧を確認できます"
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

    // selector 排他チェック: session_id (位置引数) と --index は同時指定不可。
    if session_id.is_some() && index.is_some() {
        return Err(Command::Error(format!(
            "{name}: session id (位置引数) と --index を同時に指定できません"
        )));
    }

    Ok(SessionTarget {
        socket,
        session_id,
        index,
        namespace,
    })
}

/// [`parse_session_targeted`] が返す session 選択情報 (= DR-0018 で namespace 追加)。
///
/// 旧来の `(socket, session_id, index)` tuple を struct 化し、`namespace` を足した。
/// `namespace` は `--namespace=X` flag の生値 (= 未指定なら `None`、実行時に env /
/// default へ fallback される)。
struct SessionTarget {
    socket: Option<String>,
    session_id: Option<String>,
    index: Option<i32>,
    namespace: Option<String>,
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
        Ok(t) => Command::Status(StatusConfig {
            socket: t.socket,
            session_id: t.session_id,
            index: t.index,
            namespace: t.namespace,
            format,
        }),
        Err(c) => c,
    }
}

/// `hyoui set <session> <key>=<value>` を parse する (DR-0019 Update)。
///
/// session 選択は他 CLI と同流儀 (= 位置引数 / `--index` / `--socket`、`--namespace`)。
/// `set` は session 位置引数に加えて `key=value` 位置引数を取るため、共通の
/// [`parse_session_targeted`] (= 位置引数 1 個前提) ではなく専用 loop で parse する。
/// 位置引数のうち `=` を含むものを `key=value`、それ以外を session_id として扱う。
#[allow(clippy::result_large_err)]
fn parse_set(args: &[String]) -> Command {
    let mut socket: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut index: Option<i32> = None;
    let mut namespace: Option<String> = None;
    let mut kv: Option<(String, String)> = None;
    let mut positional_session: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (opt_name, inline_value) = split_eq(arg);
        let mut consumed_extra = false;
        // option 用の値取り (= `--opt value` も許容、ただし `--` 始まりは値にしない)。
        let value: Option<String> = match &inline_value {
            Some(v) => Some(v.clone()),
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
                return Command::Help {
                    topic: HelpTopic::Set,
                };
            }
            "--socket" => match value {
                Some(v) => socket = Some(v),
                None => return Command::Error("set: --socket requires a value".into()),
            },
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Command::Error(format!("set: --namespace: {e}"));
                    }
                    namespace = Some(v);
                }
                None => return Command::Error("set: --namespace requires a value".into()),
            },
            "--index" => match value {
                Some(v) => match v.parse::<i32>() {
                    Ok(0) => {
                        return Command::Error(
                            "set: --index=0 は不正です (= 1-based、1 が最古、-1 が最新)".into(),
                        );
                    }
                    Ok(n) => index = Some(n),
                    Err(_) => {
                        return Command::Error(format!(
                            "set: --index には整数を指定してください (got: {v:?})"
                        ));
                    }
                },
                None => return Command::Error("set: --index requires a value".into()),
            },
            other if other.starts_with('-') => {
                return Command::Error(format!("set: unknown option: {other}"));
            }
            _ => {
                // 位置引数。`=` を含めば key=value、それ以外は session_id。
                consumed_extra = false;
                let raw = args[i].clone();
                if let Some(eq) = raw.find('=') {
                    if kv.is_some() {
                        return Command::Error(
                            "set: key=value を複数指定できません (= 1 回につき 1 設定)".into(),
                        );
                    }
                    let key = raw[..eq].to_string();
                    let val = raw[eq + 1..].to_string();
                    if key.is_empty() {
                        return Command::Error(format!("set: 空の key は不正です: {raw:?}"));
                    }
                    kv = Some((key, val));
                } else {
                    if positional_session.is_some() {
                        return Command::Error("set: too many positional arguments".into());
                    }
                    positional_session = Some(raw);
                }
            }
        }
        i += 1;
        if consumed_extra {
            i += 1;
        }
    }

    if let Some(sid) = positional_session {
        if let Err(e) = validate_session_id(&sid) {
            return Command::Error(format!("set: {e}"));
        }
        session_id = Some(sid);
    }

    if session_id.is_some() && index.is_some() {
        return Command::Error(
            "set: session id (位置引数) と --index を同時に指定できません".into(),
        );
    }
    // DR-0020 §2: env (= 中から実行) なら通す (= 「中から `hyoui set` で宣言」が主要
    // ユースケース)。値解決 / stale 検証は main.rs。
    if session_id.is_none() && index.is_none() && socket.is_none() && !has_self_session_env() {
        return Command::Error(
            "set: session id (positional) / --index=N / --socket=<path> のいずれかが必要です。\
             例: `hyoui set <session-id> on-child-suspend=auto-resume`"
                .into(),
        );
    }

    let Some((key, value)) = kv else {
        return Command::Error(
            "set: <key>=<value> が必要です (= 例: on-child-suspend=auto-resume)".into(),
        );
    };

    Command::Set(SetConfig {
        socket,
        session_id,
        index,
        namespace,
        key,
        value,
    })
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
        Ok(t) => {
            if since_strict && since_ms.is_none() {
                return Command::Error("tail: --since-strict requires --since=<DUR>".into());
            }
            Command::Tail(TailConfig {
                socket: t.socket,
                session_id: t.session_id,
                index: t.index,
                namespace: t.namespace,
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
    let mut index: Option<i32> = None;
    let mut namespace: Option<String> = None;
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
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Command::Error(format!("wait: --namespace: {e}"));
                    }
                    namespace = Some(v);
                }
                None => return Command::Error("wait: --namespace requires a value".into()),
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
            "--index" => match value {
                Some(v) => match v.parse::<i32>() {
                    Ok(0) => {
                        return Command::Error(
                            "wait: --index=0 は不正です (= 1-based、1 が最古、-1 が最新)".into(),
                        );
                    }
                    Ok(n) => index = Some(n),
                    Err(_) => {
                        return Command::Error(format!(
                            "wait: --index には整数を指定してください (got: {v:?})"
                        ));
                    }
                },
                None => return Command::Error("wait: --index requires a value".into()),
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
    // positionals: socket / index あり (= 明示 selector) → pattern 1 個だけ、
    // それ以外 (= session_id 位置引数) → 2 個 (= session_id, pattern)。
    //
    // DR-0020 §2 + Fable review C1 (2026-06-12): `$HYOUI_SESSION_ID` (= 中から実行)
    // は **positional に明示 session id が無いときだけ** self に効く (= 明示 > env)。
    // 旧実装は env set で selector 確定扱いにしたため、positional 2 個
    // (= `hyoui wait beta 'x'`) が「余分な positional」エラーになり、中から
    // 別 session への明示 wait が壊れていた。env なし時の挙動は従来と不変。
    let explicit_selector = socket.is_some() || index.is_some();
    let (session_id, pattern) = match (explicit_selector, positionals.len()) {
        (true, 0) => {
            return Command::Error(
                "wait: pattern が必要です。例: `hyoui wait --socket=<path> 'Continue\\?'` / \
                 `hyoui wait --index=1 'Continue\\?'`"
                    .into(),
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
                "wait: session id (位置引数) / --index=N / --socket=<path> のいずれかと pattern が必要です。\
                 例: `hyoui wait <session-id> 'Continue\\?' --timeout=5s` / \
                 `hyoui list` で session 一覧を確認できます"
                    .into(),
            );
        }
        (false, 1) => {
            if has_self_session_env() {
                // 中から (= env set) + positional 1 個 → pattern のみ、session は
                // self 解決 (= main.rs の resolve 層)。
                (None, positionals.pop().expect("non-empty"))
            } else {
                // session_id だけある状態 → pattern が無い
                return Command::Error(
                    "wait: pattern が必要です。例: `hyoui wait <session-id> 'Continue\\?'`".into(),
                );
            }
        }
        (false, 2) => {
            // 明示 session id + pattern (= env の有無に関わらず明示が優先)。
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
    if session_id.is_some() && index.is_some() {
        return Command::Error(
            "wait: session id (位置引数) と --index を同時に指定できません".into(),
        );
    }
    Command::Wait(WaitConfig {
        socket,
        session_id,
        index,
        namespace,
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
        index: None,
        namespace: None,
        stdin_eof: None,
        quiet: false,
    };

    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (name, inline_value) = split_eq(arg);
        // bool flag (= `--exclusive` / `--detach-others`) の inline value 検出用に
        // move 前に保存する (= `--exclusive=x` のような不正形を弾く)。
        let had_inline = inline_value.is_some();
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
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Command::Error(format!("attach: --namespace: {e}"));
                    }
                    cfg.namespace = Some(v);
                }
                None => return Command::Error("--namespace requires a value".into()),
            },
            "--mode" => match value {
                Some(v) => cfg.mode_str = Some(v),
                None => return Command::Error("--mode requires a value".into()),
            },
            // DR-0020 §4: `--exclusive` (= 他 rw client が居れば attach 拒否) /
            // `--detach-others` (= attach 成立時に他 client を奪取)。daemon 側
            // handshake 統合経路に実装済 (= accept.rs)。bool flag なので next token は
            // consume しない (= consumed_extra=false)、inline value は不正。
            "--exclusive" => {
                if had_inline {
                    return Command::Error("attach: --exclusive does not take a value".into());
                }
                cfg.exclusive = true;
                consumed_extra = false;
            }
            "--detach-others" => {
                if had_inline {
                    return Command::Error("attach: --detach-others does not take a value".into());
                }
                cfg.detach_others = true;
                consumed_extra = false;
            }
            // DR-0020 §5: attach 成立時の stderr ヒント (= detach/peek 案内) を抑止。
            "--quiet" => {
                if had_inline {
                    return Command::Error("attach: --quiet does not take a value".into());
                }
                cfg.quiet = true;
                consumed_extra = false;
            }
            "--debug-dump-client" => match value {
                Some(v) if !v.is_empty() => cfg.debug_dump_client = Some(v),
                Some(_) => return Command::Error("--debug-dump-client: path が空です".into()),
                None => {
                    return Command::Error("--debug-dump-client requires a value".into());
                }
            },
            "--stdin-eof" => match value.as_deref() {
                Some("detach") => cfg.stdin_eof = Some(StdinEofArg::Detach),
                Some("send-eof") => cfg.stdin_eof = Some(StdinEofArg::SendEof),
                Some(other) => {
                    return Command::Error(format!(
                        "attach: invalid --stdin-eof value: {other} (= detach | send-eof)"
                    ));
                }
                None => return Command::Error("--stdin-eof requires a value".into()),
            },
            "--index" => match value {
                Some(v) => match v.parse::<i32>() {
                    Ok(0) => {
                        return Command::Error(
                            "attach: --index=0 は不正です (= 1-based index、1 が最古、-1 が最新)"
                                .into(),
                        );
                    }
                    Ok(n) => cfg.index = Some(n),
                    Err(_) => {
                        return Command::Error(format!(
                            "attach: --index には整数を指定してください (got: {v:?})"
                        ));
                    }
                },
                None => return Command::Error("--index requires a value".into()),
            },
            other if other.starts_with('-') => {
                return Command::Error(format!("unknown attach option: {other}"));
            }
            _ => {
                // positional (= session id)。数字も session-id 扱い (kawaz 方針:
                // index 指定は --index=N option 専用、位置引数は名前として保持)。
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
            if cfg.socket.is_none() && cfg.index.is_none() {
                return Command::Error(
                    "attach: session id (positional) または --socket=<path> / --index=N が必要です。\
                     例: `hyoui attach <session-id>` / `hyoui attach --index=1` (最古) / \
                     `hyoui attach --index=-1` (最新) / `hyoui list` で session 一覧を確認できます"
                        .into(),
                );
            }
        }
        1 => cfg.session_id = Some(positionals.into_iter().next().unwrap()),
        _ => return Command::Error("attach: too many positional arguments".into()),
    }

    if cfg.session_id.is_some() && cfg.index.is_some() {
        return Command::Error(
            "attach: session id (位置引数) と --index を同時に指定できません".into(),
        );
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
        HelpTopic::Set => usage_set(),
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
        HelpTopic::Detach => usage_detach(),
        HelpTopic::Record => usage_record(),
        HelpTopic::RecordStart => usage_record_start(),
        HelpTopic::RecordStop => usage_record_stop(),
        HelpTopic::RecordList => usage_record_list(),
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

    let mut explicit_cols: Option<i32> = None;
    let mut explicit_rows: Option<i32> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut idle_timeout_ms: Option<u64> = None;
    let mut until: Option<String> = None;
    let mut socket: Option<String> = None;
    let mut on_child_suspend: Option<OnChildSuspend> = None;
    let mut stdin_eof: Option<StdinEofArg> = None;
    let mut command: Vec<String> = Vec::new();
    let mut detached = false;
    let mut session: Option<String> = None;
    let mut namespace: Option<String> = None;
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
            "--stdin-eof" => match value.as_deref() {
                Some("detach") => stdin_eof = Some(StdinEofArg::Detach),
                Some("send-eof") => stdin_eof = Some(StdinEofArg::SendEof),
                Some(other) => {
                    return Command::Error(format!(
                        "invalid --stdin-eof value: {other} (= detach | send-eof)"
                    ));
                }
                None => return Command::Error("--stdin-eof requires a value".into()),
            },
            "--on-child-suspend" => match value.as_deref() {
                Some("notify") => on_child_suspend = Some(OnChildSuspend::Notify),
                Some("auto-resume") => on_child_suspend = Some(OnChildSuspend::AutoResume),
                // DR-0019: 旧値 `follow` は `notify` に rename。移行先を明示する。
                Some("follow") => {
                    return Command::Error(
                        "--on-child-suspend=follow is removed (DR-0019); use `notify` \
                         (= leader client に通知するだけ、子は起こさない。default)"
                            .into(),
                    );
                }
                Some(other) => {
                    return Command::Error(format!(
                        "invalid --on-child-suspend value: {other} (= notify | auto-resume)"
                    ));
                }
                None => return Command::Error("--on-child-suspend requires a value".into()),
            },
            // DR-0019 §1: `--mode=interactive|headless` preset 削除。直交フラグへ誘導。
            "--mode" => {
                return Command::Error(
                    "run --mode is removed (DR-0019); the preset had no effect. \
                     起動の各軸を直交フラグで指定してください: attach しない起動は \
                     `--detached`、画面サイズは `--size`/`--cols`/`--rows`、\
                     suspend policy は `--on-child-suspend=notify|auto-resume`"
                        .into(),
                );
            }
            // DR-0015 §2.3 / DR-0019 §7: `--on-parent-suspend` 廃止 (= 軸 2 廃止)。
            // unknown option に落とさず移行先を明示する (= migration hint)。
            "--on-parent-suspend" => {
                return Command::Error(
                    "run --on-parent-suspend is removed (DR-0015 §2.3, 軸 2 廃止)。\
                     親 (= attach client) の suspend は client ローカル挙動として\
                     固定 (= follow)、daemon 側に policy は無い"
                        .into(),
                );
            }
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
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Command::Error(format!("--namespace: {e}"));
                    }
                    namespace = Some(v);
                }
                None => return Command::Error("--namespace requires a value".into()),
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

    // DR-0017 §柱2: default は **notify のみ** (= 子の suspend を勝手に起こさない)。
    // `AutoResume` は opt-in (`--on-child-suspend=auto-resume`) でのみ選べる。
    let final_child_suspend = on_child_suspend.unwrap_or(OnChildSuspend::Notify);

    // Virtual size: explicit 指定のみ Some、未指定なら None で caller (= run_command)
    // が外側 TTY size を継承する経路に流す (= ユーザ指示 2026-05-29)。
    Command::Run(RunConfig {
        cols: explicit_cols,
        rows: explicit_rows,
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
        namespace,
        stdin_eof,
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
        Ok(t) => Command::Screen(ScreenCommand::Dump(ScreenDumpConfig {
            socket: t.socket,
            session_id: t.session_id,
            index: t.index,
            namespace: t.namespace,
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
        Ok(t) => {
            let include = include.unwrap_or_else(default_snapshot_include);
            Command::Screen(ScreenCommand::Snapshot(ScreenSnapshotConfig {
                socket: t.socket,
                session_id: t.session_id,
                index: t.index,
                namespace: t.namespace,
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
        // 正規化済名 → enum は `snapshot_component_from_normalized` が正本
        // (= SSOT 定数 SNAPSHOT_INCLUDE_VALUES と同じ対応表を共有)。
        let comp = match snapshot_component_from_normalized(normalized.as_str()) {
            Some(c) => c,
            None => {
                return Err(format!(
                    "unknown component {normalized:?} (valid: Cells / Cursor / Mode / Style / Scrollback / WindowSize / Buffer / SequenceNo)"
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
        Ok(t) => Command::Lock(LockCommand::Acquire(LockAcquireConfig {
            socket: t.socket,
            session_id: t.session_id,
            index: t.index,
            namespace: t.namespace,
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
        Ok(t) => Command::Lock(LockCommand::Release(LockReleaseConfig {
            socket: t.socket,
            session_id: t.session_id,
            index: t.index,
            namespace: t.namespace,
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
        Ok(t) => Command::Unlock(LockReleaseConfig {
            socket: t.socket,
            session_id: t.session_id,
            index: t.index,
            namespace: t.namespace,
            token,
        }),
        Err(c) => c,
    }
}

/// `detach [session]` parser (= DR-0020 §4)。常に all (= 全 attach client を引き剥がす)。
///
/// session 省略時は `$HYOUI_SESSION_ID` (= 中から実行) で自セッションに解決される
/// (= `parse_session_targeted` 内の `has_self_session_env` で許容)。`--target` flag は
/// 持たない (= [`DetachConfig`] の doc を参照、Fable review M1 2026-06-12)。
#[allow(clippy::result_large_err)]
fn parse_detach(args: &[String]) -> Command {
    let res = parse_session_targeted("detach", args, HelpTopic::Detach, |opt, _value| {
        Err(Command::Error(format!("detach: unknown option: {opt}")))
    });
    match res {
        Ok(t) => Command::Detach(DetachConfig {
            socket: t.socket,
            session_id: t.session_id,
            index: t.index,
            namespace: t.namespace,
        }),
        Err(c) => c,
    }
}

/// `record` 親 subcommand の dispatcher (= DR-0016 §2)。
///
/// 引数なし / `--help` は親 help を出す (= `parse_screen` / `parse_lock` と同流儀)。
/// 最初の positional を子 subcommand 名として扱い、`start` / `stop` / `list` 以外は
/// suggest closest 付き error にする。
fn parse_record(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Help {
            topic: HelpTopic::Record,
        };
    }
    let head = args[0].as_str();
    match head {
        "--help" | "-h" => Command::Help {
            topic: HelpTopic::Record,
        },
        "start" => parse_record_start(&args[1..]),
        "stop" => parse_record_stop(&args[1..]),
        "list" => parse_record_list(&args[1..]),
        other if other.starts_with('-') => {
            Command::Error(format!("record: unknown option: {other}"))
        }
        other => {
            let base =
                format!("record: unknown subcommand `{other}` (supported: start, stop, list)");
            match suggest_closest(other, ["start", "stop", "list"]) {
                Some(s) => Command::Error(format!("{base} (did you mean `record {s}`?)")),
                None => Command::Error(base),
            }
        }
    }
}

/// `--max-bytes <N>` の suffix 付き bytes 値を parse する。
///
/// 受理: `1024` (= bytes)、`1k` / `1K` / `1kb` / `1KiB` (= 1024 bytes)、
/// `1m` / `1MB` / `1MiB` (= 1024² bytes)、`1g` / `1GB` / `1GiB` (= 1024³ bytes)。
///
/// `1MB` と `1MiB` を **同じ意味** (= 1024²) で扱う既存 input 系の慣行
/// (`DEFAULT_INPUT_MAX_FILE_BYTES` 周辺) に合わせる。decimal (= 1.5M) は受理しない
/// (= byte 単位は整数で十分、混乱回避)。
///
/// 戻り値 `Ok(0)` は呼び出し側で「明示 disable」として扱う (= main.rs 側で
/// loud warning + protocol 上は `None` を送る)。
fn parse_max_bytes(s: &str) -> Result<u64, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty value".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    // 末尾の単位を切り出す (= 数字部分との分離)。
    let (num_str, mult): (&str, u64) = if let Some(stripped) = lower.strip_suffix("gib") {
        (stripped, 1024 * 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix("mib") {
        (stripped, 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix("kib") {
        (stripped, 1024)
    } else if let Some(stripped) = lower.strip_suffix("gb") {
        (stripped, 1024 * 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix("mb") {
        (stripped, 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix("kb") {
        (stripped, 1024)
    } else if let Some(stripped) = lower.strip_suffix('g') {
        (stripped, 1024 * 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix('m') {
        (stripped, 1024 * 1024)
    } else if let Some(stripped) = lower.strip_suffix('k') {
        (stripped, 1024)
    } else if let Some(stripped) = lower.strip_suffix('b') {
        (stripped, 1)
    } else {
        (lower.as_str(), 1)
    };
    let num: u64 = num_str
        .trim()
        .parse()
        .map_err(|_| format!("invalid byte count: {s:?} (use 100 / 1k / 100m / 1g 等)"))?;
    num.checked_mul(mult)
        .ok_or_else(|| format!("byte count overflows u64: {s:?}"))
}

/// `record start <session>` parser (= DR-0016 §2)。
///
/// 受理する options:
/// - `--socket=<path>` / `--index=<N>` / 位置 session id — 共通 selector
/// - `--output <path>` — 出力 file path、**絶対 path 必須**
/// - `--stdin` / `--stdout` / `--both` — 録画 direction、互いに排他、default `--both`
/// - `--format=jsonl|raw` — file format、default `jsonl`。`raw` + (`--both` or default)
///   は parse 段 error (= raw は単一 direction 限定)
/// - `--max-bytes <N>` — 録画 bytes 上限 (default 100 MiB、`0` で disable + loud warning)
/// - `--max-duration <DUR>` — 録画 duration 上限 (default 1h、`0` で disable + loud warning)
/// - `--input-secrecy <POLICY>` — stdin redaction policy (default `redact-after-prompt`)
/// - `--prompt-pattern <regex>` — custom prompt 検出 regex (default は daemon が適用)
#[allow(clippy::result_large_err)]
fn parse_record_start(args: &[String]) -> Command {
    /// `--max-bytes` default = 100 MiB (= DR-0016 §2)。
    const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;
    /// `--max-duration` default = 1 hour (= DR-0016 §2)。
    const DEFAULT_MAX_DURATION_MS: u64 = 60 * 60 * 1000;

    let mut output: Option<PathBuf> = None;
    // direction は 3 flag のうち最後に来たもの優先 + 「複数同時指定は error」検出のため
    // Option で受け、複数立ったら error にする。
    let mut direction_flag: Option<RecordDirectionArg> = None;
    let mut direction_seen_count = 0usize;
    let mut format = RecordFormatArg::default();
    let mut max_bytes_raw: Option<u64> = None; // None = 未指定 / Some(N) = 指定値
    let mut max_duration_raw: Option<u64> = None;
    let mut input_secrecy = RecordInputSecrecyArg::default();
    let mut prompt_pattern: Option<String> = None;

    let res = parse_session_targeted(
        "record start",
        args,
        HelpTopic::RecordStart,
        |opt, value| match opt {
            "--output" => {
                let v = value.ok_or_else(|| {
                    Command::Error("record start: --output requires a value".into())
                })?;
                if v.is_empty() {
                    return Err(Command::Error(
                        "record start: --output requires a non-empty path".into(),
                    ));
                }
                let p = PathBuf::from(&v);
                if !p.is_absolute() {
                    return Err(Command::Error(format!(
                        "record start: --output must be an absolute path (got {v:?}; \
                         daemon と client の cwd が一致しない可能性があるため絶対 path 必須)"
                    )));
                }
                output = Some(p);
                Ok(true)
            }
            "--stdin" => {
                direction_seen_count += 1;
                direction_flag = Some(RecordDirectionArg::Stdin);
                Ok(false)
            }
            "--stdout" => {
                direction_seen_count += 1;
                direction_flag = Some(RecordDirectionArg::Stdout);
                Ok(false)
            }
            "--both" => {
                direction_seen_count += 1;
                direction_flag = Some(RecordDirectionArg::Both);
                Ok(false)
            }
            "--format" => {
                let v = value.ok_or_else(|| {
                    Command::Error("record start: --format requires a value".into())
                })?;
                match v.as_str() {
                    "jsonl" => format = RecordFormatArg::Jsonl,
                    "raw" => format = RecordFormatArg::Raw,
                    other => {
                        return Err(Command::Error(format!(
                            "record start: --format must be `jsonl` or `raw`, got {other:?}"
                        )));
                    }
                }
                Ok(true)
            }
            "--max-bytes" => {
                let v = value.ok_or_else(|| {
                    Command::Error("record start: --max-bytes requires a value".into())
                })?;
                let n = parse_max_bytes(&v)
                    .map_err(|e| Command::Error(format!("record start: --max-bytes: {e}")))?;
                max_bytes_raw = Some(n);
                Ok(true)
            }
            "--max-duration" => {
                let v = value.ok_or_else(|| {
                    Command::Error("record start: --max-duration requires a value".into())
                })?;
                // `0` 単体は parse_duration_ms が `Ok(0)` を返す前提 (bare 数字は
                // error だが `0ms` / `0s` 等は通る)。CLI として「disable 用の `0`」
                // は単位省略でも受理したいので、純粋な `0` を pre-check で許容する。
                let n = if v.trim() == "0" {
                    0
                } else {
                    parse_duration_ms(&v)
                        .map_err(|e| Command::Error(format!("record start: --max-duration: {e}")))?
                };
                max_duration_raw = Some(n);
                Ok(true)
            }
            "--input-secrecy" => {
                let v = value.ok_or_else(|| {
                    Command::Error("record start: --input-secrecy requires a value".into())
                })?;
                match v.as_str() {
                    "redact-after-prompt" => {
                        input_secrecy = RecordInputSecrecyArg::RedactAfterPrompt
                    }
                    "record-all" => input_secrecy = RecordInputSecrecyArg::RecordAll,
                    "never-record-stdin" => input_secrecy = RecordInputSecrecyArg::NeverRecordStdin,
                    other => {
                        return Err(Command::Error(format!(
                            "record start: --input-secrecy must be one of \
                             `redact-after-prompt` / `record-all` / `never-record-stdin`, got {other:?}"
                        )));
                    }
                }
                Ok(true)
            }
            "--prompt-pattern" => {
                let v = value.ok_or_else(|| {
                    Command::Error("record start: --prompt-pattern requires a value".into())
                })?;
                if v.is_empty() {
                    return Err(Command::Error(
                        "record start: --prompt-pattern requires a non-empty regex".into(),
                    ));
                }
                prompt_pattern = Some(v);
                Ok(true)
            }
            other => Err(Command::Error(format!(
                "record start: unknown option: {other}"
            ))),
        },
    );
    let t = match res {
        Ok(t) => t,
        Err(c) => return c,
    };

    // --output 必須
    let output_path = match output {
        Some(p) => p,
        None => {
            return Command::Error("record start: --output <path> が必要です (= 絶対 path)".into());
        }
    };

    // direction の排他チェック (= 複数 flag 同時指定は error)
    if direction_seen_count > 1 {
        return Command::Error(
            "record start: --stdin / --stdout / --both は同時に指定できません".into(),
        );
    }
    let direction = direction_flag.unwrap_or_default();

    // raw format は単一 direction 限定 (= --both は invalid)
    if matches!(format, RecordFormatArg::Raw) && matches!(direction, RecordDirectionArg::Both) {
        return Command::Error(
            "record start: --format=raw requires --stdin or --stdout (--both は jsonl 限定)".into(),
        );
    }

    // max_bytes / max_duration: 指定なし → default、明示 `0` → None (disable + warn)、
    // 指定値 → Some(N)。disable した事実は config の flag に残し、main.rs 側で
    // loud warning を出す責務を負う。
    let (max_bytes, max_bytes_disabled) = match max_bytes_raw {
        None => (Some(DEFAULT_MAX_BYTES), false),
        Some(0) => (None, true),
        Some(n) => (Some(n), false),
    };
    let (max_duration_ms, max_duration_disabled) = match max_duration_raw {
        None => (Some(DEFAULT_MAX_DURATION_MS), false),
        Some(0) => (None, true),
        Some(n) => (Some(n), false),
    };

    Command::Record(RecordCommand::Start(RecordStartConfig {
        socket: t.socket,
        session_id: t.session_id,
        index: t.index,
        namespace: t.namespace,
        direction,
        format,
        output_path,
        max_bytes,
        max_duration_ms,
        input_secrecy,
        prompt_pattern,
        max_bytes_disabled,
        max_duration_disabled,
    }))
}

/// `record stop <session>` parser (= DR-0016 §2)。
///
/// `--id <N>` と `--all` は排他。両省略は CLI 側で `record list` を query する
/// auto-select 経路 (= main.rs 側で single active のみ自動採用、複数 active なら error)。
#[allow(clippy::result_large_err)]
fn parse_record_stop(args: &[String]) -> Command {
    let mut record_id: Option<u32> = None;
    let mut all = false;

    let res =
        parse_session_targeted(
            "record stop",
            args,
            HelpTopic::RecordStop,
            |opt, value| match opt {
                "--id" => {
                    let v = value.ok_or_else(|| {
                        Command::Error("record stop: --id requires a value".into())
                    })?;
                    let n: u32 = v.parse().map_err(|_| {
                        Command::Error(format!(
                            "record stop: --id must be a non-negative integer (got {v:?})"
                        ))
                    })?;
                    record_id = Some(n);
                    Ok(true)
                }
                "--all" => {
                    all = true;
                    Ok(false)
                }
                other => Err(Command::Error(format!(
                    "record stop: unknown option: {other}"
                ))),
            },
        );
    let t = match res {
        Ok(t) => t,
        Err(c) => return c,
    };

    if record_id.is_some() && all {
        return Command::Error("record stop: --id と --all は同時に指定できません".into());
    }

    Command::Record(RecordCommand::Stop(RecordStopConfig {
        socket: t.socket,
        session_id: t.session_id,
        index: t.index,
        namespace: t.namespace,
        record_id,
        all,
    }))
}

/// `record list <session>` parser (= DR-0016 §2)。
#[allow(clippy::result_large_err)]
fn parse_record_list(args: &[String]) -> Command {
    let mut format = RecordListFormatArg::default();
    let res =
        parse_session_targeted(
            "record list",
            args,
            HelpTopic::RecordList,
            |opt, value| match opt {
                "--format" => {
                    let v = value.ok_or_else(|| {
                        Command::Error("record list: --format requires a value".into())
                    })?;
                    match v.as_str() {
                        "table" => format = RecordListFormatArg::Table,
                        "jsonl" => format = RecordListFormatArg::Jsonl,
                        other => {
                            return Err(Command::Error(format!(
                                "record list: --format must be `table` or `jsonl`, got {other:?}"
                            )));
                        }
                    }
                    Ok(true)
                }
                other => Err(Command::Error(format!(
                    "record list: unknown option: {other}"
                ))),
            },
        );
    let t = match res {
        Ok(t) => t,
        Err(c) => return c,
    };

    Command::Record(RecordCommand::List(RecordListConfig {
        socket: t.socket,
        session_id: t.session_id,
        index: t.index,
        namespace: t.namespace,
        format,
    }))
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
            set         Change a runtime setting (set <session> <key>=<value>)\n    \
            tail        Stream scrollback / live output (--follow で継続)\n    \
            wait        Wait until predicate (text/pattern/idle) matches\n    \
            screen      Dump / inspect virtual screen state (subcommands: dump)\n    \
            input       Send input via spec list (DR-0006 §8; text:/key:/wait: ...)\n    \
            lock        Acquire / release a session lock (DR-0006 §7)\n    \
            unlock      Release a session lock (= `lock release` alias)\n    \
            detach      Detach all attached clients from a session (DR-0020)\n    \
            record      Record tty I/O timeline (DR-0016; start/stop/list)\n    \
            completion  Print a shell completion script (bash|zsh|fish)\n\
        \n\
        RESERVED (not yet implemented):\n    \
            send, tx   将来 protocol 拡張用に予約\n\
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
            --size COLSxROWS              Virtual screen size, e.g. 80x24\n    \
            --cols N                      Virtual screen columns\n    \
            --rows M                      Virtual screen rows\n    \
            --timeout DUR                 Overall timeout (DUR フォーマットは下記参照)\n    \
            --idle-timeout DUR            Output idle timeout (= 子 PTY 出力が止まったら exit)\n    \
            --until PATTERN               Terminate when PATTERN appears in output\n    \
            --socket PATH                 Unix socket path for input injection\n    \
            --namespace NS                Session namespace (default: \"default\";\n                                  \
                env HYOUI_NAMESPACE で継承可、子に常時注入される)\n    \
            --on-child-suspend=notify|auto-resume\n                                  \
                Action when the child is stopped\n                                  \
                (notify: tell the leader client; auto-resume: daemon\n                                  \
                sends SIGCONT to resume the child. default: notify)\n    \
            --scrollback-rows N           vt100 内蔵 scrollback ring 行数上限\n                                  \
                (= screen dump --layer=scrollback / --layer=both で\n                                  \
                取り出せる過去 row の最大数、default 1000、0 で無効)\n    \
            --debug-dump-server PATH      子 PTY → daemon の raw bytes を file に append\n                                  \
                (state 翻訳前の bytes、ANSI escape 込み)\n    \
            --debug-dump-client PATH      daemon → client の raw bytes を file に append\n                                  \
                (state-based redraw / attach 復元込み = user の terminal 表示)\n    \
            --stdin-eof=detach|send-eof\n                                  \
                stdin EOF 時の挙動 (DR-0019)。default: 非 tty stdin なら\n                                  \
                send-eof (= EOT を子に送り `echo ... | hyoui run -- bc` で\n                                  \
                子が自然 exit)、tty なら detach。detach は EOF で切断のみ\n    \
            -h, --help                    Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            SHELL                  Fallback command when none is given (legacy)\n    \
            XDG_RUNTIME_DIR        Base directory for the auto-generated socket path\n                                   \
                (otherwise /tmp/hyoui-<uid> is used; TMPDIR is not consulted)\n    \
            HYOUI_NAMESPACE        Session namespace (= --namespace の env 経路、flag 優先)\n    \
            HYOUI_SCROLLBACK_ROWS  --scrollback-rows と同じ値を env で渡す\n                                   \
                (--scrollback-rows 指定時は flag 優先)\n    \
            HYOUI_SESSION_ID       (子へ注入) daemon が子プロセスへ常時 export する\n                                   \
                自セッション id。中から `hyoui status` 等を session 省略で\n                                   \
                叩くと自セッションに解決される (DR-0020)\n\
        \n\
        DURATION FORMAT (kawaz/timespec.mbt 仕様 + sub-ms 拡張):\n    \
            短形 ns/us/μs/ms/s/m/h/d/w または長形 second(s)/minute(s)/hour(s)/\n    \
            day(s)/week(s)。decimal (1.5h)、underscore (1_000ms)、連結 (1h30m)、\n    \
            加減 (1d-4h)。sub-ms (ns/us/μs) は accept、内部 ns 集積 → ms に floor\n    \
            (例: 500us 600us = 1.1ms → 1ms)。bare 数字 / 年 (y) / 月 (M) は **error**。\n    \
            case-insensitive。\n\
        \n\
        EXIT STATUS:\n    \
            run は daemon を fork した後 exec で attach に化けるため、attach と同じ\n    \
            exit code が適用される。\n    \
            <子の exit code>      子 PTY が exit した (= SessionExitNotify)。\n                          \
                signal 死は 128+signum (= 130=SIGINT, 137=SIGKILL, 143=SIGTERM)。\n                          \
                非 tty stdin の default (= send-eof) では stdin EOF で子が\n                          \
                自然 exit し、その子の code がここに伝搬する\n    \
            0                     detach key、または `--stdin-eof=detach` 時の stdin EOF で\n                          \
                自分から離脱した (= 子は daemon 配下に残る)\n    \
            9                     daemon との接続が予期せず失われた (= daemon 消滅の疑い)\n    \
            1                     実行エラー (= protocol violation / 出力先への書き込み失敗 等)\n    \
            2                     usage / 引数エラー\n",
    )
}

fn usage_attach() -> String {
    String::from(
        "hyoui attach — attach to an existing daemon session\n\
        \n\
        USAGE:\n    \
            hyoui attach <session-id> [options]\n    \
            hyoui attach --index=<N> [options]\n    \
            hyoui attach --socket=<path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH         Explicit socket path (alternative to session-id)\n    \
            --index N             session を mtime 昇順の index で指定 (= 1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            --mode rw|ro|rw-no-leader\n                          \
                Operating mode (default: rw)\n    \
            --exclusive           他に rw client が attach 中なら attach を拒否 (DR-0020 §4)\n    \
            --detach-others       attach 成立時に他 client を全て detach して奪取 (DR-0020 §4)\n    \
            --quiet               attach 成立時の detach/peek ヒント (stderr) を抑止 (DR-0020 §5)\n    \
            --stdin-eof=detach|send-eof\n                          \
                stdin EOF 時の挙動 (DR-0019)。default: 非 tty stdin なら\n                          \
                send-eof (= EOT を子に送る)、tty なら detach\n    \
            -h, --help            Show this help and exit\n\
        \n\
        SESSION SELECTOR:\n    \
            位置引数 (= session-id 名) または `--index=N` で指定する。位置引数の\n    \
            数字 (= `attach 1`) は session-id 名として扱う (= 数字も valid な\n    \
            session-id)。`--index=N` は `hyoui list` の mtime 昇順 sort 結果に対する\n    \
            1-based 指定で、`1` = 最古、`-1` = 最新、`2` = 2 番目に古い、...。\n    \
            stale socket は index 対象外。範囲外は error。session-id と --index は排他。\n\
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
        EXIT STATUS:\n    \
            <子の exit code>      子 PTY が exit した (= SessionExitNotify)。\n                          \
                signal 死は 128+signum (= 130=SIGINT, 137=SIGKILL, 143=SIGTERM)。\n                          \
                非 tty stdin の default (= send-eof) では stdin EOF で子が\n                          \
                自然 exit し、その子の code がここに伝搬する\n    \
            0                     detach key、または `--stdin-eof=detach` 時の stdin EOF で\n                          \
                自分から離脱した (= 子は daemon 配下に残る)\n    \
            9                     daemon との接続が予期せず失われた (= daemon 消滅の疑い)\n    \
            1                     attach 実行エラー (= protocol violation / 出力先への書き込み失敗 等)\n    \
            2                     usage / 引数エラー\n\
        \n\
        EXAMPLES:\n    \
            hyoui attach demo                       # session_id=demo に attach\n    \
            hyoui attach 1                          # session_id=\"1\" に attach (= 数字も名前扱い)\n    \
            hyoui attach --index=1                  # 1 番古い live session に attach\n    \
            hyoui attach --index=-1                 # 1 番新しい live session に attach\n    \
            hyoui attach --index=2                  # 2 番古い live session に attach\n    \
            hyoui attach --socket=/tmp/x.sock       # 直接 socket 指定\n    \
            hyoui attach demo --mode=ro             # 読み取り専用 attach\n    \
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
            hyoui status --index=<N>\n    \
            hyoui status --socket=<path>\n    \
            hyoui status                  (中から: $HYOUI_SESSION_ID で自セッション)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --index N       Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        SELF-SESSION (DR-0020 §2):\n    \
            session を省略すると `$HYOUI_SESSION_ID` (= daemon が子へ常時注入)\n    \
            で自セッションに解決される (= 中から `hyoui status` を打てる)。\n    \
            env が指す session が不在 (= stale) なら fallback せず明示エラー。\n\
        \n\
        OUTPUT (plaintext key:value 1 行ごと):\n    \
            session-id: <name>\n    \
            daemon-pid: <pid>\n    \
            child-pid: <pid> pgid=<pgid>  または  child-pid: (exited)\n    \
            child-state: running | stopped | exited [(code N)]\n    \
            on-child-suspend: notify | auto-resume  (= 現在の policy、`hyoui set` で変更可)\n    \
            daemon-version: <version>  または  daemon-version: -  (= field 無しの古い daemon)\n    \
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

fn usage_set() -> String {
    String::from(
        "hyoui set — change a runtime setting of a session (DR-0019)\n\
        \n\
        汎用 key=value 形式で daemon の runtime 設定を変更する。書き込み可能な接続\n\
        (= rw / rw-no-leader) なら誰でも変更可 (= leader を取らない一発 CLI)。\n\
        \n\
        反映タイミング: 成功出力 (= daemon の ack) が「適用完了」。ack より前に\n\
        daemon が観測済みの child stop は旧 policy で処理され得る (= 新 policy が\n\
        効くのは ack 以降に観測される stop から)。\n\
        \n\
        USAGE:\n    \
            hyoui set <session-id> <key>=<value>\n    \
            hyoui set --index=<N> <key>=<value>\n    \
            hyoui set --socket=<path> <key>=<value>\n    \
            hyoui set <key>=<value>       (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        SUPPORTED KEYS:\n    \
            on-child-suspend=notify|auto-resume\n              \
                子が self-stop (^Z 相当) したときの daemon の挙動。\n              \
                notify      = leader に通知のみ (= 子を起こさない、default)\n              \
                auto-resume = daemon が即 SIGCONT で子を復帰させる (= 無人 worker 向け)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH     Explicit socket path (alternative to session-id)\n    \
            --index N         Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            -h, --help        Show this help and exit\n\
        \n\
        EXIT CODE:\n    \
            0   設定変更成功\n    \
            1   connect / I/O 失敗、または daemon が当該 key/value を reject\n    \
            2   引数不足 / 不正 (session 指定なし、key=value なし 等)\n    \
            4   daemon が `set-v1` 未対応 (= 古い daemon、新 client で起動し直す)\n",
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
            hyoui tail --index=<N> [options]\n    \
            hyoui tail --socket=<path> [options]\n    \
            hyoui tail [options]      (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH        Explicit socket path (alternative to session-id)\n    \
            --index N            Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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
            hyoui wait --index=<N> <pattern> [options]\n    \
            hyoui wait --socket=<path> <pattern> [options]\n    \
            hyoui wait <pattern> [options]   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
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
            --index N             Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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
            hyoui list [--namespace=<ns>] [--all-namespaces] [--prune-stale] [--format=plain|jsonl]\n\
        \n\
        OPTIONS:\n    \
            --namespace NS      表示対象を指定 namespace に絞る (= default: env HYOUI_NAMESPACE\n                                \
                                or \"default\")。`--all-namespaces` とは排他\n    \
            --all-namespaces    全 namespace を横断表示 (= NS 列を追加)。`--prune-stale` 併用で\n                                \
                                全 namespace の stale socket を掃除\n    \
            --prune-stale       stale socket (= connect 不能) を unlink で削除\n    \
            --format=plain|jsonl  出力 format (= default plain)。jsonl は 1 session 1 行の JSON object\n    \
            -h, --help          Show this help and exit\n\
        \n\
        OUTPUT (plain, fixed-width columns, sorted by socket mtime ascending):\n    \
            SESSION              STATUS  PID      DUR        CLIENTS  CWD                              ARGV\n    \
            test-claude          live    12345    1h2m       2        kawaz/hyoui/main                 claude\n    \
            stale-test           stale   -        -          -        -                                -\n\
        \n\
        COLUMNS (plain):\n    \
            SESSION   session id (= socket file 名から拡張子を除いた値、20ch で truncate)\n    \
            STATUS    live | stopped | stale (= stopped は子が ^Z/SIGSTOP で停止中)\n    \
            PID       子 PTY の PID (= ps 突き合わせ用、exited / stale は -)\n    \
            DUR       socket mtime からの経過時間 (= 1h2m / 15m / 3d4h 形式)\n    \
            CLIENTS   現在 attach 中の client 数 (= status.query の結果)\n    \
            CWD       daemon 起動時の cwd (= `repos/<host>/` 前カット、~ 前カット、32ch truncate)\n    \
            ARGV      daemon が起動した子 PTY の argv (= space-join、空白含む arg は \"...\" quote)\n\
        \n\
        OUTPUT (jsonl, 1 session = 1 line):\n    \
            {\"session\":\"<id>\",\"status\":\"live|stopped|stale\",\"child_state\":\"running|stopped|null\",\"child_pid\":<n>|null,\"child_pgid\":<n>|null,\"started_unix_ms\":<ms>,\"dur_ms\":<ms>,\"socket\":\"<path>\",\"cwd\":\"<path>|null\",\"argv\":[...]|null,\"clients\":<n>|null}\n\
        \n\
        SORT ORDER:\n    \
            socket mtime ascending (= 古い session が上、新しい session が下)。\n    \
            `hyoui attach --index=1` で 1 番古い、`--index=-1` で 1 番新しい session を指す前提。\n\
        \n\
        LIVENESS PROBE (R5-H3):\n    \
            各 socket に対し best-effort connect 試行 (= 100ms timeout)。\n    \
            成功なら `live`、ECONNREFUSED / timeout なら `stale` 表示。\n    \
            stale は daemon の panic / SIGKILL で socket が unlink されずに\n    \
            残留した状態。`hyoui list --prune-stale` で掃除可能。\n\
        \n\
        SCAN ORDER (= socket_path::resolve_in_namespace と同順、最初に見つかった dir のみ):\n    \
            default namespace: base dir 直下 (= 既存互換):\n    \
            \x20 1. $XDG_RUNTIME_DIR/hyoui/\n    \
            \x20 2. /tmp/hyoui-<uid>/  (= /tmp 固定、$TMPDIR は読まない)\n    \
            その他 namespace: <base>/<ns>/。--all-namespaces は base 配下のサブ dir も走査。\n\
        \n\
        EXIT CODE:\n    \
            0   正常終了 (= 0 件でも成功扱い、stderr に `no sessions found` を 1 行)\n\
        \n\
        EXAMPLES:\n    \
            hyoui list                              # 現在の namespace (default) の session 一覧\n    \
            hyoui list --namespace=workers          # workers namespace のみ表示\n    \
            hyoui list --all-namespaces             # 全 namespace 横断 (= NS 列付き)\n    \
            hyoui list --format=jsonl               # 機械可読 (1 session 1 行 JSON)\n    \
            hyoui list --prune-stale                # stale socket を削除して live のみ残す\n    \
            hyoui list --format=jsonl | jq -r '.session'  # session id を抽出\n\
        \n\
        RELATED:\n    \
            hyoui status <id>   session 1 件の詳細\n    \
            hyoui attach <id>   session に接続\n    \
            hyoui kill <id>     session を終了\n",
    )
}

fn usage_kill() -> String {
    String::from(
        "hyoui kill — send a signal to a daemon session's child (kill(1)-style)\n\
        \n\
        USAGE:\n    \
            hyoui kill <session-id> [options]\n    \
            hyoui kill --index=<N> [options]          # 1=最古, -1=最新\n    \
            hyoui kill --all [options]                # 全 live session を kill\n    \
            hyoui kill --socket=<path> [options]\n    \
            hyoui kill -- <session-id> [options]      # `-` で始まる session-id を escape\n    \
            hyoui kill [options]                      # 中から: $HYOUI_SESSION_ID で自セッション (= exit 相当、DR-0020)\n\
        \n\
        2 軸モデル (= terminate するか / 終了を待つか は独立):\n    \
            [terminate 軸]  既定         : signal を送る。子が死ねば session も終わる\n    \
            \x20               --no-terminate: signal を送るだけで session を畳まない\n    \
            \x20                              (= stopped child を CONT で起こす等)\n    \
            [wait 軸]       既定 (即時)  : signal 送信受理で即 return (= `kill(1)` と同じ。\n    \
            \x20                              子が 1 発で死なない app でも無応答にならない)\n    \
            \x20               --wait        : 子 exit + session 終了を見届けてから return\n    \
            \x20                              (= 既定 timeout 10s。超過で exit 3 = エラー、子は生存)\n    \
            \x20               --wait=DUR    : timeout を明示 (= 既存 DUR 形式)\n    \
            \x20               --kill-on-timeout : timeout 後 SIGKILL 昇格して見届け (= 確実に殺す)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --index N       session selector index (= mtime 昇順、1 最古 / -1 最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            --all           全 live session を順次 kill (= killall 相当)\n    \
            --signal SPEC   送信 signal (= default SIGTERM)。数字 / 略名 / SIG-prefix 大文字 OK\n    \
            --wait[=DUR]    子 exit + session 終了まで見届けて return。\n    \
            \x20               裸 --wait は既定 timeout 10s、--wait=DUR で指定 (= 既存 DUR 形式)。\n    \
            \x20               timeout 超過は exit 3 (= エラー、子は生かす)。\n    \
            \x20               既定 (= 省略時) は signal 送信受理で即 return。`--no-terminate` 不可\n    \
            --kill-on-timeout  --wait の timeout 超過時に SIGKILL 昇格して見届ける\n    \
            \x20               (= 確実に殺す)。`--wait` と併用必須\n    \
            --no-terminate  signal を送るだけで session を畳まない (= stopped child を\n    \
            \x20               CONT で起こす用途等)。`--all` / `--wait` とは併用不可\n    \
            -N              POSIX kill 慣習: 短縮 signal 番号 (e.g. -9 = SIGKILL)\n    \
            -NAME           短縮 signal 名 (e.g. -KILL / -TERM / -SIGKILL も OK)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        SIGNAL SPEC (= --signal の引数 or -N short flag):\n    \
            数字:        9 / 15 / 1            (= POSIX signal 番号、OS で defined のみ)\n    \
            略名:        KILL / TERM / INT     (= SIG-prefix 自動付加)\n    \
            正規表記:    SIGKILL / SIGTERM     (= そのまま)\n    \
            大文字緩和:  kill / sigterm        (= 内部大文字化)\n    \
            短縮 flag:   -9 / -KILL / -SIGKILL (= --signal=9 と等価、POSIX kill 慣習 + 略名拡張)\n    \
            wire には正規 SIG-prefix 大文字を流す (DR-0012、daemon 側は SIG-prefix のみ解釈)\n\
        \n\
        SESSION SELECTOR:\n    \
            位置引数 (e.g. `kill demo` / `kill 1`)  => session-id 名 (= 数字も名前扱い)\n    \
            `-N` short flag (e.g. `kill -9 demo`)   => signal 解釈 (POSIX kill 慣習)\n    \
            `--index=N`                              => mtime 昇順 1-based index (= 1 最古, -1 最新)\n    \
            `-` で始まる session-id は `--` セパレータで escape (e.g. `kill -- -foo`)\n\
        \n\
        EXIT CODE:\n    \
            0   既定: signal 送信受理を確認 / --wait: session 終了を見届けた\n    \
            1   connect / send 失敗 / daemon が reject\n    \
            2   引数不足 / 排他違反\n    \
            3   --wait の timeout 超過 (= 子が終了せず、子は生存。--kill-on-timeout で SIGKILL 昇格可)\n\
        \n\
        EXAMPLES:\n    \
            hyoui kill demo                          # session_id=demo に SIGTERM (= 即時 return)\n    \
            hyoui kill demo --signal=SIGKILL         # SIGKILL を送る (= 正規表記)\n    \
            hyoui kill demo --signal=KILL            # SIGKILL を送る (= 略名)\n    \
            hyoui kill demo --signal=9               # SIGKILL を送る (= 数字)\n    \
            hyoui kill -9 demo                       # SIGKILL を送る (= 番号 短縮)\n    \
            hyoui kill -KILL demo                    # SIGKILL を送る (= 略名 短縮)\n    \
            hyoui kill -SIGTERM demo                 # SIGTERM を送る (= 正規 短縮)\n    \
            hyoui kill demo --wait                   # 子 exit を最大 10s 待つ (超過で exit 3)\n    \
            hyoui kill demo --wait=2s                 # timeout 2s で見届け (超過で exit 3、子生存)\n    \
            hyoui kill demo --wait=2s --kill-on-timeout  # 2s 後 SIGKILL 昇格して確実に殺す\n    \
            hyoui kill 1                             # session_id=\"1\" を SIGTERM (= 数字も名前)\n    \
            hyoui kill --index=1                     # 1 番古い session を SIGTERM\n    \
            hyoui kill --index=-1                    # 最新 session を SIGTERM\n    \
            hyoui kill --all                         # 全 live session を SIGTERM\n    \
            hyoui kill --all --signal=KILL           # 全 live session を SIGKILL\n    \
            hyoui kill demo --signal=CONT --no-terminate  # stopped child を起こす (= session 継続)\n    \
            hyoui kill -- -dash-id                   # session_id=\"-dash-id\" を kill (escape)\n    \
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
            hyoui screen dump --index=<N> [options]\n    \
            hyoui screen dump --socket=<path> [options]\n    \
            hyoui screen dump [options]   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --index N           Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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
            hyoui screen snapshot --index=<N> [options]\n    \
            hyoui screen snapshot --socket=<path> [options]\n    \
            hyoui screen snapshot [options]   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --index N           Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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
            hyoui lock acquire --index=<N> [options]\n    \
            hyoui lock acquire --socket=<path> [options]\n    \
            hyoui lock acquire [options]   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --index N           Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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
            hyoui lock release --index=<N> --token=<T>\n    \
            hyoui lock release --socket=<path> --token=<T>\n    \
            hyoui lock release --token=<T>   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --index N       Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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
            hyoui unlock --index=<N> --token=<T>\n    \
            hyoui unlock --socket=<path> --token=<T>\n    \
            hyoui unlock --token=<T>   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --index N       Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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

fn usage_detach() -> String {
    String::from(
        "hyoui detach — detach all attached clients from a session (DR-0020 §4)\n\
        \n\
        指定 session の attach 接続を **全部** 引き剥がす。daemon と子 PTY は影響を\n\
        受けず継続する (= DR-0015 §2.3.1、全員 detach 後も daemon 常駐 / 新規 attach 待ち)。\n\
        \n\
        対象は常に全 client: detach CLI は一時接続で要求を送る構造のため、self /\n\
        others のような部分指定は CLI からは意味を成さない (= self は一時接続の\n\
        no-op、others は実質 all)。attach 中の端末から自分だけ抜けるのは attach の\n\
        detach key (Ctrl-A d) の役割。\n\
        \n\
        USAGE:\n    \
            hyoui detach <session-id>\n    \
            hyoui detach --index=<N>\n    \
            hyoui detach --socket=<path>\n    \
            hyoui detach              (中から: $HYOUI_SESSION_ID で自セッション)\n\
        \n\
        OPTIONS:\n    \
            --socket PATH   Explicit socket path (alternative to session-id)\n    \
            --index N       Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            -h, --help      Show this help and exit\n\
        \n\
        SELF-SESSION (DR-0020 §2):\n    \
            session を省略すると `$HYOUI_SESSION_ID` で自セッションに解決される\n    \
            (= 中から `hyoui detach` で TUI 直起動から脱出)。\n\
        \n\
        EXIT CODE:\n    \
            0   detach 成功 (= 0 clients でも成功 = 冪等)\n    \
            1   connect / I/O 失敗\n    \
            2   引数不足 (session 指定なし + env なし 等)\n\
        \n\
        EXAMPLES:\n    \
            hyoui detach demo    # demo の全 client を切断\n    \
            hyoui detach         # 中から: 自セッションの全 client 切断 (= TUI 脱出)\n\
        \n\
        RELATED:\n    \
            hyoui attach <id> --detach-others  Attach しつつ他 client を奪取\n    \
            hyoui kill <id>                    session ごと終了 (= 子に signal)\n",
    )
}

fn usage_record() -> String {
    String::from(
        "hyoui record — record tty I/O timeline (DR-0016)\n\
        \n\
        USAGE:\n    \
            hyoui record <subcommand> [options]\n\
        \n\
        SUBCOMMANDS:\n    \
            start       Start a new record (writes to a file, returns record_id)\n    \
            stop        Stop a running record (--id <N> or --all)\n    \
            list        List active records for the session\n\
        \n\
        OPTIONS:\n    \
            -h, --help      Show this help and exit\n\
        \n\
        Run `hyoui record <subcommand> --help` for per-subcommand help.\n\
        \n\
        SECURITY NOTE:\n    \
            record file は子 PTY の全 I/O bytes を含むため、機密情報 (password / OTP /\n    \
            API token 等) が永続化される可能性があります。出力 file (mode 0600) は\n    \
            認証境界外に共有しないでください。\n    \
            ⚠ WARNING: `--input-secrecy` の redaction は **未実装** (= Phase 5 予定)。\n    \
            現状どの policy を指定しても stdin は素通しで記録されます\n    \
            (= password / OTP も平文で残る)。\n",
    )
}

fn usage_record_start() -> String {
    String::from(
        "hyoui record start — start recording tty I/O timeline (DR-0016 §2)\n\
        \n\
        USAGE:\n    \
            hyoui record start <session-id> --output <path> [options]\n    \
            hyoui record start --index=<N> --output <path> [options]\n    \
            hyoui record start --socket=<path> --output <path> [options]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH               Explicit socket path (alternative to session-id)\n    \
            --index N                   Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            --output PATH               出力 file path (= **絶対 path 必須**)\n    \
            --stdin                     録画 direction: 子 PTY 入力のみ\n    \
            --stdout                    録画 direction: 子 PTY 出力のみ\n    \
            --both                      録画 direction: 両方 (= default、jsonl 限定)\n    \
            --format=jsonl|raw          出力 format (default jsonl)\n                                \
                jsonl は header + body 構造化 (= 診断 timeline 用)\n                                \
                raw は単一 direction の raw bytes (= `--both` 不可、stream export 専用)\n    \
            --max-bytes N               録画 bytes 上限 (default 100 MiB、`0` で disable + 警告)\n                                \
                suffix 受理: k/K/kb/KiB (1024), m/MB/MiB (1024²), g/GB/GiB (1024³)\n    \
            --max-duration DUR          録画 duration 上限 (default 1h、`0` で disable + 警告)\n                                \
                DUR 形式: `30m` / `1h` / `1d12h` 等 (`hyoui run` の --timeout と同形式)\n    \
            --input-secrecy POLICY      stdin redaction policy (default redact-after-prompt)\n                                \
                ⚠ redaction は **未実装** (Phase 5 予定)。現状どの policy でも\n                                \
                stdin は素通しで記録される (= password / OTP も平文で残る)。\n                                \
                redact-after-prompt — (予定) password/OTP prompt 後の stdin を redact\n                                \
                record-all          — (予定) redaction なし、全 stdin を hex 記録\n                                \
                never-record-stdin  — (予定) 全 stdin を redaction (内容捨て、byte_count のみ)\n    \
            --prompt-pattern REGEX      custom prompt 検出 regex (default は daemon 適用)\n    \
            -h, --help                  Show this help and exit\n\
        \n\
        ENVIRONMENT:\n    \
            HYOUI_LOCK_TOKEN            lock token (= handshake.token)\n\
        \n\
        EXIT CODE:\n    \
            0   record 開始成功 (= record_id を stdout に出力)\n    \
            1   connect / I/O / daemon error (= cap `record-v1` 未対応含む)\n    \
            2   引数不足 / 未知 option / --output が相対 path\n\
        \n\
        EXAMPLES:\n    \
            # 最小: 絶対 path で出力先を指定 (= default --both --format=jsonl)\n    \
            hyoui record start my-session --output /tmp/rec.jsonl\n\
        \n    \
            # stdout だけを raw bytes として export (= cat 再生用)\n    \
            hyoui record start my-session --output /tmp/out.bin --format=raw --stdout\n\
        \n    \
            # 最厳 redaction: stdin を完全に捨てる\n    \
            hyoui record start my-session --output /tmp/rec.jsonl --input-secrecy=never-record-stdin\n\
        \n    \
            # 上限を緩める (5 GiB / 24h)\n    \
            hyoui record start my-session --output /tmp/long.jsonl --max-bytes=5g --max-duration=24h\n\
        \n\
        SECURITY:\n    \
            起動時に stderr へ 3 行 loud warning を出します。file は mode 0600 で daemon が\n    \
            open、symlink は ELOOP で reject、`.sh` / `~/.ssh/` 等 sensitive path は reject\n    \
            されます (= daemon 側 validation、DR-0016 §9)。\n",
    )
}

fn usage_record_stop() -> String {
    String::from(
        "hyoui record stop — stop a running record (DR-0016 §2)\n\
        \n\
        USAGE:\n    \
            hyoui record stop <session-id> [--id <N> | --all]\n    \
            hyoui record stop --index=<N> [--id <N> | --all]\n    \
            hyoui record stop --socket=<path> [--id <N> | --all]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH    Explicit socket path (alternative to session-id)\n    \
            --index N        Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            --id N           停止対象の record_id (= record start の戻り値、または\n                             \
                record list で確認)\n    \
            --all            同 session の全 active record を一括停止 (= `--id` と排他)\n    \
            -h, --help       Show this help and exit\n\
        \n\
        --id / --all のどちらも省略した場合: client 側で `record list` を先に問い合わせ、\n    \
        single active record があればそれを自動採用、複数 active なら error、none なら\n    \
        error にする (= ambiguity を CLI 側で吸収、protocol message は単一 record 停止のみ)。\n\
        \n\
        EXIT CODE:\n    \
            0   停止成功\n    \
            1   connect / I/O / daemon error / multiple active 時の --id 不足\n    \
            2   引数不足 (--id と --all 同時指定 等)\n\
        \n\
        EXAMPLES:\n    \
            hyoui record stop my-session --id 1            # 特定 record_id を停止\n    \
            hyoui record stop my-session --all             # 全 active record を停止\n    \
            hyoui record stop my-session                   # single active なら自動採用\n",
    )
}

fn usage_record_list() -> String {
    String::from(
        "hyoui record list — list active records for the session (DR-0016 §2)\n\
        \n\
        USAGE:\n    \
            hyoui record list <session-id> [--format=table|jsonl]\n    \
            hyoui record list --index=<N> [--format=table|jsonl]\n    \
            hyoui record list --socket=<path> [--format=table|jsonl]\n\
        \n\
        OPTIONS:\n    \
            --socket PATH       Explicit socket path (alternative to session-id)\n    \
            --index N           Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
            --format table|jsonl\n                                \
                                Output format (default table)\n                                \
                                table — 人間可読の固定長 column 1 行 1 record\n                                \
                                jsonl — RecordInfo を 1 record 1 行の JSON Lines\n    \
            -h, --help          Show this help and exit\n\
        \n\
        EXIT CODE:\n    \
            0   一覧取得成功 (active 0 件でも 0)\n    \
            1   connect / I/O / daemon error\n\
        \n\
        EXAMPLES:\n    \
            hyoui record list my-session                   # default の table 表示\n    \
            hyoui record list my-session --format=jsonl    # scripting / jq 用\n",
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
    /// `--index=N` session selector (= mtime 昇順、1=最古 / -1=最新)。
    pub index: Option<i32>,
    /// `--namespace=X` flag の生値 (= DR-0018、未指定なら None)。
    pub namespace: Option<String>,
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
    if !s.len().is_multiple_of(2) {
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
    let mut index: Option<i32> = None;
    let mut namespace: Option<String> = None;
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
            "--namespace" => match value {
                Some(v) => {
                    if let Err(e) = validate_namespace(&v) {
                        return Command::Error(format!("input: --namespace: {e}"));
                    }
                    namespace = Some(v);
                }
                None => return Command::Error("input: --namespace requires a value".into()),
            },
            "--index" => match value {
                Some(v) => match v.parse::<i32>() {
                    Ok(0) => {
                        return Command::Error(
                            "input: --index=0 は不正です (= 1-based、1 が最古、-1 が最新)".into(),
                        );
                    }
                    Ok(n) => index = Some(n),
                    Err(_) => {
                        return Command::Error(format!(
                            "input: --index には整数を指定してください (got: {v:?})"
                        ));
                    }
                },
                None => return Command::Error("input: --index requires a value".into()),
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
    // ただし `--socket` / `--index` 指定時は session_id を省略でき、全 positional が spec。
    //
    // 戦略:
    // 1. `--socket` or `--index` 指定 → 全 positional を spec として parse
    // 2. それ以外: 第 1 positional に spec prefix (= `:` 区切り) が無く
    //    `validate_session_id` を通るなら **明示 session id** として受理
    //    (= env の有無に関わらず明示が優先、Fable review C1 2026-06-12)。
    // 3. 第 1 が spec 形なら: `$HYOUI_SESSION_ID` (= 中から実行) があるときだけ
    //    全 positional を spec として受理し session は self 解決 (DR-0020 §2)。
    //    env なしなら従来通り validate エラー (= 旧挙動不変)。
    //
    // 旧実装は env set で無条件に全 positional を spec 扱いにしたため、
    // `hyoui input beta text:hi` の "beta" が spec parse に回って壊れていた。
    let (session_id, spec_strs): (Option<String>, &[String]) = if socket.is_some()
        || index.is_some()
    {
        (None, positionals.as_slice())
    } else {
        match positionals.first() {
            None => {
                if has_self_session_env() {
                    // 中から + positional ゼロ → 後段の「spec list が空」エラーに
                    // 落とす (= session 解決の問題ではなく spec 不足が本質)。
                    (None, positionals.as_slice())
                } else {
                    return Command::Error(
                        "input: session id (positional) / --index=N / --socket=<path> のいずれかが必要です。\
                         例: `hyoui input <session-id> text:hello key:Enter` / `hyoui input --index=1 text:hello`"
                            .into(),
                    );
                }
            }
            Some(first) => {
                // spec は必ず `<prefix>:<value>` 形式 (= `:` を含む)。session id の
                // whitelist は `:` を許さないため、`:` の有無で両者を判別できる。
                let looks_like_spec = first.contains(':');
                if !looks_like_spec && validate_session_id(first).is_ok() {
                    // 明示 session id (= env より優先)。
                    (Some(first.clone()), &positionals[1..])
                } else if has_self_session_env() {
                    // 中から + 第 1 が spec 形 → 全部 spec、session は self 解決。
                    (None, positionals.as_slice())
                } else {
                    // env なし: 従来通り第 1 を session id 候補として validate し、
                    // 失敗理由をそのまま返す (= 旧挙動不変)。
                    if let Err(e) = validate_session_id(first) {
                        return Command::Error(format!("input: {e}"));
                    }
                    (Some(first.clone()), &positionals[1..])
                }
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
        index,
        namespace,
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
            hyoui input --index=<N> <spec>... [options]\n    \
            hyoui input --socket=<path> <spec>... [options]\n    \
            hyoui input <spec>... [options]   (中から: $HYOUI_SESSION_ID で自セッション、DR-0020)\n\
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
            --index N          Session selector (= mtime 昇順、1=最古, -1=最新)\n    \
            --namespace NS    Session namespace (default \"default\"; env HYOUI_NAMESPACE 経路)\n    \
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

/// session namespace の予約名 (= socket dir 直下にマップする default ns、DR-0018)。
///
/// `default` は「従来通り `<base>/hyoui-<uid>/` 直下に socket を置く」ことを意味する
/// 予約名。ユーザが `--namespace=default` を明示しても、namespace 未指定時と完全に
/// 同じ dir 構造になる (= 既存 session との後方互換、dir 移動なし)。
pub const DEFAULT_NAMESPACE: &str = "default";

/// session `namespace` を path traversal / 制御文字 / 過長から守る whitelist validator
/// (= DR-0018)。
///
/// 許可: `session_id` と同等 (= `[A-Za-z0-9._-]{1,64}`)。さらに以下を明示 reject:
///
/// - 空 string (= "")
/// - `.` 単独、`..` 単独 (= path 構成要素として親 dir 参照になる)
/// - `/` を含む (= path separator。**将来の階層 namespace 用に予約**: DR-0018 で
///   フラット ns を採用したが、`/` を区切りとして後方互換で階層化できるよう、現状は
///   ns 名に `/` を含めること自体を禁止する)
///
/// `validate_session_id` と判定は同等だが、error 文言を `namespace` 文脈にして
/// ユーザに分かりやすくする (= 同一実装に委譲しつつ文言だけ差し替え)。
///
/// # Errors
///
/// validator に反する場合、人間可読な reason 文字列を返す。
pub fn validate_namespace(namespace: &str) -> Result<(), String> {
    validate_session_id(namespace).map_err(|e| e.replace("session_id", "namespace"))
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
/// reserved (`send` / `detach` / `tx`) も含める
/// (= 「予約済」と気づかせるほうが UX 改善になる)。
///
/// 実装済 subcommand のみが欲しい場合は [`IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS`] を使う
/// (= completion script の single source of truth)。reserved だけが欲しい場合は
/// [`RESERVED_TOP_LEVEL_SUBCOMMANDS`]。
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
    "record",
];

// =============================================================================
// Completion single source of truth (= DR-0014 §検証主義)
//
// completion script (crates/hyoui-cli/src/completion.rs) と parse 実装の乖離を
// 機械検証で防ぐための SSOT 定数群。parse 実装からも本定数を参照し、completion 側
// テストが「定数の全要素が 3 shell 出力に含まれる」「reserved 語が含まれない」を
// 検証する。新規 subcommand / option / 列挙値を足すときは **まず本定数を更新** する。
// =============================================================================

/// 実装済 (= dispatch 経路が存在する) top-level subcommand 一覧。
///
/// completion script はこの全要素を補完候補として出力しなければならない
/// (= completion.rs のテストで機械検証)。reserved 語 (`send` / `tx`) は
/// **含めない** (= 実装では `parse_args` が「予約だが未実装」error を返すため、
/// 補完候補に出すと user を誤誘導する)。
pub const IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS: &[&str] = &[
    "run",
    "attach",
    "list",
    "kill",
    "status",
    "set",
    "tail",
    "wait",
    "screen",
    "input",
    "lock",
    "unlock",
    "detach",
    "record",
    "completion",
];

/// 予約済だが未実装の top-level subcommand 一覧 (= `parse_args` が error を返す)。
///
/// completion script はこれらを補完候補に **出してはならない**
/// (= completion.rs のテストで機械検証、出ると user を誤誘導する)。
/// `detach` は DR-0020 §4 で実装済 (= IMPLEMENTED へ移動)。
pub const RESERVED_TOP_LEVEL_SUBCOMMANDS: &[&str] = &["send", "tx"];

/// `hyoui screen` の子 subcommand 一覧 (= `parse_screen` が dispatch する値)。
pub const SCREEN_SUBCOMMANDS: &[&str] = &["dump", "snapshot"];

/// `hyoui lock` の子 subcommand 一覧 (= `parse_lock` が dispatch する値)。
pub const LOCK_SUBCOMMANDS: &[&str] = &["acquire", "release"];

/// `hyoui record` の子 subcommand 一覧 (= `parse_record` が dispatch する値)。
pub const RECORD_SUBCOMMANDS: &[&str] = &["start", "stop", "list"];

/// `hyoui screen snapshot --include` が受理する正規化済 component 名一覧。
///
/// `parse_snapshot_include` が本定数を正本として参照する (= 乖離防止)。
/// completion script は本定数の全要素を `--include` の補完候補に出す。
/// 値は parse 段の正規化 (= lowercase + `-`/`_` 除去) 後の形 (= `windowsize` /
/// `sequenceno`) で持つ。`SnapshotCliComponent` との対応は
/// [`snapshot_component_from_normalized`] で 1:1。
pub const SNAPSHOT_INCLUDE_VALUES: &[&str] = &[
    "cells",
    "cursor",
    "mode",
    "style",
    "scrollback",
    "windowsize",
    "buffer",
    "sequenceno",
];

/// `hyoui screen dump --format` が受理する値一覧 (= primary name のみ、alias 除く)。
pub const SCREEN_DUMP_FORMAT_VALUES: &[&str] = &["ansi", "binary", "cbor", "text/plain"];

/// `hyoui screen dump --layer` が受理する値一覧。
pub const SCREEN_DUMP_LAYER_VALUES: &[&str] = &["visible", "scrollback", "both"];

/// `hyoui screen snapshot --format` が受理する値一覧。
pub const SCREEN_SNAPSHOT_FORMAT_VALUES: &[&str] = &["cbor", "json"];

/// `hyoui list --format` が受理する値一覧。
pub const LIST_FORMAT_VALUES: &[&str] = &["plain", "jsonl"];

/// `hyoui status --format` が受理する値一覧。
pub const STATUS_FORMAT_VALUES: &[&str] = &["plain", "json"];

/// `hyoui record list --format` が受理する値一覧。
pub const RECORD_LIST_FORMAT_VALUES: &[&str] = &["table", "jsonl"];

/// `hyoui record start --format` が受理する値一覧。
pub const RECORD_START_FORMAT_VALUES: &[&str] = &["jsonl", "raw"];

/// `hyoui record start --input-secrecy` が受理する policy 値一覧。
pub const RECORD_INPUT_SECRECY_VALUES: &[&str] =
    &["redact-after-prompt", "record-all", "never-record-stdin"];

/// `正規化済 component 名 → SnapshotCliComponent` を 1:1 で解決する。
///
/// `parse_snapshot_include` と SSOT 定数 [`SNAPSHOT_INCLUDE_VALUES`] の両方から
/// 参照する単一の対応表 (= 列挙値の追加漏れを 1 箇所に集約)。`normalized` は
/// lowercase + `-`/`_` 除去済の文字列を渡す。
fn snapshot_component_from_normalized(normalized: &str) -> Option<SnapshotCliComponent> {
    Some(match normalized {
        "cells" => SnapshotCliComponent::Cells,
        "cursor" => SnapshotCliComponent::Cursor,
        "mode" => SnapshotCliComponent::Mode,
        "style" => SnapshotCliComponent::Style,
        "scrollback" => SnapshotCliComponent::Scrollback,
        "windowsize" => SnapshotCliComponent::WindowSize,
        "buffer" => SnapshotCliComponent::Buffer,
        "sequenceno" => SnapshotCliComponent::SequenceNo,
        _ => return None,
    })
}

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

    // -------- Completion SSOT 定数 ↔ parse 実装の整合性検証 --------
    //
    // completion.rs はこれら定数を single source of truth として参照する。
    // 定数が parse 実装からズレると completion も連鎖してズレるため、定数自体が
    // 実装と一致することを本 module で機械検証する (= 二段構えの整合性ガード)。

    #[test]
    fn ssot_implemented_subcommands_are_dispatchable() {
        // 実装済 subcommand は「予約だが未実装」error を返さない。
        for sub in IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS {
            let cmd = parse_args(&args(&[sub]));
            if let Command::Error(msg) = &cmd {
                assert!(
                    !msg.contains("reserved but not yet implemented"),
                    "implemented subcommand `{sub}` returned reserved error: {msg}"
                );
            }
            // UnknownSubcommand にもならない (= dispatch 経路が存在する)。
            assert!(
                !matches!(
                    &cmd,
                    Command::Help {
                        topic: HelpTopic::UnknownSubcommand(_)
                    }
                ),
                "implemented subcommand `{sub}` is treated as unknown"
            );
        }
    }

    #[test]
    fn ssot_reserved_subcommands_return_reserved_error() {
        for sub in RESERVED_TOP_LEVEL_SUBCOMMANDS {
            match parse_args(&args(&[sub])) {
                Command::Error(msg) => assert!(
                    msg.contains("reserved but not yet implemented"),
                    "reserved subcommand `{sub}` returned unexpected error: {msg}"
                ),
                other => panic!("reserved subcommand `{sub}` expected Error, got {other:?}"),
            }
        }
    }

    #[test]
    fn ssot_implemented_and_reserved_are_disjoint() {
        for r in RESERVED_TOP_LEVEL_SUBCOMMANDS {
            assert!(
                !IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS.contains(r),
                "`{r}` is in both implemented and reserved lists"
            );
        }
    }

    #[test]
    fn ssot_snapshot_include_values_all_parse() {
        for v in SNAPSHOT_INCLUDE_VALUES {
            assert!(
                parse_snapshot_include(v).is_ok(),
                "SNAPSHOT_INCLUDE_VALUES entry `{v}` rejected by parse_snapshot_include"
            );
            assert!(
                snapshot_component_from_normalized(v).is_some(),
                "SNAPSHOT_INCLUDE_VALUES entry `{v}` has no enum mapping"
            );
        }
        // 旧 completion の誤値は実装に存在しない (= reject される)。
        for bogus in ["screen", "size", "title"] {
            assert!(
                parse_snapshot_include(bogus).is_err(),
                "stale include value `{bogus}` unexpectedly accepted"
            );
        }
    }

    #[test]
    fn ssot_screen_subcommands_dispatch() {
        for sub in SCREEN_SUBCOMMANDS {
            // session 無しなので Error にはなるが、UnknownSubcommand error ではない。
            if let Command::Error(msg) = parse_args(&args(&["screen", sub])) {
                assert!(
                    !msg.contains("unknown subcommand"),
                    "screen `{sub}` treated as unknown: {msg}"
                );
            }
        }
    }

    #[test]
    fn ssot_lock_subcommands_dispatch() {
        for sub in LOCK_SUBCOMMANDS {
            if let Command::Error(msg) = parse_args(&args(&["lock", sub])) {
                assert!(
                    !msg.contains("unknown subcommand"),
                    "lock `{sub}` treated as unknown: {msg}"
                );
            }
        }
    }

    #[test]
    fn ssot_record_subcommands_dispatch() {
        for sub in RECORD_SUBCOMMANDS {
            if let Command::Error(msg) = parse_args(&args(&["record", sub])) {
                assert!(
                    !msg.contains("unknown subcommand"),
                    "record `{sub}` treated as unknown: {msg}"
                );
            }
        }
    }

    #[test]
    fn ssot_status_format_values_all_parse() {
        for v in STATUS_FORMAT_VALUES {
            match parse_args(&args(&["status", "demo", "--format", v])) {
                Command::Status(_) => {}
                other => panic!("status --format={v} rejected: {other:?}"),
            }
        }
    }

    #[test]
    fn ssot_list_format_values_all_parse() {
        for v in LIST_FORMAT_VALUES {
            match parse_args(&args(&["list", &format!("--format={v}")])) {
                Command::List(_) => {}
                other => panic!("list --format={v} rejected: {other:?}"),
            }
        }
    }

    #[test]
    fn ssot_screen_dump_enum_values_all_parse() {
        for v in SCREEN_DUMP_FORMAT_VALUES {
            match parse_args(&args(&["screen", "dump", "demo", "--format", v])) {
                Command::Screen(_) => {}
                other => panic!("screen dump --format={v} rejected: {other:?}"),
            }
        }
        for v in SCREEN_DUMP_LAYER_VALUES {
            match parse_args(&args(&["screen", "dump", "demo", "--layer", v])) {
                Command::Screen(_) => {}
                other => panic!("screen dump --layer={v} rejected: {other:?}"),
            }
        }
    }

    #[test]
    fn ssot_screen_snapshot_format_values_all_parse() {
        for v in SCREEN_SNAPSHOT_FORMAT_VALUES {
            match parse_args(&args(&["screen", "snapshot", "demo", "--format", v])) {
                Command::Screen(_) => {}
                other => panic!("screen snapshot --format={v} rejected: {other:?}"),
            }
        }
    }

    #[test]
    fn ssot_record_format_and_secrecy_values_all_parse() {
        for v in RECORD_LIST_FORMAT_VALUES {
            match parse_args(&args(&["record", "list", "demo", "--format", v])) {
                Command::Record(_) => {}
                other => panic!("record list --format={v} rejected: {other:?}"),
            }
        }
        for v in RECORD_START_FORMAT_VALUES {
            // raw は単一 direction 限定なので --stdout を併せて与える。
            match parse_args(&args(&[
                "record",
                "start",
                "demo",
                "--output",
                "/tmp/r.out",
                "--stdout",
                "--format",
                v,
            ])) {
                Command::Record(_) => {}
                other => panic!("record start --format={v} rejected: {other:?}"),
            }
        }
        for v in RECORD_INPUT_SECRECY_VALUES {
            match parse_args(&args(&[
                "record",
                "start",
                "demo",
                "--output",
                "/tmp/r.out",
                "--input-secrecy",
                v,
            ])) {
                Command::Record(_) => {}
                other => panic!("record start --input-secrecy={v} rejected: {other:?}"),
            }
        }
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
        match parse_args(&args(&["run"])) {
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
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Notify);
                // DR-0019 §5: 未指定なら None (= exec attach 側で tty 判定して解決)。
                assert_eq!(cfg.stdin_eof, None);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_stdin_eof_send_eof_and_detach_parse() {
        // DR-0019 §5: run --stdin-eof=send-eof|detach を明示値として保持する。
        match parse_args(&args(&["run", "--stdin-eof=send-eof", "--", "bc"])) {
            Command::Run(cfg) => assert_eq!(cfg.stdin_eof, Some(StdinEofArg::SendEof)),
            other => panic!("expected Run, got {other:?}"),
        }
        match parse_args(&args(&["run", "--stdin-eof=detach", "--", "bc"])) {
            Command::Run(cfg) => assert_eq!(cfg.stdin_eof, Some(StdinEofArg::Detach)),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_stdin_eof_invalid_value_is_error() {
        match parse_args(&args(&["run", "--stdin-eof=bogus", "--", "bc"])) {
            Command::Error(msg) => assert!(msg.contains("stdin-eof"), "msg: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn attach_stdin_eof_parse() {
        // DR-0019 §5: attach --stdin-eof も同じ値を受ける (= run/attach 共通 flag)。
        match parse_args(&args(&["attach", "demo", "--stdin-eof=send-eof"])) {
            Command::Attach(cfg) => assert_eq!(cfg.stdin_eof, Some(StdinEofArg::SendEof)),
            other => panic!("expected Attach, got {other:?}"),
        }
        match parse_args(&args(&["attach", "demo"])) {
            Command::Attach(cfg) => assert_eq!(cfg.stdin_eof, None),
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn run_default_suspend_is_notify_only() {
        // DR-0017 §柱2: default は notify-only。auto-resume は
        // opt-in に限定 (= 勝手に子を起こさない)。
        match parse_args(&args(&["run", "--", "cat"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Notify);
                // size 未指定なら None = caller (= run_command) が外側 TTY size or
                // 80x24 fallback で解決する経路 (= ユーザ指示 2026-05-29)。
                assert_eq!(cfg.cols, None);
                assert_eq!(cfg.rows, None);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_auto_resume_is_opt_in() {
        // DR-0017 §柱2: auto-resume は明示 opt-in でのみ選べる (= default にはならない)。
        match parse_args(&args(&[
            "run",
            "--on-child-suspend=auto-resume",
            "--",
            "cat",
        ])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::AutoResume);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_on_child_suspend_old_follow_value_is_error() {
        // DR-0019: 旧値 `follow` は `notify` に rename。エラー文に移行先を明記する
        // (= migration hint、`--signum` 廃止 (DR-0012) と同じ流儀)。
        match parse_args(&args(&["run", "--on-child-suspend=follow", "--", "cat"])) {
            Command::Error(msg) => {
                assert!(msg.contains("follow"), "should mention old value: {msg}");
                assert!(msg.contains("notify"), "should hint new value: {msg}");
                assert!(msg.contains("DR-0019"), "should cite DR: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_mode_flag_is_removed_with_migration_hint() {
        // DR-0019 §1: `--mode` preset を削除。指定時は unknown option ではなく
        // 移行先を示す明示エラーを返す (= migration hint)。
        match parse_args(&args(&["run", "--mode=headless", "--", "claude"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--mode"), "should mention --mode: {msg}");
                assert!(msg.contains("DR-0019"), "should cite DR: {msg}");
                // 移行先 (= --detached / --size / --on-child-suspend) を示す。
                assert!(msg.contains("--detached"), "should hint --detached: {msg}");
                assert!(
                    msg.contains("--on-child-suspend"),
                    "should hint --on-child-suspend: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_on_parent_suspend_flag_is_removed_with_migration_hint() {
        // DR-0019 §7 / DR-0015 §2.3: `--on-parent-suspend` は削除済。指定時は
        // unknown option ではなく「軸 2 廃止」を示す明示エラーを返す。
        match parse_args(&args(&[
            "run",
            "--on-parent-suspend=decouple",
            "--",
            "claude",
        ])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("--on-parent-suspend"),
                    "should mention flag: {msg}"
                );
                assert!(
                    msg.contains("DR-0015") || msg.contains("DR-0019"),
                    "should cite DR: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_explicit_suspend_overrides_default() {
        // --on-child-suspend=notify を明示しても default (= Notify) と同じ。
        match parse_args(&args(&["run", "--on-child-suspend=notify", "--", "cat"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.on_child_suspend, OnChildSuspend::Notify);
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_size_parses_cols_and_rows() {
        match parse_args(&args(&["run", "--size", "120x40", "--", "vim"])) {
            Command::Run(cfg) => {
                assert_eq!(cfg.cols, Some(120));
                assert_eq!(cfg.rows, Some(40));
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
                assert_eq!(cfg.cols, Some(100));
                assert_eq!(cfg.rows, Some(30));
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

    // ──────────────────────────────────────────────────────────────────────
    // DR-0019 Update: `set` subcommand parse
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_set_session_and_key_value() {
        match parse_args(&args(&["set", "demo", "on-child-suspend=auto-resume"])) {
            Command::Set(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.key, "on-child-suspend");
                assert_eq!(cfg.value, "auto-resume");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_key_value_order_independent() {
        // key=value が session より前でも parse できる (= 位置非依存)。
        match parse_args(&args(&["set", "on-child-suspend=notify", "demo"])) {
            Command::Set(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.key, "on-child-suspend");
                assert_eq!(cfg.value, "notify");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_with_index_selector() {
        match parse_args(&args(&["set", "--index=-1", "on-child-suspend=notify"])) {
            Command::Set(cfg) => {
                assert_eq!(cfg.index, Some(-1));
                assert!(cfg.session_id.is_none());
                assert_eq!(cfg.key, "on-child-suspend");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_value_can_contain_equals() {
        // value 側に `=` を含んでも、最初の `=` で key/value 分割する。
        match parse_args(&args(&["set", "demo", "k=a=b"])) {
            Command::Set(cfg) => {
                assert_eq!(cfg.key, "k");
                assert_eq!(cfg.value, "a=b");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_requires_key_value() {
        // session だけで key=value が無ければ error。
        match parse_args(&args(&["set", "demo"])) {
            Command::Error(msg) => assert!(msg.contains("key") && msg.contains("value")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_requires_session() {
        // key=value だけで session 指定が無ければ error。
        match parse_args(&args(&["set", "on-child-suspend=notify"])) {
            Command::Error(msg) => assert!(msg.contains("session id") || msg.contains("socket")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_rejects_two_key_values() {
        match parse_args(&args(&["set", "demo", "a=1", "b=2"])) {
            Command::Error(msg) => assert!(msg.contains("key=value")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_help_routes_to_set_topic() {
        match parse_args(&args(&["set", "--help"])) {
            Command::Help { topic } => assert!(matches!(topic, HelpTopic::Set)),
            other => panic!("expected Help(Set), got {other:?}"),
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
    fn attach_with_mode_parses() {
        match parse_args(&args(&["attach", "demo", "--mode", "ro"])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.mode_str.as_deref(), Some("ro"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn attach_exclusive_sets_flag() {
        // DR-0020 §4: `--exclusive` は実装済 (= daemon handshake で占有判定)。
        match parse_args(&args(&["attach", "demo", "--exclusive"])) {
            Command::Attach(cfg) => {
                assert!(cfg.exclusive, "exclusive フラグが立つべき");
                assert!(!cfg.detach_others);
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn attach_detach_others_sets_flag() {
        // DR-0020 §4: `--detach-others` は実装済 (= daemon handshake で奪取)。
        match parse_args(&args(&["attach", "demo", "--detach-others"])) {
            Command::Attach(cfg) => {
                assert!(cfg.detach_others, "detach_others フラグが立つべき");
                assert!(!cfg.exclusive);
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Attach, got {other:?}"),
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

    /// 位置引数の数字は session-id 扱い (= kawaz 方針: index は --index 専用)。
    #[test]
    fn attach_positional_number_is_session_id() {
        match parse_args(&args(&["attach", "1"])) {
            Command::Attach(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("1"));
                assert_eq!(cfg.index, None);
            }
            other => panic!("expected Attach with session_id='1', got {other:?}"),
        }
    }

    /// `-1` のような short flag は attach に signal 概念がないので unknown option。
    #[test]
    fn attach_negative_short_flag_is_unknown_option() {
        match parse_args(&args(&["attach", "-1"])) {
            Command::Error(msg) => assert!(msg.contains("unknown") || msg.contains("-1")),
            other => panic!("expected Error for `-1`, got {other:?}"),
        }
    }

    /// `--index=N` option で index 設定 (= 1=最古、-1=最新、2=2番目に古い)。
    #[test]
    fn attach_index_option_sets_index() {
        for (input, expected) in [("--index=1", 1i32), ("--index=-1", -1), ("--index=2", 2)] {
            match parse_args(&args(&["attach", input])) {
                Command::Attach(cfg) => {
                    assert_eq!(cfg.index, Some(expected));
                    assert_eq!(cfg.session_id, None);
                }
                other => panic!("expected Attach({input}), got {other:?}"),
            }
        }
    }

    /// `--index=0` は不正 (= 1-based、0 は意味なし)。
    #[test]
    fn attach_index_zero_is_error() {
        match parse_args(&args(&["attach", "--index=0"])) {
            Command::Error(msg) => assert!(msg.contains("0"), "got msg={msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `--index` と位置引数の session-id 同時指定はエラー (= 排他)。
    #[test]
    fn attach_index_and_session_id_conflict() {
        match parse_args(&args(&["attach", "demo", "--index=1"])) {
            Command::Error(msg) => assert!(msg.contains("--index") || msg.contains("session")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `--index=foo` のような非整数値はエラー。
    #[test]
    fn attach_index_non_integer_is_error() {
        match parse_args(&args(&["attach", "--index=foo"])) {
            Command::Error(msg) => assert!(msg.contains("整数") || msg.contains("--index")),
            other => panic!("expected Error, got {other:?}"),
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
    fn usage_run_non_empty() {
        let text = usage(&HelpTopic::Run);
        assert!(text.contains("hyoui run"));
        assert!(text.contains("--on-child-suspend"));
        // run には --mode を出さない (= 削除済、attach の --mode とは別物)。
        assert!(!text.contains("--mode"));
        // parser が受理しない --on-parent-suspend を help に載せない。
        assert!(!text.contains("--on-parent-suspend"));
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

    // =========================================================================
    // DR-0018: --namespace / --all-namespaces parse tests
    // =========================================================================

    /// `run --namespace=t1` が `RunConfig.namespace = Some("t1")` を設定する。
    #[test]
    fn run_namespace_flag_sets_config() {
        match parse_args(&args(&["run", "--namespace=t1", "--", "cat"])) {
            Command::Run(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("t1")),
            other => panic!("expected Run(namespace=t1), got {other:?}"),
        }
    }

    /// `run` で `--namespace` 未指定なら `None` (= 実行時に env / default へ fallback)。
    #[test]
    fn run_namespace_default_is_none() {
        match parse_args(&args(&["run", "--", "cat"])) {
            Command::Run(cfg) => assert_eq!(cfg.namespace, None),
            other => panic!("expected Run(namespace=None), got {other:?}"),
        }
    }

    /// namespace の validate: `/` 入り / `..` / 空文字は parse 段で reject。
    #[test]
    fn run_namespace_rejects_invalid_values() {
        for bad in ["a/b", "..", "../x", ""] {
            match parse_args(&args(&["run", &format!("--namespace={bad}"), "--", "cat"])) {
                Command::Error(msg) => {
                    assert!(
                        msg.contains("namespace"),
                        "error should mention namespace (bad={bad:?}): {msg}"
                    );
                }
                other => panic!("expected Error for namespace {bad:?}, got {other:?}"),
            }
        }
    }

    /// `default` という名前は予約 (= 直下マッピング) だが parse は通常通り通す。
    #[test]
    fn run_namespace_default_name_is_accepted() {
        match parse_args(&args(&["run", "--namespace=default", "--", "cat"])) {
            Command::Run(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("default")),
            other => panic!("expected Run(namespace=default), got {other:?}"),
        }
    }

    /// session-targeted 系 (= parse_session_targeted 経由) でも `--namespace` が効く。
    #[test]
    fn session_targeted_commands_accept_namespace() {
        match parse_args(&args(&["status", "demo", "--namespace=t1"])) {
            Command::Status(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("t1")),
            other => panic!("expected Status(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["tail", "demo", "--namespace=t1"])) {
            Command::Tail(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("t1")),
            other => panic!("expected Tail(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["wait", "demo", "READY", "--namespace=t1"])) {
            Command::Wait(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("t1")),
            other => panic!("expected Wait(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["kill", "demo", "--namespace=t1"])) {
            Command::Kill(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("t1")),
            other => panic!("expected Kill(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["attach", "demo", "--namespace=t1"])) {
            Command::Attach(cfg) => assert_eq!(cfg.namespace.as_deref(), Some("t1")),
            other => panic!("expected Attach(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["input", "demo", "text:hi", "--namespace=t1"])) {
            Command::Input(cmd) => assert_eq!(cmd.namespace.as_deref(), Some("t1")),
            other => panic!("expected Input(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["screen", "dump", "demo", "--namespace=t1"])) {
            Command::Screen(ScreenCommand::Dump(cfg)) => {
                assert_eq!(cfg.namespace.as_deref(), Some("t1"));
            }
            other => panic!("expected ScreenDump(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["record", "list", "demo", "--namespace=t1"])) {
            Command::Record(RecordCommand::List(cfg)) => {
                assert_eq!(cfg.namespace.as_deref(), Some("t1"));
            }
            other => panic!("expected RecordList(namespace=t1), got {other:?}"),
        }
    }

    /// `list --namespace=t1` / `--all-namespaces` の設定値と排他チェック。
    #[test]
    fn list_namespace_and_all_namespaces() {
        match parse_args(&args(&["list", "--namespace=t1"])) {
            Command::List(cfg) => {
                assert_eq!(cfg.namespace.as_deref(), Some("t1"));
                assert!(!cfg.all_namespaces);
            }
            other => panic!("expected List(namespace=t1), got {other:?}"),
        }
        match parse_args(&args(&["list", "--all-namespaces"])) {
            Command::List(cfg) => {
                assert!(cfg.all_namespaces);
                assert_eq!(cfg.namespace, None);
            }
            other => panic!("expected List(all_namespaces), got {other:?}"),
        }
        // 排他: 同時指定は error。
        match parse_args(&args(&["list", "--namespace=t1", "--all-namespaces"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("--namespace") && msg.contains("--all-namespaces"),
                    "error should mention both flags: {msg}"
                );
            }
            other => panic!("expected Error for exclusive flags, got {other:?}"),
        }
        // `--all-namespaces` は値を取らない。
        match parse_args(&args(&["list", "--all-namespaces=yes"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for valued --all-namespaces, got {other:?}"),
        }
        // list の `--namespace` で validate 違反は reject。
        match parse_args(&args(&["list", "--namespace=a/b"])) {
            Command::Error(msg) => {
                assert!(msg.contains("namespace"), "got: {msg}");
            }
            other => panic!("expected Error for invalid ns, got {other:?}"),
        }
    }

    /// `validate_namespace`: session_id と同等の whitelist + 文言が namespace 文脈。
    #[test]
    fn validate_namespace_whitelist_and_wording() {
        // 正常系: フラットな一意名。
        for ok in ["default", "t1", "workers", "task-12.x_y"] {
            validate_namespace(ok).unwrap_or_else(|e| panic!("{ok:?} should pass: {e}"));
        }
        // 異常系: `/` (= 将来の階層 ns 用に予約) / traversal / 空 / 制御文字。
        for bad in ["a/b", "/abs", "..", ".", "", "a\nb", "a b"] {
            let err = validate_namespace(bad).expect_err(&format!("{bad:?} must err"));
            assert!(
                err.contains("namespace"),
                "error should use namespace wording (bad={bad:?}): {err}"
            );
            assert!(
                !err.contains("session_id"),
                "error must not leak session_id wording (bad={bad:?}): {err}"
            );
        }
        // 過長 (= MAX_SESSION_ID_LEN 同等の 64 bytes 上限)。
        let too_long = "a".repeat(MAX_SESSION_ID_LEN + 1);
        assert!(validate_namespace(&too_long).is_err(), "65 bytes must err");
        let max_ok = "a".repeat(MAX_SESSION_ID_LEN);
        assert!(validate_namespace(&max_ok).is_ok(), "64 bytes must pass");
    }

    /// `--format=plain` は `ListFormat::Plain` を設定する (= default と同等だが明示)。
    #[test]
    fn list_format_plain_sets_plain() {
        match parse_args(&args(&["list", "--format=plain"])) {
            Command::List(cfg) => assert_eq!(cfg.format, ListFormat::Plain),
            other => panic!("expected List(format=Plain), got {other:?}"),
        }
    }

    /// `--format=jsonl` は `ListFormat::Jsonl` を設定する。
    #[test]
    fn list_format_jsonl_sets_jsonl() {
        match parse_args(&args(&["list", "--format=jsonl"])) {
            Command::List(cfg) => assert_eq!(cfg.format, ListFormat::Jsonl),
            other => panic!("expected List(format=Jsonl), got {other:?}"),
        }
    }

    /// `--format=<unknown>` は `Command::Error` を返す。
    #[test]
    fn list_format_unknown_value_is_error() {
        match parse_args(&args(&["list", "--format=yaml"])) {
            Command::Error(msg) => {
                assert!(msg.contains("yaml"), "error should mention rejected value");
                assert!(msg.contains("--format"), "error should mention --format");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `--format` の値なし (= `--format` 単独) は `Command::Error` を返す。
    #[test]
    fn list_format_requires_value() {
        match parse_args(&args(&["list", "--format"])) {
            Command::Error(msg) => {
                assert!(msg.contains("--format"), "error should mention --format");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `--prune-stale=true` のような値付きは `Command::Error` (= bool flag に値は取らない)。
    #[test]
    fn list_prune_stale_does_not_accept_value() {
        match parse_args(&args(&["list", "--prune-stale=true"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("--prune-stale"),
                    "error should mention --prune-stale"
                );
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
            (HelpTopic::Run, "hyoui run", &["--cols", "--timeout"]),
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

    /// DR-0017 §柱2: `--no-terminate` が cfg.no_terminate=true で格納され、
    /// `--signal` と併用できる (= stopped child を CONT で起こす経路)。
    #[test]
    fn parse_kill_no_terminate_with_signal() {
        match parse_args(&args(&["kill", "demo", "--signal=CONT", "--no-terminate"])) {
            Command::Kill(cfg) => {
                assert!(cfg.no_terminate, "--no-terminate must set no_terminate");
                assert_eq!(cfg.signal.as_deref(), Some("SIGCONT"));
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Kill(no_terminate, signal=SIGCONT), got {other:?}"),
        }
    }

    /// DR-0017 §柱2: `--no-terminate` は `--all` と併用不可。
    #[test]
    fn parse_kill_no_terminate_rejects_all() {
        match parse_args(&args(&["kill", "--all", "--no-terminate"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("no-terminate") && msg.contains("all"),
                    "error should mention both flags: {msg}"
                );
            }
            other => panic!("expected Error for --all --no-terminate, got {other:?}"),
        }
    }

    /// 即時応答化: `--wait` 未指定なら cfg.wait=false (= default 即時 return)。
    #[test]
    fn parse_kill_default_is_immediate() {
        match parse_args(&args(&["kill", "demo"])) {
            Command::Kill(cfg) => {
                assert!(!cfg.wait, "default kill must be immediate (wait=false)");
                assert!(!cfg.no_terminate, "default kill must not be no_terminate");
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Kill(wait=false), got {other:?}"),
        }
    }

    /// 即時応答化: `--wait` が cfg.wait=true で格納される (= 従来挙動)。
    #[test]
    fn parse_kill_wait_flag() {
        match parse_args(&args(&["kill", "demo", "--wait"])) {
            Command::Kill(cfg) => {
                assert!(cfg.wait, "--wait must set wait=true");
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Kill(wait=true), got {other:?}"),
        }
        // --signal との併用 OK。
        match parse_args(&args(&["kill", "demo", "--signal=KILL", "--wait"])) {
            Command::Kill(cfg) => {
                assert!(cfg.wait);
                assert_eq!(cfg.signal.as_deref(), Some("SIGKILL"));
            }
            other => panic!("expected Kill(wait=true, signal=SIGKILL), got {other:?}"),
        }
    }

    /// 2 軸の整理: `--wait` は `--no-terminate` と併用不可。
    #[test]
    fn parse_kill_wait_rejects_no_terminate() {
        match parse_args(&args(&["kill", "demo", "--wait", "--no-terminate"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("wait") && msg.contains("no-terminate"),
                    "error should mention both flags: {msg}"
                );
            }
            other => panic!("expected Error for --wait --no-terminate, got {other:?}"),
        }
        // 順序を入れ替えても同じ。
        match parse_args(&args(&["kill", "demo", "--no-terminate", "--wait"])) {
            Command::Error(_) => {}
            other => panic!("expected Error for --no-terminate --wait, got {other:?}"),
        }
    }

    /// 即時応答化: `--wait` は `--all` と併用可 (= killall で各 session の終了を見届け)。
    #[test]
    fn parse_kill_wait_with_all_ok() {
        match parse_args(&args(&["kill", "--all", "--wait"])) {
            Command::Kill(cfg) => {
                assert!(cfg.all);
                assert!(cfg.wait);
            }
            other => panic!("expected Kill(all=true, wait=true), got {other:?}"),
        }
    }

    /// 裸 `--wait` (= 値なし) は default timeout 10s が入る。
    #[test]
    fn parse_kill_bare_wait_sets_default_timeout() {
        match parse_args(&args(&["kill", "demo", "--wait"])) {
            Command::Kill(cfg) => {
                assert!(cfg.wait);
                assert_eq!(
                    cfg.wait_timeout_ms,
                    Some(KILL_WAIT_DEFAULT_TIMEOUT_MS),
                    "bare --wait must default to {KILL_WAIT_DEFAULT_TIMEOUT_MS}ms"
                );
            }
            other => panic!("expected Kill, got {other:?}"),
        }
    }

    /// `--wait=<DUR>` は既存 DUR 形式で timeout を上書きする。
    #[test]
    fn parse_kill_wait_with_duration() {
        match parse_args(&args(&["kill", "demo", "--wait=2s"])) {
            Command::Kill(cfg) => {
                assert!(cfg.wait);
                assert_eq!(cfg.wait_timeout_ms, Some(2_000));
            }
            other => panic!("expected Kill(wait=2s), got {other:?}"),
        }
        match parse_args(&args(&["kill", "demo", "--wait=500ms"])) {
            Command::Kill(cfg) => assert_eq!(cfg.wait_timeout_ms, Some(500)),
            other => panic!("expected Kill(wait=500ms), got {other:?}"),
        }
        // 不正 DUR は parse error。
        match parse_args(&args(&["kill", "demo", "--wait=abc"])) {
            Command::Error(msg) => assert!(msg.contains("--wait"), "msg: {msg}"),
            other => panic!("expected Error for --wait=abc, got {other:?}"),
        }
    }

    /// `--wait demo` の `demo` は session-id (= 次 arg を timeout として消費しない)。
    #[test]
    fn parse_kill_bare_wait_does_not_consume_next_arg() {
        match parse_args(&args(&["kill", "--wait", "demo"])) {
            Command::Kill(cfg) => {
                assert!(cfg.wait);
                assert_eq!(cfg.wait_timeout_ms, Some(KILL_WAIT_DEFAULT_TIMEOUT_MS));
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Kill(wait, session=demo), got {other:?}"),
        }
    }

    /// `--kill-on-timeout` は `--wait` 必須 (= 単独指定は parse error)。
    #[test]
    fn parse_kill_kill_on_timeout_requires_wait() {
        match parse_args(&args(&["kill", "demo", "--kill-on-timeout"])) {
            Command::Error(msg) => {
                assert!(
                    msg.contains("--kill-on-timeout") && msg.contains("--wait"),
                    "error should mention both flags: {msg}"
                );
            }
            other => panic!("expected Error for --kill-on-timeout without --wait, got {other:?}"),
        }
        // --wait と併用すれば OK。
        match parse_args(&args(&["kill", "demo", "--wait=2s", "--kill-on-timeout"])) {
            Command::Kill(cfg) => {
                assert!(cfg.wait);
                assert!(cfg.kill_on_timeout);
                assert_eq!(cfg.wait_timeout_ms, Some(2_000));
            }
            other => panic!("expected Kill(kill_on_timeout), got {other:?}"),
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

    /// kawaz 方針 2026-05-30: POSIX kill 慣習に揃えて略名 / 数字 / 小文字を accept、
    /// CLI 段で正規 SIG-prefix 大文字に normalize して wire に流す。daemon 側は
    /// 引き続き SIG-prefix 大文字のみ解釈 (defense in depth)。
    #[test]
    fn parse_kill_accepts_aliases_and_numbers_and_lowercase() {
        let cases = [
            ("TERM", "SIGTERM"),
            ("KILL", "SIGKILL"),
            ("sigterm", "SIGTERM"),
            ("sigkill", "SIGKILL"),
            ("term", "SIGTERM"),
            ("15", "SIGTERM"),
            ("9", "SIGKILL"),
            ("1", "SIGHUP"),
            ("SIGINT", "SIGINT"),
        ];
        for (input, expected_wire) in cases {
            match parse_args(&args(&["kill", "demo", "--signal", input])) {
                Command::Kill(cfg) => {
                    assert_eq!(
                        cfg.signal.as_deref(),
                        Some(expected_wire),
                        "input {input:?} should normalize to {expected_wire}"
                    );
                }
                other => panic!("expected Kill cfg for `--signal {input}`, got {other:?}"),
            }
        }
    }

    /// 不正な signal spec (= 完全未知の文字列 / 範囲外数字) はエラー。
    #[test]
    fn parse_kill_rejects_truly_invalid_signal() {
        for bogus in &["SIG", "sig_term", "FOOBAR", "999"] {
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

    /// POSIX kill 慣習: `-9` / `-KILL` / `-SIGKILL` 等の short flag を signal とみなす。
    #[test]
    fn parse_kill_short_flag_signal() {
        let cases = [
            ("-9", "SIGKILL"),
            ("-15", "SIGTERM"),
            ("-1", "SIGHUP"),
            ("-KILL", "SIGKILL"),
            ("-TERM", "SIGTERM"),
            ("-SIGINT", "SIGINT"),
            ("-sigterm", "SIGTERM"),
        ];
        for (input, expected_wire) in cases {
            match parse_args(&args(&["kill", input, "demo"])) {
                Command::Kill(cfg) => {
                    assert_eq!(
                        cfg.signal.as_deref(),
                        Some(expected_wire),
                        "short flag {input:?} should normalize to {expected_wire}"
                    );
                    assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                }
                other => panic!("expected Kill cfg for `{input} demo`, got {other:?}"),
            }
        }
    }

    /// `--all` flag は cfg.all を true に。session-id / --index と排他。
    #[test]
    fn parse_kill_all_flag() {
        match parse_args(&args(&["kill", "--all"])) {
            Command::Kill(cfg) => {
                assert!(cfg.all);
                assert_eq!(cfg.session_id, None);
                assert_eq!(cfg.index, None);
            }
            other => panic!("expected Kill(all=true), got {other:?}"),
        }
        // session-id と排他
        match parse_args(&args(&["kill", "--all", "demo"])) {
            Command::Error(msg) => assert!(msg.contains("--all") || msg.contains("排他")),
            other => panic!("expected Error for --all+positional, got {other:?}"),
        }
        // --index と排他
        match parse_args(&args(&["kill", "--all", "--index=1"])) {
            Command::Error(msg) => assert!(msg.contains("--all") || msg.contains("排他")),
            other => panic!("expected Error for --all+index, got {other:?}"),
        }
    }

    /// 位置引数の正数は index 解釈 (= 1 番古い session を指す)、負数は signal。
    #[test]
    fn parse_kill_positional_semantics() {
        // 位置引数の数字も session-id 扱い (kawaz 方針: index は --index 専用)
        match parse_args(&args(&["kill", "2"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("2"));
                assert_eq!(cfg.index, None);
                assert_eq!(cfg.signal, None);
            }
            other => panic!("expected Kill(session=\"2\"), got {other:?}"),
        }
        // `-9` 等の short flag は signal 解釈 (POSIX kill 慣習)
        match parse_args(&args(&["kill", "-9", "demo"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.signal.as_deref(), Some("SIGKILL"));
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.index, None);
            }
            other => panic!("expected Kill(signal=SIGKILL, session=demo), got {other:?}"),
        }
    }

    /// `--` セパレータで `-` 始まる session-id を escape できる。
    #[test]
    fn parse_kill_dashdash_escape() {
        match parse_args(&args(&["kill", "--", "-dash-id"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.session_id.as_deref(), Some("-dash-id"));
                assert_eq!(cfg.index, None);
                assert_eq!(cfg.signal, None);
            }
            other => panic!("expected Kill(session=\"-dash-id\"), got {other:?}"),
        }
    }

    /// `--index=N` で index 指定 (= 正/負/0 範囲外)。
    #[test]
    fn parse_kill_index_option() {
        match parse_args(&args(&["kill", "--index=1"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.index, Some(1));
                assert_eq!(cfg.session_id, None);
            }
            other => panic!("expected Kill(index=1), got {other:?}"),
        }
        match parse_args(&args(&["kill", "--index=-1"])) {
            Command::Kill(cfg) => {
                assert_eq!(cfg.index, Some(-1));
            }
            other => panic!("expected Kill(index=-1), got {other:?}"),
        }
        match parse_args(&args(&["kill", "--index=0"])) {
            Command::Error(msg) => assert!(msg.contains("0"), "got msg={msg}"),
            other => panic!("expected Error for --index=0, got {other:?}"),
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

    // ─────────────────────────────────────────────────────────────────────────
    // DR-0016 Phase 7: `record` subcommand (start/stop/list) parser tests
    // ─────────────────────────────────────────────────────────────────────────

    /// `record start --output /abs.jsonl <session>` の最小受理形を確認 (= default を全部適用)。
    #[test]
    fn record_start_minimal_args() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/rec.jsonl",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
                assert_eq!(cfg.direction, RecordDirectionArg::Both);
                assert_eq!(cfg.format, RecordFormatArg::Jsonl);
                assert_eq!(cfg.input_secrecy, RecordInputSecrecyArg::RedactAfterPrompt);
                assert_eq!(cfg.output_path, PathBuf::from("/tmp/rec.jsonl"));
                // default 100 MiB / 1h が wire 値に適用される
                assert_eq!(cfg.max_bytes, Some(100 * 1024 * 1024));
                assert_eq!(cfg.max_duration_ms, Some(60 * 60 * 1000));
                assert!(!cfg.max_bytes_disabled);
                assert!(!cfg.max_duration_disabled);
                assert!(cfg.prompt_pattern.is_none());
            }
            other => panic!("expected Command::Record(Start), got {other:?}"),
        }
    }

    /// `--format=raw` + default `--both` (= 未指定) は parse 段で reject されることを確認。
    #[test]
    fn record_start_raw_both_rejected() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/rec.bin",
            "--format=raw",
        ]));
        match cmd {
            Command::Error(msg) => {
                assert!(
                    msg.contains("raw") && msg.contains("--both"),
                    "expected raw+both rejection, got {msg}"
                );
            }
            other => panic!("expected Command::Error, got {other:?}"),
        }
    }

    /// `--format=raw --stdout` は受理されることを確認。
    #[test]
    fn record_start_raw_with_stdout_ok() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/out.bin",
            "--format=raw",
            "--stdout",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.format, RecordFormatArg::Raw);
                assert_eq!(cfg.direction, RecordDirectionArg::Stdout);
            }
            other => panic!("expected Start with raw+stdout, got {other:?}"),
        }
    }

    /// `--format=raw --stdin` も受理 (= 対称性確認)。
    #[test]
    fn record_start_raw_with_stdin_ok() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/in.bin",
            "--format=raw",
            "--stdin",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.format, RecordFormatArg::Raw);
                assert_eq!(cfg.direction, RecordDirectionArg::Stdin);
            }
            other => panic!("expected Start with raw+stdin, got {other:?}"),
        }
    }

    /// `--format=raw --both` (= 明示) も reject されることを確認。
    #[test]
    fn record_start_raw_explicit_both_rejected() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.bin",
            "--format=raw",
            "--both",
        ]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("raw")));
    }

    /// `--output` に相対 path を与えると CLI 段で reject されることを確認 (= daemon
    /// 接続前の早期 fail で integration test を成立させる前提)。
    #[test]
    fn record_start_output_must_be_absolute() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "./relative.jsonl",
        ]));
        match cmd {
            Command::Error(msg) => {
                assert!(
                    msg.contains("absolute"),
                    "expected absolute-path rejection, got {msg}"
                );
            }
            other => panic!("expected Command::Error, got {other:?}"),
        }
    }

    /// `--output` を省略すると error (= 必須 flag)。
    #[test]
    fn record_start_output_required() {
        let cmd = parse_args(&args(&["record", "start", "demo"]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("--output")));
    }

    /// `--input-secrecy` の 3 variant がそれぞれ parse 可能。
    #[test]
    fn record_start_input_secrecy_variants() {
        for (val, expected) in [
            (
                "redact-after-prompt",
                RecordInputSecrecyArg::RedactAfterPrompt,
            ),
            ("record-all", RecordInputSecrecyArg::RecordAll),
            (
                "never-record-stdin",
                RecordInputSecrecyArg::NeverRecordStdin,
            ),
        ] {
            let cmd = parse_args(&args(&[
                "record",
                "start",
                "demo",
                "--output",
                "/tmp/x.jsonl",
                "--input-secrecy",
                val,
            ]));
            match cmd {
                Command::Record(RecordCommand::Start(cfg)) => {
                    assert_eq!(cfg.input_secrecy, expected, "{val} 不一致");
                }
                other => panic!("{val}: expected Start, got {other:?}"),
            }
        }
    }

    /// 未知 `--input-secrecy` 値は error。
    #[test]
    fn record_start_input_secrecy_unknown_errors() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--input-secrecy=bogus",
        ]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("input-secrecy")));
    }

    /// `--max-bytes 0` で wire 値が `None` (= disable) になり、`max_bytes_disabled = true`
    /// が立つ (= main.rs が loud warning を出す根拠)。stderr 出力は CLI 層では行わない
    /// (= cli.rs は pure module)。
    #[test]
    fn record_start_max_bytes_zero_disables_with_flag() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--max-bytes",
            "0",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.max_bytes, None);
                assert!(cfg.max_bytes_disabled);
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// `--max-bytes 5m` は 5 MiB (= 5 * 1024²) として解釈される。
    #[test]
    fn record_start_max_bytes_suffix_mib() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--max-bytes",
            "5m",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.max_bytes, Some(5 * 1024 * 1024));
                assert!(!cfg.max_bytes_disabled);
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// `--max-bytes 1g` で 1 GiB として解釈 (= 各 suffix の動作確認)。
    #[test]
    fn record_start_max_bytes_suffix_gib() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--max-bytes=1g",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.max_bytes, Some(1024 * 1024 * 1024));
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// `--max-duration 0` で wire 値が `None`、disable flag が立つ。
    #[test]
    fn record_start_max_duration_zero_disables_with_flag() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--max-duration",
            "0",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.max_duration_ms, None);
                assert!(cfg.max_duration_disabled);
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// `--max-duration 30m` は 30 分 = 1_800_000 ms。
    #[test]
    fn record_start_max_duration_human_readable() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--max-duration",
            "30m",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.max_duration_ms, Some(30 * 60 * 1000));
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// 複数 direction flag (= `--stdin --stdout` 同時指定) は error。
    #[test]
    fn record_start_multiple_direction_flags_rejected() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--stdin",
            "--stdout",
        ]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("同時")));
    }

    /// `--prompt-pattern <regex>` が config に乗ること。
    #[test]
    fn record_start_prompt_pattern() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--prompt-pattern",
            "(?i)passcode",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.prompt_pattern.as_deref(), Some("(?i)passcode"));
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// 空 `--prompt-pattern` は reject。
    #[test]
    fn record_start_empty_prompt_pattern_rejected() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "demo",
            "--output",
            "/tmp/x.jsonl",
            "--prompt-pattern",
            "",
        ]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("prompt-pattern")));
    }

    /// `--socket` 指定で session selector が満たせること (= 位置引数なしでも OK)。
    #[test]
    fn record_start_socket_alternative() {
        let cmd = parse_args(&args(&[
            "record",
            "start",
            "--socket=/tmp/foo.sock",
            "--output",
            "/tmp/x.jsonl",
        ]));
        match cmd {
            Command::Record(RecordCommand::Start(cfg)) => {
                assert_eq!(cfg.socket.as_deref(), Some("/tmp/foo.sock"));
                assert!(cfg.session_id.is_none());
            }
            other => panic!("expected Start with socket, got {other:?}"),
        }
    }

    /// `record stop --id 1 --all` 両指定は error。
    #[test]
    fn record_stop_id_and_all_conflict() {
        let cmd = parse_args(&args(&["record", "stop", "demo", "--id", "1", "--all"]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("--id") && m.contains("--all")));
    }

    /// `record stop` で `--id` も `--all` も省略は OK (= main.rs 側 auto-select 経路)。
    #[test]
    fn record_stop_neither_id_nor_all_ok() {
        let cmd = parse_args(&args(&["record", "stop", "demo"]));
        match cmd {
            Command::Record(RecordCommand::Stop(cfg)) => {
                assert!(cfg.record_id.is_none());
                assert!(!cfg.all);
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    /// `record stop --id 42` で record_id が乗ること。
    #[test]
    fn record_stop_id_only() {
        let cmd = parse_args(&args(&["record", "stop", "demo", "--id", "42"]));
        match cmd {
            Command::Record(RecordCommand::Stop(cfg)) => {
                assert_eq!(cfg.record_id, Some(42));
                assert!(!cfg.all);
            }
            other => panic!("expected Stop with id, got {other:?}"),
        }
    }

    /// `record stop --all` で all=true。
    #[test]
    fn record_stop_all_only() {
        let cmd = parse_args(&args(&["record", "stop", "demo", "--all"]));
        match cmd {
            Command::Record(RecordCommand::Stop(cfg)) => {
                assert!(cfg.all);
                assert!(cfg.record_id.is_none());
            }
            other => panic!("expected Stop with all, got {other:?}"),
        }
    }

    /// `record stop --id <負数>` は u32 parse 失敗 → error。
    #[test]
    fn record_stop_negative_id_rejected() {
        let cmd = parse_args(&args(&["record", "stop", "demo", "--id", "-1"]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("--id")));
    }

    /// `record list` default format は table。
    #[test]
    fn record_list_format_default_table() {
        let cmd = parse_args(&args(&["record", "list", "demo"]));
        match cmd {
            Command::Record(RecordCommand::List(cfg)) => {
                assert_eq!(cfg.format, RecordListFormatArg::Table);
                assert_eq!(cfg.session_id.as_deref(), Some("demo"));
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    /// `record list --format=jsonl`。
    #[test]
    fn record_list_format_jsonl() {
        let cmd = parse_args(&args(&["record", "list", "demo", "--format=jsonl"]));
        match cmd {
            Command::Record(RecordCommand::List(cfg)) => {
                assert_eq!(cfg.format, RecordListFormatArg::Jsonl);
            }
            other => panic!("expected List with jsonl, got {other:?}"),
        }
    }

    /// `record list --format=bad` は error。
    #[test]
    fn record_list_unknown_format_rejected() {
        let cmd = parse_args(&args(&["record", "list", "demo", "--format=bad"]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("--format")));
    }

    /// `hyoui record` 引数なし → 親 help を出す (= `parse_screen` / `parse_lock` と同流儀)。
    #[test]
    fn record_no_args_shows_parent_help() {
        let cmd = parse_args(&args(&["record"]));
        assert!(matches!(
            cmd,
            Command::Help {
                topic: HelpTopic::Record
            }
        ));
    }

    /// `record --help` も親 help。
    #[test]
    fn record_help_flag_shows_parent_help() {
        let cmd = parse_args(&args(&["record", "--help"]));
        assert!(matches!(
            cmd,
            Command::Help {
                topic: HelpTopic::Record
            }
        ));
    }

    /// 未知 record subcommand は edit distance suggest 付き error。
    #[test]
    fn record_unknown_subcommand_errors() {
        let cmd = parse_args(&args(&["record", "startt", "demo"]));
        match cmd {
            Command::Error(msg) => {
                assert!(msg.contains("unknown subcommand"), "got {msg}");
                // edit distance 1 で `start` が suggest されるはず
                assert!(msg.contains("start"), "got {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// session selector が無いと error (= status / tail / wait と同一規則)。
    #[test]
    fn record_start_requires_session_or_socket() {
        let cmd = parse_args(&args(&["record", "start", "--output", "/tmp/x.jsonl"]));
        assert!(matches!(cmd, Command::Error(ref m) if m.contains("session")));
    }

    /// usage が record subcommands を列挙する。
    #[test]
    fn usage_record_lists_subcommands() {
        let text = usage(&HelpTopic::Record);
        assert!(text.contains("start"));
        assert!(text.contains("stop"));
        assert!(text.contains("list"));
    }

    /// `record start` usage が `--input-secrecy` / `--max-bytes` / `--output` を含む。
    #[test]
    fn usage_record_start_lists_key_options() {
        let text = usage(&HelpTopic::RecordStart);
        assert!(text.contains("--output"));
        assert!(text.contains("--max-bytes"));
        assert!(text.contains("--input-secrecy"));
        assert!(text.contains("--format=jsonl|raw"));
    }

    /// `record stop` usage が `--id` / `--all` を含む。
    #[test]
    fn usage_record_stop_lists_key_options() {
        let text = usage(&HelpTopic::RecordStop);
        assert!(text.contains("--id"));
        assert!(text.contains("--all"));
    }

    /// `record list` usage が `--format=table|jsonl` を含む。
    #[test]
    fn usage_record_list_lists_key_options() {
        let text = usage(&HelpTopic::RecordList);
        assert!(text.contains("--format"));
        assert!(text.contains("table"));
        assert!(text.contains("jsonl"));
    }

    /// top-level usage に record が列挙される。
    #[test]
    fn usage_top_lists_record() {
        let text = usage(&HelpTopic::Top);
        assert!(text.contains("record"));
    }

    /// `parse_max_bytes` の suffix 解釈 unit test。
    #[test]
    fn parse_max_bytes_suffix_variants() {
        assert_eq!(parse_max_bytes("0").unwrap(), 0);
        assert_eq!(parse_max_bytes("1024").unwrap(), 1024);
        assert_eq!(parse_max_bytes("1k").unwrap(), 1024);
        assert_eq!(parse_max_bytes("1K").unwrap(), 1024);
        assert_eq!(parse_max_bytes("1kb").unwrap(), 1024);
        assert_eq!(parse_max_bytes("1KiB").unwrap(), 1024);
        assert_eq!(parse_max_bytes("1m").unwrap(), 1024 * 1024);
        assert_eq!(parse_max_bytes("1MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_max_bytes("1MiB").unwrap(), 1024 * 1024);
        assert_eq!(parse_max_bytes("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_max_bytes("100b").unwrap(), 100);
    }

    /// `parse_max_bytes` の invalid 形式は error。
    #[test]
    fn parse_max_bytes_invalid() {
        assert!(parse_max_bytes("").is_err());
        assert!(parse_max_bytes("abc").is_err());
        assert!(parse_max_bytes("1.5m").is_err()); // decimal 非対応
    }
}
