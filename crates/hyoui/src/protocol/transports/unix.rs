//! Unix domain socket transport (DR-0008 §5 MVP)。
//!
//! [`std::os::unix::net::UnixStream`] を newtype で包んで [`Transport`] を
//! 実装する。reader/writer の分離は `UnixStream::try_clone()` で同 fd 複製。

use std::os::unix::net::UnixStream;

use super::Transport;

/// Unix domain socket 1 connection の wire 接続。
///
/// 通常は daemon が `bind`/`accept` した stream、または client が `connect` した
/// stream を受け取って `new` で wrap する。
#[derive(Debug)]
pub struct UnixStreamTransport {
    inner: UnixStream,
}

impl UnixStreamTransport {
    /// 既存 `UnixStream` を transport として wrap する。
    pub fn new(stream: UnixStream) -> Self {
        Self { inner: stream }
    }

    /// 内部の `UnixStream` を借りる (= local address や peer credentials の取得用)。
    pub fn as_stream(&self) -> &UnixStream {
        &self.inner
    }

    /// 内部の `UnixStream` を取り出す (= 所有権を呼び出し側に返す)。
    pub fn into_inner(self) -> UnixStream {
        self.inner
    }
}

impl Transport for UnixStreamTransport {
    type Reader = UnixStream;
    type Writer = UnixStream;

    fn split(self) -> std::io::Result<(UnixStream, UnixStream)> {
        let writer = self.inner.try_clone()?;
        Ok((self.inner, writer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ControlMessage, Frame, HandshakeRequest, Mode};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;

    fn pair() -> (UnixStreamTransport, UnixStreamTransport) {
        let (a, b) = StdUnixStream::pair().expect("pair");
        (UnixStreamTransport::new(a), UnixStreamTransport::new(b))
    }

    #[test]
    fn raw_data_frame_roundtrip_over_unix_socketpair() {
        let (left, right) = pair();
        let (mut lr, mut lw) = left.split().expect("split left");
        let (mut rr, _rw) = right.split().expect("split right");

        // left writer から右 reader に raw data frame を 1 つ流す
        let frame = Frame::raw_data(b"hello via unix socketpair".to_vec());
        let writer_thread = std::thread::spawn(move || {
            frame.encode_to(&mut lw).expect("encode");
        });

        let got = Frame::decode_from(&mut rr).expect("decode");
        writer_thread.join().expect("writer thread");
        assert_eq!(got, Frame::raw_data(b"hello via unix socketpair".to_vec()));

        // lr/rw が残っていることを compile check (= split 後も両端 4 つの handle 持てる)
        let _ = (&mut lr, _rw);
    }

    #[test]
    fn handshake_roundtrip_over_unix_socketpair() {
        let (left, right) = pair();
        let (_lr, mut lw) = left.split().expect("split left");
        let (mut rr, _rw) = right.split().expect("split right");

        let msg = ControlMessage::HandshakeRequest(HandshakeRequest {
            caps: vec!["data".into(), "lock".into()],
            mode: Mode::Rw,
            exclusive: false,
            detach_others: false,
            token: Some("tok-abc".into()),
        });
        let body = msg.encode_to_vec().expect("cbor encode");

        let frame = Frame::cbor_control(body);
        let writer = std::thread::spawn(move || {
            frame.encode_to(&mut lw).expect("encode frame");
        });

        let got_frame = Frame::decode_from(&mut rr).expect("decode frame");
        writer.join().expect("writer");
        assert_eq!(got_frame.ty, crate::protocol::TYPE_CBOR_CONTROL);

        let got_msg = ControlMessage::decode_from(got_frame.body.as_slice()).expect("decode msg");
        assert_eq!(got_msg, msg);
    }

    #[test]
    fn split_clones_underlying_fd() {
        let (a, _b) = StdUnixStream::pair().expect("pair");
        let t = UnixStreamTransport::new(a);
        let (mut r, mut w) = t.split().expect("split");

        // 同じ socket を指しているので、reader 側に書いた bytes は writer 側からも
        // 見える (= 双方向 stream)。ここでは writer 側に書いて reader 側から読む
        // 簡易確認。socketpair は対向なので writer → 相手側 _b → ... ではなく
        // writer/reader が同じ stream の両端のように振る舞う。
        //
        // 実際: try_clone() は同 fd の duplicate であり、書いた bytes は対向 (= _b)
        // のみが読める。よってここでは clone した w から書いて、_b から読めることを
        // 確認する。
        let mut other = _b;
        let payload = b"abcde";
        w.write_all(payload).expect("write");
        let mut buf = [0u8; 5];
        other.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, payload);

        // r からも書ける (try_clone なので双方向)
        let payload2 = b"fghij";
        r.write_all(payload2).expect("write via reader handle");
        let mut buf2 = [0u8; 5];
        other.read_exact(&mut buf2).expect("read 2");
        assert_eq!(&buf2, payload2);
    }
}
