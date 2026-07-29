//! tty stdin の byte stream を「キーイベント」と「素通し byte 列」に分解する
//! (DR-0029 §2 の判定表が正本)。
//!
//! attach client が意味を持たせるキーは **Ctrl+Z だけ**で、それ以外は解釈せず素通しする
//! (= DR-0005「子から見た透過性」)。にもかかわらず decoder が要るのは、端末が送る Ctrl+Z の
//! byte 列が **子アプリの有効化した keyboard protocol で変わる**ため:
//!
//! - `0x1a` — protocol 無効時の legacy 符号化
//! - `CSI 122;5u` — kitty keyboard protocol (`z`=122 / ctrl=5)。子が `CSI > 1 u` を出すと有効
//! - `CSI 27;5;122~` — xterm modifyOtherKeys。子が `CSI > 4;2 m` を出すと有効
//!
//! 単一 byte の比較だけで済ませると、`CSI > 1 u` を出す TUI (= claude code 等) の下で
//! Ctrl+Z ガードが丸ごと素通りする (Ghostty × claude code で実測、
//! docs/issue/2026-07-29-bug-ctrlz-guard-bypassed-by-keyboard-protocol.md)。
//!
//! 符号化差・`read(2)` 境界を跨いだ sequence の再組立・「途中まで一致したが外れた byte 列を
//! 取りこぼさず流す」は全部この層に閉じる。上位 (= [`super::attach`] のガード state machine)
//! はトークンだけを見て、符号化を一切知らない。
//!
//! **意図的に汎用 keymap 機構にしていない** (= 他キーを扱う予定が無い、DR-0029 §3 で
//! prefix key 体系を全廃した)。キーを増やす必要が出たら [`CTRLZ_ENCODINGS`] と同型の
//! テーブルを足し、[`KeyToken`] に variant を足すだけで済む素直さは保つ。

/// Ctrl+Z (= `SIGTSTP` を生む byte)。
pub const CTRL_Z_BYTE: u8 = 0x1a;

/// Ctrl+Z **押下**として扱う byte 列。
///
/// kitty CSI-u は event type 付き (`:1` press / `:2` repeat) と alternate key 付き
/// (`122:122`) の変種を持つので併記する。key **release** (`:3`) は押下ではないので
/// 含めない (= 素通しして子に届ける)。
///
/// repeat (`:2`) を押下扱いにするのは、素通しにすると「Ctrl+Z 長押し 1 回」で子が止まり
/// DR-0029 の目的 (= 反射的な Ctrl+Z で子を止めない) の真逆になるため。
///
/// 列挙に無い符号化は素通し = ガード無効に倒れる。誤検出で無関係なキーを握り潰すより
/// 安全側に振っている。
const CTRLZ_ENCODINGS: &[&[u8]] = &[
    &[CTRL_Z_BYTE],
    b"\x1b[122;5u",
    b"\x1b[122;5:1u",
    b"\x1b[122;5:2u",
    b"\x1b[122:122;5u",
    b"\x1b[122:122;5:1u",
    b"\x1b[122:122;5:2u",
    b"\x1b[27;5;122~",
];

/// 未確定 (= キー符号化の途中まで一致) の byte 列を保持する上限。
///
/// 端末は 1 キーイベントを 1 write で送るので通常この保持は起きない。`read(2)` 境界を
/// 跨いで分割着信した場合だけ効く。
///
/// Design rationale: 20ms にしたのは `ESC` 単打 (= Esc キー) の即時性を壊さないため。
/// `ESC` は CSI-u 符号化の先頭 byte でもあるので保持対象になるが、子アプリ側は `ESC` の
/// 曖昧性解決に 25ms 以上待つのが通例なので、20ms 以内に送り出せば子から見た Esc の
/// 解釈は変わらない。
const HOLD: std::time::Duration = std::time::Duration::from_millis(20);

/// [`KeyDecoder`] が出すトークン。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyToken {
    /// Ctrl+Z 押下。`bytes` は **受信した原 byte 列**。
    ///
    /// 子へ届ける判断になったときは、この byte 列をそのまま送る (= `0x1a` へ正規化
    /// しない。子は自分が要求した protocol の符号化を期待している)。
    CtrlZ { bytes: Vec<u8> },
    /// 解釈しない入力。そのまま子へ流す。
    Passthrough(Vec<u8>),
}

/// byte stream → [`KeyToken`] 列の変換器。
///
/// `feed` は純粋関数 + 持ち越し state (= 未確定 byte 列とその保持期限) で表現してあり、
/// 時刻を引数で受けるので chunk 境界と時定数を deterministic に test できる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KeyDecoder {
    /// キー符号化の途中まで一致していて、続きが来ないと判定できない byte 列。
    partial: Vec<u8>,
    /// `partial` を諦めて [`KeyToken::Passthrough`] に落とす時刻。
    hold_deadline: Option<std::time::Instant>,
}

/// `scan` の判定結果。
#[derive(Debug, PartialEq, Eq)]
enum Scan {
    /// 先頭 `len` bytes が Ctrl+Z 押下。
    CtrlZ { len: usize },
    /// 先頭 1 byte はどのキー符号化の一部にもなり得ない。
    Other,
    /// 先頭がキー符号化の途中まで一致。続きを見ないと判定できない。
    Incomplete,
}

/// buffer 先頭を符号化テーブルと照合する。
fn scan(buf: &[u8]) -> Scan {
    let mut incomplete = false;
    for seq in CTRLZ_ENCODINGS {
        if buf.len() >= seq.len() {
            if &buf[..seq.len()] == *seq {
                return Scan::CtrlZ { len: seq.len() };
            }
        } else if seq.starts_with(buf) {
            // buf 全体が seq の途中まで一致 = まだ Ctrl+Z になり得る。
            incomplete = true;
        }
    }
    if incomplete {
        Scan::Incomplete
    } else {
        Scan::Other
    }
}

impl KeyDecoder {
    /// chunk を食わせてトークン列を返す。
    ///
    /// 空 chunk で呼ぶと保持期限の評価だけを行う (= 打鍵が続かなくても、Ctrl+Z に
    /// ならなかった byte 列が期限到来で `Passthrough` として出てくる)。
    pub(crate) fn feed(&mut self, chunk: &[u8], now: std::time::Instant) -> Vec<KeyToken> {
        let mut out = Vec::new();
        let mut pass: Vec<u8> = Vec::new();

        // 保持期限切れ = キー符号化ではなかった。溜めた byte を素通しに落とす。
        // 新しい chunk とは連結しない (= 数秒前の `ESC` と今来た `[122;5u` を 1 つの
        // キーとして誤認しないため)。
        if !self.partial.is_empty() && self.hold_deadline.is_some_and(|d| now >= d) {
            pass.append(&mut self.partial);
        }
        let mut buf = std::mem::take(&mut self.partial);
        buf.extend_from_slice(chunk);
        let prev_deadline = self.hold_deadline.take();

        let mut i = 0;
        while i < buf.len() {
            match scan(&buf[i..]) {
                // 続きが来ないと判定できないので保持する。ここで流してしまうと、後続 byte で
                // Ctrl+Z と確定しても先頭の `ESC` が既に子へ漏れている。
                Scan::Incomplete => {
                    self.partial = buf[i..].to_vec();
                    // 期限を延ばすのは **続きの byte が実際に届いた時だけ** (= 分割着信が
                    // 進行中なら待つ価値がある)。入力が無い呼び出し (= poll 満了での
                    // `feed(&[], now)`) で延長すると、期限到来のたびに再武装して未確定
                    // byte を永久に握り続ける。
                    self.hold_deadline = Some(if chunk.is_empty() {
                        prev_deadline.unwrap_or(now + HOLD)
                    } else {
                        now + HOLD
                    });
                    break;
                }
                Scan::CtrlZ { len } => {
                    if !pass.is_empty() {
                        out.push(KeyToken::Passthrough(std::mem::take(&mut pass)));
                    }
                    out.push(KeyToken::CtrlZ {
                        bytes: buf[i..i + len].to_vec(),
                    });
                    i += len;
                }
                Scan::Other => {
                    pass.push(buf[i]);
                    i += 1;
                }
            }
        }
        if !pass.is_empty() {
            out.push(KeyToken::Passthrough(pass));
        }
        out
    }

    /// 未確定 byte を保持している場合、その保持期限。
    ///
    /// 呼び出し側の `poll(2)` timeout に反映して、打鍵が続かなくても期限到来で
    /// `feed(&[], now)` に来られるようにする。
    pub(crate) fn hold_deadline(&self) -> Option<std::time::Instant> {
        self.hold_deadline
    }

    /// 保持中の未確定 byte を取り出して decoder を空にする (= ガード無効化時の bypass 用)。
    pub(crate) fn take_partial(&mut self) -> Vec<u8> {
        self.hold_deadline = None;
        std::mem::take(&mut self.partial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn ctrlz(bytes: &[u8]) -> KeyToken {
        KeyToken::CtrlZ {
            bytes: bytes.to_vec(),
        }
    }
    fn pass(bytes: &[u8]) -> KeyToken {
        KeyToken::Passthrough(bytes.to_vec())
    }

    /// 実端末が Ctrl+Z に使う符号化はどれも 1 つの `CtrlZ` トークンになり、原 byte 列を
    /// 保持する (= 子へ流す時に正規化しないため)。
    #[test]
    fn every_encoding_decodes_to_one_ctrlz_token() {
        for seq in CTRLZ_ENCODINGS {
            let mut d = KeyDecoder::default();
            assert_eq!(
                d.feed(seq, Instant::now()),
                vec![ctrlz(seq)],
                "符号化 {seq:?} が 1 トークンにならない"
            );
            assert_eq!(d, KeyDecoder::default(), "state を残さない");
        }
    }

    /// key release (= kitty の event type `:3`) は押下ではないので素通しする。
    #[test]
    fn key_release_is_passthrough() {
        let mut d = KeyDecoder::default();
        let release = b"\x1b[122;5:3u";
        assert_eq!(d.feed(release, Instant::now()), vec![pass(release)]);
    }

    /// Ctrl+Z 以外のキーは解釈しない (= 矢印 / 他の CSI-u / 通常文字)。
    #[test]
    fn other_keys_are_passthrough() {
        for seq in [
            &b"\x1b[A"[..],
            &b"\x1b[97;5u"[..],
            &b"hello"[..],
            &b"\x03"[..],
        ] {
            let mut d = KeyDecoder::default();
            assert_eq!(d.feed(seq, Instant::now()), vec![pass(seq)], "{seq:?}");
        }
    }

    /// 素通し byte は連続する限り 1 トークンにまとまり、Ctrl+Z を境に分かれる。
    #[test]
    fn passthrough_runs_are_coalesced_around_keys() {
        let mut d = KeyDecoder::default();
        assert_eq!(
            d.feed(b"ab\x1acd\x1b[122;5uef", Instant::now()),
            vec![
                pass(b"ab"),
                ctrlz(&[CTRL_Z_BYTE]),
                pass(b"cd"),
                ctrlz(b"\x1b[122;5u"),
                pass(b"ef"),
            ]
        );
    }

    /// `read(2)` 境界がどこに落ちても 1 つの Ctrl+Z として再組立される。
    /// 未確定の間は 1 byte も外へ出さない (= 先頭 `ESC` を子へ漏らさない)。
    #[test]
    fn sequences_are_reassembled_across_every_split_point() {
        let t0 = Instant::now();
        for seq in CTRLZ_ENCODINGS {
            for split in 1..seq.len() {
                let mut d = KeyDecoder::default();
                assert_eq!(
                    d.feed(&seq[..split], t0),
                    vec![],
                    "{seq:?} split={split}: 未確定 byte を出してはいけない"
                );
                assert!(d.hold_deadline().is_some());
                assert_eq!(
                    d.feed(&seq[split..], t0 + Duration::from_millis(1)),
                    vec![ctrlz(seq)],
                    "{seq:?} split={split}: 再組立されない"
                );
                assert_eq!(d, KeyDecoder::default());
            }
        }
    }

    /// 続きが来ないまま保持期限を過ぎたら、溜めた byte を素通しとして出す
    /// (= 子への入力を握ったまま失わない)。
    #[test]
    fn partial_is_flushed_as_passthrough_when_hold_expires() {
        let t0 = Instant::now();
        let mut d = KeyDecoder::default();
        assert_eq!(d.feed(b"\x1b[122", t0), vec![]);
        assert_eq!(d.feed(&[], t0 + HOLD - Duration::from_millis(1)), vec![]);
        assert_eq!(d.feed(&[], t0 + HOLD), vec![pass(b"\x1b[122")]);
        assert_eq!(d, KeyDecoder::default());
    }

    /// 期限切れの未確定 byte は、後から来た chunk と連結しない (= 別のキーとして扱う)。
    ///
    /// 素通し byte がどう token に区切られるかは意味を持たないので、
    /// 「`CtrlZ` にならないこと」と「byte 列が順序どおり保たれること」だけを見る。
    #[test]
    fn expired_partial_is_not_joined_with_later_chunk() {
        let t0 = Instant::now();
        let mut d = KeyDecoder::default();
        assert_eq!(d.feed(b"\x1b", t0), vec![]);
        let tokens = d.feed(b"[122;5u", t0 + Duration::from_secs(5));
        assert!(
            tokens.iter().all(|t| matches!(t, KeyToken::Passthrough(_))),
            "5 秒前の ESC と今の残りを 1 つの Ctrl+Z にしてはいけない: {tokens:?}"
        );
        let bytes: Vec<u8> = tokens
            .iter()
            .flat_map(|t| match t {
                KeyToken::Passthrough(b) | KeyToken::CtrlZ { bytes: b } => b.clone(),
            })
            .collect();
        assert_eq!(bytes, b"\x1b[122;5u".to_vec(), "byte を落とさない");
    }

    /// 入力が無い呼び出し (= poll 満了) で保持期限を再武装しない。再武装すると
    /// 未確定 byte を永久に握り続ける。
    #[test]
    fn idle_feed_does_not_extend_hold() {
        let t0 = Instant::now();
        let mut d = KeyDecoder::default();
        d.feed(b"\x1b", t0);
        let deadline = d.hold_deadline().expect("保持中");
        d.feed(&[], t0 + Duration::from_millis(1));
        assert_eq!(d.hold_deadline(), Some(deadline), "期限が動いてはいけない");
        assert_eq!(d.feed(&[], deadline), vec![pass(b"\x1b")]);
    }

    /// 続きの byte が実際に届いた場合は期限を延ばす (= 分割着信が進行中なら待つ)。
    #[test]
    fn arriving_continuation_extends_hold() {
        let t0 = Instant::now();
        let mut d = KeyDecoder::default();
        d.feed(b"\x1b", t0);
        let first = d.hold_deadline().expect("保持中");
        d.feed(b"[", t0 + Duration::from_millis(5));
        assert!(d.hold_deadline().expect("保持中") > first);
    }

    /// 保持期限は未確定 byte がある間だけ立つ。
    #[test]
    fn hold_deadline_is_set_only_while_holding() {
        let t0 = Instant::now();
        let mut d = KeyDecoder::default();
        assert_eq!(d.hold_deadline(), None);
        d.feed(b"\x1b", t0);
        assert_eq!(d.hold_deadline(), Some(t0 + HOLD));
        d.feed(b"[A", t0);
        assert_eq!(d.hold_deadline(), None);
    }

    /// bypass 経路用に、保持中の byte を取り出して空にできる。
    #[test]
    fn take_partial_drains_held_bytes() {
        let mut d = KeyDecoder::default();
        d.feed(b"\x1b[", Instant::now());
        assert_eq!(d.take_partial(), b"\x1b[".to_vec());
        assert_eq!(d, KeyDecoder::default());
    }
}
