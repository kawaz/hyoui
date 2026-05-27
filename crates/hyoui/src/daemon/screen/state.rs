//! `ScreenState` — vt100 Parser を内包し、daemon が保持する screen state の正本。
//!
//! DR-0013 §3 で確定した wrapper module の中核。`Parser::process` で子 PTY bytes を
//! state に流し込み、`Screen` 経由で cell / cursor / mode 等を expose する。
//!
//! Phase A の責務:
//!
//! 1. 子 PTY bytes を 1 度だけ feed する経路を提供する (= `process`)
//! 2. attach 復元用の sequence を組み立てるための primitive を expose (= `state_formatted`,
//!    `alternate_screen`, `cursor_position`, `cursor_visible`, `size`)
//! 3. DEC sync update (`?2026h` / `?2026l`) の同期中フラグを保持し、redraw の deferred
//!    判定に使う (= `sync_in_progress`)
//! 4. stalled sequence 検出用に `last_feed_at: Instant` を保持し、5 秒経過判定 (`is_stalled`)
//!    と内部 buffer reset (`reset_stalled`) を提供する
//!
//! Phase B 追加分:
//!
//! - **input bytes log の統合** (= [`super::input_log::InputLog`]、resize 救済策、§7):
//!   `process` 内で primary buffer 中のみ push、alt mode 切替で hook、`resize` で
//!   新 Parser を組み立てて replay
//! - **DEC sync chunk 跨ぎ対応**: `process` 間の境界で `\x1b[?2026` の partial を
//!   carry する (= chunk 末尾 8 bytes を `sync_scan_carry` に保持して次 chunk と連結
//!   して走査)
//! - **stalled reset 判定**: connector が `note_stalled_outcome` で 3 回連続 detect を
//!   観測したら `reset` を呼ぶ判断ができるよう、connector に counter API を提供する
//! - **`current_seqno`** (= byte feed 毎 increment、Phase B incremental sync の土台、§4)
//!
//! 持ち越し (Phase C 以降):
//!
//! - structured snapshot (= §11) は [`super::snapshot`] が担当 (本 module は cell
//!   iter / cursor / mode の getter のみ提供)
//! - per-line SequenceNo の真の活用は incremental sync 本体 (DirtyLinesNotify /
//!   GetLines) と一緒に実装、ここでは counter フィールドのみ用意

use std::time::{Duration, Instant};

use super::input_log::InputLog;

/// stalled sequence reset の閾値。tmux `input.c` 標準と揃える (= 5 秒)。
///
/// `last_feed_at` から本値経過しても新規 bytes が来なければ、parser 内部 buffer に
/// 取り残された partial escape sequence を捨てて整合を取る。broken byte stream で
/// parser が永久に partial 状態に閉じ込められるのを防ぐ (DR-0013 §5)。
pub(crate) const STALLED_RESET_TIMEOUT: Duration = Duration::from_secs(5);

/// stalled detect が **連続** で何回観測されたら自動 reset するかの閾値。
///
/// `STALLED_RESET_TIMEOUT * STALLED_RESET_CONSECUTIVE_DETECTS` 経過しても新 byte が
/// 来ない broken stream 状態でだけ reset するため、誤発火耐性が高い。
///
/// 3 回 (= 15 秒) を採用: Phase A は warn のみ、本値で初めて自動 reset まで進む。
pub(crate) const STALLED_RESET_CONSECUTIVE_DETECTS: u32 = 3;

/// DEC sync update の chunk 跨ぎ検出用 carry の最大長 (= 検索 needle `\x1b[?2026h` /
/// `\x1b[?2026l` は 8 byte なので、`needle.len() - 1 = 7` byte 残せば次 chunk で
/// 確実に matching を継続できる)。
const SYNC_SCAN_CARRY_LEN: usize = 7;

/// daemon が保持する screen state の正本 wrapper。
///
/// vt100 `Parser` (= `Screen` を内包) をそのまま正本にし、hyoui 側の責務 (= sync
/// flag / stalled timer / 補完 hook / input bytes log / SequenceNo) を追加 layer
/// として持つ。`process` は 1 度だけ呼び、子 PTY bytes は本 wrapper を経由してから
/// broadcast / wait / tail に流れる (= DR-0013 §1「raw byte の直接 broadcast はしない」)。
pub(crate) struct ScreenState {
    parser: vt100::Parser,
    /// 最後に `process` で bytes を feed した時刻。stalled 判定に使う。
    last_feed_at: Instant,
    /// DEC sync update (`\x1b[?2026h` ... `\x1b[?2026l`) の同期中フラグ。
    ///
    /// vt100 は本 mode を内部処理しないため (vt100 0.16 時点)、hyoui wrapper で
    /// process 時に検出する。同期中は `redraw_sequence` の戻り値を blocking する
    /// 設計とし、中途半端な state の redraw を send しない (= DR-0013 §6 + alacritty
    /// `event_loop.rs:166` pattern)。
    sync_in_progress: bool,
    /// 直前 chunk 末尾 (高々 `SYNC_SCAN_CARRY_LEN` byte) を持ち越して、`\x1b[?2026h/l`
    /// が chunk 境界に跨った場合の取りこぼしを防ぐ (DR-0013 task A-7 持ち越し解消)。
    sync_scan_carry: Vec<u8>,
    /// primary buffer 中の子 PTY bytes を bounded ring に貯める resize 救済策。
    /// `process` 内で primary 中のみ push、`resize` で新 Parser に replay する。
    input_log: InputLog,
    /// `process` 毎に増える monotonic counter (DR-0013 §4 Phase B incremental sync
    /// の土台、§10 PDU serial と同期しない別 layer の SequenceNo)。本 phase ではフィー
    /// ルドの導入のみで、DirtyLinesNotify / GetLines の本体は Phase C 以降。
    current_seqno: u64,
    /// 連続 stalled detect 回数 (= caller が `note_stalled_outcome` で更新)。
    /// `STALLED_RESET_CONSECUTIVE_DETECTS` 到達で自動 reset を実行する判断材料。
    consecutive_stalled_detects: u32,
}

impl ScreenState {
    /// rows × cols viewport、scrollback_len 行 ring、input log は **無効** (= capacity 0)
    /// の new state を作る。
    ///
    /// input_log capacity を指定したい場合は [`Self::with_input_log_capacity`] を使う。
    /// `vt100::Parser::new` をそのまま呼ぶ薄い factory。`last_feed_at` は now で
    /// 初期化し、初期状態では sync flag は off。
    ///
    /// 本 factory は test と health.rs 専用 (= production code は input_log capacity を
    /// 指定する `with_input_log_capacity` を使う)。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        Self::with_input_log_capacity(rows, cols, scrollback_len, 0)
    }

    /// rows × cols viewport、scrollback_len 行 ring、input_log capacity 指定で
    /// new state を作る。
    pub(crate) fn with_input_log_capacity(
        rows: u16,
        cols: u16,
        scrollback_len: usize,
        input_log_capacity: usize,
    ) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, scrollback_len),
            last_feed_at: Instant::now(),
            sync_in_progress: false,
            sync_scan_carry: Vec::with_capacity(SYNC_SCAN_CARRY_LEN),
            input_log: InputLog::new(input_log_capacity),
            current_seqno: 0,
            consecutive_stalled_detects: 0,
        }
    }

    /// 子 PTY 出力 bytes を vt100 parser に流し込む。
    ///
    /// 1. vt100 Parser に feed
    /// 2. `last_feed_at` を now に更新
    /// 3. DEC sync update (`?2026h` / `?2026l`) を carry + 新 bytes で走査して
    ///    `sync_in_progress` を更新 (= chunk 跨ぎでも取りこぼさない)
    /// 4. alt screen flag の値変化を input log に伝播
    /// 5. primary buffer 中なら input log に push (= resize 救済策、§7)
    /// 6. `current_seqno` を increment (= Phase B incremental sync の SequenceNo 土台)
    /// 7. 連続 stalled detect counter を 0 にリセット (= 新 byte が来た = 健全)
    pub(crate) fn process(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parser.process(bytes);
        self.last_feed_at = Instant::now();
        self.consecutive_stalled_detects = 0;
        // DEC sync update の状態更新。carry を新 bytes の前に連結してから走査し、
        // 次回 chunk のため末尾 SYNC_SCAN_CARRY_LEN bytes を保持する。
        update_sync_flag_with_carry(&mut self.sync_in_progress, &mut self.sync_scan_carry, bytes);
        // alt screen flag を input_log に同期。`Screen::alternate_screen()` の
        // 値変化は本 process 内で起こりうるので、毎回 set する (= 同値 set は no-op)。
        let alt = self.parser.screen().alternate_screen();
        self.input_log.set_alt_mode(alt);
        // primary buffer 中なら log に push。alt 中は InputLog 側で skip される。
        self.input_log.push(bytes);
        self.current_seqno = self.current_seqno.wrapping_add(1);
    }

    /// 現在 alt screen (`?1049h` / `?47h` / `?1047h`) に居るか。
    pub(crate) fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// 0-origin cursor position `(row, col)`。
    ///
    /// Phase A 時点では caller は無いが、Phase B の structured snapshot / debug
    /// inspection (DR-0013 §9) で expose 予定のため API として保持する。
    #[allow(dead_code)]
    pub(crate) fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// cursor が可視か (`?25h` で true、`?25l` で false)。
    ///
    /// Phase B の structured snapshot 用 (DR-0013 §9)。
    #[allow(dead_code)]
    pub(crate) fn cursor_visible(&self) -> bool {
        !self.parser.screen().hide_cursor()
    }

    /// viewport size `(rows, cols)`。
    ///
    /// Phase A では `reset` で内部利用、Phase B の snapshot でも expose 予定。
    pub(crate) fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// DEC sync update 同期中なら true。同期中は attach 復元 redraw を送らない。
    pub(crate) fn sync_in_progress(&self) -> bool {
        self.sync_in_progress
    }

    /// attach 復元用の primary sequence (= `Screen::state_formatted()`)。
    ///
    /// alt screen mode の prepend は本関数では行わず、caller (= `redraw.rs`) が
    /// 組み立てる。本関数は vt100 が出す raw bytes をそのまま返すだけ。
    pub(crate) fn state_formatted(&self) -> Vec<u8> {
        self.parser.screen().state_formatted()
    }

    /// `last_feed_at` から `STALLED_RESET_TIMEOUT` 経過していれば true。
    pub(crate) fn is_stalled(&self, now: Instant) -> bool {
        now.duration_since(self.last_feed_at) >= STALLED_RESET_TIMEOUT
    }

    /// 現在 SequenceNo (DR-0013 §4 Phase B incremental sync 土台)。
    ///
    /// `process` の各 chunk 終了時にチェックして「自分が持つ since_seqno との差」
    /// から dirty 量を概算する用途。`u64::wrapping_add` で 64bit 一周しても
    /// 健全動作 (= 実運用で reach 不可能だが defense-in-depth)。
    pub(crate) fn current_seqno(&self) -> u64 {
        self.current_seqno
    }

    /// input_log 内に保持されている byte 数 (= debug / metrics 用)。
    #[cfg(test)]
    pub(crate) fn input_log_len(&self) -> usize {
        self.input_log.len()
    }

    /// input_log に capacity を指定する factory が使われたかの確認用 (test only)。
    #[cfg(test)]
    pub(crate) fn input_log_capacity(&self) -> usize {
        self.input_log.capacity()
    }

    /// connector が stalled detect 結果を本 state に通知する。
    ///
    /// `Detected` を **連続で** `STALLED_RESET_CONSECUTIVE_DETECTS` 回受け取ったら
    /// `Some(StalledAction::ResetRequested)` を返す。`Healthy` を 1 度でも受けたら
    /// counter をリセットする。caller は `ResetRequested` を受けたら `reset()` を
    /// 呼ぶ判断ができる (DR-0013 §5 Phase B の自動 reset 判定)。
    pub(crate) fn note_stalled_outcome(&mut self, is_detected: bool) -> Option<StalledAction> {
        if is_detected {
            self.consecutive_stalled_detects = self.consecutive_stalled_detects.saturating_add(1);
            if self.consecutive_stalled_detects >= STALLED_RESET_CONSECUTIVE_DETECTS {
                Some(StalledAction::ResetRequested)
            } else {
                None
            }
        } else {
            self.consecutive_stalled_detects = 0;
            None
        }
    }

    /// 現在の連続 stalled detect 回数 (= test / metrics 用)。
    #[cfg(test)]
    pub(crate) fn consecutive_stalled_detects(&self) -> u32 {
        self.consecutive_stalled_detects
    }

    /// 内部 parser を新規構築して partial sequence buffer を捨てる。
    ///
    /// `scrollback_len` は **元の Parser と同じ値** を保持し続けたいが、vt100 0.16
    /// では `Parser` から `scrollback_len` を引き出す public API が無い。本実装では
    /// `new` / `with_input_log_capacity` で渡された値を覚えていないため、reset 時は
    /// 0 を渡す (= scrollback 機能は失われる、reset 後の挙動は debug 用途中心の
    /// 想定)。完全 reset しても困らない設計の積み重ねが前提。
    ///
    /// reset 後の `last_feed_at` は now、`sync_in_progress` / `sync_scan_carry` /
    /// `consecutive_stalled_detects` も 0 / 空に戻る。`current_seqno` も 0 から
    /// やり直す (= incremental sync は次の since_seqno=0 から再開)。input_log は
    /// 一緒に clear する (= replay 用 byte も無効と扱う)。
    pub(crate) fn reset(&mut self) {
        let (rows, cols) = self.size();
        self.parser = vt100::Parser::new(rows, cols, 0);
        self.last_feed_at = Instant::now();
        self.sync_in_progress = false;
        self.sync_scan_carry.clear();
        self.consecutive_stalled_detects = 0;
        self.current_seqno = 0;
        // input_log は capacity を維持しつつ中身だけ捨てる。
        // (alt mode flag も primary 開始に戻す = reset 直後は primary 前提)
        self.input_log.set_alt_mode(false);
        // VecDeque を drain で空にする (= public clear が test only なので直接操作)
        let cap = self.input_log.capacity();
        self.input_log = InputLog::new(cap);
    }

    /// viewport を `(rows, cols)` に変更し、input log を新 Parser に replay する。
    ///
    /// vt100 `Parser::set_size` は **truncate のみで真の reflow なし** のため、
    /// 本 wrapper では「新 Parser を作って input_log を再 feed」する設計に倒す
    /// (DR-0013 §7)。
    ///
    /// 設計判断:
    /// - alt screen 中 (= `alternate_screen() == true`) は input_log replay しない
    ///   (= alt 中の bytes は log に push されていないため意味なし)。新 Parser だけ
    ///   作って alt mode 復元 sequence (`\x1b[?1049h`) を流し込む。子側で
    ///   再描画させるのが PTY 接続の自然な挙動。
    /// - primary buffer 中なら input_log の bytes を新 Parser に process する。
    ///   capacity 内の最新 bytes 列が cell grid + cursor + mode に再構築される。
    /// - `sync_in_progress` / `sync_scan_carry` は新 Parser に合わせて clear。
    ///   replay 中に `?2026h` を踏めば再度 detect される。
    /// - `last_feed_at` は now (= resize 時刻)、stalled counter は 0。
    /// - `current_seqno` は **保持** (= incremental sync の連続性、resize で乱さない)。
    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        let was_alt = self.parser.screen().alternate_screen();
        let mut new_parser = vt100::Parser::new(rows, cols, 0);

        if was_alt {
            // alt mode フラグだけ復元 (= cells は子側 redraw に任せる)
            new_parser.process(b"\x1b[?1049h");
            self.input_log.set_alt_mode(true);
        } else if !self.input_log.is_empty() {
            let replay = self.input_log.drain_for_replay();
            new_parser.process(&replay);
            self.input_log.set_alt_mode(false);
        }

        self.parser = new_parser;
        self.last_feed_at = Instant::now();
        self.sync_in_progress = false;
        self.sync_scan_carry.clear();
        self.consecutive_stalled_detects = 0;
    }

    /// 過去 row へのアクセス用に scrollback offset 付き行を iter する (DR-0013 §8)。
    ///
    /// vt100 0.16 では `Screen::scrollback()` (= offset getter) と `Screen::set_scrollback`
    /// (= offset setter) しか提供されないため、本 wrapper では「指定 offset を一時的に
    /// set して contents 取得 → 元に戻す」流れで row iter を実現する。Phase C で
    /// debug/inspection 経由で row 単位の差分送出が必要になった際の primitive。
    ///
    /// 本実装は debug snapshot からのみ呼ばれる想定で、hot loop には乗らない
    /// (= O(rows × cols) の cell copy で十分)。
    pub(crate) fn snapshot_visible_rows(&self) -> Vec<Vec<RowCellSnap>> {
        let (rows, cols) = self.size();
        let mut out = Vec::with_capacity(rows as usize);
        let scr = self.parser.screen();
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols as usize);
            for c in 0..cols {
                let cell_info = scr
                    .cell(r, c)
                    .map(RowCellSnap::from_cell)
                    .unwrap_or_default();
                row.push(cell_info);
            }
            out.push(row);
        }
        out
    }

    /// cursor shape など mode 系の情報を取りまとめて返す。
    pub(crate) fn snapshot_cursor(&self) -> CursorSnap {
        let scr = self.parser.screen();
        let (row, col) = scr.cursor_position();
        CursorSnap {
            row,
            col,
            visible: !scr.hide_cursor(),
        }
    }

    /// mode 系の情報をまとめて返す (= alt / app_keypad / bracketed_paste 等)。
    pub(crate) fn snapshot_mode(&self) -> ModeSnap {
        let scr = self.parser.screen();
        ModeSnap {
            alternate_screen: scr.alternate_screen(),
            application_keypad: scr.application_keypad(),
            application_cursor: scr.application_cursor(),
            bracketed_paste: scr.bracketed_paste(),
            hide_cursor: scr.hide_cursor(),
        }
    }

    /// テスト用に内部 `Screen` を直接覗く。本 module 外には公開しない。
    #[cfg(test)]
    pub(crate) fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}

/// `note_stalled_outcome` の戻り値 (= caller が判断材料にする action 種別)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StalledAction {
    /// 連続 detect 数が閾値に達したので reset が推奨される。caller (= session)
    /// が `reset()` を呼ぶか、追加の log だけに留めるか判断する。
    ResetRequested,
}

/// 1 cell 分の snapshot data (= debug / snapshot protocol 用)。Phase B では
/// `snapshot.rs` の圧縮 wrapper が本構造を更に圧縮するため、ここでは生情報を保持。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RowCellSnap {
    /// cell の表示文字列 (combining char / 全角を保つため `String`)。
    pub contents: String,
    /// 全角 cell の先頭か (`true` なら is_wide)。
    pub is_wide: bool,
    /// 全角 cell の継続 cell か (`true` なら is_wide_continuation)。
    pub is_wide_continuation: bool,
    /// bold / italic / underline 等のフラグ pack (= bit 0 bold / 1 italic /
    /// 2 underline / 3 inverse)。Phase B では bool 個別保持ではなく u8 で
    /// 4 フラグだけまとめる (snapshot wrapper 側で詳細を圧縮するため)。
    pub attrs: u8,
}

impl RowCellSnap {
    fn from_cell(cell: &vt100::Cell) -> Self {
        let mut attrs = 0u8;
        if cell.bold() {
            attrs |= 1;
        }
        if cell.italic() {
            attrs |= 1 << 1;
        }
        if cell.underline() {
            attrs |= 1 << 2;
        }
        if cell.inverse() {
            attrs |= 1 << 3;
        }
        Self {
            contents: cell.contents().to_string(),
            is_wide: cell.is_wide(),
            is_wide_continuation: cell.is_wide_continuation(),
            attrs,
        }
    }
}

/// cursor 状態の snapshot。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorSnap {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// mode flag の snapshot (= alt / app_keypad / cursor / bracketed_paste / hide_cursor)。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModeSnap {
    pub alternate_screen: bool,
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub hide_cursor: bool,
}

/// `bytes` 中の DEC sync update (`\x1b[?2026h` / `\x1b[?2026l`) を検出し、
/// `flag` を更新する (chunk 跨ぎ対応 wrapper)。
///
/// 動作:
/// 1. carry (= 前 chunk 末尾の最大 `SYNC_SCAN_CARRY_LEN` byte) と新 bytes を連結
/// 2. 連結 buffer に対し最終出現位置で flag 更新
/// 3. carry を新 buffer の末尾 (高々 `SYNC_SCAN_CARRY_LEN` byte) に更新
///
/// これで `\x1b[?2026h` / `\x1b[?2026l` (= 8 byte) が chunk 境界に分断されても、
/// 直前 carry + 新 bytes の連結 buffer 内に完全 needle が必ず含まれる
/// (= 8 byte needle の 7 byte までを前 chunk に持っている状態でも次 chunk 1 byte で
/// 完成)。
fn update_sync_flag_with_carry(flag: &mut bool, carry: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    const ON: &[u8] = b"\x1b[?2026h";
    const OFF: &[u8] = b"\x1b[?2026l";

    // 連結 buffer を組み立てる。carry は短く高々 7 byte、bytes は chunk size
    // (典型 8 KiB) なので Vec 1 つで OK。
    let mut combined = Vec::with_capacity(carry.len() + bytes.len());
    combined.extend_from_slice(carry);
    combined.extend_from_slice(bytes);

    let last_on = find_last_subseq(&combined, ON);
    let last_off = find_last_subseq(&combined, OFF);
    match (last_on, last_off) {
        (None, None) => {}
        (Some(_), None) => *flag = true,
        (None, Some(_)) => *flag = false,
        (Some(a), Some(b)) => *flag = a > b,
    }

    // 次回 chunk のため、末尾 SYNC_SCAN_CARRY_LEN byte を carry に持ち越す。
    let keep = combined.len().min(SYNC_SCAN_CARRY_LEN);
    carry.clear();
    carry.extend_from_slice(&combined[combined.len() - keep..]);
}

/// `haystack` 中の `needle` の最後の出現位置を返す素朴 search。`needle` が空なら
/// None。bytes len は典型 8 KiB 以下、needle は 8 byte 固定で性能影響なし。
fn find_last_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut last = None;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            last = Some(i);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PoC §1: 基本構築 + process / size getter。
    #[test]
    fn new_and_size() {
        let s = ScreenState::new(24, 80, 100);
        assert_eq!(s.size(), (24, 80));
        assert!(!s.alternate_screen());
        assert_eq!(s.cursor_position(), (0, 0));
    }

    /// PoC §2: 簡易 process → screen content 反映。
    #[test]
    fn process_writes_cells() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process(b"hello");
        let scr = s.screen();
        let c = scr.cell(0, 0).unwrap();
        assert_eq!(c.contents(), "h");
        let c = scr.cell(0, 4).unwrap();
        assert_eq!(c.contents(), "o");
    }

    /// PoC §2: state_formatted の roundtrip (= 同じ Parser に流し戻して内容一致)。
    #[test]
    fn state_formatted_roundtrip_simple() {
        let mut s1 = ScreenState::new(5, 40, 100);
        s1.process(b"hello\x1b[1;31mRED\x1b[0m world\r\nline2");
        let formatted = s1.state_formatted();

        let mut s2 = ScreenState::new(5, 40, 100);
        s2.process(&formatted);

        // 主要 cell の中身が一致すること。
        for r in 0..5 {
            for c in 0..40 {
                let c1 = s1.screen().cell(r, c).unwrap();
                let c2 = s2.screen().cell(r, c).unwrap();
                assert_eq!(c1.contents(), c2.contents(), "char mismatch at ({r},{c})");
                assert_eq!(c1.fgcolor(), c2.fgcolor(), "fg mismatch at ({r},{c})");
                assert_eq!(c1.bold(), c2.bold(), "bold mismatch at ({r},{c})");
            }
        }
    }

    /// PoC §4: alt screen 切替の判定。
    #[test]
    fn alternate_screen_toggles() {
        let mut s = ScreenState::new(10, 40, 100);
        s.process(b"primary text");
        assert!(!s.alternate_screen());
        s.process(b"\x1b[?1049h");
        assert!(s.alternate_screen());
        s.process(b"alt text");
        assert!(s.alternate_screen());
        s.process(b"\x1b[?1049l");
        assert!(!s.alternate_screen());
    }

    /// PoC §7: cursor visibility hook。
    #[test]
    fn cursor_visible_toggles() {
        let mut s = ScreenState::new(5, 40, 100);
        assert!(s.cursor_visible(), "default visible");
        s.process(b"\x1b[?25l");
        assert!(!s.cursor_visible());
        s.process(b"\x1b[?25h");
        assert!(s.cursor_visible());
    }

    /// PoC §7: cursor 位置の取得。
    #[test]
    fn cursor_position_after_move() {
        let mut s = ScreenState::new(10, 40, 100);
        s.process(b"\x1b[5;10H");
        // 1-origin ANSI → 0-origin vt100
        assert_eq!(s.cursor_position(), (4, 9));
    }

    /// PoC §8: wide char (= 日本語) が 2 cell に保持される。
    #[test]
    fn wide_char_occupies_two_cells() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process("あ".as_bytes());
        let scr = s.screen();
        let c0 = scr.cell(0, 0).unwrap();
        let c1 = scr.cell(0, 1).unwrap();
        assert_eq!(c0.contents(), "あ");
        assert!(c0.is_wide());
        assert!(c1.is_wide_continuation());
        // 次の cursor 位置は col=2
        assert_eq!(s.cursor_position(), (0, 2));
    }

    /// DEC sync update mode の hook: `?2026h` で sync_in_progress = true、
    /// `?2026l` で false。
    #[test]
    fn sync_update_flag_tracks_dec_sync() {
        let mut s = ScreenState::new(5, 40, 100);
        assert!(!s.sync_in_progress());
        s.process(b"\x1b[?2026h");
        assert!(s.sync_in_progress(), "sync should be ON");
        s.process(b"some draw");
        assert!(s.sync_in_progress(), "sync stays ON during draws");
        s.process(b"\x1b[?2026l");
        assert!(!s.sync_in_progress(), "sync should be OFF after l");
    }

    /// 1 chunk 内に sync on/off が両方ある場合は「最終出現」を採用する。
    #[test]
    fn sync_update_flag_uses_last_occurrence_in_chunk() {
        let mut s = ScreenState::new(5, 40, 100);
        // off → on の順で同 chunk
        s.process(b"\x1b[?2026l\x1b[?2026h");
        assert!(s.sync_in_progress(), "last is ON");
        // on → off の順で同 chunk
        s.process(b"\x1b[?2026h\x1b[?2026l");
        assert!(!s.sync_in_progress(), "last is OFF");
    }

    /// stalled 判定: 5 秒経過判定の境界を Instant 操作なしで確認するため、
    /// `STALLED_RESET_TIMEOUT` を直接超過させた `Instant` を渡す。
    #[test]
    fn is_stalled_after_timeout() {
        let s = ScreenState::new(5, 40, 100);
        // 直後は stalled でない
        assert!(!s.is_stalled(Instant::now()));
        // 6 秒先の Instant を渡せば stalled
        let future = Instant::now() + STALLED_RESET_TIMEOUT + Duration::from_secs(1);
        assert!(s.is_stalled(future));
    }

    /// reset で parser が新規構築され、過去の cell 内容が消える。
    #[test]
    fn reset_clears_screen() {
        let mut s = ScreenState::new(5, 40, 100);
        s.process(b"hello");
        assert_eq!(s.screen().cell(0, 0).unwrap().contents(), "h");
        s.reset();
        // 新 Parser は cell が空
        let c = s.screen().cell(0, 0).unwrap();
        assert_eq!(c.contents(), "");
        assert!(!s.sync_in_progress());
        assert_eq!(s.consecutive_stalled_detects(), 0);
        assert_eq!(s.current_seqno(), 0);
    }

    /// DEC sync update が **chunk 境界に跨る** 場合でも carry 経由で検出できる。
    /// 旧 Phase A 実装は素朴 substring search で chunk 跨ぎを取りこぼしていた
    /// (DR-0013 task A-7 持ち越し)。
    #[test]
    fn sync_update_flag_handles_chunk_boundary() {
        let mut s = ScreenState::new(5, 40, 100);
        // 8 byte needle `\x1b[?2026h` を 5 byte + 3 byte に分割
        s.process(b"\x1b[?20"); // partial 5 byte
        assert!(!s.sync_in_progress(), "partial alone must not flip flag");
        s.process(b"26h"); // 残り 3 byte → carry と連結して完全 match
        assert!(s.sync_in_progress(), "carry should reconstruct ON");

        // OFF も同様に分割
        s.process(b"some draws");
        assert!(s.sync_in_progress());
        s.process(b"\x1b[?2026"); // 7 byte
        assert!(s.sync_in_progress());
        s.process(b"l"); // 残り 1 byte
        assert!(!s.sync_in_progress(), "OFF reconstructed across chunks");
    }

    /// 連続 stalled detect が閾値に達したら `ResetRequested` を返す。
    /// Healthy を 1 度受けると counter は reset される。
    #[test]
    fn note_stalled_outcome_counts_consecutive_detects() {
        let mut s = ScreenState::new(5, 40, 100);
        assert_eq!(s.note_stalled_outcome(true), None);
        assert_eq!(s.note_stalled_outcome(true), None);
        // 3 回目で閾値到達
        assert_eq!(
            s.note_stalled_outcome(true),
            Some(StalledAction::ResetRequested)
        );
        // Healthy で counter リセット
        assert_eq!(s.note_stalled_outcome(false), None);
        assert_eq!(s.consecutive_stalled_detects(), 0);
        // 再度 detect 1 回では requested にならない
        assert_eq!(s.note_stalled_outcome(true), None);
    }

    /// `process` の度に `current_seqno` が increment される。
    #[test]
    fn current_seqno_increments_per_process() {
        let mut s = ScreenState::new(5, 40, 100);
        assert_eq!(s.current_seqno(), 0);
        s.process(b"a");
        assert_eq!(s.current_seqno(), 1);
        s.process(b"b");
        assert_eq!(s.current_seqno(), 2);
        // 空 bytes は SequenceNo を進めない
        s.process(b"");
        assert_eq!(s.current_seqno(), 2);
    }

    /// input log capacity を指定した state で primary buffer 中の bytes が log に
    /// 保持される。alt screen 中は log に push されない。
    #[test]
    fn input_log_records_primary_bytes_only() {
        let mut s = ScreenState::with_input_log_capacity(5, 40, 100, 1024);
        assert_eq!(s.input_log_capacity(), 1024);
        s.process(b"hello");
        assert_eq!(s.input_log_len(), 5);

        // alt screen に入る → 以降の bytes は log に push されない
        s.process(b"\x1b[?1049h"); // 8 byte の `?1049h` も含め、alt 切替前の bytes は log に入る
        let len_after_alt_enter = s.input_log_len();
        s.process(b"alt-text");
        assert_eq!(
            s.input_log_len(),
            len_after_alt_enter,
            "alt screen bytes must not extend log"
        );

        // primary に戻ると再度 push される
        s.process(b"\x1b[?1049l");
        s.process(b"+more");
        assert!(s.input_log_len() > len_after_alt_enter);
    }

    /// resize は primary buffer 中なら input log を新 Parser に replay する。
    #[test]
    fn resize_replays_primary_input_log() {
        let mut s = ScreenState::with_input_log_capacity(5, 80, 0, 1024);
        s.process(b"hello world");
        let pre_size = s.size();
        assert_eq!(pre_size, (5, 80));

        // 縮小 → 拡大しても "hello world" が cell に再構築される
        s.resize(5, 40);
        assert_eq!(s.size(), (5, 40));
        // 新 Parser に input log が流し込まれるので cell に文字が乗る
        let scr = s.screen();
        // "hello world" は最大 11 cell、cell(0, 0) は 'h'
        assert_eq!(scr.cell(0, 0).unwrap().contents(), "h");
        assert_eq!(scr.cell(0, 10).unwrap().contents(), "d");
    }

    /// alt screen 中の resize は input log を流し込まず、alt mode 復元 sequence だけ
    /// 新 Parser に feed する (= 子側 redraw に任せる方針)。
    #[test]
    fn resize_in_alt_screen_does_not_replay_log() {
        let mut s = ScreenState::with_input_log_capacity(10, 80, 0, 1024);
        s.process(b"primary text");
        s.process(b"\x1b[?1049h"); // alt 進入
        s.process(b"alt-content");

        s.resize(10, 40);
        // alt mode は保持
        assert!(s.alternate_screen());
        // 新 Parser には primary の文字も alt の文字も入っていない
        // (= 子側 redraw 期待で空のまま)
        let scr = s.screen();
        // alt 直後の cell は空 (= alt 進入で clear されたあとなので primary 文字は無い、
        // かつ alt 文字は新 Parser に流していない)
        assert_eq!(scr.cell(0, 0).unwrap().contents(), "");
    }

    /// QA edge: input log capacity が 0 でも resize が panic しない (= replay
    /// 元 buffer が空のまま新 Parser を作るだけ)。capacity 0 = 「log を取らない」
    /// 設定の安全性を保護する。
    #[test]
    fn resize_with_zero_capacity_input_log_does_not_panic() {
        let mut s = ScreenState::with_input_log_capacity(5, 80, 0, 0);
        assert_eq!(s.input_log_capacity(), 0);
        s.process(b"hello world");
        assert_eq!(s.input_log_len(), 0, "0-capacity log never accumulates");

        // resize は input log replay が no-op になるだけで panic しない
        s.resize(5, 40);
        assert_eq!(s.size(), (5, 40));
        // log が空なので新 Parser の cell も空
        let scr = s.screen();
        assert_eq!(scr.cell(0, 0).unwrap().contents(), "");
    }

    /// QA edge: 巨大 chunk (= 64KB の連続入力) を 1 度に process しても
    /// `current_seqno` は 1 しか進まない (= bytes 数ではなく call 回数で
    /// increment される仕様の保護)。state-based wait-idle の挙動前提。
    #[test]
    fn process_large_chunk_advances_seqno_by_one() {
        let mut s = ScreenState::new(24, 80, 100);
        let big = vec![b'.'; 64 * 1024];
        let before = s.current_seqno();
        s.process(&big);
        assert_eq!(s.current_seqno(), before + 1);
    }

    /// QA edge: stalled detect の counter が `note_stalled_outcome(false)` で
    /// 確実に 0 に reset され、その後再度 3 連続検知でも閾値判定が成立すること
    /// (= state を持ち越して誤動作しない保護)。既存 test は 1 サイクルのみで
    /// 「再利用」を確認していなかったため補強。
    #[test]
    fn stalled_counter_resets_and_retriggers_after_healthy() {
        let mut s = ScreenState::new(5, 40, 100);
        // 1 サイクル目: 3 連続で閾値達成
        assert_eq!(s.note_stalled_outcome(true), None);
        assert_eq!(s.note_stalled_outcome(true), None);
        assert_eq!(
            s.note_stalled_outcome(true),
            Some(StalledAction::ResetRequested)
        );
        // healthy で reset
        assert_eq!(s.note_stalled_outcome(false), None);
        assert_eq!(s.consecutive_stalled_detects(), 0);
        // 2 サイクル目: 再度 3 連続で閾値判定が成立する
        assert_eq!(s.note_stalled_outcome(true), None);
        assert_eq!(s.note_stalled_outcome(true), None);
        assert_eq!(
            s.note_stalled_outcome(true),
            Some(StalledAction::ResetRequested)
        );
    }
}
