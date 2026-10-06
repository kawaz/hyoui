//! プロセスの stop/run 状態を kernel から直接読む (= `waitpid` に依存しない観測)。
//!
//! `waitpid(WCONTINUED)` は macOS では **子が自分で自分を止めた場合に continued を
//! 一切報告しない** (実測 2026-07-29、下記マトリクス)。そのため「子が resume したか」
//! を waitpid だけで判定すると、self-stop した子については永久に stopped 扱いのまま
//! になる。本 module は kernel の process state を直接読むことでこの穴を塞ぐ。
//!
//! | 子の止まり方 | CONT の送り主 | `waitpid(WCONTINUED)` の報告 |
//! |---|---|---|
//! | 外部から `kill -TSTP <pid>` | daemon / 外部どちら経由でも | 報告される |
//! | 子自身が `kill -STOP $$` | daemon 経由 (= DR-0030 の resume) | **報告されない** |
//! | 子自身が `kill -STOP $$` | 外部 `kill -CONT <pid>` | **報告されない** |

/// `pid` が停止中 (= macOS `SSTOP` / Linux stat の `T`) なら `Some(true)`、
/// 走行中なら `Some(false)`。取得できなければ `None` (= 判定を保留させる)。
pub fn is_stopped(pid: i32) -> Option<bool> {
    imp::is_stopped(pid)
}

/// 自プロセスの制御端末として開けるパス (DR-0042 決定 4、attach client の入力端末)。
///
/// 制御端末が無ければ `None`。macOS の `/dev/tty` は `poll(2)` に `POLLNVAL` を返し
/// (実測 2026-10-06、PTY を制御端末に持つプロセスで `open("/dev/tty")` した fd を poll すると
/// revents = 0x20)、poll で入力を待つ attach client の入力端末にできない。そこで kernel が
/// 持つ制御端末の device 番号 (`proc_bsdinfo.e_tdev`) を `devname(3)` で実体のパス
/// (`/dev/ttys005` 等) に直して返す。Linux の `/dev/tty` は poll できるのでそのまま返す。
pub fn controlling_tty_path() -> Option<std::path::PathBuf> {
    imp::controlling_tty_path()
}

#[cfg(target_os = "macos")]
mod imp {
    pub(super) fn controlling_tty_path() -> Option<std::path::PathBuf> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: info は生存中のローカルで、size ちょうどの領域を渡している。自分の pid。
        let n = unsafe {
            libc::proc_pidinfo(
                std::process::id() as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        // 制御端末が無いと e_tdev は NODEV (= -1)。
        if n != size || info.e_tdev == u32::MAX {
            return None;
        }
        // SAFETY: devname は static buffer への ptr か NULL を返す。NUL 終端の C 文字列として
        // 直ちに複製し、ptr は保持しない (= 次の呼び出しで上書きされる buffer を残さない)。
        let name = unsafe { libc::devname(info.e_tdev as libc::dev_t, libc::S_IFCHR) };
        if name.is_null() {
            return None;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_str().ok()?;
        // 見つからない時の devname は "??" を返す。
        if name.is_empty() || name.starts_with('?') {
            return None;
        }
        Some(std::path::Path::new("/dev").join(name))
    }

    pub(super) fn is_stopped(pid: i32) -> Option<bool> {
        // `proc_pidinfo(PROC_PIDTBSDINFO)` は read-only。対象が自分の子なので
        // 権限も要らない。`pbi_status` が SSTOP なら停止中。
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: info は生存中のローカルで、size ちょうどの領域を渡している。
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        if n != size {
            return None;
        }
        Some(info.pbi_status == libc::SSTOP)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) fn controlling_tty_path() -> Option<std::path::PathBuf> {
        // Linux の /dev/tty は poll できる。開けるか (= 制御端末があるか) は開く側が判定する。
        Some(std::path::PathBuf::from("/dev/tty"))
    }

    pub(super) fn is_stopped(pid: i32) -> Option<bool> {
        // /proc/<pid>/stat の 3 番目のフィールドが state。comm は括弧で囲まれ空白を
        // 含みうるので、最後の `)` 以降を見る。
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.rsplit_once(')')?.1;
        let state = rest.split_whitespace().next()?;
        Some(state == "T")
    }
}

/// 子 `pid` が exit するまで block して待つ。`WNOWAIT` なので reap せず zombie の
/// まま残す (= 後続の `waitpid` が exit を観測できる)。nix の `waitid` は macOS で
/// 未提供のため libc を直接使う。test が「時間に依存せず zombie を作る」ための道具。
#[cfg(test)]
pub fn wait_exit_nowait(pid: i32) -> std::io::Result<()> {
    // SAFETY: siginfo_t は全ビット 0 が有効な POD。waitid は有効な out ポインタに書くだけ。
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自プロセスは走行中なので `Some(false)`。
    #[test]
    fn self_is_not_stopped() {
        assert_eq!(is_stopped(std::process::id() as i32), Some(false));
    }

    /// 存在しない pid は `None` (= 判定不能)。
    #[test]
    fn unknown_pid_is_none() {
        assert_eq!(is_stopped(i32::MAX), None);
    }
}
