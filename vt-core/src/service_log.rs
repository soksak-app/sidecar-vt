//! The service log (core docs/spec/diagnostics.md#forms): the standard error of the service holds text records of one
//! form, `<time> <level> sidecar <where>: <text>`, one record on one line.

/// The record without its line end. A line feed in the text is written as the two characters `\n`.
pub fn record_line(level: &str, place: &str, text: impl std::fmt::Display) -> String {
    format!(
        "{} {level} sidecar {place}: {}",
        crate::performance::now_iso8601_ms(),
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
