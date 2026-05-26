//! Frame layer: `[u32 LE size][u8 type][body]`、wire 外枠は永久固定。
//!
//! [`Frame::encode_to`] / [`Frame::decode_from`] が wire 上の 1 frame 単位を
//! バイトストリームに対して読み書きする。上位 (`messages::*`) は body bytes
//! を CBOR で encode/decode する。

use std::io::{self, Read, Write};

/// 1 frame の最大サイズ (DR-0008 §1.1)。
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Raw PTY data frame の type tag (body は raw bytes 透過)。
pub const TYPE_RAW_DATA: u8 = 0x00;

/// CBOR-encoded control message frame の type tag (body は 1 個の CBOR item)。
pub const TYPE_CBOR_CONTROL: u8 = 0x01;

/// Protocol-level violation (recoverable: 通常は当該 peer を disconnect)。
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// `size` が `MAX_FRAME_SIZE` を超過。
    #[error("frame size {0} exceeds MAX_FRAME_SIZE ({MAX_FRAME_SIZE})")]
    FrameTooLarge(u32),

    /// `size < 1` で `type` byte すら入らない。
    #[error("frame size {0} is too small to contain type byte")]
    FrameTooSmall(u32),

    /// `type` byte が未知 (`0x02..` 等)。
    #[error("unknown frame type tag: 0x{0:02x}")]
    UnknownType(u8),

    /// peer が frame の途中で接続を閉じた。
    #[error("unexpected EOF: {0}")]
    UnexpectedEof(&'static str),
}

/// I/O error と protocol violation を束ねる frame layer の error 型。
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// 下位の I/O syscall 失敗。
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// wire 上の protocol violation。
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
}

/// 1 frame 分の wire データ。
///
/// `ty` は demux tag (`TYPE_RAW_DATA` or `TYPE_CBOR_CONTROL`)、`body` は
/// type に応じた中身 (raw bytes or CBOR-encoded item の bytes)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// frame type tag。
    pub ty: u8,
    /// frame body bytes。
    pub body: Vec<u8>,
}

impl Frame {
    /// 新しい raw-data frame を組み立てる。
    pub fn raw_data(body: Vec<u8>) -> Self {
        Frame {
            ty: TYPE_RAW_DATA,
            body,
        }
    }

    /// 新しい CBOR-control frame を組み立てる (body は既に CBOR encode 済)。
    pub fn cbor_control(body: Vec<u8>) -> Self {
        Frame {
            ty: TYPE_CBOR_CONTROL,
            body,
        }
    }

    /// frame をバイトストリームに書き出す。
    ///
    /// Wire layout: `[u32 LE size][u8 type][body]`、`size = 1 + body.len()`。
    ///
    /// # Errors
    ///
    /// * [`FrameError::Protocol`] — `body.len() + 1 > MAX_FRAME_SIZE`。
    /// * [`FrameError::Io`] — 下位 I/O が失敗。
    pub fn encode_to<W: Write>(&self, w: &mut W) -> Result<(), FrameError> {
        let size = self
            .body
            .len()
            .checked_add(1)
            .ok_or(ProtocolError::FrameTooLarge(u32::MAX))?;
        if size > MAX_FRAME_SIZE {
            return Err(ProtocolError::FrameTooLarge(size as u32).into());
        }
        // size は u32 に収まる (MAX_FRAME_SIZE = 16 MiB)。
        let size_u32 = size as u32;

        // header + body を 1 つの Vec にまとめて write_all (= 中途半端な
        // 半送信を avoid)。
        let mut buf = Vec::with_capacity(4 + size);
        buf.extend_from_slice(&size_u32.to_le_bytes());
        buf.push(self.ty);
        buf.extend_from_slice(&self.body);
        w.write_all(&buf)?;
        Ok(())
    }

    /// バイトストリームから 1 frame を読み出す。
    ///
    /// # Errors
    ///
    /// * [`FrameError::Protocol::FrameTooLarge`] — `size > MAX_FRAME_SIZE`。
    /// * [`FrameError::Protocol::FrameTooSmall`] — `size < 1` (type byte が入らない)。
    /// * [`FrameError::Protocol::UnknownType`] — `type` byte が `0x02..`。
    /// * [`FrameError::Protocol::UnexpectedEof`] — peer が EOF/部分受信で切断。
    /// * [`FrameError::Io`] — 下位 I/O 失敗。
    pub fn decode_from<R: Read>(r: &mut R) -> Result<Frame, FrameError> {
        let mut size_buf = [0u8; 4];
        read_exact_eof(r, &mut size_buf, "size header")?;
        let size = u32::from_le_bytes(size_buf);

        if (size as usize) > MAX_FRAME_SIZE {
            return Err(ProtocolError::FrameTooLarge(size).into());
        }
        if size < 1 {
            return Err(ProtocolError::FrameTooSmall(size).into());
        }

        let mut ty_buf = [0u8; 1];
        read_exact_eof(r, &mut ty_buf, "type byte")?;
        let ty = ty_buf[0];
        if ty != TYPE_RAW_DATA && ty != TYPE_CBOR_CONTROL {
            return Err(ProtocolError::UnknownType(ty).into());
        }

        let body_len = (size - 1) as usize;
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            read_exact_eof(r, &mut body, "body")?;
        }

        Ok(Frame { ty, body })
    }
}

/// `read_exact` のラッパー。`std::io::Error` の `UnexpectedEof` を
/// [`ProtocolError::UnexpectedEof`] に変換する。
fn read_exact_eof<R: Read>(
    r: &mut R,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), FrameError> {
    match r.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            Err(ProtocolError::UnexpectedEof(what).into())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn roundtrip(frame: &Frame) -> Frame {
        let mut buf = Vec::new();
        frame.encode_to(&mut buf).expect("encode");
        let mut cur = Cursor::new(buf);
        Frame::decode_from(&mut cur).expect("decode")
    }

    #[test]
    fn raw_data_empty_roundtrip() {
        let f = Frame::raw_data(Vec::new());
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn raw_data_small_roundtrip() {
        let f = Frame::raw_data(b"hello, hyoui".to_vec());
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn cbor_control_roundtrip() {
        // 中身は実 CBOR でなくてもよい (frame layer は body bytes 透過)。
        let f = Frame::cbor_control(vec![0xa1, 0x63, b'a', b'b', b'c', 0x01]);
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn raw_data_max_size_roundtrip() {
        // body = MAX_FRAME_SIZE - 1 (= type byte の分を引いた最大 body)。
        let body = vec![0xAB; MAX_FRAME_SIZE - 1];
        let f = Frame::raw_data(body);
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn encode_rejects_oversized_body() {
        // body = MAX_FRAME_SIZE (= 1 byte over)
        let f = Frame::raw_data(vec![0u8; MAX_FRAME_SIZE]);
        let err = f.encode_to(&mut Vec::new()).expect_err("must reject");
        match err {
            FrameError::Protocol(ProtocolError::FrameTooLarge(_)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_oversized_size() {
        let mut buf = Vec::new();
        let bogus = (MAX_FRAME_SIZE as u32) + 1;
        buf.extend_from_slice(&bogus.to_le_bytes());
        let mut cur = Cursor::new(buf);
        let err = Frame::decode_from(&mut cur).expect_err("must reject");
        match err {
            FrameError::Protocol(ProtocolError::FrameTooLarge(n)) => assert_eq!(n, bogus),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_zero_size() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_le_bytes());
        let mut cur = Cursor::new(buf);
        let err = Frame::decode_from(&mut cur).expect_err("must reject");
        match err {
            FrameError::Protocol(ProtocolError::FrameTooSmall(0)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_unknown_type() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes()); // size = 1 (type byte のみ)
        buf.push(0x02); // 予約 type
        let mut cur = Cursor::new(buf);
        let err = Frame::decode_from(&mut cur).expect_err("must reject");
        match err {
            FrameError::Protocol(ProtocolError::UnknownType(0x02)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_eof_before_header() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        let err = Frame::decode_from(&mut cur).expect_err("must error");
        match err {
            FrameError::Protocol(ProtocolError::UnexpectedEof("size header")) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_eof_in_type_byte() {
        // size header だけあって type byte が無い
        let mut buf = Vec::new();
        buf.extend_from_slice(&5u32.to_le_bytes());
        let mut cur = Cursor::new(buf);
        let err = Frame::decode_from(&mut cur).expect_err("must error");
        match err {
            FrameError::Protocol(ProtocolError::UnexpectedEof("type byte")) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_eof_in_body() {
        // size = 5 だが body 1 byte で打ち切り
        let mut buf = Vec::new();
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.push(TYPE_RAW_DATA);
        buf.push(b'a'); // body 1 byte だけ (4 byte 足りない)
        let mut cur = Cursor::new(buf);
        let err = Frame::decode_from(&mut cur).expect_err("must error");
        match err {
            FrameError::Protocol(ProtocolError::UnexpectedEof("body")) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn wire_layout_little_endian() {
        // body = "hi" (2 byte)、type = 0x00 → size = 3
        // → wire = 03 00 00 00 | 00 | 68 69
        let f = Frame::raw_data(b"hi".to_vec());
        let mut buf = Vec::new();
        f.encode_to(&mut buf).expect("encode");
        assert_eq!(buf, vec![0x03, 0x00, 0x00, 0x00, 0x00, b'h', b'i']);
    }

    #[test]
    fn wire_layout_cbor_control() {
        // body = [0xa1, 0x01, 0x02] (3 byte 想定の CBOR map)、type = 0x01 → size = 4
        // → wire = 04 00 00 00 | 01 | a1 01 02
        let f = Frame::cbor_control(vec![0xa1, 0x01, 0x02]);
        let mut buf = Vec::new();
        f.encode_to(&mut buf).expect("encode");
        assert_eq!(buf, vec![0x04, 0x00, 0x00, 0x00, 0x01, 0xa1, 0x01, 0x02]);
    }
}
