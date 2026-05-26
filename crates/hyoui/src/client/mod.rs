//! hyoui client (attach 側、DR-0008 §Lifecycle)。
//!
//! daemon と対称の構造で、stdin → socket / socket → stdout の bytes 中継 +
//! resize/signal 等の control message 送信を担う。MVP は `attach` 1 種類のみ、
//! list/status/kill は薄い 1-shot client として後の Phase で追加。

mod attach;

pub use attach::{AttachOptions, ClientConnection, StdinEofAction, resolve_detach_prefix_from_env};
