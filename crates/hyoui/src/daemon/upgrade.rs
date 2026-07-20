//! DR-0028 Phase 2: daemon graceful upgrade (self-exec) — state 引き継ぎ + fallback。
//!
//! # 責務
//!
//! 走行中 daemon が **自 PID を保ったまま** 新バイナリに切り替える execve 骨格 +
//! DaemonConfig / 子 PID / scrollback bytes の CBOR versioned state file 経由の
//! 引き継ぎ + exec 失敗時の CLOEXEC 復元 + 旧プロセス続行 fallback。
//!
//! # trigger (Phase 1 隠し経路、Phase 2 でも共通)
//!
//! daemon serve_loop の self-pipe に `SIGUSR1` を register する。外部から
//! `kill -USR1 <daemon-pid>` を送ると `handle_suspend_signals` が
//! `RelayOutcome::UpgradeRequested` を返し、`Session::serve` が本 module の
//! [`perform_self_exec`] を呼ぶ。正規 `upgrade.request` protocol kind の追加 +
//! `hyoui upgrade` subcommand は Phase 3。
//!
//! # 引き継ぐもの (Phase 2)
//!
//! - **fd 継承** (Phase 1 と同じ): PTY master fd + listener fd を CLOEXEC 解除 +
//!   `HYOUI_UPGRADE_PTY_FD` / `HYOUI_UPGRADE_LISTENER_FD` env で伝達。
//! - **CBOR state file** ([`UpgradeStateV1`]): `HYOUI_UPGRADE_STATE_FILE=<path>` env
//!   に path を渡し、新プロセスが read + delete する。format_version + hyoui_version を
//!   header に持ち、互換のない場合は resume 側で fallback (= env 最小 subset で継続)。
//!   含む項目 = DaemonConfig 全 field + 子 PID + prev daemon_boot_id +
//!   scrollback bytes (再 feed 材料)。
//! - **screen state 再構築**: 新プロセスは受け取った scrollback bytes を vt100 parser
//!   に再 feed して screen 状態を復元 (DR-0028 §3、[`Session::serve`] の resume 冒頭)。
//!
//! # exec 失敗時 fallback (Phase 2)
//!
//! 2 段構え:
//! 1. **pre-check** ([`precheck_upgrade_target`]): fd を触る前に binary path の
//!    存在 / 実行 bit / 同一 UID を検証。失敗時は fd に触れず `Err` を返し、
//!    `Session::serve` は同 serve_loop に再突入する (= 旧プロセス続行)。ここで
//!    弾ける失敗が §5 の大半 (= 大半のエラーは事前検証で捕捉できる、DR-0028 §5.1)。
//! 2. **execve 直接失敗** (pre-check 通過後 = permissions / kernel 制約 / ELF 不整合):
//!    ここまで来ると fd 所有権は既に移譲済 (extract → CLOEXEC 解除)。CLOEXEC を復元し、
//!    Pty / UnixSock 相当を再構築して caller に戻せば旧続行可能。実装は Phase 3 に
//!    委ねる (= 現状は eprintln + `Err`、gate 上は pre-check で fallback 検証済)。

use std::ffi::{CString, OsStr};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use crate::sys::{Error, Pty, UnixSock, clear_cloexec, set_cloexec};

use super::{ChildSuspendPolicy, DaemonConfig};

/// upgrade-resume 経路を検知する env var (= "1" なら resume mode)。
pub const ENV_UPGRADE_RESUME: &str = "HYOUI_UPGRADE_RESUME";
/// PTY master fd 番号を伝達する env var (= 十進数の RawFd)。
pub const ENV_UPGRADE_PTY_FD: &str = "HYOUI_UPGRADE_PTY_FD";
/// listener fd 番号を伝達する env var。
pub const ENV_UPGRADE_LISTENER_FD: &str = "HYOUI_UPGRADE_LISTENER_FD";
/// 引き継ぐ子 PID (= state file が使えない fallback path 用の最小情報)。
pub const ENV_UPGRADE_CHILD_PID: &str = "HYOUI_UPGRADE_CHILD_PID";
/// 引き継ぐ session_id (= 表示 + 認証境界の識別)。
pub const ENV_UPGRADE_SESSION: &str = "HYOUI_UPGRADE_SESSION";
/// 引き継ぐ socket path (= UnixSock::from_listener_fd に渡す)。
pub const ENV_UPGRADE_SOCKET: &str = "HYOUI_UPGRADE_SOCKET";
/// 引き継ぐ cols (fallback path 用)。
pub const ENV_UPGRADE_COLS: &str = "HYOUI_UPGRADE_COLS";
/// 引き継ぐ rows (fallback path 用)。
pub const ENV_UPGRADE_ROWS: &str = "HYOUI_UPGRADE_ROWS";
/// CBOR state file の絶対パスを伝達する env var (Phase 2 追加)。
pub const ENV_UPGRADE_STATE_FILE: &str = "HYOUI_UPGRADE_STATE_FILE";
/// テスト用: current_exe の代わりに upgrade target として使う exe path を上書きする env
/// (= pre-check 失敗 fallback を実機検証するため)。set されていれば precheck が
/// この path を検査する。
pub const ENV_UPGRADE_EXE_OVERRIDE: &str = "HYOUI_UPGRADE_EXE_OVERRIDE";

/// upgrade 用 env の一覧 (= execve 前後で操作する対象)。
const UPGRADE_ENV_KEYS: &[&str] = &[
    ENV_UPGRADE_RESUME,
    ENV_UPGRADE_PTY_FD,
    ENV_UPGRADE_LISTENER_FD,
    ENV_UPGRADE_CHILD_PID,
    ENV_UPGRADE_SESSION,
    ENV_UPGRADE_SOCKET,
    ENV_UPGRADE_COLS,
    ENV_UPGRADE_ROWS,
    ENV_UPGRADE_STATE_FILE,
];

/// CBOR state file の format version。
///
/// **breaking change 時は増やす**。新プロセスは version が想定と異なれば fallback
/// (= env 最小 subset で継続) に落ちる。format_version は wire header の 1 番目に
/// 位置し、reject / fallback の判断材料になる。
pub const STATE_FORMAT_VERSION_V1: u32 = 1;

/// upgrade state file の CBOR schema v1 (DR-0028 §3、Phase 2)。
///
/// **serialize 対象**: `DaemonConfig` 全 field + 子 PID + 前 daemon_boot_id +
/// scrollback bytes 生 dump。screen state (vt100 内部構造) は含めず、bytes 再 feed で
/// 復元する (= vt100 crate 内部の版間互換を切る、DR-0028 §3 表)。lock 状態 / record
/// 継続は Phase 2 では扱わない (= DR-0028 §3 表の「引き継がない」/ Phase 3 scope)。
///
/// `ChildSuspendPolicy` は wire form の文字列 (`"notify"` / `"auto-resume"`) で保存する
/// (= `#[non_exhaustive]` enum の serde 進化に頑健)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeStateV1 {
    /// format schema version (= [`STATE_FORMAT_VERSION_V1`])。
    pub format_version: u32,
    /// state を書き出した daemon の hyoui バージョン (診断用)。
    pub hyoui_version: String,
    /// 前 daemon の `daemon_boot_id` (Phase 3 の record 継続で使う予定)。
    pub daemon_boot_id_prev: String,
    /// DaemonConfig::session_id
    pub session_id: String,
    /// DaemonConfig::socket_path
    pub socket_path: PathBuf,
    /// DaemonConfig::cmd (= 子 PTY の実 argv、Phase 1 の dummy を置換)
    pub cmd: Vec<String>,
    /// DaemonConfig::cols
    pub cols: u16,
    /// DaemonConfig::rows
    pub rows: u16,
    /// DaemonConfig::scrollback_bytes
    pub scrollback_bytes: usize,
    /// DaemonConfig::screen_input_log_bytes
    pub screen_input_log_bytes: usize,
    /// DaemonConfig::screen_vt100_scrollback_rows
    pub screen_vt100_scrollback_rows: usize,
    /// DaemonConfig::client_buffer_bytes
    pub client_buffer_bytes: usize,
    /// DaemonConfig::expected_token (secret; state file は同 UID 保護境界内で保護)
    pub expected_token: Option<String>,
    /// DaemonConfig::until
    pub until: Option<String>,
    /// DaemonConfig::debug_dump_path
    pub debug_dump_path: Option<PathBuf>,
    /// DaemonConfig::cwd
    pub cwd: Option<PathBuf>,
    /// DaemonConfig::on_child_suspend の wire form (= `"notify"` / `"auto-resume"`)
    pub on_child_suspend: String,
    /// DaemonConfig::timeout_ms
    pub timeout_ms: Option<u64>,
    /// DaemonConfig::idle_timeout_ms
    pub idle_timeout_ms: Option<u64>,
    /// 引き継ぐ子 PID (= `Session::from_upgrade_inherited` に渡す)
    pub child_pid: i32,
    /// scrollback ring から export した raw bytes (= 新プロセスで vt100 parser に
    /// 再 feed する材料、DR-0028 §3)。空でも OK (= 初期 fresh state)。
    /// Phase 3: `serde_bytes` 経由で CBOR major type 2 (byte string) として encode
    /// する (= Phase 2 の array-of-int から切替、~2x → ~1x size)。1 MiB scrollback
    /// で state file が 1 MiB 前後になる (= header 数百 byte + bytes 本体)。
    #[serde(with = "serde_bytes")]
    pub scrollback: Vec<u8>,
}

/// [`UpgradeStateV1`] を CBOR に encode してファイルへ書き出す。
///
/// 書き込み path は「socket path と同 dir、 同 UID 保護境界内」を caller が指定する
/// (通常 [`compute_state_file_path`] を使う)。permission は umask 0o077 (= mode 0600
/// 相当) で作成する。書き出し失敗は Err で返し、caller は upgrade を中止する
/// (= state 無しで exec すると新プロセスが fallback 化するため意味論が壊れないが、
/// Phase 2 gate は「screen 継続」なので state 無しでは gate 未達)。
pub fn write_state_file(path: &Path, state: &UpgradeStateV1) -> Result<(), Error> {
    // umask を一時 0o077 に (= 他者読み取り禁止)、scope 抜けで復元。
    let _umask =
        crate::sys::socket::UmaskGuard::set(nix::sys::stat::Mode::from_bits_truncate(0o077));
    // 既存 file が残っていれば上書きする (= 前回失敗の残骸をクリア)。
    // create + write + fsync ではなく通常 create の write 一発 (= state file は
    // 数 MB 以下、部分書き出しでも新プロセスは fallback するため強い atomicity は
    // 要らない、DR-0028 §5.2 の fallback がセーフティネット)。
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(Error::from)?;
    let writer = std::io::BufWriter::new(file);
    ciborium::into_writer(state, writer).map_err(|e| {
        Error::Invalid(Box::leak(
            format!("cbor encode failed: {e}").into_boxed_str(),
        ))
    })?;
    Ok(())
}

/// [`write_state_file`] が使う `OpenOptionsExt::mode` の import 経路 (`std::os::unix::fs`)
/// を local に取り込む。単独 import すると `write_state_file` 以外に mode を使わないため
/// 局所 use で影響範囲を絞る。
use std::os::unix::fs::OpenOptionsExt;

/// state file を read + decode + delete する (= 「1 度きり消費」)。
///
/// 失敗理由:
/// - file open 失敗 (= 事前削除された / permission 違反) → Err
/// - CBOR decode 失敗 (= 版跨ぎで schema 変わった等) → Err
/// - `format_version` が [`STATE_FORMAT_VERSION_V1`] と異なる → Err
///
/// caller (= [`crate::daemon::Session::from_upgrade_inherited`]) は Err の場合、
/// env 最小 subset (= [`read_upgrade_env`]) で fallback resume することが期待される。
/// 成功 / 失敗どちらでも file 自体は削除する (= 二次 process が読める状態を残さない)。
pub fn read_and_consume_state_file(path: &Path) -> Result<UpgradeStateV1, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("state file open failed: {} ({e})", path.display()));
    // read 前 / 後どちらでも delete を試す (= 失敗時のクリーンアップ)。
    let file = match file {
        Ok(f) => f,
        Err(msg) => {
            let _ = std::fs::remove_file(path);
            return Err(msg);
        }
    };
    let reader = std::io::BufReader::new(file);
    let decode: Result<UpgradeStateV1, _> = ciborium::from_reader(reader);
    let _ = std::fs::remove_file(path);
    let state = decode.map_err(|e| format!("state file cbor decode failed: {e}"))?;
    if state.format_version != STATE_FORMAT_VERSION_V1 {
        return Err(format!(
            "state file format_version mismatch: got {}, expected {}",
            state.format_version, STATE_FORMAT_VERSION_V1
        ));
    }
    Ok(state)
}

/// socket path と同じ dir に state file path を組み立てる helper。
/// パス例: `/Users/.../hyoui/<session>.sock.upgrade-state.<pid>`。
///
/// PID を接尾辞に付けるのは同 session の複数 upgrade を並列に走らせない衛生策
/// (= 本 Phase では並列 upgrade は起きない前提だが、path 衝突を作らない)。
pub fn compute_state_file_path(socket_path: &Path) -> PathBuf {
    let pid = std::process::id();
    let mut name = socket_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".upgrade-state.{pid}"));
    socket_path
        .parent()
        .unwrap_or_else(|| Path::new("/tmp"))
        .join(name)
}

/// upgrade target の pre-check (DR-0028 §5.1)。
///
/// [`ENV_UPGRADE_EXE_OVERRIDE`] が set されていればその path、無ければ
/// `std::env::current_exe()` を対象に検査:
///
/// - 存在 (= `metadata()` が成功)
/// - regular file
/// - 実行 bit (= mode & 0o111 != 0)
/// - 現在の euid が所有者 (= DR-0028 §2)
///
/// すべて通れば `Ok(path)`。1 つでも失敗すれば `Err(reason)`。**この段階で失敗した
/// upgrade は fd に一切触れず** 呼び出し側 (= [`crate::daemon::Session::serve`]) が
/// 旧 serve_loop に再突入する (= DR-0028 §5.1 の「大半は事前検証で弾く」)。
pub fn precheck_upgrade_target() -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let path = if let Some(p) = std::env::var_os(ENV_UPGRADE_EXE_OVERRIDE) {
        PathBuf::from(p)
    } else {
        std::env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?
    };
    let meta = std::fs::metadata(&path)
        .map_err(|e| format!("upgrade target `{}` stat failed: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!(
            "upgrade target `{}` is not a regular file",
            path.display()
        ));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "upgrade target `{}` is not executable (mode={:o})",
            path.display(),
            meta.permissions().mode() & 0o777
        ));
    }
    let euid = nix::unistd::geteuid().as_raw();
    if meta.uid() != euid {
        return Err(format!(
            "upgrade target `{}` uid={} != current euid {}",
            path.display(),
            meta.uid(),
            euid
        ));
    }
    Ok(path)
}

/// DR-0028 Phase 3: `perform_self_exec` の結果。**成功時は execve が戻らないので
/// 変数体無し** (= 戻ってきた時点で必ずいずれかの失敗)。
#[derive(Debug)]
#[non_exhaustive]
pub enum PerformSelfExecOutcome {
    /// state file 書き出しなど pre-check 通過後だが fd 移譲前の段階で失敗した
    /// (= pty / listener は本関数の scope で drop 済、socket unlink 発生)。caller は
    /// session-fatal error 扱いにする。
    PrepFailed(Error),
    /// pre-check + fd 移譲 + CLOEXEC clear は済んだが **execve syscall 自体が
    /// 失敗** (DR-0028 §5.2 の Phase 3 fallback)。CLOEXEC を復元し、state file を
    /// 削除し、Pty / UnixSock を **再構築して返す**。caller は
    /// [`crate::daemon::Session::from_upgrade_inherited_parts`] 相当の経路で
    /// serve_loop に再突入する (= 旧プロセス継続)。
    ExecFailed {
        /// 再構築された PTY (master fd は CLOEXEC 復元済み)。
        pty: Pty,
        /// 再構築された listener (socket path は前と同じ、unlink されない)。
        listener: UnixSock,
        /// 子 PID (exec しなかったので変わらない)。
        child: Pid,
        /// execve が返した errno を含む error 詳細。
        error: Error,
    },
}

/// DR-0028 Phase 2/3: self-exec を実行する。**成功時は戻り値を返さない** (= execve
/// が現プロセス image を新バイナリで置換)。失敗時のみ [`PerformSelfExecOutcome`] を返す。
///
/// caller (= `Session::serve` の UpgradeRequested 分岐) は先に
/// [`precheck_upgrade_target`] を呼んで OK なら `exe_path` を渡す (= exec 直前で
/// 追加の pre-check を重複しない)。`pty` / `listener` の所有権はここで取得し、
/// CLOEXEC 解除してから execve へ飛ぶ。**execve 失敗時は CLOEXEC を復元 + fd を
/// 再パッケージして返す** (Phase 3 追加、DR §5.2)。
///
/// state file は先に本関数内で書き出す (= 失敗すると upgrade 中断、DR-0028 §3 の
/// 「screen 継続」gate 達成のため state 無しで進めない)。
///
/// # Arguments
///
/// - `pty` / `listener` / `child`: 引き継ぐ fd + 子 PID
/// - `config`: DaemonConfig 全体 (= state file にシリアライズする材料)
/// - `scrollback_bytes`: 新プロセスに渡す scrollback 生 bytes (= 再 feed 材料)
/// - `daemon_boot_id_prev`: 前 daemon の boot_id (= record 継続 Phase 4 で使う)
/// - `exe_path`: `precheck_upgrade_target` が返した実行 target
#[allow(clippy::too_many_arguments)]
pub fn perform_self_exec(
    pty: Pty,
    listener: UnixSock,
    child: Pid,
    config: &DaemonConfig,
    scrollback_bytes: Vec<u8>,
    daemon_boot_id_prev: &str,
    exe_path: &Path,
) -> PerformSelfExecOutcome {
    // 0. state file を先に書く (= 書けない環境なら upgrade 中断、fd はまだ手つかず)。
    let state_path = compute_state_file_path(&config.socket_path);
    let state = UpgradeStateV1 {
        format_version: STATE_FORMAT_VERSION_V1,
        hyoui_version: crate::VERSION.to_string(),
        daemon_boot_id_prev: daemon_boot_id_prev.to_string(),
        session_id: config.session_id.clone(),
        socket_path: config.socket_path.clone(),
        cmd: config.cmd.clone(),
        cols: config.cols,
        rows: config.rows,
        scrollback_bytes: config.scrollback_bytes,
        screen_input_log_bytes: config.screen_input_log_bytes,
        screen_vt100_scrollback_rows: config.screen_vt100_scrollback_rows,
        client_buffer_bytes: config.client_buffer_bytes,
        expected_token: config.expected_token.clone(),
        until: config.until.clone(),
        debug_dump_path: config.debug_dump_path.clone(),
        cwd: config.cwd.clone(),
        on_child_suspend: config.on_child_suspend.as_str().to_string(),
        timeout_ms: config.timeout_ms,
        idle_timeout_ms: config.idle_timeout_ms,
        child_pid: child.as_raw(),
        scrollback: scrollback_bytes,
    };
    if let Err(e) = write_state_file(&state_path, &state) {
        eprintln!(
            "hyoui upgrade: state file write failed: {e} (path={}); aborting upgrade",
            state_path.display()
        );
        // pty / listener はまだ本関数 scope が保持 → 関数 return で drop され socket unlink。
        // これは exec しない選択なので session-fatal error として caller に伝える。
        return PerformSelfExecOutcome::PrepFailed(e);
    }

    // 1. fd を取り出す。UnixSock は Drop で socket file を unlink するので
    //    `into_parts_for_exec` で Drop を bypass。Pty も `into_master()` で master
    //    OwnedFd を取り出し (残りの Pty 部分は関数 scope 抜けで no-op drop)。
    let master_owned = pty.into_master();
    let (listener_owned, socket_path) = listener.into_parts_for_exec();

    // 2. CLOEXEC 解除 (= execve で新プロセスへ fd を継承させる)。
    if let Err(e) = clear_cloexec(&master_owned) {
        eprintln!("hyoui upgrade: CLOEXEC clear on master fd failed: {e}");
        // Phase 3 fallback: fd を Pty / UnixSock として再構築して caller に返す
        // (= old serve_loop 継続)。ここではまだ execve に到達していないので、
        // master_owned は close されず新 Pty に譲る。listener 側も同様。
        let pty = Pty::from_master_fd(master_owned);
        let listener = UnixSock::from_listener_fd(listener_owned, socket_path);
        let _ = std::fs::remove_file(&state_path);
        return PerformSelfExecOutcome::ExecFailed {
            pty,
            listener,
            child,
            error: e,
        };
    }
    if let Err(e) = clear_cloexec(&listener_owned) {
        eprintln!("hyoui upgrade: CLOEXEC clear on listener fd failed: {e}");
        // master 側 CLOEXEC は復元してから再パッケージ。
        let _ = set_cloexec(&master_owned);
        let pty = Pty::from_master_fd(master_owned);
        let listener = UnixSock::from_listener_fd(listener_owned, socket_path);
        let _ = std::fs::remove_file(&state_path);
        return PerformSelfExecOutcome::ExecFailed {
            pty,
            listener,
            child,
            error: e,
        };
    }

    let pty_raw = master_owned.as_raw_fd();
    let listener_raw = listener_owned.as_raw_fd();

    // 3. envp 構築 (= 現在の environ + upgrade 用 var、既存 HYOUI_UPGRADE_* は
    //    上書きする)。
    let mut envp: Vec<CString> = Vec::new();
    for (k, v) in std::env::vars_os() {
        if UPGRADE_ENV_KEYS
            .iter()
            .any(|uk| k.as_os_str() == OsStr::new(*uk))
        {
            continue;
        }
        let mut buf: Vec<u8> = Vec::with_capacity(k.len() + 1 + v.len());
        buf.extend_from_slice(k.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(v.as_bytes());
        if let Ok(c) = CString::new(buf) {
            envp.push(c);
        }
    }
    let push_kv = |envp: &mut Vec<CString>, k: &str, v: String| {
        let buf = format!("{k}={v}");
        if let Ok(c) = CString::new(buf) {
            envp.push(c);
        }
    };
    push_kv(&mut envp, ENV_UPGRADE_RESUME, "1".to_string());
    push_kv(&mut envp, ENV_UPGRADE_PTY_FD, pty_raw.to_string());
    push_kv(&mut envp, ENV_UPGRADE_LISTENER_FD, listener_raw.to_string());
    push_kv(&mut envp, ENV_UPGRADE_CHILD_PID, child.as_raw().to_string());
    push_kv(&mut envp, ENV_UPGRADE_SESSION, config.session_id.clone());
    push_kv(
        &mut envp,
        ENV_UPGRADE_SOCKET,
        socket_path.to_string_lossy().into_owned(),
    );
    push_kv(&mut envp, ENV_UPGRADE_COLS, config.cols.to_string());
    push_kv(&mut envp, ENV_UPGRADE_ROWS, config.rows.to_string());
    push_kv(
        &mut envp,
        ENV_UPGRADE_STATE_FILE,
        state_path.to_string_lossy().into_owned(),
    );

    let exe_c = match CString::new(exe_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => {
            eprintln!(
                "hyoui upgrade: exe_path contained NUL: {}",
                exe_path.display()
            );
            // CLOEXEC 復元 + 再パッケージ (execve に到達していないので fallback path)。
            let _ = set_cloexec(&master_owned);
            let _ = set_cloexec(&listener_owned);
            let pty = Pty::from_master_fd(master_owned);
            let listener = UnixSock::from_listener_fd(listener_owned, socket_path);
            let _ = std::fs::remove_file(&state_path);
            return PerformSelfExecOutcome::ExecFailed {
                pty,
                listener,
                child,
                error: Error::Invalid("exe_path contained NUL"),
            };
        }
    };
    let argv: Vec<CString> = vec![exe_c.clone()];

    // 4. execve — 成功時は返らない。失敗時は Phase 3 fallback: CLOEXEC 復元 +
    //    Pty/UnixSock 再構築 + state file 削除 → caller に返却 (= 旧 serve_loop 続行)。
    match nix::unistd::execve(&exe_c, &argv, &envp) {
        Ok(_infallible) => unreachable!("execve returned Ok(Infallible)"),
        Err(e) => {
            eprintln!(
                "hyoui upgrade: execve failed after pre-check: {e} (exe={}); restoring CLOEXEC + resuming old daemon",
                exe_path.display()
            );
            // CLOEXEC 復元 (= 通常運用の defense-in-depth に戻す。失敗しても
            // fd は使えるので警告のみ)。
            if let Err(re) = set_cloexec(&master_owned) {
                eprintln!("hyoui upgrade: CLOEXEC restore on master fd failed: {re} (continuing)");
            }
            if let Err(re) = set_cloexec(&listener_owned) {
                eprintln!(
                    "hyoui upgrade: CLOEXEC restore on listener fd failed: {re} (continuing)"
                );
            }
            let pty = Pty::from_master_fd(master_owned);
            let listener = UnixSock::from_listener_fd(listener_owned, socket_path);
            let _ = std::fs::remove_file(&state_path);
            PerformSelfExecOutcome::ExecFailed {
                pty,
                listener,
                child,
                error: Error::Errno(e),
            }
        }
    }
}

/// upgrade-resume 経路で env から取り出した最小 init セット (= state file が
/// 読めない fallback 用)。
#[derive(Debug)]
pub struct UpgradeResumeEnv {
    /// 引き継ぐ session_id (= `hyoui list` 表示 + 認証境界の識別)。
    pub session_id: String,
    /// 引き継ぐ socket path (= bind 済 listener fd と対応)。
    pub socket: PathBuf,
    /// 前 daemon が CLOEXEC を解除して渡した PTY master の raw fd。
    pub pty_fd: std::os::fd::RawFd,
    /// 同じく渡された listener の raw fd。
    pub listener_fd: std::os::fd::RawFd,
    /// 引き継ぐ子 process の PID (= exec 前後で不変)。
    pub child_pid: i32,
    /// 引き継ぐ PTY 列数。
    pub cols: u16,
    /// 引き継ぐ PTY 行数。
    pub rows: u16,
    /// state file の path (= 別途 [`read_and_consume_state_file`] で読み込む)。
    pub state_file: Option<PathBuf>,
}

/// 起動時に `HYOUI_UPGRADE_RESUME=1` を検知したら、他 env から init を取り出す。
///
/// 呼び出し後、caller (= hyoui-cli main) は取り出した env をすべて **remove** して
/// 孫プロセスへの漏れを防ぐこと。
pub fn read_upgrade_env() -> Result<UpgradeResumeEnv, String> {
    let get = |k: &str| -> Result<String, String> {
        std::env::var(k).map_err(|_| format!("hyoui upgrade-resume: env `{k}` missing"))
    };
    let parse_i32 = |k: &str, s: String| -> Result<i32, String> {
        s.parse::<i32>()
            .map_err(|e| format!("hyoui upgrade-resume: env `{k}` parse int failed: {e}"))
    };
    let parse_u16 = |k: &str, s: String| -> Result<u16, String> {
        s.parse::<u16>()
            .map_err(|e| format!("hyoui upgrade-resume: env `{k}` parse u16 failed: {e}"))
    };
    let session_id = get(ENV_UPGRADE_SESSION)?;
    let socket = PathBuf::from(get(ENV_UPGRADE_SOCKET)?);
    let pty_fd = parse_i32(ENV_UPGRADE_PTY_FD, get(ENV_UPGRADE_PTY_FD)?)?;
    let listener_fd = parse_i32(ENV_UPGRADE_LISTENER_FD, get(ENV_UPGRADE_LISTENER_FD)?)?;
    let child_pid = parse_i32(ENV_UPGRADE_CHILD_PID, get(ENV_UPGRADE_CHILD_PID)?)?;
    let cols = parse_u16(ENV_UPGRADE_COLS, get(ENV_UPGRADE_COLS)?)?;
    let rows = parse_u16(ENV_UPGRADE_ROWS, get(ENV_UPGRADE_ROWS)?)?;
    let state_file = std::env::var_os(ENV_UPGRADE_STATE_FILE).map(PathBuf::from);
    Ok(UpgradeResumeEnv {
        session_id,
        socket,
        pty_fd,
        listener_fd,
        child_pid,
        cols,
        rows,
        state_file,
    })
}

/// `ChildSuspendPolicy` wire 名から enum を復元する helper (= 未知値は Notify)。
pub fn parse_on_child_suspend(s: &str) -> ChildSuspendPolicy {
    ChildSuspendPolicy::from_wire(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_v1_round_trip_via_cbor() {
        let orig = UpgradeStateV1 {
            format_version: STATE_FORMAT_VERSION_V1,
            hyoui_version: "0.9.14-test".to_string(),
            daemon_boot_id_prev: "abcd-boot".to_string(),
            session_id: "demo".to_string(),
            socket_path: PathBuf::from("/tmp/x.sock"),
            cmd: vec!["bash".into(), "-i".into()],
            cols: 100,
            rows: 40,
            scrollback_bytes: 1 << 20,
            screen_input_log_bytes: 1 << 20,
            screen_vt100_scrollback_rows: 1000,
            client_buffer_bytes: 8 << 20,
            expected_token: Some("s3cr3t".into()),
            until: Some("READY>".into()),
            debug_dump_path: None,
            cwd: Some(PathBuf::from("/work")),
            on_child_suspend: "auto-resume".to_string(),
            timeout_ms: Some(60_000),
            idle_timeout_ms: None,
            child_pid: 12345,
            scrollback: b"hello scrollback".to_vec(),
        };
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        write_state_file(tmp.path(), &orig).expect("write ok");
        let read = read_and_consume_state_file(tmp.path()).expect("read ok");
        assert_eq!(read.session_id, orig.session_id);
        assert_eq!(read.cmd, orig.cmd);
        assert_eq!(read.cols, orig.cols);
        assert_eq!(read.rows, orig.rows);
        assert_eq!(read.child_pid, orig.child_pid);
        assert_eq!(read.scrollback, orig.scrollback);
        assert_eq!(read.on_child_suspend, "auto-resume");
        assert_eq!(read.format_version, STATE_FORMAT_VERSION_V1);
        // consume 済で file は消えている。
        assert!(!tmp.path().exists(), "state file should be deleted");
    }

    #[test]
    fn state_v1_version_mismatch_is_err() {
        let mut state = UpgradeStateV1 {
            format_version: 999, // future version
            hyoui_version: "test".into(),
            daemon_boot_id_prev: "x".into(),
            session_id: "s".into(),
            socket_path: PathBuf::from("/tmp/s"),
            cmd: vec!["cat".into()],
            cols: 80,
            rows: 24,
            scrollback_bytes: 0,
            screen_input_log_bytes: 0,
            screen_vt100_scrollback_rows: 0,
            client_buffer_bytes: 0,
            expected_token: None,
            until: None,
            debug_dump_path: None,
            cwd: None,
            on_child_suspend: "notify".into(),
            timeout_ms: None,
            idle_timeout_ms: None,
            child_pid: 1,
            scrollback: vec![],
        };
        state.format_version = 999;
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        // 直接 encode (write_state_file は format_version を検証しない)
        let file = std::fs::File::create(tmp.path()).unwrap();
        ciborium::into_writer(&state, file).unwrap();
        let err = read_and_consume_state_file(tmp.path()).unwrap_err();
        assert!(err.contains("format_version mismatch"), "err: {err}");
        assert!(!tmp.path().exists(), "state file should be deleted");
    }

    #[test]
    fn state_v1_corrupt_file_is_err() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), b"not-cbor-data").unwrap();
        let err = read_and_consume_state_file(tmp.path()).unwrap_err();
        assert!(err.contains("cbor decode"), "err: {err}");
        assert!(!tmp.path().exists(), "state file should be deleted");
    }

    /// precheck の env 依存分岐を **1 test 内で連続実行** して並列 race を避ける
    /// (= 独立 test に分けると parallel runner が env を割り込ませる。DR-0028
    /// Phase 2 では pre-check の env 分岐仕様のカバレッジが要件で、分割の価値は薄い)。
    #[test]
    fn precheck_env_branches_serial() {
        let saved = std::env::var_os(ENV_UPGRADE_EXE_OVERRIDE);
        // 分岐 1: override 無し → current_exe が使われ、必ず存在 + 実行 bit あり
        // SAFETY: test 内 single test なので同時実行なし、production 経路と分離。
        unsafe { std::env::remove_var(ENV_UPGRADE_EXE_OVERRIDE) };
        let ok_path = precheck_upgrade_target().expect("current_exe should pass");
        assert!(ok_path.is_file(), "path {ok_path:?} should be file");

        // 分岐 2: override に不在 path → stat 失敗で Err
        unsafe { std::env::set_var(ENV_UPGRADE_EXE_OVERRIDE, "/nonexistent/hyoui-upgrade-test") };
        let err = precheck_upgrade_target().expect_err("should fail on nonexistent path");
        assert!(err.contains("stat failed"), "err: {err}");

        // restore
        match saved {
            Some(v) => unsafe { std::env::set_var(ENV_UPGRADE_EXE_OVERRIDE, v) },
            None => unsafe { std::env::remove_var(ENV_UPGRADE_EXE_OVERRIDE) },
        }
    }

    #[test]
    fn compute_state_file_path_shape() {
        let sock = PathBuf::from("/foo/bar/session.sock");
        let p = compute_state_file_path(&sock);
        assert_eq!(p.parent().unwrap(), Path::new("/foo/bar"));
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("session.sock.upgrade-state."),
            "name: {name}"
        );
        assert!(
            name.ends_with(&std::process::id().to_string()),
            "name: {name}"
        );
    }
}
