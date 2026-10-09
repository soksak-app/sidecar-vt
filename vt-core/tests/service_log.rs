// The standard error of the service is the service log, whose lines are text records of one form
// (core docs/spec/diagnostics.md#forms): `<time> <level> sidecar <where>: <text>`.
use soksak_sidecar_vt_core::service_log::{log_line, record_line};

#[test]
fn a_record_has_the_time_the_level_the_layer_the_place_and_the_text_on_one_line() {
    let line = record_line("error", "surface close", "first\nsecond");
    let (time, rest) = line.split_once(' ').expect("a time and a rest");
    assert_eq!(
        time.len(),
        24,
        "the time is ISO-8601 with milliseconds: {time}"
    );
    assert!(time.ends_with('Z'));
    assert_eq!(rest, r"error sidecar surface close: first\nsecond");
    assert!(!line.contains('\n'), "a record holds a line feed");
}

#[test]
fn the_line_that_is_written_ends_with_one_line_feed() {
    let line = log_line("info", "font", "no listed font is installed");
    assert!(
        line.ends_with("info sidecar font: no listed font is installed\n"),
        "{line:?}"
    );
    assert_eq!(line.matches('\n').count(), 1);
}
