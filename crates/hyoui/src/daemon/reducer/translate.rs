//! serve_loop の poll IO event を [`DaemonMsg`] へ写す純粋関数群 (DR-0025 Phase 1b
//! 後半、translate 併走方式)。
//!
//! DR-0025 §IO boundary と reducer boundary の関係 が定める `poll → translate →
//! reduce → effect` loop の **translate 段** を担う。各関数は 1 つの IO event
//! (= PTY master read / SIGCHLD / 子 exit / client frame / client detach) を対応する
//! [`DaemonMsg`] variant へ 1:1 で写すだけの pure function で、IO も state も持たない
//! (= gate 1b-3 の characterization test で入出力を固定できる)。
//!
//! serve_loop はこれらの結果を [`super::handle`] に渡して super-reducer を実走させる
//! (= translate 併走: stub domain は空 [`super::DomainOutput`] を返し、既存 handler は
//! 従来通り走るため挙動不変)。後続 Phase で各 domain reducer に振る舞いが入っても、
//! 本 module が保つ「IO event → DaemonMsg」の写像は変わらない (= 意味論の回帰基準)。
//!
//! payload は placeholder 主義 (DR-0025 §read-only view の hot path 分離思想):
//! frame 実体は運ばず client id / byte 列 / pid 等の軽量情報のみ載せる。

use nix::unistd::Pid;

use super::child::ChildMsg;
use super::client::ClientMsg;
use super::tty::TtyMsg;
use super::{ClientId, DaemonMsg};

/// PTY master fd の read bytes → TTY Layer 1 event (DR-0025 §IO boundary の
/// `DaemonMsg::Tty(TtyLayer1Bytes { bytes })`)。
///
/// serve_loop の master read `Ok(n)` 経路で子 PTY 生 bytes を写す。実 parser pipeline
/// (Layer 2/3) は Phase 6a、本 Phase では Layer 1 の byte 到着のみ。
pub(in crate::daemon) fn tty_master_read(bytes: &[u8]) -> DaemonMsg {
    DaemonMsg::Tty(TtyMsg::Layer1Bytes {
        bytes: bytes.to_vec(),
    })
}

/// SIGCHLD self-pipe 発火 → Child SIGCHLD event (DR-0025 §IO boundary の
/// `DaemonMsg::Child(SigchldReceived { pid })`)。
///
/// serve_loop の `sigchld_ready` 経路で発火する。`pid` は監視対象の子 PID。waitpid
/// による state 遷移解釈は Phase 3 の Child reducer が担う。
pub(in crate::daemon) fn sigchld_received(pid: Pid) -> DaemonMsg {
    DaemonMsg::Child(ChildMsg::SigchldReceived { pid })
}

/// 子 exit 検出 → Child Reaped event (DR-0025 §Child domain の `Reaped`)。
///
/// `exit_status` は判明していれば `Some(code)`、未取得なら `None` (= 既存
/// `RelayOutcome::ChildExited(Option<i32>)` と同じ意味論)。
pub(in crate::daemon) fn child_reaped(exit_status: Option<i32>) -> DaemonMsg {
    DaemonMsg::Child(ChildMsg::Reaped { exit_status })
}

/// client frame 受信 → Client FrameReceived event (DR-0025 §IO boundary の
/// `DaemonMsg::Client(FrameReceived..)`)。
///
/// serve_loop の client reader decode `Ok(frame)` 経路で写す。frame 実体は運ばず
/// client id のみ (= placeholder 主義)。raw_data / control の認可判定は Phase 2 の
/// Client reducer が LockState view を read して担う。
pub(in crate::daemon) fn client_frame_received(client_id: u64) -> DaemonMsg {
    DaemonMsg::Client(ClientMsg::FrameReceived {
        client_id: ClientId(client_id),
    })
}

/// raw_data frame (= `TYPE_RAW_DATA`) → Client RawDataReceived event
/// (DR-0025 §Phase 2-β の実体化点)。
///
/// `mode` は認可判定 (Ro reject) 用の client mode スナップショット (= ClientRegistry
/// pure ミラー導入までの中間形)。bytes は frame body の所有権をそのまま受け取る
/// (= copy しない)。
pub(in crate::daemon) fn client_raw_data(
    client_id: u64,
    mode: crate::protocol::Mode,
    bytes: Vec<u8>,
) -> DaemonMsg {
    DaemonMsg::Client(ClientMsg::RawDataReceived {
        client_id,
        mode,
        bytes,
    })
}

/// client drop (detach cascade) → Client Detached event (DR-0025 §cross-domain
/// `Client ──→ Serve` / `Client ──→ Lock` の起点)。
///
/// serve_loop の drop cascade で client を remove する時に写す。Phase 2 で Client
/// reducer が lock auto-release (`Client ──→ Lock`) / leader cascade を 1 本化する。
pub(in crate::daemon) fn client_detached(client_id: u64) -> DaemonMsg {
    DaemonMsg::Client(ClientMsg::Detached {
        client_id: ClientId(client_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // gate 1b-3: translate 関数群の characterization test。各 IO event から
    // 対応する DaemonMsg variant が 1:1 で生成されることを固定する。DaemonMsg /
    // 各 domain Msg は `#[derive(Debug)]` のみで PartialEq を持たないため、pattern
    // match で variant を弁別し内部 field を assert する形で入出力を凍結する。
    // これらは serve_loop への挿入位置には依存しない (= translate は pure)。後続
    // Phase で reducer に振る舞いが入っても本写像が不変であることの回帰基準。

    /// master read bytes は `TtyMsg::Layer1Bytes` に byte 列そのままで写る (= 無加工転送、
    /// parse は Layer 2/3 = Phase 6a の責務)。
    #[test]
    fn tty_master_read_maps_bytes_verbatim() {
        let raw = [0x1b, b'[', b'A']; // ESC [ A (= CUU、意味解釈はせず byte のまま)
        match tty_master_read(&raw) {
            DaemonMsg::Tty(TtyMsg::Layer1Bytes { bytes }) => {
                assert_eq!(bytes, raw.to_vec(), "byte 列を無加工で運ぶ");
            }
            other => panic!("expected Tty(Layer1Bytes), got {other:?}"),
        }
    }

    /// 空 slice も `Layer1Bytes(空)` に写る (= 空判定や EOF 解釈は reducer 側の責務、
    /// translate は写すだけ)。serve_loop 側で `Ok(0)` は別経路 (child_reaped) に流れるが、
    /// 関数単体としては空入力を素直に写せることを固定する。
    #[test]
    fn tty_master_read_maps_empty_slice() {
        match tty_master_read(&[]) {
            DaemonMsg::Tty(TtyMsg::Layer1Bytes { bytes }) => {
                assert!(bytes.is_empty(), "空 read は空 Layer1Bytes")
            }
            other => panic!("expected Tty(Layer1Bytes), got {other:?}"),
        }
    }

    /// SIGCHLD 発火は監視対象 pid を載せた `Child::SigchldReceived` に写る。
    #[test]
    fn sigchld_received_carries_pid() {
        match sigchld_received(Pid::from_raw(4321)) {
            DaemonMsg::Child(ChildMsg::SigchldReceived { pid }) => {
                assert_eq!(pid, Pid::from_raw(4321), "監視対象 pid を保持");
            }
            other => panic!("expected Child(SigchldReceived), got {other:?}"),
        }
    }

    /// 子 exit の code(Some) は `Reaped` にそのまま保存される。
    #[test]
    fn child_reaped_preserves_exit_code() {
        match child_reaped(Some(42)) {
            DaemonMsg::Child(ChildMsg::Reaped { exit_status }) => {
                assert_eq!(exit_status, Some(42));
            }
            other => panic!("expected Child(Reaped), got {other:?}"),
        }
    }

    /// code 未取得 (None) も `Reaped(None)` に写る (= `RelayOutcome::ChildExited(None)`
    /// と同意味論。code 取得は waitpid の別責務なので translate は None を保つ)。
    #[test]
    fn child_reaped_preserves_unknown_exit() {
        match child_reaped(None) {
            DaemonMsg::Child(ChildMsg::Reaped { exit_status }) => {
                assert_eq!(exit_status, None)
            }
            other => panic!("expected Child(Reaped), got {other:?}"),
        }
    }

    /// client frame 受信は素の `u64` id を `ClientId` newtype に包んで `FrameReceived`
    /// に写す (= frame 実体は運ばない placeholder 主義)。
    #[test]
    fn client_frame_received_wraps_id() {
        match client_frame_received(7) {
            DaemonMsg::Client(ClientMsg::FrameReceived { client_id }) => {
                assert_eq!(client_id, ClientId(7));
            }
            other => panic!("expected Client(FrameReceived), got {other:?}"),
        }
    }

    /// client detach は `Detached` に id を包んで写す (= Phase 2 の lock auto-release /
    /// leader cascade 1 本化の起点)。
    #[test]
    fn client_detached_wraps_id() {
        match client_detached(9) {
            DaemonMsg::Client(ClientMsg::Detached { client_id }) => {
                assert_eq!(client_id, ClientId(9));
            }
            other => panic!("expected Client(Detached), got {other:?}"),
        }
    }
}
