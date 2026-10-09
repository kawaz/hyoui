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

/// Raw-data write 完了 ack frame の type tag (DR-0021)。
///
/// body は 1 個の CBOR map で、`RawAck` schema (`{ "result": "ok" | "error",
/// "code": ..., "message": ... }`) を持つ。`TYPE_RAW_DATA` を受け取った daemon が
/// master PTY への `write_all_with_idle_timeout` を return した時点で client に
/// 返す (= bytes 系 spec の完了点を「socket flush」から「PTY drain」に強める)。
///
/// 明示 seq id は持たない (= connection-level の同期 = 1 raw_data → 1 ack)。client
/// は次の raw_data を送る前に ack 受信を待つことで race を排除する。
pub const TYPE_RAW_ACK: u8 = 0x02;

/// Protocol-level violation (recoverable: 通常は当該 peer を disconnect)。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
#[non_exhaustive]
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

    /// 新しい raw-ack frame を組み立てる (DR-0021、body は CBOR encode 済 `RawAck`)。
    pub fn raw_ack(body: Vec<u8>) -> Self {
        Frame {
            ty: TYPE_RAW_ACK,
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
        check_size(size)?;

        let mut ty_buf = [0u8; 1];
        read_exact_eof(r, &mut ty_buf, "type byte")?;
        let ty = ty_buf[0];
        check_type(ty)?;

        let body_len = (size - 1) as usize;
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            read_exact_eof(r, &mut body, "body")?;
        }

        Ok(Frame { ty, body })
    }
}

/// size header の検証 ([`Frame::decode_from`] と [`FrameDecoder`] で共有)。
fn check_size(size: u32) -> Result<(), ProtocolError> {
    if (size as usize) > MAX_FRAME_SIZE {
        return Err(ProtocolError::FrameTooLarge(size));
    }
    if size < 1 {
        return Err(ProtocolError::FrameTooSmall(size));
    }
    Ok(())
}

/// type byte の検証 ([`Frame::decode_from`] と [`FrameDecoder`] で共有)。
fn check_type(ty: u8) -> Result<(), ProtocolError> {
    if ty != TYPE_RAW_DATA && ty != TYPE_CBOR_CONTROL && ty != TYPE_RAW_ACK {
        return Err(ProtocolError::UnknownType(ty));
    }
    Ok(())
}

/// size header (4 byte) + type byte (1 byte)。
const SIZE_HEADER_LEN: usize = 4;

/// frame を取り出した後に decoder が持ち続けてよい capacity の下限側の上限。
///
/// frame を取り出した後の capacity は `max(RETAINED_CAPACITY, 2 * 未消費 bytes)` を
/// 超えない ([`FrameDecoder::next_frame`])。大きな frame (最大 16 MiB) を 1 度受けた
/// client が、続く小さな未消費分 (次の frame の header 1 byte 等) だけを残して止まって
/// も、その capacity を接続中ずっと抱え続けないための上限。
const RETAINED_CAPACITY: usize = 64 * 1024;

/// 届いた bytes を順に受け取り、frame が揃った分だけ取り出す増分 decoder
/// (DR-0037 段階 1、client 受信)。
///
/// [`Frame::decode_from`] は frame を読み切るまで `read_exact` で待つため、相手が
/// frame の途中で止まると呼び出し側が戻らない。本 decoder は I/O を持たない純粋な
/// state で、呼び出し側が `recv` で得た bytes を [`push`](Self::push) し、
/// [`next_frame`](Self::next_frame) で揃った frame を 1 つずつ取り出す。
///
/// - size header が揃った時点で size を検証し、type byte が揃った時点で type を検証する
///   (上限超過の size を宣言した frame の body を待たずに error にする)
/// - error を返した後の状態は保証しない (呼び出し側は当該 peer を切る)
/// - 受信途中の bytes は push された分だけ保持する (宣言 size の先行確保はしない)
#[derive(Debug, Default)]
pub struct FrameDecoder {
    /// 受信済みで未消費の bytes は `buf[pos..]`。
    buf: Vec<u8>,
    /// 取り出し済み frame の末尾 (= 次の frame の先頭)。
    pos: usize,
}

impl FrameDecoder {
    /// 空の decoder を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 受信した bytes を末尾に足す。
    pub fn push(&mut self, bytes: &[u8]) {
        // 取り出し済みの prefix はここでまとめて詰める (= frame を 1 つ取り出すたびに
        // 詰めると、1 回の受信に小さい frame が多数入った時に memmove が frame 数に
        // 比例して重なる)。
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// 揃った frame を 1 つ取り出す。揃っていなければ `Ok(None)`。
    ///
    /// # Errors
    ///
    /// * [`ProtocolError::FrameTooLarge`] — size が `MAX_FRAME_SIZE` を超える
    /// * [`ProtocolError::FrameTooSmall`] — size が 0
    /// * [`ProtocolError::UnknownType`] — 未知の type byte
    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtocolError> {
        let Some(total) = self.complete_frame_len()? else {
            return Ok(None);
        };
        let start = self.pos;
        let ty = self.buf[start + SIZE_HEADER_LEN];
        let body = self.buf[start + SIZE_HEADER_LEN + 1..start + total].to_vec();
        self.pos += total;
        let remaining = self.buf.len() - self.pos;
        if self.buf.capacity() > RETAINED_CAPACITY && self.buf.capacity() / 2 >= remaining {
            // 確保が未消費分に対して過大なので、未消費分だけを新しい領域へ移す。移すのは
            // capacity の半分以下で、移した後の capacity は未消費分ちょうどになるため、
            // 続けて取り出しても移す量は毎回半分以下に減る (= 全量 memmove を繰り返さない)。
            self.buf = self.buf[self.pos..].to_vec();
            self.pos = 0;
        } else if remaining == 0 {
            self.buf.clear();
            self.pos = 0;
        }
        Ok(Some(Frame { ty, body }))
    }

    /// 追加の受信なしで [`next_frame`](Self::next_frame) が `Ok(None)` 以外を返すか
    /// (= 揃った frame か、検出済みの protocol error が buffer にある)。
    pub fn is_ready(&self) -> bool {
        !matches!(self.complete_frame_len(), Ok(None))
    }

    /// 先頭 frame が揃っていればその wire 上の長さ (header 込み) を返す。
    fn complete_frame_len(&self) -> Result<Option<usize>, ProtocolError> {
        let avail = &self.buf[self.pos..];
        let Some(size_bytes) = avail.first_chunk::<SIZE_HEADER_LEN>() else {
            return Ok(None);
        };
        let size = u32::from_le_bytes(*size_bytes);
        check_size(size)?;
        let Some(&ty) = avail.get(SIZE_HEADER_LEN) else {
            return Ok(None);
        };
        check_type(ty)?;
        let total = SIZE_HEADER_LEN + size as usize;
        Ok((avail.len() >= total).then_some(total))
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
        // 0x02 は TYPE_RAW_ACK (DR-0021) で許可されるようになったので、未知 type の
        // 検証は 0x03 以降の予約値を使う。
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes()); // size = 1 (type byte のみ)
        buf.push(0x03); // 予約 type
        let mut cur = Cursor::new(buf);
        let err = Frame::decode_from(&mut cur).expect_err("must reject");
        match err {
            FrameError::Protocol(ProtocolError::UnknownType(0x03)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn raw_ack_empty_roundtrip() {
        let f = Frame::raw_ack(Vec::new());
        assert_eq!(roundtrip(&f), f);
        assert_eq!(f.ty, TYPE_RAW_ACK);
    }

    #[test]
    fn raw_ack_with_cbor_body_roundtrip() {
        // CBOR map {"result": "ok"} = a1 66 72 65 73 75 6c 74 62 6f 6b
        let body = vec![
            0xa1, 0x66, b'r', b'e', b's', b'u', b'l', b't', 0x62, b'o', b'k',
        ];
        let f = Frame::raw_ack(body);
        assert_eq!(roundtrip(&f), f);
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

    // ---- FrameDecoder (DR-0037 段階 1) ----

    fn encode_all(frames: &[Frame]) -> Vec<u8> {
        let mut wire = Vec::new();
        for f in frames {
            f.encode_to(&mut wire).expect("encode");
        }
        wire
    }

    /// decoder から揃った frame を全部取り出す (error は panic)。
    fn drain_frames(dec: &mut FrameDecoder) -> Vec<Frame> {
        let mut out = Vec::new();
        while let Some(f) = dec.next_frame().expect("no protocol error") {
            out.push(f);
        }
        out
    }

    /// 3 種の type と空 body / 1 byte body / 数百 byte body を混ぜた frame 列。
    fn sample_frames() -> Vec<Frame> {
        vec![
            Frame::raw_data(Vec::new()),
            Frame::cbor_control(vec![0xa1, 0x61, b'k', 0x01]),
            Frame::raw_ack(vec![0xa0]),
            Frame::raw_data((0..=255u8).cycle().take(300).collect()),
            Frame::raw_data(vec![b'z']),
        ]
    }

    /// wire を 2 箇所 (i, j) で 3 分割して届けるすべての組合せで、元の frame 列が
    /// 順序どおり過不足なく取り出せる (= header 内・type byte・body 内・frame 境界の
    /// どこで切れても組み立て直せる)。
    #[test]
    fn decoder_reassembles_across_every_split_pair() {
        let frames = sample_frames();
        let wire = encode_all(&frames);
        for i in 0..=wire.len() {
            for j in i..=wire.len() {
                let mut dec = FrameDecoder::new();
                let mut got = Vec::new();
                for chunk in [&wire[..i], &wire[i..j], &wire[j..]] {
                    dec.push(chunk);
                    got.extend(drain_frames(&mut dec));
                }
                assert_eq!(got, frames, "split at ({i}, {j})");
                assert!(!dec.is_ready(), "nothing left after ({i}, {j})");
            }
        }
    }

    /// 1 byte ずつ届けても、frame の最終 byte が届いた瞬間にだけ 1 frame 取り出せる。
    #[test]
    fn decoder_byte_by_byte_yields_frame_exactly_at_last_byte() {
        let frames = sample_frames();
        let wire = encode_all(&frames);
        // 各 frame の終端 offset (exclusive)。
        let mut ends = Vec::new();
        let mut off = 0;
        for f in &frames {
            off += SIZE_HEADER_LEN + 1 + f.body.len();
            ends.push(off);
        }
        let mut dec = FrameDecoder::new();
        let mut got = Vec::new();
        for (k, b) in wire.iter().enumerate() {
            dec.push(std::slice::from_ref(b));
            let ready = dec.is_ready();
            assert_eq!(ready, ends.contains(&(k + 1)), "is_ready after byte {k}");
            if let Some(f) = dec.next_frame().expect("no error") {
                got.push(f);
            }
            assert!(dec.next_frame().expect("no error").is_none());
        }
        assert_eq!(got, frames);
    }

    /// 1 回の push に複数 frame + 次 frame の途中までが入った場合、揃った frame を
    /// 1 つずつ取り出せ、途中の frame は残りが届いてから取り出せる。
    #[test]
    fn decoder_yields_multiple_frames_from_single_push_one_at_a_time() {
        let frames = sample_frames();
        let wire = encode_all(&frames);
        // 最後の frame の 1 byte 手前まで。
        let cut = wire.len() - 1;
        let mut dec = FrameDecoder::new();
        dec.push(&wire[..cut]);
        for want in &frames[..frames.len() - 1] {
            assert!(dec.is_ready());
            assert_eq!(dec.next_frame().expect("ok").as_ref(), Some(want));
        }
        assert!(!dec.is_ready(), "last frame is still partial");
        assert!(dec.next_frame().expect("ok").is_none());
        dec.push(&wire[cut..]);
        assert_eq!(
            dec.next_frame().expect("ok").as_ref(),
            frames.last(),
            "completed by the remaining byte"
        );
    }

    /// 上限超過の size は size header (4 byte) が揃った時点で error になる
    /// (= body を待たない)。宣言 size の分の buffer を先行確保しない。
    #[test]
    fn decoder_rejects_oversized_size_without_waiting_for_body() {
        let bogus = (MAX_FRAME_SIZE as u32) + 1;
        let header = bogus.to_le_bytes();
        let mut dec = FrameDecoder::new();
        dec.push(&header[..3]);
        assert!(!dec.is_ready());
        assert!(dec.next_frame().expect("3 bytes: undecided").is_none());
        dec.push(&header[3..]);
        assert!(dec.is_ready(), "error is ready without more input");
        match dec.next_frame() {
            Err(ProtocolError::FrameTooLarge(n)) => assert_eq!(n, bogus),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// 上限ちょうどの size は受理され (= body 待ち)、宣言 size の先行確保もしない。
    /// body を分割して届けると最大 frame も組み立てられる。
    #[test]
    fn decoder_accepts_max_size_frame_in_chunks_without_preallocating() {
        let body = vec![0xCD; MAX_FRAME_SIZE - 1];
        let wire = encode_all(&[Frame::raw_data(body.clone())]);
        let mut dec = FrameDecoder::new();
        dec.push(&wire[..SIZE_HEADER_LEN + 1]);
        assert!(!dec.is_ready(), "max size is not an error, waits for body");
        assert!(
            dec.buf.capacity() < RETAINED_CAPACITY,
            "must not reserve the declared size up front (capacity = {})",
            dec.buf.capacity()
        );
        for chunk in wire[SIZE_HEADER_LEN + 1..].chunks(64 * 1024) {
            assert!(dec.next_frame().expect("ok").is_none());
            dec.push(chunk);
        }
        let f = dec.next_frame().expect("ok").expect("complete");
        assert_eq!(f.ty, TYPE_RAW_DATA);
        assert_eq!(f.body, body);
        // 大きな frame を取り出した後は capacity を抱え続けない。
        assert_eq!(dec.buf.capacity(), 0);
    }

    /// 大きな frame の直後に次の frame の header 1 byte だけが続いて止まっても、大きな
    /// frame を取り出した時点で capacity が上限 (`max(RETAINED_CAPACITY, 2 * 未消費)`)
    /// 以下に戻り、残りの 1 byte は保たれて次の frame を組み立てられる。
    #[test]
    fn decoder_releases_capacity_when_tiny_remainder_follows_large_frame() {
        let big = Frame::raw_data(vec![0x5A; 4 * 1024 * 1024]);
        let next = Frame::cbor_control(vec![0xa0]);
        let next_wire = encode_all(std::slice::from_ref(&next));
        let mut wire = encode_all(std::slice::from_ref(&big));
        wire.push(next_wire[0]);

        let mut dec = FrameDecoder::new();
        for chunk in wire.chunks(64 * 1024) {
            dec.push(chunk);
        }
        assert!(
            dec.buf.capacity() > RETAINED_CAPACITY,
            "前提: 大きく確保している"
        );
        assert_eq!(dec.next_frame().expect("ok"), Some(big));
        assert!(
            dec.buf.capacity() <= RETAINED_CAPACITY,
            "capacity must drop with only 1 byte left (capacity = {})",
            dec.buf.capacity()
        );
        assert!(!dec.is_ready());
        dec.push(&next_wire[1..]);
        assert_eq!(dec.next_frame().expect("ok"), Some(next));
    }

    /// 取り出した後の capacity は、未消費分が大きくても `max(RETAINED_CAPACITY,
    /// 2 * 未消費)` を超えない (= 未消費分が半分以上を占める間は移さない)。
    #[test]
    fn decoder_capacity_bound_holds_with_large_remainder() {
        let big = Frame::raw_data(vec![1; 2 * 1024 * 1024]);
        let partial_next = Frame::raw_data(vec![2; 3 * 1024 * 1024]);
        let partial_wire = encode_all(std::slice::from_ref(&partial_next));
        let mut wire = encode_all(std::slice::from_ref(&big));
        // 次の frame の 1 MiB 分だけ (残りはまだ届いていない)。
        wire.extend_from_slice(&partial_wire[..1024 * 1024]);

        let mut dec = FrameDecoder::new();
        for chunk in wire.chunks(64 * 1024) {
            dec.push(chunk);
        }
        assert_eq!(dec.next_frame().expect("ok"), Some(big));
        let remaining = dec.buf.len() - dec.pos;
        assert_eq!(remaining, 1024 * 1024);
        assert!(
            dec.buf.capacity() <= RETAINED_CAPACITY.max(2 * remaining),
            "capacity {} exceeds bound for remaining {remaining}",
            dec.buf.capacity()
        );
        for chunk in partial_wire[1024 * 1024..].chunks(64 * 1024) {
            assert!(dec.next_frame().expect("ok").is_none());
            dec.push(chunk);
        }
        assert_eq!(dec.next_frame().expect("ok"), Some(partial_next));
        assert_eq!(dec.buf.capacity(), 0, "nothing left, nothing retained");
    }

    #[test]
    fn decoder_rejects_zero_size() {
        let mut dec = FrameDecoder::new();
        dec.push(&0u32.to_le_bytes());
        match dec.next_frame() {
            Err(ProtocolError::FrameTooSmall(0)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// 未知 type は type byte が揃った時点で error (= 宣言 body を待たない)。
    #[test]
    fn decoder_rejects_unknown_type_at_type_byte() {
        let mut dec = FrameDecoder::new();
        dec.push(&100u32.to_le_bytes());
        assert!(!dec.is_ready());
        dec.push(&[0x03]);
        match dec.next_frame() {
            Err(ProtocolError::UnknownType(0x03)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// 壊れた frame の手前に揃っている正常 frame は、error より先に取り出せる
    /// (= 「正常 frame を処理してから切断」の順序を decoder が崩さない)。
    #[test]
    fn decoder_yields_preceding_frames_before_corrupt_one() {
        let good = sample_frames();
        let mut wire = encode_all(&good);
        wire.extend_from_slice(&1u32.to_le_bytes());
        wire.push(0x7f); // 未知 type
        let mut dec = FrameDecoder::new();
        dec.push(&wire);
        for want in &good {
            assert_eq!(dec.next_frame().expect("ok").as_ref(), Some(want));
        }
        assert!(dec.is_ready(), "corrupt frame is ready as an error");
        match dec.next_frame() {
            Err(ProtocolError::UnknownType(0x7f)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// decoder と blocking の `decode_from` は同じ wire から同じ frame 列を得る。
    #[test]
    fn decoder_agrees_with_decode_from() {
        let frames = sample_frames();
        let wire = encode_all(&frames);
        let mut cur = Cursor::new(wire.clone());
        let mut via_read = Vec::new();
        for _ in 0..frames.len() {
            via_read.push(Frame::decode_from(&mut cur).expect("decode"));
        }
        let mut dec = FrameDecoder::new();
        dec.push(&wire);
        assert_eq!(drain_frames(&mut dec), via_read);
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
