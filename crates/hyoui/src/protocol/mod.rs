//! hyoui wire protocol (DR-0008 確定版)。
//!
//! - Frame layout: `[u32 LE size][u8 type][body]`、wire 外枠は永久固定。
//! - `type = 0x00` は raw PTY data (body は生 bytes 透過)。
//! - `type = 0x01` は CBOR-encoded control message (body は 1 個の CBOR item、典型は map)。
//! - `type = 0x02..` は予約 (受信時 protocol error)。
//! - Max frame size: 16 MiB。
//!
//! Schema evolution は cap flags (`Vec<String>`) 一本。固定 PROTOCOL_VERSION は持たない。
//! 詳細は `docs/decisions/DR-0008-protocol-design.md` を参照。

pub mod caps;
mod frame;
pub mod messages;
pub mod transports;

pub use caps::{MVP_CAPS, MissingCapability, intersect_caps, require_cap};
pub use frame::{
    Frame, FrameError, MAX_FRAME_SIZE, ProtocolError, TYPE_CBOR_CONTROL, TYPE_RAW_ACK,
    TYPE_RAW_DATA,
};
pub use messages::{
    ControlMessage, ControlMessageError, ErrorCode, ErrorMessage, HandshakeRequest,
    HandshakeResponse, Mode, RawAck, RawAckError, RawAckResult,
};
pub use transports::{Transport, UnixStreamTransport};
