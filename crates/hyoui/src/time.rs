//! epoch 時刻を ISO 8601 (UTC) に整形する。
//!
//! Design rationale: 時刻 crate を足さずに自前で持つ。offset は UTC (`Z`) 固定で
//! local offset を解決しない — 絶対時刻として読めれば足り、tz database を引くために
//! 依存を増やす理由が無い。
//!
//! 使い手は web gateway の `/auth/*` 応答と `hello.auth_expires_at` (DR-0036 決定 5)、
//! unit 登録簿の `added_at` / `started_at` (DR-0034 決定 2) で、どちらも同じ表記を出す。

/// 今の時刻 (秒精度、UTC の ISO 8601)。
pub fn now_iso8601() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    format_iso8601_utc(seconds)
}

/// 今の時刻 (unix epoch の ms)。
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// unix epoch の ms を UTC の ISO 8601 (秒精度) に整形する。
pub fn format_unix_ms_iso8601(unix_ms: u64) -> String {
    format_iso8601_utc((unix_ms / 1000) as i64)
}

/// epoch 秒を UTC の ISO 8601 に整形する。
pub fn format_iso8601_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// epoch からの日数を暦の (年, 月, 日) に開く。
///
/// Howard Hinnant の `civil_from_days` (public domain) と同じ式で、3 月を年の
/// 起点に取り直してうるう年の分岐を無くしている。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_seconds_format_as_iso8601() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601_utc(1), "1970-01-01T00:00:01Z");
        // うるう日を跨ぐ境界 (= civil_from_days の分岐が効く位置)。
        assert_eq!(format_iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_iso8601_utc(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(format_iso8601_utc(1_709_251_200), "2024-03-01T00:00:00Z");
        // 400 年周期の境界 (2100 は平年)。
        assert_eq!(format_iso8601_utc(4_107_542_400), "2100-03-01T00:00:00Z");
        // epoch 以前も破綻しない (= div_euclid / rem_euclid)。
        assert_eq!(format_iso8601_utc(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn unix_ms_truncates_to_seconds() {
        assert_eq!(format_unix_ms_iso8601(1_999), "1970-01-01T00:00:01Z");
    }

    #[test]
    fn now_is_well_formed() {
        assert!(now_iso8601().ends_with('Z'));
        assert_eq!(now_iso8601().len(), "1970-01-01T00:00:00Z".len());
        assert!(
            now_unix_ms() > 1_700_000_000_000,
            "epoch ms として現実的な値"
        );
    }
}
