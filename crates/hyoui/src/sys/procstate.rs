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

#[cfg(target_os = "macos")]
mod imp {
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
    pub(super) fn is_stopped(pid: i32) -> Option<bool> {
        // /proc/<pid>/stat の 3 番目のフィールドが state。comm は括弧で囲まれ空白を
        // 含みうるので、最後の `)` 以降を見る。
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.rsplit_once(')')?.1;
        let state = rest.split_whitespace().next()?;
        Some(state == "T")
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
