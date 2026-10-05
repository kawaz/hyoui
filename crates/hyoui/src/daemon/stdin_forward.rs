//! `hyoui run --detached` で呼び出し元の非 tty stdin を子 PTY に流す (DR-0019 §5)。
//!
//! attach しない起動でも `printf ... | hyoui run --detached -- cmd` の子が入力を受け取り、
//! EOF で終われるようにする (= `--detached` の有無で子の挙動が変わらない)。子の stdin は
//! PTY のまま (= isatty も外からの操作も保つ) で、daemon が master に書き込む。
//!
//! 構成:
//!
//! - **reader thread**: 引き継いだ fd を blocking のまま読み、内部 pipe に書く。fd 自体は
//!   nonblocking にしない (= O_NONBLOCK は open file description 単位なので、同じ pipe を
//!   共有する呼び出し元の読み手まで EAGAIN を見ることになる。DR-0037 E-1 と同じ理由)。
//!   通常ファイルの read が遅い fs で止まっても serve loop には及ばない (DR-0037 I-2)。
//! - **serve loop 側 ([`StdinForward`])**: 内部 pipe の読み側を nonblocking で poll し、
//!   読んだ chunk を master に nonblocking で書く。書き切れない間は内部 pipe を poll から
//!   外して `POLLOUT` を待つ (= 読み続けて溜め込まない。滞留は chunk 1 個 + 内部 pipe の
//!   容量で頭打ち)。

use std::io::Read;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::errno::Errno;

use crate::stdin_eof::{EofTracker, StdinEofAction};
use crate::sys::FdExt as _;

/// 1 回の read で扱う上限 (= 滞留する chunk の大きさ)。
const CHUNK: usize = 8192;

/// daemon が引き継いだ stdin と EOF 時の挙動 (= `Session::set_stdin_forward` で渡す)。
#[derive(Debug)]
pub struct StdinSource {
    /// 呼び出し元の stdin (CLOEXEC 付きで持つ = 子 PTY / upgrade の exec に漏らさない)。
    pub fd: OwnedFd,
    /// EOF を観測した時の挙動。
    pub eof: StdinEofAction,
}

/// serve loop が持つ転送状態。
#[derive(Debug)]
pub(super) struct StdinForward {
    /// reader thread が書く内部 pipe の読み側 (nonblocking)。EOF まで読んだら `None`。
    pipe: Option<OwnedFd>,
    /// master に書き切れていない bytes (= 先頭が次に書く byte)。
    pending: Vec<u8>,
    /// EOF の EOT を pending に積み済みか (= 2 度積まない)。
    eof_queued: bool,
    /// 子に送り終えた入力の末尾 (= EOT の個数を決める)。
    tracker: EofTracker,
    eof: StdinEofAction,
    /// reader thread が read error で止まった (= EOF ではない。EOT を送らない)。
    read_failed: Arc<AtomicBool>,
}

/// serve loop の 1 周で転送が poll に求めるもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Want {
    /// 内部 pipe の `POLLIN` を待つ。
    Read,
    /// master の `POLLOUT` を待つ (= pending を書き切るまで内部 pipe は読まない)。
    Write,
    /// 何も待たない (= 転送が終わった)。
    Done,
}

impl StdinForward {
    /// reader thread を起こして転送を始める。
    pub(super) fn start(src: StdinSource) -> crate::sys::Result<Self> {
        // pipe2 は macOS に無いので pipe + fcntl。serve loop は子の fork を済ませた後で、
        // この間に fork / exec する thread も居ないので CLOEXEC を後付けしても漏れない。
        let (rd, wr) = nix::unistd::pipe()?;
        crate::sys::set_cloexec(&rd)?;
        crate::sys::set_cloexec(&wr)?;
        rd.set_nonblocking(true)?;
        let read_failed = Arc::new(AtomicBool::new(false));
        let failed = Arc::clone(&read_failed);
        let StdinSource { fd, eof } = src;
        std::thread::Builder::new()
            .name("hyoui-stdin-forward".into())
            .spawn(move || pump(fd, wr, &failed))?;
        Ok(Self {
            pipe: Some(rd),
            pending: Vec::new(),
            eof_queued: false,
            tracker: EofTracker::new(),
            eof,
            read_failed,
        })
    }

    /// 今の周回で待つもの。`paused` は lock が他 client に握られている間 (= DR-0022 の
    /// 「holder 以外の入力を割り込ませない」を守るため、書かずに止まる)。
    pub(super) fn want(&self, paused: bool) -> Want {
        if paused {
            return Want::Done;
        }
        if !self.pending.is_empty() {
            Want::Write
        } else if self.pipe.is_some() {
            Want::Read
        } else {
            Want::Done
        }
    }

    /// 内部 pipe の読み側 (= `Want::Read` の時に poll へ積む fd)。
    pub(super) fn pipe_fd(&self) -> Option<BorrowedFd<'_>> {
        self.pipe.as_ref().map(AsFd::as_fd)
    }

    /// 内部 pipe が readable / HUP になった時に 1 chunk 読む。EOF なら EOT を pending に
    /// 積む (`SendEof` の時)。
    pub(super) fn on_pipe_ready(&mut self) {
        let Some(pipe) = self.pipe.as_ref() else {
            return;
        };
        let mut buf = [0u8; CHUNK];
        match nix::unistd::read(pipe, &mut buf) {
            Ok(0) => self.finish_reading(),
            Ok(n) => self.pending.extend_from_slice(&buf[..n]),
            Err(Errno::EAGAIN | Errno::EINTR) => {}
            Err(_) => {
                self.read_failed.store(true, Ordering::Release);
                self.finish_reading();
            }
        }
    }

    /// master が writable の時に pending を書けるだけ書く。master が壊れていたら転送を
    /// やめる (= 子が居なくなった。serve loop は master 側で別途それを観測する)。
    pub(super) fn on_master_writable(&mut self, master: BorrowedFd<'_>) {
        while !self.pending.is_empty() {
            match nix::unistd::write(master, &self.pending) {
                Ok(0) => break,
                Ok(n) => {
                    self.tracker.observe(&self.pending[..n]);
                    self.pending.drain(..n);
                }
                Err(Errno::EINTR) => continue,
                Err(Errno::EAGAIN) => break,
                Err(_) => {
                    self.pending.clear();
                    self.pipe = None;
                    self.eof_queued = true;
                    break;
                }
            }
        }
        if self.pending.is_empty() && self.pipe.is_none() {
            self.queue_eof_once();
        }
    }

    /// 内部 pipe を閉じ、pending が空なら EOF の EOT を積む (= 先に積んだ入力を書き切って
    /// から EOT を足すので、EOT の個数が入力の末尾と食い違わない)。
    fn finish_reading(&mut self) {
        self.pipe = None;
        if self.pending.is_empty() {
            self.queue_eof_once();
        }
    }

    fn queue_eof_once(&mut self) {
        if self.eof_queued {
            return;
        }
        self.eof_queued = true;
        if self.eof == StdinEofAction::SendEof && !self.read_failed.load(Ordering::Acquire) {
            self.pending.extend_from_slice(self.tracker.eof_bytes());
        }
    }
}

/// reader thread の本体: `src` を EOF まで blocking で読み、内部 pipe に書く。read error は
/// `failed` に記録してから閉じる (= serve loop は EOF と区別して EOT を送らない)。内部 pipe
/// の読み側が閉じた (= daemon が転送をやめた) 時は EPIPE で抜ける (SIGPIPE は Rust runtime
/// が無視に設定済み)。
fn pump(src: OwnedFd, sink: OwnedFd, failed: &AtomicBool) {
    let mut src = std::fs::File::from(src);
    let sink = std::fs::File::from(sink);
    let mut buf = [0u8; CHUNK];
    loop {
        match src.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if sink.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                failed.store(true, Ordering::Release);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 転送を最後まで進め、master 側 (= ここでは pipe) に書かれた bytes を返す。
    /// 入力の書き手と master の読み手は別 thread (= pipe 容量を超える入力でも詰まらない)。
    /// `Want::Write` の時は serve loop と同じく master の `POLLOUT` を待ってから書く。
    fn drive(input: &[u8], eof: StdinEofAction) -> Vec<u8> {
        use nix::poll::{PollFd, PollFlags, PollTimeout};
        let (src_rd, src_wr) = nix::unistd::pipe().expect("pipe");
        let (master_rd, master_wr) = nix::unistd::pipe().expect("pipe");
        master_wr.set_nonblocking(true).expect("nonblock");
        let mut fwd = StdinForward::start(StdinSource { fd: src_rd, eof }).expect("start");
        let input = input.to_vec();
        let writer = std::thread::spawn(move || {
            std::fs::File::from(src_wr)
                .write_all(&input)
                .expect("write");
        });
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            std::fs::File::from(master_rd)
                .read_to_end(&mut out)
                .expect("read");
            out
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "転送が終わらない");
            let timeout = PollTimeout::from(1000u16);
            match fwd.want(false) {
                Want::Done => break,
                Want::Write => {
                    let mut fds = [PollFd::new(master_wr.as_fd(), PollFlags::POLLOUT)];
                    nix::poll::poll(&mut fds, timeout).expect("poll");
                    fwd.on_master_writable(master_wr.as_fd());
                }
                Want::Read => {
                    let fd = fwd.pipe_fd().expect("pipe fd");
                    let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
                    nix::poll::poll(&mut fds, timeout).expect("poll");
                    fwd.on_pipe_ready();
                }
            }
        }
        writer.join().expect("writer");
        drop(master_wr);
        reader.join().expect("reader")
    }

    /// 改行で終わる入力は入力そのもの + EOT 1 個。
    #[test]
    fn forwards_lines_then_one_eot() {
        assert_eq!(drive(b"l1\nl2\n", StdinEofAction::SendEof), b"l1\nl2\n\x04");
    }

    /// 改行で終わらない入力は EOT 2 個 (= attach と同じ判定)。
    #[test]
    fn unterminated_input_gets_two_eots() {
        assert_eq!(drive(b"hoge", StdinEofAction::SendEof), b"hoge\x04\x04");
    }

    /// 空入力 (= 即 EOF) は EOT 1 個。
    #[test]
    fn empty_input_gets_one_eot() {
        assert_eq!(drive(b"", StdinEofAction::SendEof), b"\x04");
    }

    /// `--stdin-eof=detach` は入力だけ流し、EOT を送らない。
    #[test]
    fn detach_forwards_without_eot() {
        assert_eq!(drive(b"hoge", StdinEofAction::Detach), b"hoge");
    }

    /// chunk を跨ぐ大きな入力も順序どおり全部届き、末尾で EOT の個数が決まる。
    #[test]
    fn large_input_is_forwarded_in_order() {
        let input: Vec<u8> = (0..100_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let out = drive(&input, StdinEofAction::SendEof);
        assert_eq!(&out[..input.len()], &input[..]);
        assert_eq!(&out[input.len()..], b"\x04\x04");
    }

    /// lock で止まっている間は何も待たない (= 書かない)。解ければ再開する。
    #[test]
    fn paused_waits_for_nothing() {
        let (src_rd, _src_wr) = nix::unistd::pipe().expect("pipe");
        let fwd = StdinForward::start(StdinSource {
            fd: src_rd,
            eof: StdinEofAction::SendEof,
        })
        .expect("start");
        assert_eq!(fwd.want(true), Want::Done);
        assert_eq!(fwd.want(false), Want::Read);
    }
}
