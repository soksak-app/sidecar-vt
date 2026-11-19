// The prompt region of core docs/spec/terminal-state.md#prompt-region through the real engine. Each case of the shared
// cases (`tests/fixtures/terminal-state-prompt-region.json`, the copy of core
// `docs/spec/cases/terminal-state/prompt-region.json`) gives the size of the screen and steps; each step is an output
// that the engine reads or a resize, and states the rows of the screen and of the history, the cursor and the errors
// after it.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::{Cell, Engine, EngineEvent};

const CASES: &str = include_str!("fixtures/terminal-state-prompt-region.json");

/// The text of a row without its trailing blanks; a cell without a character is a blank.
fn row_text(row: &[Cell]) -> String {
    row.iter()
        .map(|cell| cell.ch.as_deref().unwrap_or(" "))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The rows of the screen.
fn screen_rows(engine: &mut AlacrittyEngine) -> Vec<String> {
    engine
        .screen()
        .lines
        .iter()
        .map(|row| row_text(row))
        .collect()
}

/// The rows of the history from the oldest, read through the viewport one screen at a time.
fn history_rows(engine: &mut AlacrittyEngine) -> Vec<String> {
    let (_, history) = engine.scrollback();
    let lines = engine.screen().lines.len();
    let mut rows = Vec::with_capacity(history);
    let mut remaining = history;
    while remaining > 0 {
        engine.scroll_to_newest();
        engine.scroll_viewport(remaining as i32);
        let take = remaining.min(lines);
        rows.extend(screen_rows(engine).into_iter().take(take));
        remaining -= take;
    }
    engine.scroll_to_newest();
    rows
}

fn size(value: &Value) -> (u16, u16) {
    let pair = value.as_array().expect("a size is a pair");
    let number = |index: usize| {
        u16::try_from(pair[index].as_u64().expect("a size holds numbers")).expect("a size fits")
    };
    (number(0), number(1))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("a list of rows")
        .iter()
        .map(|row| row.as_str().expect("a row is a string").to_string())
        .collect()
}

/// Runs the steps of one case and returns the first difference.
fn run_case(case: &Value) -> Result<(), String> {
    let mut engine = AlacrittyEngine::new();
    let (cols, rows) = size(&case["size"]);
    engine.resize(cols, rows);
    engine.drain_events();
    for (index, step) in case["steps"]
        .as_array()
        .expect("a case has steps")
        .iter()
        .enumerate()
    {
        if let Some(output) = step.get("output") {
            engine.feed(output.as_str().expect("an output is a string").as_bytes());
        } else {
            let (cols, rows) = size(&step["resize"]);
            engine.resize(cols, rows);
        }
        let errors: Vec<String> = engine
            .drain_events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::Error(error) => Some(error),
                _ => None,
            })
            .collect();
        let mut found = vec![("errors", format!("{errors:?}"))];
        let mut expected = vec![(
            "errors",
            format!("{:?}", step.get("errors").map(strings).unwrap_or_default()),
        )];
        if let Some(screen) = step.get("screen") {
            found.push(("screen", format!("{:?}", screen_rows(&mut engine))));
            expected.push(("screen", format!("{:?}", strings(screen))));
        }
        if let Some(history) = step.get("history") {
            found.push(("history", format!("{:?}", history_rows(&mut engine))));
            expected.push(("history", format!("{:?}", strings(history))));
        }
        if let Some(cursor) = step.get("cursor") {
            let at = engine.cursor();
            found.push(("cursor", format!("{:?}", (at.row, at.col))));
            expected.push(("cursor", format!("{:?}", size(cursor))));
        }
        for ((what, found), (_, expected)) in found.iter().zip(&expected) {
            if found != expected {
                return Err(format!(
                    "step {index}: the {what} is {found}, expected {expected}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn each_shared_case_gives_its_rows_after_each_step() {
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
