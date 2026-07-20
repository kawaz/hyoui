//! DR-0028 Phase 1: daemon graceful upgrade (self-exec) — 骨格 PoC。
//!
//! # 責務
//!
//! 走行中 daemon が **自 PID を保ったまま** 新バイナリに切り替える execve 骨格。
//! Phase 1 は「PTY master fd + Unix socket listener fd + 子 PID を新プロセスへ引き
//! 継ぐ最小ループ」を実証するのみで、protocol message / drain / state file / 正規
//! CLI は Phase 2/3 で追加する (= DR-0028 §Implementation phases に従う)。
//!
//! # trigger (Phase 1 隠し経路)
//!
//! daemon serve_loop の self-pipe に **`SIGUSR1`** を register する。外部から
//! `kill -USR1 <daemon-pid>` を送ると `handle_suspend_signals` が
//! `RelayOutcome::UpgradeRequested` を返し、`Session::serve` が本 module の
//! [`perform_self_exec`] を呼ぶ。正規 `upgrade.request` protocol kind の追加 +
//! `hyoui upgrade` subcommand は Phase 3。
//!
//! # 引き継ぐもの (最小)
//!
//! - **PTY master fd**: `HYOUI_UPGRADE_PTY_FD=<n>` env で伝達 + `FD_CLOEXEC` 解除。
//! - **listener fd**: `HYOUI_UPGRADE_LISTENER_FD=<n>` env + CLOEXEC 解除。socket
//!   path は既に bind 済のまま (= 新プロセスで再 bind せず継続使用)。
//! - **子 PID**: `HYOUI_UPGRADE_CHILD_PID=<n>` env。exec は同 PID を保つので
//!   parent-child 関係と SIGCHLD 経路は kernel が自動維持する (DR-0028 §1)。
//! - **session_id / socket path / cols / rows**: 起動時 config の最小 subset。
//!   scrollback / record 継続 / on_child_suspend / lock 状態 等は Phase 2 で
//!   一時ファイル (CBOR) 経由に拡張する。
//!
//! # 引き継がないもの (Phase 1)
//!
//! - attach client 接続: exec 直前に個別に close されない (= socket が閉じるので
//!   client 側で切れる)。client fd は accept 時 `FD_CLOEXEC` set 済 (socket.rs)
//!   なので exec で kernel が閉じてくれる。
//! - scrollback / screen state: 新プロセスは fresh state で開始する (= 子出力の
//!   再 feed による復元は Phase 2)。
//! - lock 状態 / record 継続 / config の細部: Phase 2 scope。
//!
//! # exec 失敗時 (Phase 1 fallback は最小)
//!
//! DR-0028 §5 は「CLOEXEC 復元 + 旧続行」を規定するが Phase 1 PoC では実装せず、
//! `eprintln` + `Err` を返して session を終了させる (= 子は orphan 化して SIGHUP、
//! 通常の daemon exit 経路と同様の後始末)。Phase 2 で 2 相分離 + CLOEXEC 復元を
//! 追加する。

use std::ffi::{CString, OsStr};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use nix::unistd::Pid;

use crate::sys::{Error, Pty, UnixSock, clear_cloexec};

use super::DaemonConfig;

/// upgrade-resume 経路を検知する env var (= "1" なら resume mode)。
pub const ENV_UPGRADE_RESUME: &str = "HYOUI_UPGRADE_RESUME";
/// PTY master fd 番号を伝達する env var (= 十進数の RawFd)。
pub const ENV_UPGRADE_PTY_FD: &str = "HYOUI_UPGRADE_PTY_FD";
/// listener fd 番号を伝達する env var。
pub const ENV_UPGRADE_LISTENER_FD: &str = "HYOUI_UPGRADE_LISTENER_FD";
/// 引き継ぐ子 PID。
pub const ENV_UPGRADE_CHILD_PID: &str = "HYOUI_UPGRADE_CHILD_PID";
/// 引き継ぐ session_id (= 表示 + 認証境界の識別)。
pub const ENV_UPGRADE_SESSION: &str = "HYOUI_UPGRADE_SESSION";
/// 引き継ぐ socket path (= UnixSock::from_listener_fd に渡す)。
pub const ENV_UPGRADE_SOCKET: &str = "HYOUI_UPGRADE_SOCKET";
/// 引き継ぐ cols。
pub const ENV_UPGRADE_COLS: &str = "HYOUI_UPGRADE_COLS";
/// 引き継ぐ rows。
pub const ENV_UPGRADE_ROWS: &str = "HYOUI_UPGRADE_ROWS";

/// DR-0028 Phase 1: self-exec を実行する。**成功時は戻り値を返さない** (= execve
/// が現プロセス image を新バイナリで置換)。失敗時のみ `Err(Error)` を返す。
///
/// caller (= `Session::serve` の UpgradeRequested 分岐) が渡す `pty` / `listener`
/// の所有権を取り、CLOEXEC を解除してから execve に飛び込む。exec 成功時は memory
/// が全て wipe されるため Drop は走らず (= socket unlink されず継続)、失敗時は
/// PoC scope として fd を leak して session error で abort する。
///
/// # Phase 1 制約
///
/// - exec 失敗時の CLOEXEC 復元 + 旧続行は未実装 (Phase 2)。
/// - envp は現在の environ + 追加 var で組み立てる (= `std::env::vars()` を読む、
///   env は上書きしない)。upgrade 経路の env pollution は Phase 3 で正規化。
pub fn perform_self_exec(pty: Pty, listener: UnixSock, child: Pid, config: &DaemonConfig) -> Error {
    // 1. fd を取り出す。UnixSock は Drop で socket file を unlink するので
    //    `into_parts_for_exec` で Drop を bypass。Pty は Drop で fd を close する
    //    が、CLOEXEC を解いた後 execve に成功すれば memory wipe で Drop 未実行、
    //    失敗時は下でリークさせる (Phase 2 で fallback 化)。
    let master_owned = pty.into_master();
    let (listener_owned, socket_path) = listener.into_parts_for_exec();

    // 2. CLOEXEC 解除 (= execve で新プロセスへ fd を継承させる)。
    if let Err(e) = clear_cloexec(&master_owned) {
        eprintln!("hyoui upgrade: CLOEXEC clear on master fd failed: {e}");
        // 以降 fd は Drop で close される。listener は forget されているので
        // socket 残骸を防ぐため明示 unlink。
        let _ = nix::unistd::unlink(&socket_path);
        return e;
    }
    if let Err(e) = clear_cloexec(&listener_owned) {
        eprintln!("hyoui upgrade: CLOEXEC clear on listener fd failed: {e}");
        let _ = nix::unistd::unlink(&socket_path);
        return e;
    }

    let pty_raw = master_owned.as_raw_fd();
    let listener_raw = listener_owned.as_raw_fd();

    // 3. envp 構築 (= 現在の environ + upgrade 用 var、既存 HYOUI_UPGRADE_* は
    //    上書きする)。skip すべき key set。
    let upgrade_keys: &[&str] = &[
        ENV_UPGRADE_RESUME,
        ENV_UPGRADE_PTY_FD,
        ENV_UPGRADE_LISTENER_FD,
        ENV_UPGRADE_CHILD_PID,
        ENV_UPGRADE_SESSION,
        ENV_UPGRADE_SOCKET,
        ENV_UPGRADE_COLS,
        ENV_UPGRADE_ROWS,
    ];
    let mut envp: Vec<CString> = Vec::new();
    for (k, v) in std::env::vars_os() {
        // `k` が upgrade 用 key と衝突するなら skip (= 下で新規に追加する)。
        if upgrade_keys.iter().any(|uk| k.as_os_str() == OsStr::new(*uk)) {
            continue;
        }
        let mut buf: Vec<u8> = Vec::with_capacity(k.len() + 1 + v.len());
        buf.extend_from_slice(k.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(v.as_bytes());
        match CString::new(buf) {
            Ok(c) => envp.push(c),
            Err(_) => {
                // NUL 含みの env は skip (= 実運用でまず起きないが defensive)。
                continue;
            }
        }
    }
    // 追加 var
    let push_kv = |envp: &mut Vec<CString>, k: &str, v: String| {
        let mut buf = String::with_capacity(k.len() + 1 + v.len());
        buf.push_str(k);
        buf.push('=');
        buf.push_str(&v);
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

    // 4. exe path。DR-0028 §2 の既定 (= 自身の `current_exe`)。
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hyoui upgrade: current_exe failed: {e}");
            let _ = nix::unistd::unlink(&socket_path);
            return Error::Errno(nix::errno::Errno::ENOENT);
        }
    };
    let exe_c = match CString::new(exe.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("hyoui upgrade: current_exe path contained NUL: {exe:?}");
            let _ = nix::unistd::unlink(&socket_path);
            return Error::Invalid("current_exe contained NUL");
        }
    };
    // argv = [exe_path]。upgrade 経路の init 情報は env で伝えるため追加 arg 無し
    // (= `ps` 表示上 daemon の argv が Phase 1 では pretty ではないが、Phase 3 で
    // 正規 `hyoui __upgrade-resume` subcommand を argv[1] に置いて可読性を上げる)。
    let argv: Vec<CString> = vec![exe_c.clone()];

    // 5. execve — 成功時は返らない。失敗時は Err(Errno) を返す。
    match nix::unistd::execve(&exe_c, &argv, &envp) {
        Ok(_infallible) => {
            // 型的に到達不能。
            unreachable!("execve returned Ok(Infallible)")
        }
        Err(e) => {
            eprintln!("hyoui upgrade: execve failed: {e} (exe={exe:?})");
            // Phase 1: fallback せず session error として abort する。fd は
            // Drop で close (master 側)、listener は forget 済 fd なので明示 unlink。
            let _ = nix::unistd::unlink(&socket_path);
            // listener_owned はまだ scope 内、Drop で close される。
            let _ = listener_owned;
            let _ = master_owned;
            Error::Errno(e)
        }
    }
}

/// upgrade-resume 経路で env から取り出した最小 init セット (= caller は fd を
/// `own_raw_fd` で `OwnedFd` 化して `Session::from_upgrade_inherited` に渡す)。
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
    Ok(UpgradeResumeEnv {
        session_id,
        socket,
        pty_fd,
        listener_fd,
        child_pid,
        cols,
        rows,
    })
}
