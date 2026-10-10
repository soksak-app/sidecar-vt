//! The service log (core docs/spec/diagnostics.md#forms): the standard error of the service holds text records of one
//! form, `<time> <level> sidecar <where>: <text>`, one record on one line.

use std::time::{SystemTime, UNIX_EPOCH};

/// The record without its line end. A line feed in the text is written as the two characters `\n`.
pub fn record_line(level: &str, place: &str, text: impl std::fmt::Display) -> String {
    format!(
        "{} {level} sidecar {place}: {}",
        now_iso8601_ms(),
        text.to_string().replace('\n', "\\n")
    )
}

/// The record with its line end, which is written with one write.
pub fn log_line(level: &str, place: &str, text: impl std::fmt::Display) -> String {
    format!("{}\n", record_line(level, place, text))
}

/// Writes a record of level `error` to the standard error.
pub fn log_error(place: &str, text: impl std::fmt::Display) {
    eprint!("{}", log_line("error", place, text));
}

/// Writes a record of level `info` to the standard error.
pub fn log_info(place: &str, text: impl std::fmt::Display) {
    eprint!("{}", log_line("info", place, text));
}

/// 유닉스 시각(밀리초)을 ISO-8601 로 바꾼다(종속성을 더하지 않고 직접 계산).
fn now_iso8601_ms() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default(); // 기본값: 시계는 에포크 이전을 돌려주지 않는다
    let millis_total = since.as_millis();
    let days = (millis_total / 86_400_000) as i64;
    let millis_day = (millis_total % 86_400_000) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = millis_day / 3_600_000;
    let minute = (millis_day % 3_600_000) / 60_000;
    let second = (millis_day % 60_000) / 1_000;
    let millis = millis_day % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// 날수를 역·월·일로 바꾼다(Howard Hinnant 의 civil_from_days).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_converter_matches_a_known_instant() {
        // 2026-09-29T10:20:30.400Z — 유닉스 밀리초 1790665230400.
        let since = std::time::Duration::from_millis(1_790_665_230_400);
        assert_eq!(
            civil_from_days((since.as_millis() / 86_400_000) as i64),
            (2026, 9, 29)
        );
    }
}
