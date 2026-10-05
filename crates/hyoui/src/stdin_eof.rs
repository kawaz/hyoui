//! 非 tty stdin の EOF を子 PTY に伝える byte 列の決定 (DR-0019 §5)。
//!
//! 子の stdin は PTY なので pipe の EOF そのものは届かない。canonical mode の line
//! discipline では VEOF (0x04) が「その時点の行を区切りなしで読み手に渡す」働きをし、
//! 行頭 (= 未読の行が空) で来た時だけ読み手の `read(2)` が 0 を返す (= EOF)。
//! 入力の最後が改行なら 0x04 1 個で行頭の EOF になるが、改行で終わらない時の 1 個は
//! 途中の行を確定させるだけで EOF にならない。2 個送ると 1 個目で途中の行を渡し、
//! 2 個目が行頭の EOF になる。
//!
//! attach client (stdin を raw_data で送る) と daemon (`run --detached` で引き継いだ
//! stdin を PTY に流す) の両方が本 module で同じ判定をする。

/// stdin EOF を観測した時の挙動 (DR-0019 §5)。attach client と detached の daemon 転送で
/// 共通。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StdinEofAction {
    /// EOF を子に伝えない。attach client は自分から離脱し (= 子は daemon 配下に残る)、
    /// detached の daemon 転送は転送を止めるだけ。
    Detach,
    /// EOT (0x04) を子 PTY に送る ([`EofTracker::eof_bytes`] の個数)。canonical mode の
    /// 子 (例: bc / cat) は行頭の EOT を read EOF として解釈するため、
    /// `echo "1+2" | hyoui run -- bc` のような pattern で子が自然終了する。
    SendEof,
}

/// attach client の stdin EOF 挙動 (DR-0019 §5)。`--stdin-eof` の明示値が優先し、無ければ
/// 非 tty は EOF を伝え (= pipe-through の透過性回復)、tty は離脱する。
pub fn attach_eof_action(stdin_is_tty: bool, explicit: Option<StdinEofAction>) -> StdinEofAction {
    explicit.unwrap_or(if stdin_is_tty {
        StdinEofAction::Detach
    } else {
        StdinEofAction::SendEof
    })
}

/// `run --detached` で daemon が stdin を子へ転送するか (DR-0019 §5)。`Some(action)` なら
/// 転送し、EOF で `action` に従う。tty は読まない (= detached は端末を読まない)。非 tty は
/// `/dev/null` も含めて種類を問わず転送し、EOF 挙動は attach と同じ (= 明示値、無ければ
/// EOF を伝える)。子が EOT をどう解釈するかは hyoui の責務外で、attach と detached で同じ
/// 結果になることだけを保証する。
pub fn detached_forward(
    stdin_is_tty: bool,
    explicit: Option<StdinEofAction>,
) -> Option<StdinEofAction> {
    (!stdin_is_tty).then(|| attach_eof_action(false, explicit))
}

/// EOF (0x04)。canonical mode の既定 VEOF。
const EOT: u8 = 0x04;

/// 子 PTY に送り終えた入力の末尾を覚え、EOF 時に送る byte 列を決める。
///
/// 末尾だけを見るのは、line discipline の行 buffer に未確定の行が残っているかが
/// 「最後に送った byte が行区切りか」で決まるため。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EofTracker {
    last: Option<u8>,
}

impl EofTracker {
    /// 空の状態 (= まだ何も送っていない)。
    pub fn new() -> Self {
        Self::default()
    }

    /// 子 PTY に送った bytes を記録する。空 slice は末尾を変えない。
    pub fn observe(&mut self, forwarded: &[u8]) {
        if let Some(&b) = forwarded.last() {
            self.last = Some(b);
        }
    }

    /// EOF を伝えるために送る byte 列。
    ///
    /// - 何も送っていない / 最後が改行 (LF / CR): `[0x04]` (= 行頭の EOF)
    /// - それ以外: `[0x04, 0x04]` (= 途中の行の確定 + 行頭の EOF)
    ///
    /// CR も行区切りに数えるのは、既定の ICRNL で CR が LF に変換されて行を閉じるため。
    pub fn eof_bytes(&self) -> &'static [u8] {
        match self.last {
            None | Some(b'\n' | b'\r') => &[EOT],
            Some(_) => &[EOT, EOT],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 何も送っていない (= 空入力 / 即 EOF) は行頭なので 1 個。
    #[test]
    fn nothing_forwarded_sends_one_eot() {
        assert_eq!(EofTracker::new().eof_bytes(), &[0x04]);
    }

    /// 改行で終わる入力は行頭に居るので 1 個 (= `echo 1+2 | hyoui run -- bc`)。
    #[test]
    fn trailing_lf_or_cr_sends_one_eot() {
        let mut t = EofTracker::new();
        t.observe(b"l1\nl2\n");
        assert_eq!(t.eof_bytes(), &[0x04]);
        let mut t = EofTracker::new();
        t.observe(b"line\r");
        assert_eq!(t.eof_bytes(), &[0x04]);
    }

    /// 改行で終わらない入力 (= `printf hoge |`) は途中の行の確定と EOF で 2 個。
    #[test]
    fn unterminated_line_sends_two_eots() {
        let mut t = EofTracker::new();
        t.observe(b"hoge");
        assert_eq!(t.eof_bytes(), &[0x04, 0x04]);
    }

    /// attach: 明示値が優先、無ければ非 tty だけ EOF を伝える。
    #[test]
    fn attach_eof_action_by_tty() {
        use StdinEofAction::{Detach, SendEof};
        assert_eq!(attach_eof_action(false, None), SendEof);
        assert_eq!(attach_eof_action(true, None), Detach);
        assert_eq!(attach_eof_action(false, Some(Detach)), Detach);
        assert_eq!(attach_eof_action(true, Some(SendEof)), SendEof);
    }

    /// detached: tty は読まない。非 tty は attach と同じ EOF 挙動で転送する。
    #[test]
    fn detached_forward_matches_attach_for_non_tty() {
        use StdinEofAction::{Detach, SendEof};
        assert_eq!(detached_forward(true, None), None);
        assert_eq!(detached_forward(true, Some(SendEof)), None);
        for explicit in [None, Some(Detach), Some(SendEof)] {
            assert_eq!(
                detached_forward(false, explicit),
                Some(attach_eof_action(false, explicit))
            );
        }
    }

    /// 判定は最後に送った chunk の末尾で決まる (= chunk 境界を跨いでも正しい)。
    /// 空 chunk は末尾を変えない。
    #[test]
    fn last_chunk_tail_decides() {
        let mut t = EofTracker::new();
        t.observe(b"abc\n");
        t.observe(b"de");
        assert_eq!(t.eof_bytes(), &[0x04, 0x04]);
        t.observe(b"f\n");
        t.observe(b"");
        assert_eq!(t.eof_bytes(), &[0x04]);
    }
}
