// The alternate screen through a resize (S215). A program of the alternate screen redraws after a resize on the screen that
// the terminal gives it, so the engine resizes the alternate screen as the library resizes a grid without history, also
// while it keeps the rows that the scrolls of the alternate screen move off its top (S208). The fixture
// `tests/fixtures/alternate-resize-tui.json` holds the output that a full-screen program wrote to a real PTY while the PTY was
// resized as a drag of a divider resizes it; the test feeds it with its resizes to the engine and to a `Term` of
// alacritty_terminal with the default configuration, which is the reference, and compares the rows and the cursor after each
// step.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::{Cell, Engine};

/// The size of the reference terminal.
struct TermSize {
    columns: usize,
    lines: usize,
}

impl TermSize {
    fn new(columns: usize, lines: usize) -> Self {
        Self { columns, lines }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

const RECORDING: &str = include_str!("fixtures/alternate-resize-tui.json");

/// The text of a row of the engine without its trailing blanks; a cell without a character is a blank, and the continuation
/// cell of a wide character has no text.
fn engine_row(row: &[Cell]) -> String {
    row.iter()
        .filter(|cell| cell.width != 0)
        .map(|cell| cell.ch.as_deref().unwrap_or(" "))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The rows of the reference in the same form.
fn reference_rows(term: &Term<VoidListener>) -> Vec<String> {
    let grid = term.grid();
    (0..grid.screen_lines() as i32)
        .map(|line| {
            (0..grid.columns())
                .map(|column| &grid[Line(line)][Column(column)])
                .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
                .map(|cell| {
                    let mut text = cell.c.to_string();
                    text.extend(cell.zerowidth().into_iter().flatten());
                    text
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex text of the recording"))
        .collect()
}

/// The rows of a screen and the row and column of its cursor.
type View = (Vec<String>, u16, u16);

/// The engine and the reference, fed with the same steps.
struct Terminals {
    engine: AlacrittyEngine,
    term: Term<VoidListener>,
    processor: Processor,
}

impl Terminals {
    fn new(cols: usize, rows: usize) -> Terminals {
        let mut engine = AlacrittyEngine::new();
        engine.resize(cols as u16, rows as u16);
        Terminals {
            engine,
            term: Term::new(Config::default(), &TermSize::new(cols, rows), VoidListener),
            processor: Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.engine.feed(bytes);
        self.processor.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        self.engine.resize(cols as u16, rows as u16);
        self.term.resize(TermSize::new(cols, rows));
    }

    /// The rows and the cursor of the engine and of the reference.
    fn screens(&mut self) -> (View, View) {
        let screen = self.engine.screen();
        let actual = screen.lines.iter().map(|row| engine_row(row)).collect();
        let cursor = self.term.grid().cursor.point;
        (
            (actual, screen.cursor.row, screen.cursor.col),
            (
                reference_rows(&self.term),
                cursor.line.0 as u16,
                cursor.column.0 as u16,
            ),
        )
    }
}

#[test]
fn a_resized_alternate_screen_matches_the_screen_of_the_library() {
    let recording: Value = serde_json::from_str(RECORDING).expect("parse the recording");
    let steps = recording["steps"]
        .as_array()
        .expect("steps of the recording");
    let size = &recording["size"];
    let mut terminals = Terminals::new(
        size["cols"].as_u64().expect("cols") as usize,
        size["rows"].as_u64().expect("rows") as usize,
    );
    for (index, step) in steps.iter().enumerate() {
        if let Some(output) = step["output"].as_str() {
            terminals.feed(&hex(output));
        } else {
            terminals.resize(
                step["resize"]["cols"].as_u64().expect("cols of a resize") as usize,
                step["resize"]["rows"].as_u64().expect("rows of a resize") as usize,
            );
        }
        // An open synchronized update holds output in each terminal until it ends, so the screens are compared only between
        // updates.
        if terminals.engine.sync_deadline().is_some() {
            continue;
        }
        let (actual, expected) = terminals.screens();
        assert_eq!(
            actual,
            expected,
            "the screen after step {index} of {} ({step}) differs from the screen of the library",
            steps.len()
        );
    }
}

/// The alternate grid is resized while the primary screen is shown, and the program enters the alternate screen again: the
/// rows that the smaller height cut off stay out of the alternate screen and out of its kept rows.
#[test]
fn an_alternate_screen_resized_while_hidden_returns_without_its_old_rows() {
    let mut terminals = Terminals::new(20, 6);
    terminals.feed(b"\x1b[?1049h\x1b[Hone\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    terminals.feed(b"\x1b[?1049l");
    terminals.resize(20, 3);
    terminals.feed(b"\x1b[?1049h");
    terminals.resize(20, 6);
    let (actual, expected) = terminals.screens();
    assert_eq!(
        actual, expected,
        "the screen after the alternate screen returned"
    );
    terminals.feed(b"\x1b[6;1Hlast");
    let (actual, expected) = terminals.screens();
    assert_eq!(
        actual, expected,
        "the screen after output on the returned alternate screen"
    );
    assert_eq!(
        terminals.engine.alternate_rows().history,
        0,
        "no row left the returned alternate screen by a scroll"
    );
}
