//! `wait.request` predicate (text / pattern / idle) を担う module
//! (DR-0009 Phase E 前半で `session.rs` から分離)。
//!
//! ## 構成
//!
//! - [`PendingWait`]: 1 wait の pending 状態 (predicate + options + deadline +
//!   accumulated + last_activity + compiled regex + strip carry)
//! - [`handle_wait_request`]: `wait.request` 受信時の per-client cap / 空 needle reject /
//!   regex compile / PendingWait push (`Idle { ms: 0 }` の即成立分岐含む)
//! - [`update_waits_on_master_bytes`]: 新規 master bytes を strip_carry / newline 変換を
//!   適用して accumulated に追加し、各 predicate を scan、match した wait を remove
//! - [`compute_wait_poll_timeout`]: pending_waits の最も早い deadline / idle 期限から
//!   poll timeout を計算 (R4-C8 で `Instant + Duration` の overflow を `checked_add` で防御)
//! - [`check_wait_timeouts`]: poll timeout 経過後の idle 経過 / deadline 経過チェック
//!
//! ## session.rs / 他 module との接続
//!
//! - `handle_wait_request` は control.rs の dispatcher (`handle_wait_request_dispatch`)
//!   から呼ばれる (= cap check 後)
//! - `update_waits_on_master_bytes` / `compute_wait_poll_timeout` / `check_wait_timeouts`
//!   は session.rs の `serve_loop` から呼ばれる
//! - `PendingWait::client_id` は serve_loop の drop cascade (`pending_waits.retain`)
//!   から参照される。それ以外のフィールドは wait.rs 内に閉じる

use std::time::Instant;

use nix::poll::PollTimeout;

use crate::protocol::ControlMessage;
use crate::protocol::messages::{
    ErrorCode, ErrorMessage, WaitMatchOptions, WaitOutcome, WaitPredicate, WaitRequest, WaitResult,
};

use super::broadcast::{ClientHandle, send_control};

/// `wait.request` の pending 状態 (Phase 11c)。
///
/// 各 wait は self-contained:
/// - `predicate`: text / pattern (compiled regex) / idle
/// - `options`: strip_escapes / newline_convert_lf
/// - `deadline`: timeout_ms から計算した絶対時刻 (= 無限 wait は None)
/// - `accumulated`: wait 開始後に蓄積した master bytes (predicate scan 対象)
/// - `last_activity`: Idle 用に最後に master 出力があった時刻 (= 開始時 = now)
/// - `compiled_regex`: Pattern predicate のみ、wait 開始時 1 回 compile
/// - `strip_carry`: `strip_escapes=true` 時に chunk 境界を跨ぐ partial ANSI
///   escape を持ち越すための stateful stripper (R4-H3)。
pub(super) struct PendingWait {
    /// `serve_loop` の drop cascade (= client disconnect 時に
    /// `pending_waits.retain(|w| w.client_id != ch.id)`) でアクセスするため
    /// `pub(super)` で公開する。他フィールドは wait.rs 内に閉じる。
    pub(super) client_id: u64,
    predicate: WaitPredicate,
    options: WaitMatchOptions,
    deadline: Option<Instant>,
    accumulated: Vec<u8>,
    last_activity: Instant,
    compiled_regex: Option<regex::bytes::Regex>,
    strip_carry: crate::strip::StripAnsiCarry,
}

/// `accumulated` の上限 (= memory bound)。超過すると古い byte から truncate。
const WAIT_ACCUMULATED_LIMIT: usize = 1024 * 1024;

/// 1 client が同時に持てる pending wait の上限。超過すると新規 `wait.request` は
/// error code=`wait.too-many` で reject (= N × WAIT_ACCUMULATED_LIMIT の OOM 防止)。
const MAX_WAITS_PER_CLIENT: usize = 16;

/// `wait.request` を処理する (Phase 11c)。
///
/// PendingWait を作って `pending_waits` に push。各 predicate ごとに:
/// - Text: substring match (= accumulated に対して `.windows().any()` 相当)
/// - Pattern: regex compile を 1 度実行、accumulated に対して `is_match`
///   (compile 失敗で error code=`wait.invalid-pattern` を返却)
/// - Idle: master 出力が `ms` 静かなら成立 (= 開始時 last_activity = now、
///   master 出力で last_activity 更新、`compute_wait_poll_timeout` で
///   `last_activity + ms - now` を poll timeout として使う)
pub(super) fn handle_wait_request(
    idx: usize,
    req: WaitRequest,
    clients: &mut [ClientHandle],
    pending_waits: &mut Vec<PendingWait>,
) {
    let client_id = clients[idx].id;
    // per-client pending wait count cap (= memory DoS 対策)
    let existing = pending_waits
        .iter()
        .filter(|w| w.client_id == client_id)
        .count();
    if existing >= MAX_WAITS_PER_CLIENT {
        let _ = send_control(
            &clients[idx],
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::WaitTooMany,
                message: format!(
                    "too many pending waits for this client (limit {MAX_WAITS_PER_CLIENT})"
                ),
                details: None,
            }),
        );
        return;
    }
    // Text/Pattern の空 needle reject (= Round2 #1)。
    // 空 value だと scan ループの `accumulated.windows(0)` が std spec で panic、
    // daemon thread を落とすため事前に明示 error 返却で防ぐ。
    match &req.predicate {
        WaitPredicate::Text { value } if value.is_empty() => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::WaitInvalidText,
                    message: "text predicate value must not be empty".into(),
                    details: None,
                }),
            );
            return;
        }
        WaitPredicate::Pattern { regex } if regex.is_empty() => {
            let _ = send_control(
                &clients[idx],
                ControlMessage::Error(ErrorMessage {
                    code: ErrorCode::WaitInvalidPattern,
                    message: "pattern regex must not be empty".into(),
                    details: None,
                }),
            );
            return;
        }
        _ => {}
    }

    let now = Instant::now();
    let deadline = req
        .timeout_ms
        .and_then(|ms| now.checked_add(std::time::Duration::from_millis(ms)));

    let compiled_regex = match &req.predicate {
        WaitPredicate::Pattern { regex: r } => {
            // regex compile DoS 対策: 巨大 alternation / 深い nest で daemon の
            // event loop を block しないように size_limit / dfa_size_limit を絞る。
            // 既定 (10 MB / 2 MB) → 64 KB / 64 KB に削減 (= 通常用途で十分)。
            // pattern 文字列長も上限を設けて短時間で reject する。
            const PATTERN_MAX_LEN: usize = 1024;
            const REGEX_SIZE_LIMIT: usize = 64 * 1024;
            if r.len() > PATTERN_MAX_LEN {
                let _ = send_control(
                    &clients[idx],
                    ControlMessage::Error(ErrorMessage {
                        code: ErrorCode::WaitInvalidPattern,
                        message: format!(
                            "regex too long: {} bytes (limit {PATTERN_MAX_LEN})",
                            r.len()
                        ),
                        details: None,
                    }),
                );
                return;
            }
            match regex::bytes::RegexBuilder::new(r)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
            {
                Ok(re) => Some(re),
                Err(_) => {
                    let _ = send_control(
                        &clients[idx],
                        ControlMessage::Error(ErrorMessage {
                            code: ErrorCode::WaitInvalidPattern,
                            message: "regex failed to compile (syntax or size limit)".into(),
                            details: None,
                        }),
                    );
                    return;
                }
            }
        }
        _ => None,
    };

    let wait = PendingWait {
        client_id,
        predicate: req.predicate,
        options: req.options,
        deadline,
        accumulated: Vec::new(),
        last_activity: now,
        compiled_regex,
        strip_carry: crate::strip::StripAnsiCarry::new(),
    };

    // 開始即 (= accumulated 空) に match することは Text/Pattern では起きないが、
    // Idle (ms = 0) だけは即成立しうる。即成立なら send + skip push。
    if matches!(wait.predicate, WaitPredicate::Idle { ms: 0 }) {
        let _ = send_control(
            &clients[idx],
            ControlMessage::WaitResult(WaitResult {
                outcome: WaitOutcome::Matched,
                matched_offset: None,
            }),
        );
        return;
    }
    pending_waits.push(wait);
}

/// 各 pending wait の `accumulated` に新規 master bytes を append し、predicate を
/// scan。マッチした wait は client へ `wait.result(Matched)` を送って remove する。
///
/// `WaitMatchOptions::strip_escapes` / `newline_convert_lf` は scan 前に新 bytes に
/// 適用する。`strip_escapes` は per-wait の `StripAnsiCarry` で chunk 境界を跨ぐ
/// partial ANSI escape を持ち越すため、escape を挟んで分割された needle も正しく
/// match できる (R4-H3)。
///
/// ## invariant: needle 検出範囲 (R5-H19 / R5-FRM-C3)
///
/// `accumulated` は memory bound (= [`WAIT_ACCUMULATED_LIMIT`]) を超えた場合、
/// `drain(..(len - limit))` で **先頭から** 余剰 byte を捨てる。新 bytes は
/// `extend_from_slice` で末尾に追加されるので、trim 後の `accumulated` は
/// **常に「最新 `WAIT_ACCUMULATED_LIMIT` バイト」** を保持する。
///
/// この invariant により、needle (= Text の value / Pattern の regex match) が
/// 「最新 `WAIT_ACCUMULATED_LIMIT` バイト以内に出現」していれば必ず検出される。
/// 逆に言うと、daemon は `WAIT_ACCUMULATED_LIMIT` を超えて古い byte に出現した
/// needle は検出できない (= sliding window scan)。これは memory bound と
/// detection completeness のトレードオフによる設計判断 (DR-0008 §wait scan)。
///
/// **将来 trim ロジックを変更する人へ**: trim 後に「最新 `WAIT_ACCUMULATED_LIMIT`
/// バイトが保持されている」という invariant を壊さないこと。例えば「時間経過で
/// 古い chunk を間引く」「特定区切り文字で分割」等を入れる場合、needle が
/// 「最新の limit バイト中に必ず含まれる」という保証が崩れる。本関数末尾の
/// `debug_assert!` で trim 後の長さ上限を機械検証している。
pub(super) fn update_waits_on_master_bytes(
    pending_waits: &mut Vec<PendingWait>,
    clients: &mut [ClientHandle],
    new_bytes: &[u8],
    now: Instant,
) {
    let mut matched_indices: Vec<usize> = Vec::new();
    for (i, w) in pending_waits.iter_mut().enumerate() {
        // Idle 用に last_activity 更新 (= 静寂タイマーリセット)
        w.last_activity = now;

        let mut bytes_to_add: Vec<u8> = if w.options.strip_escapes {
            // stateful: 前 chunk の末尾で未完了の escape を carry。
            w.strip_carry.push(new_bytes)
        } else {
            new_bytes.to_vec()
        };
        if w.options.newline_convert_lf {
            bytes_to_add = crate::strip::normalize_lf(&bytes_to_add);
        }
        w.accumulated.extend_from_slice(&bytes_to_add);
        // memory bound: head から trim、末尾 WAIT_ACCUMULATED_LIMIT バイトを保持
        if w.accumulated.len() > WAIT_ACCUMULATED_LIMIT {
            let drop_n = w.accumulated.len() - WAIT_ACCUMULATED_LIMIT;
            w.accumulated.drain(..drop_n);
        }
        // R5-H19: trim 後の invariant を機械検証 (= needle 検出範囲 = 末尾 limit
        // バイト)。将来 trim ロジックが変わって invariant が壊れたら test で検出。
        debug_assert!(
            w.accumulated.len() <= WAIT_ACCUMULATED_LIMIT,
            "trim must keep accumulated within WAIT_ACCUMULATED_LIMIT (= {WAIT_ACCUMULATED_LIMIT} bytes), got {}",
            w.accumulated.len()
        );

        let matched = match &w.predicate {
            WaitPredicate::Text { value } => w
                .accumulated
                .windows(value.len())
                .any(|win| win == value.as_bytes()),
            WaitPredicate::Pattern { .. } => w
                .compiled_regex
                .as_ref()
                .map(|re| re.is_match(&w.accumulated))
                .unwrap_or(false),
            WaitPredicate::Idle { .. } => false, // Idle は静寂判定なので bytes 増加で match しない
        };
        if matched {
            matched_indices.push(i);
        }
    }
    // matched を逆順で remove + WaitResult 送信
    for i in matched_indices.into_iter().rev() {
        let w = pending_waits.remove(i);
        if let Some(ch) = clients.iter().find(|c| c.id == w.client_id) {
            let _ = send_control(
                ch,
                ControlMessage::WaitResult(WaitResult {
                    outcome: WaitOutcome::Matched,
                    matched_offset: None,
                }),
            );
        }
    }
}

/// poll timeout を pending_waits の最も早い deadline (= timeout / idle 期限) から
/// 計算する。pending が無ければ `PollTimeout::NONE` (= 無限 block)。
pub(super) fn compute_wait_poll_timeout(pending_waits: &[PendingWait]) -> PollTimeout {
    let now = Instant::now();
    let mut earliest: Option<std::time::Duration> = None;
    for w in pending_waits {
        let candidates: [Option<std::time::Duration>; 2] = [
            w.deadline
                .map(|d| d.saturating_duration_since(now))
                .map(|d| d.max(std::time::Duration::ZERO)),
            match w.predicate {
                WaitPredicate::Idle { ms } => {
                    // u64::MAX 等の極端な ms で `Instant + Duration` が overflow すると
                    // panic するため checked_add で防ぐ。overflow した場合は事実上
                    // 「無限に先」なので候補に含めない (= None)。
                    let idle_dur = std::time::Duration::from_millis(ms);
                    w.last_activity
                        .checked_add(idle_dur)
                        .map(|target| target.saturating_duration_since(now))
                }
                _ => None,
            },
        ];
        for cand in candidates.into_iter().flatten() {
            earliest = Some(match earliest {
                None => cand,
                Some(prev) => prev.min(cand),
            });
        }
    }
    match earliest {
        None => PollTimeout::NONE,
        Some(d) => {
            // PollTimeout は ms 精度。0 (= 即時 timeout) を許容、上限は i32 max ms。
            // `as_millis()` は u128 を返すので `try_from + unwrap_or(i32::MAX)` で
            // saturating cast (= u64::MAX ms 等が来ても panic しない)。
            let ms: i32 = i32::try_from(d.as_millis()).unwrap_or(i32::MAX);
            PollTimeout::try_from(ms).unwrap_or(PollTimeout::NONE)
        }
    }
}

/// poll が timeout で起きた時に各 pending_wait の deadline / idle 経過をチェック。
/// Idle 経過 → WaitResult(Matched)、deadline 経過 → WaitResult(Timeout) として remove。
pub(super) fn check_wait_timeouts(
    pending_waits: &mut Vec<PendingWait>,
    clients: &mut [ClientHandle],
) {
    let now = Instant::now();
    let mut to_remove: Vec<(usize, WaitOutcome)> = Vec::new();
    for (i, w) in pending_waits.iter().enumerate() {
        // Idle predicate: now - last_activity >= idle_ms なら成立
        // u64::MAX 等で `Instant + Duration` が overflow すると panic するため
        // checked_add で防ぐ。overflow した場合は事実上「無限に先」なので Match しない。
        if let WaitPredicate::Idle { ms } = w.predicate {
            if let Some(target) = w
                .last_activity
                .checked_add(std::time::Duration::from_millis(ms))
            {
                if now >= target {
                    to_remove.push((i, WaitOutcome::Matched));
                    continue;
                }
            }
        }
        // 絶対 timeout: deadline 経過なら Timeout
        if let Some(dl) = w.deadline {
            if now >= dl {
                to_remove.push((i, WaitOutcome::Timeout));
            }
        }
    }
    for (i, outcome) in to_remove.into_iter().rev() {
        let w = pending_waits.remove(i);
        if let Some(ch) = clients.iter().find(|c| c.id == w.client_id) {
            let _ = send_control(
                ch,
                ControlMessage::WaitResult(WaitResult {
                    outcome,
                    matched_offset: None,
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pending_wait_idle(ms: u64, last_activity: Instant) -> PendingWait {
        PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Idle { ms },
            options: WaitMatchOptions::default(),
            deadline: None,
            accumulated: Vec::new(),
            last_activity,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }
    }

    #[test]
    fn compute_wait_poll_timeout_saturates_on_u64_max() {
        // Idle{ms: u64::MAX} で last_activity + idle_dur が overflow しても
        // panic せず、有効な PollTimeout を返すこと。
        let now = Instant::now();
        let waits = vec![make_pending_wait_idle(u64::MAX, now)];
        // 単に panic しないことを確認 (= R4-C8 の主目的)。
        let _ = compute_wait_poll_timeout(&waits);
    }

    #[test]
    fn compute_wait_poll_timeout_saturates_on_large_idle_ms() {
        // i32::MAX ms (= ~24.8 日) を超える idle dur でも panic せず、
        // PollTimeout に saturate (= i32::MAX ms 相当) すること。
        let now = Instant::now();
        let huge_ms = (i32::MAX as u64) + 1;
        let waits = vec![make_pending_wait_idle(huge_ms, now)];
        let to = compute_wait_poll_timeout(&waits);
        // PollTimeout::NONE ではなく具体的な値を返すはず (saturate 成功)。
        assert_ne!(to, PollTimeout::NONE, "should saturate, not NONE");
    }

    #[test]
    fn compute_wait_poll_timeout_handles_mixed_normal_and_overflow() {
        // 通常の Idle と overflow する Idle が混ざっても、通常側の
        // 早い deadline が選ばれて panic しないこと。
        let now = Instant::now();
        let waits = vec![
            make_pending_wait_idle(u64::MAX, now), // overflow → 候補から除外
            make_pending_wait_idle(100, now),      // 100ms 後
        ];
        let to = compute_wait_poll_timeout(&waits);
        // 100ms の方が earliest として採用されるはず。
        assert_ne!(to, PollTimeout::NONE);
    }

    #[test]
    fn check_wait_timeouts_does_not_panic_on_u64_max_idle() {
        // R4-C8: check_wait_timeouts の `last_activity + Duration::from_millis(u64::MAX)`
        // も overflow で panic していた。checked_add で防げていることを確認。
        let now = Instant::now();
        let mut waits = vec![make_pending_wait_idle(u64::MAX, now)];
        let mut clients: Vec<ClientHandle> = Vec::new();
        // panic しなければ OK。overflow した Idle は Matched 扱いされず、
        // pending_waits に残り続ける。
        check_wait_timeouts(&mut waits, &mut clients);
        assert_eq!(waits.len(), 1, "overflow Idle should not match");
    }

    /// R4-H3: needle が chunk 境界を跨いでも match できる。`accumulated` は元々
    /// chunk 横断で蓄積されるため plain text では問題ないが、ここでは特に
    /// `strip_escapes=true` + ANSI escape が chunk 境界で分割された場合に
    /// 後続 chunk の escape parameter (例: `1m`) が raw text として漏れず、
    /// needle が正しく検出されることを確認する。
    #[test]
    fn wait_text_matches_across_chunk_boundary_with_strip_escapes() {
        let now = Instant::now();
        let mut waits = vec![PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Text {
                value: "READY".into(),
            },
            options: WaitMatchOptions {
                strip_escapes: true,
                newline_convert_lf: false,
            },
            deadline: None,
            accumulated: Vec::new(),
            last_activity: now,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }];
        // No clients registered; update_waits_on_master_bytes silently skips
        // sending when the client_id is unknown. The match itself is still
        // observable via `waits.is_empty()` after the call (matched waits are
        // removed).
        let mut clients: Vec<ClientHandle> = Vec::new();

        // chunk1: 通常テキスト + CSI escape の途中まで。
        update_waits_on_master_bytes(&mut waits, &mut clients, b"prefix\x1b[3", now);
        // ここでは "READY" は到達していない → match なし
        assert_eq!(waits.len(), 1, "match before READY arrives");

        // chunk2: escape を完結 (`1m`) + needle "READY"。
        // stateless strip だと `1m` が raw として accumulated に入り、かつ
        // chunk1 末尾の `\x1b[3` も raw として残るので、両者を結合した文字列に
        // "READY" は含まれるが、本テストの主眼は「stripped 出力に raw `1m` が
        // 漏れないこと」(= false positive 防止)。
        update_waits_on_master_bytes(&mut waits, &mut clients, b"1mREADY\n", now);
        assert!(
            waits.is_empty(),
            "needle should match across split escape, but wait is still pending"
        );
    }

    /// R4-H3 (negative): 直前 chunk の partial escape が次 chunk と結合されて
    /// false-positive を生まないこと。chunk1 末尾の `\x1b[3` と chunk2 先頭の
    /// `1m` で完結する escape は raw `[31m` を accumulated に漏らさない。
    #[test]
    fn wait_text_no_false_positive_from_split_escape_params() {
        let now = Instant::now();
        // needle は escape の parameter `[31m` を狙う。stateless 実装だとここに
        // ヒットして false positive になる。stateful なら strip されるので不一致。
        let mut waits = vec![PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Text {
                value: "[31m".into(),
            },
            options: WaitMatchOptions {
                strip_escapes: true,
                newline_convert_lf: false,
            },
            deadline: None,
            accumulated: Vec::new(),
            last_activity: now,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }];
        let mut clients: Vec<ClientHandle> = Vec::new();

        // chunk1: ESC `[` (= CSI 開始) で終わる
        update_waits_on_master_bytes(&mut waits, &mut clients, b"\x1b[", now);
        // chunk2: `31m` で escape 完結、その後 plain text
        update_waits_on_master_bytes(&mut waits, &mut clients, b"31mhello", now);

        assert_eq!(
            waits.len(),
            1,
            "split CSI params must not leak as raw text and false-match"
        );
    }

    /// R5-H19: 大量の master bytes を投入しても `accumulated.len()` が
    /// `WAIT_ACCUMULATED_LIMIT` を超えないこと (= trim invariant の機械検証)。
    ///
    /// pending wait が text predicate で「絶対に match しない needle」を持つ状態で、
    /// `WAIT_ACCUMULATED_LIMIT` の 3 倍の master bytes を投入する。各 iteration の
    /// `debug_assert!` が trim 後の上限を機械検証している。
    #[test]
    fn update_waits_keeps_accumulated_within_limit() {
        let now = Instant::now();
        let mut waits = vec![PendingWait {
            client_id: 1,
            predicate: WaitPredicate::Text {
                // 絶対 match しない needle (= 全 bytes を投入し終わるまで wait は残る)
                value: "__NEVER_MATCH_NEEDLE__".into(),
            },
            options: WaitMatchOptions::default(),
            deadline: None,
            accumulated: Vec::new(),
            last_activity: now,
            compiled_regex: None,
            strip_carry: crate::strip::StripAnsiCarry::new(),
        }];
        let mut clients: Vec<ClientHandle> = Vec::new();

        // 1 chunk = 256 KiB を 13 回 = 約 3.25 MiB (> 3 × 1 MiB) 投入。
        // debug_assert! が trim 後の不変量を毎 iteration で検証する。
        let chunk = vec![b'x'; 256 * 1024];
        for _ in 0..13 {
            update_waits_on_master_bytes(&mut waits, &mut clients, &chunk, now);
        }

        assert_eq!(
            waits.len(),
            1,
            "needle should never match, wait must remain"
        );
        let w = &waits[0];
        // release build でも assert で invariant を確認 (debug_assert は debug only)
        assert!(
            w.accumulated.len() <= WAIT_ACCUMULATED_LIMIT,
            "accumulated must stay within WAIT_ACCUMULATED_LIMIT, got {} bytes",
            w.accumulated.len()
        );
        // 末尾 1 MiB を保持していること (= ちょうど limit に張り付く)
        assert_eq!(
            w.accumulated.len(),
            WAIT_ACCUMULATED_LIMIT,
            "after enough input, accumulated should saturate at the limit"
        );
    }
}
