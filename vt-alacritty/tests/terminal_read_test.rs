// The kept rows of the alternate screen of core docs/spec/terminal-read.md through the real engine. Each case of the shared
// cases (`tests/fixtures/terminal-read-alternate-rows.json`, the copy of core
// `docs/spec/cases/terminal-read/alternate-rows.json`) gives the size of the screen, the history limit and an output, then
// the reads of the alternate rows and the alternate member of the state after it; a case with `then` writes a second output
// and checks its reads.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::{Cell, Engine, EngineEvent, RowScreen};

const CASES: &str = include_str!("fixtures/terminal-read-alternate-rows.json");

/// The text of a row without its trailing blanks; a cell without a character is a blank.
fn row_text(row: &[Cell]) -> String {
    row.iter()
        .map(|cell| cell.ch.as_deref().unwrap_or(" "))
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn number(value: &Value, what: &str) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("{what} is a number"))
}

/// Runs the reads of one step and compares the alternate member and the dropped rows that the engine reported.
fn check_step(
    engine: &mut AlacrittyEngine,
    step: &Value,
    dropped_before: u64,
) -> Result<u64, String> {
    let reported: u64 = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::AlternateRowsDropped { rows, .. } => Some(rows as u64),
            _ => None,
        })
        .sum();
    for read in step["reads"].as_array().expect("a step has reads") {
        let screen = match read["screen"].as_str().expect("a read names its screen") {
            "alternate" => RowScreen::Alternate,
            "primary" => RowScreen::Primary,
            other => return Err(format!("the case names the screen {other}")),
        };
        let from = number(&read["from"], "from");
        let count = u32::try_from(number(&read["count"], "count")).expect("a count fits");
        let rows = engine
            .screen_rows(screen, from, count)
            .map_err(|error| format!("the read from {from} count {count} failed: {error}"))?;
        let found: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        let expected: Vec<String> = read["text"]
            .as_array()
            .expect("a read has text")
            .iter()
            .map(|row| row.as_str().expect("a row is a string").to_string())
            .collect();
        if found != expected {
            return Err(format!(
                "the read from {from} count {count} is {found:?}, expected {expected:?}"
            ));
        }
    }
    let alternate = engine.alternate_rows();
    let expected = &step["alternate"];
    let found = (
        alternate.active,
        alternate.history as u64,
        alternate.dropped,
    );
    let wanted = (
        expected["active"].as_bool().expect("active is a boolean"),
        number(&expected["history"], "history"),
        number(&expected["dropped"], "dropped"),
    );
    if found != wanted {
        return Err(format!(
            "the alternate member is {found:?}, expected {wanted:?}"
        ));
    }
    if dropped_before + reported != alternate.dropped {
        return Err(format!(
            "the engine reported {reported} dropped rows after {dropped_before}, and the member counts {}",
            alternate.dropped
        ));
    }
    Ok(alternate.dropped)
}

fn run_case(case: &Value) -> Result<(), String> {
    let limit =
        usize::try_from(number(&case["historyLimit"], "historyLimit")).expect("a limit fits");
    let mut engine = AlacrittyEngine::with_history_limit(limit);
    let cols = u16::try_from(number(&case["cols"], "cols")).expect("cols fit");
    let rows = u16::try_from(number(&case["rows"], "rows")).expect("rows fit");
    engine.resize(cols, rows);
    engine.drain_events();
    engine.feed(
        case["output"]
            .as_str()
            .expect("an output is a string")
            .as_bytes(),
    );
    let dropped = check_step(&mut engine, case, 0)?;
    if let Some(then) = case.get("then") {
        engine.feed(
            then["output"]
                .as_str()
                .expect("an output is a string")
                .as_bytes(),
        );
        check_step(&mut engine, then, dropped).map_err(|error| format!("then: {error}"))?;
    }
    Ok(())
}

#[test]
fn each_shared_case_reads_the_kept_rows_of_the_alternate_screen() {
    let cases: Value = serde_json::from_str(CASES).expect("the shared cases are JSON");
    let failures: Vec<String> = cases["cases"]
        .as_array()
        .expect("the cases are a list")
        .iter()
        .filter_map(|case| {
            let name = case["name"].as_str().expect("a case has a name");
            run_case(case)
                .err()
                .map(|failure| format!("{name}: {failure}"))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "failed cases:\n{}",
        failures.join("\n")
    );
}

#[test]
fn a_read_of_the_alternate_rows_outside_the_kept_rows_is_refused_with_the_kept_range() {
    let mut engine = AlacrittyEngine::with_history_limit(1000);
    engine.resize(10, 5);
    engine.feed(b"\x1b[?1049h\x1b[H1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7");
    let error = engine
        .screen_rows(RowScreen::Alternate, 0, 8)
        .expect_err("row 7 is not kept");
    assert_eq!(
        error,
        "screen.read alternate from 0 count 8 is outside the kept rows 0 to 6"
    );
}
