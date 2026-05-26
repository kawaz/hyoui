//! Transport 抽象 (DR-0008 §5)。
//!
//! `Transport` は wire 上で `Frame` を流す薄い `Send + 'static` 層。daemon の
//! multi-attach は thread + bounded channel で実装するため、read 側と write 側を
//! 別 owner に move できる必要がある。これは [`Transport::split`] が担う。
//!
//! 各 transport は同じ wire format (= `Frame` の `[u32 LE size][u8 type][body]`)
//! を流す。上位 (`daemon` / `client`) は `Frame` 単位で扱い、Transport 種別 (Unix /
//! TCP / WebSocket / SSH stdio) を意識しない。

use std::io::{Read, Write};

mod unix;

pub use unix::UnixStreamTransport;

/// 1 connection 分の双方向 wire。
///
/// `split` で「frame を読む側」と「frame を書く側」を別 owner に分離できる。
/// MVP の daemon は per-client writer thread + main thread reader poll の構成で
/// 使う。
pub trait Transport: Send + 'static {
    /// 読む側 (frame の `decode_from` に渡す)。
    type Reader: Read + Send + 'static;

    /// 書く側 (frame の `encode_to` に渡す)。
    type Writer: Write + Send + 'static;

    /// reader と writer に分割する。
    ///
    /// # Errors
    ///
    /// 内部 fd の `try_clone` 等で失敗する可能性がある (= OS resource limit 等)。
    fn split(self) -> std::io::Result<(Self::Reader, Self::Writer)>;
}
