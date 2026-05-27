//! screen dump bytes を決定的 snapshot 文字列に変換する正規化 helper。
//!
//! zellij の `account_for_races_in_snapshot` pattern。snapshot 比較前に
//! **非決定的に変動する領域** を regex で削り `<TIMESTAMP>` / `<HOME>` 等の
//! placeholder に置換する。snapshot の差分が「意味のある screen state 変更」
//! のみを表現するようにする狙い。
//!
//! 正規化の rule は **最小限** に留め、過剰な lossy 化を避ける (= 本当に
//! 変わってよい部分だけ伏せる、cell 内容や cursor 位置の変化は素通し)。

#![allow(dead_code)] // 各 test は subset しか使わないため

use std::sync::OnceLock;

use regex::bytes::Regex;

/// screen dump bytes を文字列化して正規化する。
///
/// 適用される rule (順番が意味を持つ):
///
/// 1. **ISO-8601 風 timestamp**: `2026-05-27T12:34:56` (`Z` / 小数秒 / `+09:00`
///    suffix も許容) → `<TIMESTAMP>`
/// 2. **ユーザ HOME 絶対 path**: `/Users/<name>/...` / `/home/<name>/...` を
///    `<HOME>/...` に圧縮
/// 3. **session_id の `run-<pid>-<rand4hex>` 形式**: 自動生成 session id は
///    pid + rand なので run ごとに変わる → `run-<PID>-<RAND>` に圧縮。**ただし
///    test 側が固定 session 名を渡すなら一致しないので影響なし**
/// 4. **本物のパス区切り内の数字 PID 系**: 過剰削除を避けるため、明示的な
///    pattern (= `pid=12345` / `pid 12345`) のみを `pid=<PID>` 化
///
/// **重要**: cursor / cell 内容 / 色 / ANSI sequence は触らない (= snapshot の
/// 本来の比較対象)。
pub fn normalize_screen_dump(bytes: &[u8]) -> String {
    let mut buf = bytes.to_vec();
    for (re, replacement) in rules() {
        buf = re.replace_all(&buf, *replacement).into_owned();
    }
    // utf8 不正 byte は lossy 変換 (= replacement char に)。snapshot 比較なので
    // 厳密 utf8 性は要求しない。
    String::from_utf8_lossy(&buf).into_owned()
}

/// 正規化 rule 一覧 (regex + 置換 string)。
fn rules() -> &'static [(Regex, &'static [u8])] {
    static RULES: OnceLock<Vec<(Regex, &'static [u8])>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            // 1. ISO-8601 timestamp (= 日時 + 任意の小数秒 / TZ suffix)。
            //    例: `2026-05-27T12:34:56`, `2026-05-27T12:34:56.123Z`, `2026-05-27T12:34:56+09:00`
            (
                Regex::new(
                    r"(?-u)\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?\b",
                )
                .expect("regex 1"),
                b"<TIMESTAMP>" as &[u8],
            ),
            // 2. macOS / Linux 系の HOME 絶対 path。
            //    `/Users/<name>/` (macOS) または `/home/<name>/` (Linux) → `<HOME>/`
            (
                Regex::new(r"(?-u)/(?:Users|home)/[A-Za-z0-9_.-]+(/|\b)").expect("regex 2"),
                b"<HOME>$1" as &[u8],
            ),
            // 3. `run-<pid>-<8hex>` 形式の auto session id。
            //    `run-12345-9af3a17c` のような形式を一括で `run-<PID>-<RAND>` に。
            (
                Regex::new(r"(?-u)\brun-\d+-[0-9a-f]{8}\b").expect("regex 3"),
                b"run-<PID>-<RAND>" as &[u8],
            ),
            // 4. `pid=<N>` / `pid:<N>` のような明示 PID 表記。
            //    過剰削除しないよう **key=value** 形式のみ対象。
            (
                Regex::new(r"(?-u)\b(pid|ppid|pgid|sid)([=:])(\d+)\b").expect("regex 4"),
                b"$1$2<PID>" as &[u8],
            ),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_iso8601_timestamp() {
        let input = b"booted at 2026-05-27T12:34:56 fine";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "booted at <TIMESTAMP> fine");
    }

    #[test]
    fn normalize_iso8601_with_tz() {
        let input = b"x 2026-05-27T12:34:56+09:00 y";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "x <TIMESTAMP> y");
    }

    #[test]
    fn normalize_iso8601_with_subseconds_z() {
        let input = b"x 2026-05-27T12:34:56.123456Z y";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "x <TIMESTAMP> y");
    }

    #[test]
    fn normalize_macos_home_path() {
        let input = b"cwd=/Users/kawaz/repos/hyoui";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "cwd=<HOME>/repos/hyoui");
    }

    #[test]
    fn normalize_linux_home_path() {
        let input = b"cwd=/home/kawaz/proj";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "cwd=<HOME>/proj");
    }

    #[test]
    fn normalize_auto_session_id() {
        let input = b"session run-12345-9af3a17c started";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "session run-<PID>-<RAND> started");
    }

    #[test]
    fn normalize_pid_key_value() {
        let input = b"pid=12345 ppid:6789 pgid=42";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "pid=<PID> ppid:<PID> pgid=<PID>");
    }

    #[test]
    fn normalize_does_not_touch_normal_text() {
        let input = b"hello world\r\n";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "hello world\r\n");
    }

    #[test]
    fn normalize_preserves_ansi_sequences() {
        let input = b"\x1b[31mred\x1b[0m";
        let got = normalize_screen_dump(input);
        assert_eq!(got, "\u{1b}[31mred\u{1b}[0m");
    }

    #[test]
    fn normalize_combined() {
        let input = b"start 2026-05-27T12:34:56 in /Users/k/x session=run-1-abcdef01";
        let got = normalize_screen_dump(input);
        assert_eq!(
            got,
            "start <TIMESTAMP> in <HOME>/x session=run-<PID>-<RAND>"
        );
    }
}
