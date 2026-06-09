//! State-based wait core (DR-0006 §9 / DR-0013 §9)。
//!
//! `hyoui wait` 単独 subcommand と `hyoui input` family の `wait:` / `wait-idle:`
//! spec の **共通実装**。daemon の `StateSnapshotRequest` を polling して、
//! 現在 visible な cells を行 join した text に対して regex match する。
//!
//! ## ポイント
//!
//! - 旧 wait (= scrollback bytes regex) は **廃止**。本 module は state-based のみ。
//! - polling 周期は default 100ms。`--poll-interval` または `HYOUI_WAIT_POLL_MS`
//!   で override 可。
//! - timeout は default なし (= 永久 wait)。`--timeout` で指定。
//! - daemon の cells payload は CBOR encoded `ScreenSnapshot` (= daemon 内部型) で、
//!   client 側では本 module の [`SnapshotCells`] / [`SnapshotCellPos`] /
//!   [`SnapshotCell`] が鏡像を担う。
//! - wait-idle は `SequenceNo` 比較で実装する。snapshot を polling して
//!   `current_seqno` (= 子からの output で増える) が `<duration>` 期間 unchanged
//!   になったら成立。Phase A2 (= daemon-side `last_input_at` 配線) が完成したら
//!   そちらに切り替える。

use std::io::Cursor;
use std::time::{Duration, Instant};

use hyoui::client::ClientConnection;
use hyoui::protocol::ControlMessage;
use hyoui::protocol::messages::{
    ErrorMessage, SnapshotComponent, StateSnapshotRequest, StateSnapshotResponse,
};
use hyoui::sys::poll::{PollFlags, PollOutcome, poll};
use nix::poll::{PollFd, PollTimeout};
use regex::Regex;
use serde::Deserialize;

/// snapshot request 送信後、daemon の response 1 frame を待つ受信タイムアウト。
///
/// daemon は `screen.snapshot.request` を受けたら同期で即応答する設計のため、
/// 健全な daemon ではこの上限に届くことはない。daemon が half-open (= process は
/// 消えたが kernel が socket FIN を流していない) で固まったケースで永久 hang を
/// 防ぐための上限値。`--timeout` (= pattern が出ない / idle にならない) とは別物で、
/// 本値超過は「daemon 無応答」= I/O error として扱い、wait の意味論を変えない。
const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// state-based wait の outcome (= subcommand / input family 共通)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// match 成功 (= visible state に pattern が出現、または idle 条件成立)。
    Matched,
    /// timeout 到達。
    Timeout,
    /// daemon が `screen.snapshot.request` に対して error を返した。
    DaemonError(String),
    /// recv / send 系の I/O エラー。
    IoError(String),
    /// regex compile 失敗 (= caller が早期に compile しておくのが望ましいが、
    /// 万一弾けなかった場合のための fallback variant)。
    InvalidPattern(String),
}

/// `--poll-interval` / `HYOUI_WAIT_POLL_MS` 未指定時の default。
///
/// 100ms = 10Hz。TUI の描画頻度に対し十分追従できる。短くしすぎると daemon の
/// snapshot 構築コスト (= cells を sparse 化する Iterator) を不必要に消費する。
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 環境変数経由の polling 周期 override (= ms)。
pub const POLL_INTERVAL_ENV: &str = "HYOUI_WAIT_POLL_MS";

/// daemon の `ScreenSnapshot` (= daemon/screen/snapshot.rs の pub(crate) 型) の
/// 鏡像 (= 必要な field のみ抽出)。CBOR map 上の field 名は daemon 側と一致。
///
/// `serde(default)` を多用しているのは、daemon が「空 cell は省略 (sparse)」する
/// 仕様 (DR-0013 §11 圧縮) に追随するため。`current_seqno` / `cursor` は
/// `screen.snapshot.request.include` に依らず常に出るが、ここでは text match に
/// 直接必要な部分だけ取り出す。
#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotCells {
    /// viewport 行数。
    #[serde(default)]
    pub rows: u16,
    /// viewport 列数。
    #[serde(default)]
    pub cols: u16,
    /// sparse cells (= 空白 default cell は省略)。
    #[serde(default)]
    pub cells: Vec<SnapshotCellPos>,
}

/// sparse cell の wire 表現 (= `(row, col, CellSnapshot)` の triplet)。
#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotCellPos {
    /// 0-origin row。
    pub r: u16,
    /// 0-origin col。
    pub c: u16,
    /// cell 中身。
    pub cell: SnapshotCell,
}

/// 1 cell 分の内容 (= text / attrs / wide。match に必要なのは text のみ)。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SnapshotCell {
    /// cell 表示文字列。
    #[serde(default, rename = "t")]
    pub text: String,
    /// 属性 bit pack (= 本 module では未使用、forward-compat 用に保持)。
    #[allow(dead_code)]
    #[serde(default, rename = "a")]
    pub attrs: u8,
    /// 全角先頭 cell flag (= 本 module では未使用、forward-compat 用に保持)。
    #[allow(dead_code)]
    #[serde(default, rename = "w")]
    pub wide: bool,
}

impl SnapshotCells {
    /// sparse cells を **行 join した text** に変換する (DR-0006 §9.1)。
    ///
    /// 仕様:
    /// - 各 row を `cols` 個の半角空白で初期化
    /// - sparse cell の `text` を該当 row の `c` 位置に書き込む
    /// - row 単位で末尾空白を trim (= TUI が padding として書き込む空白に対する
    ///   誤マッチを防ぐ、DR-0006 §9.1 step 3)
    /// - 行間は `\n` で結合 (= regex の `^` / `$` が行頭/行末に効く、step 4)
    /// - ANSI escape は構築過程で発生しない (= cell 単位で text を集めるので)
    ///
    /// 全角文字 (= 2 col 占有) は先頭 cell に文字列が入り、継続 cell は sparse
    /// 表現上 skip される (= daemon 側 `build_screen_snapshot` の挙動)。結果の
    /// text 上は「全角文字 + 半角空白 1 個」のように 1 cell + padding で並ぶが、
    /// 末尾 trim で除去されるため実害は少ない。完全に layout を保ちたい用途は
    /// 別 task で対応する。
    pub fn to_text(&self) -> String {
        let rows = self.rows as usize;
        let cols = self.cols as usize;
        if rows == 0 || cols == 0 {
            return String::new();
        }
        // 行ごとに `cols` 個分の cell slot を String で持つ (= UTF-8 grapheme を
        // そのまま入れたいので `Vec<char>` ではなく `Vec<String>`)。空 cell は
        // 半角空白で埋める (= TUI が描いた padding と区別なく扱う)。
        let mut grid: Vec<Vec<String>> = (0..rows)
            .map(|_| (0..cols).map(|_| String::from(" ")).collect())
            .collect();
        for cp in &self.cells {
            let r = cp.r as usize;
            let c = cp.c as usize;
            if r >= rows || c >= cols {
                continue; // 範囲外は無視 (= defensive、daemon バグ対策)
            }
            // text 空は空白扱い (= daemon 側で skip されているはずだが defensive)。
            if cp.cell.text.is_empty() {
                continue;
            }
            grid[r][c] = cp.cell.text.clone();
        }
        let mut out = String::with_capacity(rows * (cols + 1));
        for (i, row) in grid.iter().enumerate() {
            let line: String = row.iter().fold(String::new(), |mut acc, s| {
                acc.push_str(s);
                acc
            });
            out.push_str(line.trim_end_matches(' '));
            if i + 1 < rows {
                out.push('\n');
            }
        }
        out
    }
}

/// `StateSnapshotResponse` 1 回分の取得結果 (= polling 呼び出しが返す中間値)。
#[derive(Debug, Clone)]
pub struct SnapshotProbe {
    /// visible cells (= include に `Cells` を要求した場合のみ取れる)。
    pub cells: Option<SnapshotCells>,
    /// `current_seqno` (= include に `SequenceNo` を要求した場合のみ取れる)。
    pub sequence_no: Option<u64>,
}

/// `conn` の reader fd を poll して、recv 可能 (= POLLIN) になるまで最大 `timeout`
/// 待つ。daemon 消失 (POLLHUP / POLLERR) と無応答 timeout を `WaitOutcome` に変換する。
///
/// - POLLIN: 即 `Ok(())`、caller が `recv_control` で frame を読める。
/// - POLLHUP / POLLERR: daemon が socket を閉じた / 異常 → `IoError` (= daemon 消失)。
/// - timeout 超過: daemon 無応答 → `IoError`。`--timeout` (= pattern が出ない) とは
///   別物として扱い、ユーザ指定 timeout の意味論を変えない。
/// - EINTR: signal で中断されただけなので re-poll する (= timeout は通算で計る)。
fn wait_recv_ready(conn: &ClientConnection, timeout: Duration) -> Result<(), WaitOutcome> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(WaitOutcome::IoError(
                "daemon が応答しません (= snapshot response 受信 timeout、daemon が消失/停止した可能性)"
                    .to_string(),
            ));
        }
        let fd = conn.reader_fd();
        let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
        let to = PollTimeout::try_from(remaining.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(PollTimeout::NONE);
        match poll(&mut fds, to) {
            Ok(PollOutcome::Ready(_)) => {
                let re = fds[0].revents().unwrap_or(PollFlags::empty());
                if re.contains(PollFlags::POLLIN) {
                    return Ok(());
                }
                if re.contains(PollFlags::POLLHUP) || re.contains(PollFlags::POLLERR) {
                    return Err(WaitOutcome::IoError(
                        "daemon が socket を閉じました (= daemon 消失/停止)".to_string(),
                    ));
                }
                // 想定外 revents は re-poll で確実な outcome を取り直す。
            }
            Ok(PollOutcome::Timeout) => {
                return Err(WaitOutcome::IoError(
                    "daemon が応答しません (= snapshot response 受信 timeout、daemon が消失/停止した可能性)"
                        .to_string(),
                ));
            }
            // EINTR: self-pipe 等の signal 割り込み、通算 deadline で再 poll。
            Ok(PollOutcome::Interrupted) => continue,
            Ok(_) => continue,
            Err(e) => {
                return Err(WaitOutcome::IoError(format!("poll 失敗: {e}")));
            }
        }
    }
}

/// daemon から snapshot を 1 回取得する low-level helper。
///
/// caller は include を必要最小限にすること (= cells を要らない場合は SequenceNo
/// のみ、idle 判定なら SequenceNo のみで OK)。
pub fn fetch_snapshot(
    conn: &mut ClientConnection,
    include: Vec<SnapshotComponent>,
) -> Result<SnapshotProbe, WaitOutcome> {
    let req = StateSnapshotRequest {
        include,
        serial: None,
    };
    conn.send_control(&ControlMessage::StateSnapshotRequest(req))
        .map_err(|e| WaitOutcome::IoError(format!("snapshot request send 失敗: {e}")))?;
    loop {
        // recv_control は blocking で socket-level read timeout を持たない。daemon が
        // half-open (= process 消失だが FIN 未着) で固まると永久 hang するため、
        // reader fd を poll で監視して RECV_TIMEOUT / POLLHUP を検知する。
        wait_recv_ready(conn, RECV_TIMEOUT)?;
        let msg = conn
            .recv_control(None)
            .map_err(|e| WaitOutcome::IoError(format!("snapshot response recv 失敗: {e}")))?;
        match msg {
            ControlMessage::StateSnapshotResponse(resp) => {
                return parse_probe(resp);
            }
            ControlMessage::Error(ErrorMessage { code, message, .. }) => {
                return Err(WaitOutcome::DaemonError(format!("[{code:?}] {message}")));
            }
            // 非同期通知系は skip して次の frame を待つ。
            ControlMessage::ModeChange(_) | ControlMessage::LeaderNotify(_) => continue,
            other => {
                return Err(WaitOutcome::DaemonError(format!(
                    "snapshot 応答待ち中に予期しない message を受信: {other:?}"
                )));
            }
        }
    }
}

fn parse_probe(resp: StateSnapshotResponse) -> Result<SnapshotProbe, WaitOutcome> {
    let cells = match resp.cells {
        Some(bytes) => {
            let cur = Cursor::new(bytes);
            let snap: SnapshotCells = ciborium::de::from_reader(cur).map_err(|e| {
                WaitOutcome::DaemonError(format!("cells payload の CBOR decode 失敗: {e}"))
            })?;
            Some(snap)
        }
        None => None,
    };
    Ok(SnapshotProbe {
        cells,
        sequence_no: resp.sequence_no,
    })
}

/// `wait:<pattern>` 系 (= subcommand + input family) の共通実装。
///
/// `pattern` を regex として visible cells text に対して match。成立まで polling。
///
/// - `timeout = None` で永久 wait
/// - `poll_interval` は最低 1ms (= 0ms はビジーループになるので強制 1ms)
pub fn wait_for_pattern(
    conn: &mut ClientConnection,
    pattern: &str,
    timeout: Option<Duration>,
    poll_interval: Duration,
) -> WaitOutcome {
    // multiline mode を default ON (DR-0006 §9.5)。`^` / `$` を行頭/行末で効かせる。
    let re = match Regex::new(&format!("(?m){pattern}")) {
        Ok(r) => r,
        Err(e) => return WaitOutcome::InvalidPattern(format!("regex compile 失敗: {e}")),
    };
    let start = Instant::now();
    let interval = poll_interval.max(Duration::from_millis(1));
    let include = vec![
        SnapshotComponent::Cells,
        SnapshotComponent::WindowSize,
        SnapshotComponent::SequenceNo,
    ];
    loop {
        match fetch_snapshot(conn, include.clone()) {
            Ok(probe) => {
                if let Some(cells) = probe.cells {
                    let text = cells.to_text();
                    if re.is_match(&text) {
                        return WaitOutcome::Matched;
                    }
                }
            }
            Err(out) => return out,
        }
        if let Some(t) = timeout
            && start.elapsed() >= t
        {
            return WaitOutcome::Timeout;
        }
        std::thread::sleep(interval);
    }
}

/// `wait-idle:<duration>` 系の共通実装 (Phase A1 = SequenceNo 観察)。
///
/// snapshot を polling し、`current_seqno` が `idle_for` 期間 unchanged なら成立。
/// `current_seqno` は子からの output で増えるので、それが止まる = 描画 idle と
/// 解釈できる。Phase A2 (= daemon-side `last_input_at` 配線) が完成したら、
/// より厳密な「子からの入力 bytes idle」に切り替える。
///
/// - `timeout` は **絶対 timeout** (= idle 検出までの最大 budget)。`None` で無限。
/// - `poll_interval` は最低 1ms。default は 100ms。
/// - `idle_for` < `poll_interval` のときは `poll_interval` 1 周分待ったら成立。
pub fn wait_for_idle(
    conn: &mut ClientConnection,
    idle_for: Duration,
    timeout: Option<Duration>,
    poll_interval: Duration,
) -> WaitOutcome {
    let start = Instant::now();
    let interval = poll_interval.max(Duration::from_millis(1));
    let include = vec![SnapshotComponent::SequenceNo];
    // 最後に観測した seqno と、その seqno に切り替わった時刻。
    let mut last_seqno: Option<u64> = None;
    let mut last_change: Instant = Instant::now();
    loop {
        let probe = match fetch_snapshot(conn, include.clone()) {
            Ok(p) => p,
            Err(out) => return out,
        };
        let seqno = probe.sequence_no.unwrap_or(0);
        match last_seqno {
            None => {
                last_seqno = Some(seqno);
                last_change = Instant::now();
            }
            Some(prev) if prev != seqno => {
                last_seqno = Some(seqno);
                last_change = Instant::now();
            }
            _ => {
                if last_change.elapsed() >= idle_for {
                    return WaitOutcome::Matched;
                }
            }
        }
        if let Some(t) = timeout
            && start.elapsed() >= t
        {
            return WaitOutcome::Timeout;
        }
        std::thread::sleep(interval);
    }
}

/// `HYOUI_WAIT_POLL_MS` 環境変数を読んで `Duration` を返す。値が不正なら None。
///
/// CLI 側 `--poll-interval` で override 済の場合は本関数は呼ばない (= CLI 引数
/// 優先)。
pub fn poll_interval_from_env() -> Option<Duration> {
    let raw = std::env::var(POLL_INTERVAL_ENV).ok()?;
    parse_poll_interval_ms(&raw)
}

/// 文字列 (= 単位なし ms 表記) を [`Duration`] に変換する。
///
/// 環境変数 [`POLL_INTERVAL_ENV`] のテスト容易化のため [`poll_interval_from_env`]
/// から切り出した pure な helper。
pub fn parse_poll_interval_ms(s: &str) -> Option<Duration> {
    let ms: u64 = s.trim().parse().ok()?;
    Some(Duration::from_millis(ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(r: u16, c: u16, t: &str) -> SnapshotCellPos {
        SnapshotCellPos {
            r,
            c,
            cell: SnapshotCell {
                text: t.into(),
                attrs: 0,
                wide: false,
            },
        }
    }

    #[test]
    fn empty_grid_yields_empty_text() {
        let s = SnapshotCells {
            rows: 0,
            cols: 0,
            cells: vec![],
        };
        assert_eq!(s.to_text(), "");
    }

    #[test]
    fn cells_to_text_places_chars_with_padding() {
        let s = SnapshotCells {
            rows: 2,
            cols: 5,
            cells: vec![cell(0, 0, "h"), cell(0, 1, "i"), cell(1, 2, "!")],
        };
        // row 0: "hi" + trailing trim、row 1: "  !" + trailing trim
        assert_eq!(s.to_text(), "hi\n  !");
    }

    #[test]
    fn cells_outside_bounds_are_ignored() {
        let s = SnapshotCells {
            rows: 1,
            cols: 3,
            cells: vec![cell(0, 0, "a"), cell(5, 5, "z"), cell(0, 99, "y")],
        };
        assert_eq!(s.to_text(), "a");
    }

    #[test]
    fn cells_to_text_supports_regex_match() {
        let s = SnapshotCells {
            rows: 1,
            cols: 20,
            cells: "Continue? [Y/n]"
                .chars()
                .enumerate()
                .map(|(i, ch)| cell(0, i as u16, &ch.to_string()))
                .collect(),
        };
        let text = s.to_text();
        let re = Regex::new(r"Continue\?").unwrap();
        assert!(re.is_match(&text), "text={text:?}");
    }

    #[test]
    fn multiline_anchor_matches_per_row() {
        let s = SnapshotCells {
            rows: 3,
            cols: 6,
            cells: vec![
                cell(0, 0, "f"),
                cell(1, 0, "B"),
                cell(1, 1, "A"),
                cell(1, 2, "R"),
                cell(2, 0, "x"),
            ],
        };
        let text = s.to_text();
        // 中央行が "BAR" で始まるかは multiline `^` で見られる。
        let re = Regex::new(r"(?m)^BAR$").unwrap();
        assert!(re.is_match(&text), "text={text:?}");
    }

    #[test]
    fn poll_interval_string_parses_ms() {
        // env を直接触ると hyoui-cli の `unsafe_code = "forbid"` lint で
        // コンパイル不能。文字列 → Duration の純粋 helper を直接 test する。
        assert_eq!(
            parse_poll_interval_ms("250"),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            parse_poll_interval_ms(" 1500 "),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_poll_interval_ms("abc"), None);
        assert_eq!(parse_poll_interval_ms(""), None);
    }

    #[test]
    fn invalid_pattern_returns_invalid_pattern_outcome() {
        // unbalanced bracket → regex compile error
        // wait_for_pattern を直接 invoke するには ClientConnection が要るが、
        // ここでは regex compile 部分の動作を直接確認する代わりに、コア
        // ロジック (= Regex::new) を経由しないと InvalidPattern にならない
        // という前提を別経路で test。
        // (clippy::invalid_regex を避けるため、構築は実行時 String 経由)
        let pattern = format!("(?m){}", "[unbalanced");
        let result = Regex::new(&pattern);
        assert!(result.is_err());
    }

    /// QA edge: rows>0 / cols>0 でも cells が完全に空なら、空 row が `\n` で
    /// 結合された text が返る (= 各 row は trim_end で空 string になる)。
    /// 空行が正しく描画されることを保護する (= DR-0006 §9.5 multiline `^$` の
    /// 想定挙動)。
    #[test]
    fn to_text_all_empty_yields_blank_lines() {
        let s = SnapshotCells {
            rows: 3,
            cols: 5,
            cells: vec![],
        };
        // 3 row、全空 → "" + "\n" + "" + "\n" + "" = "\n\n"
        assert_eq!(s.to_text(), "\n\n");
    }

    /// QA edge: 同じ (r, c) に複数 cell entry が来た場合、後勝ち (= 上書き) が
    /// 期待される。daemon が dedupe する保証はないので CLI 側で defensive に
    /// 後勝ち動作することを保護する。
    #[test]
    fn to_text_duplicate_position_last_wins() {
        let s = SnapshotCells {
            rows: 1,
            cols: 3,
            cells: vec![cell(0, 0, "a"), cell(0, 0, "Z")],
        };
        assert_eq!(s.to_text(), "Z");
    }

    /// QA edge: 多 byte 文字 (= 日本語、wide) が text に含まれた状態で regex
    /// match できる (= grapheme cluster の状態のまま `regex` crate が扱える)。
    /// state-based wait の Unicode 対応保護。
    #[test]
    fn to_text_supports_japanese_regex_match() {
        let s = SnapshotCells {
            rows: 1,
            cols: 6,
            cells: vec![cell(0, 0, "確"), cell(0, 1, "認")],
        };
        let text = s.to_text();
        // `(?u)` を付けなくても regex crate の default は Unicode-aware
        let re = Regex::new(r"確認").unwrap();
        assert!(re.is_match(&text), "text={text:?}");
    }

    /// QA edge: poll-interval 0 ms は parse 成功 (= helper の責務は単純な
    /// `Duration::from_millis`)。実利用時の clamp (= 最低 1ms) は
    /// `wait_for_pattern` 内 `poll_interval.max(Duration::from_millis(1))` で
    /// 担保されるため、本 helper が `0` を accept しても上位で守られる。
    #[test]
    fn parse_poll_interval_ms_accepts_zero() {
        assert_eq!(parse_poll_interval_ms("0"), Some(Duration::from_millis(0)));
    }

    /// QA edge: 負値 / 浮動小数 / 単位付き (= `1s`) は ms 単位の符号無し整数
    /// 想定外なので None を返す。CLI 側で env 変数経由の不正値が default に
    /// 落ちる安全動作を保護する。
    #[test]
    fn parse_poll_interval_ms_rejects_non_integer() {
        assert_eq!(parse_poll_interval_ms("-1"), None);
        assert_eq!(parse_poll_interval_ms("1.5"), None);
        assert_eq!(parse_poll_interval_ms("1s"), None);
        assert_eq!(parse_poll_interval_ms("100ms"), None);
    }
}
