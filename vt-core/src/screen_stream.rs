//! The order of the screen messages of a surface (core docs/spec/persistent-services.md#screen-message). Each message has a
//! number, and a message carries in its change form only the rows that changed since the sent message that it names. The
//! full form carries every cell; it is sent when no sent screen is held (the first screen of the surface in a run, after a
//! message that could not be sent, and the reply of `screen.read`), after a resize, and when more than half of the rows
//! changed, because a change of most rows is larger than the full form.

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::{Map, Value, json};

/// The rows of a sent message, which the next change compares with.
pub struct SentScreen {
    number: u64,
    rows: u16,
    cols: u16,
    lines: Vec<Vec<Value>>,
}

/// The number of the next message and the sent screen that it may change.
pub struct ScreenOrder {
    number: u64,
    previous: Option<SentScreen>,
}

/// The screen messages of one surface: the number of the last message and the screen of the last sent message.
#[derive(Default)]
pub struct ScreenStream {
    number: u64,
    sent: Option<SentScreen>,
}

impl ScreenStream {
    /// Gives the next message its number and the sent screen. The sent screen is taken, so a message that is not sent
    /// leaves no screen and the message after it is the full form.
    pub fn order(&mut self) -> ScreenOrder {
        self.number += 1;
        ScreenOrder {
            number: self.number,
            previous: self.sent.take(),
        }
    }

    /// Holds the screen of a message that was sent, which the next change compares with.
    pub fn sent(&mut self, screen: SentScreen) {
        self.sent = Some(screen);
    }

    /// Forgets the sent screen, so the next message is the full form; the reply of `screen.read` does this.
    pub fn forget(&mut self) {
        self.sent = None;
    }
}

/// The cells of one message: the full form, or the rows that changed since the message `previous`.
pub struct ScreenCells {
    previous: Option<u64>,
    changed: Vec<usize>,
    screen: SentScreen,
}

/// The cells of a screen of `rows` × `cols` cells in row-major order for the message of `order`.
pub fn screen_cells(order: ScreenOrder, rows: u16, cols: u16, cells: Vec<Value>) -> ScreenCells {
    let width = usize::from(cols).max(1);
    let mut lines = Vec::with_capacity(usize::from(rows));
    let mut cells = cells.into_iter();
    loop {
        let line: Vec<Value> = cells.by_ref().take(width).collect();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let change = order
        .previous
        .filter(|previous| previous.rows == rows && previous.cols == cols)
        .map(|previous| {
            let changed: Vec<usize> = (0..lines.len())
                .filter(|&row| previous.lines.get(row) != Some(&lines[row]))
                .collect();
            (previous.number, changed)
        })
        .filter(|(_, changed)| changed.len() * 2 <= lines.len());
    let (previous, changed) = match change {
        Some((previous, changed)) => (Some(previous), changed),
        None => (None, Vec::new()),
    };
    ScreenCells {
        previous,
        changed,
        screen: SentScreen {
            number: order.number,
            rows,
            cols,
            lines,
        },
    }
}

impl ScreenCells {
    /// Inserts `number`, `base` and `cells`, or `previous` and `changed`, into the members of a message. The cells are
    /// copied; [`ScreenCells::line`] writes the message without the copy.
    pub fn insert_into(&self, members: &mut Map<String, Value>) {
        members.insert("number".to_string(), json!(self.screen.number));
        members.insert("base".to_string(), json!(self.previous.is_none()));
        match self.previous {
            None => {
                let cells = self.screen.lines.iter().flatten().cloned().collect();
                members.insert("cells".to_string(), Value::Array(cells));
            }
            Some(previous) => {
                members.insert("previous".to_string(), json!(previous));
                let changed = self
                    .changed
                    .iter()
                    .map(|&row| json!({"row": row, "cells": self.screen.lines[row]}))
                    .collect();
                members.insert("changed".to_string(), Value::Array(changed));
            }
        }
    }

    /// The line `{"surface", "body"}` of the message whose other members are `members`, written from the held rows.
    pub fn line(&self, surface: &str, members: &Map<String, Value>) -> Result<String, String> {
        serde_json::to_string(&Envelope {
            surface,
            body: Body {
                members,
                cells: self,
            },
        })
        .map_err(|error| format!("screen encoding failed: {error}"))
    }

    /// The screen that the message carries, which the stream holds once the message was sent.
    pub fn sent(self) -> SentScreen {
        self.screen
    }
}

struct Envelope<'a> {
    surface: &'a str,
    body: Body<'a>,
}

impl Serialize for Envelope<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("body", &self.body)?;
        map.serialize_entry("surface", self.surface)?;
        map.end()
    }
}

struct Body<'a> {
    members: &'a Map<String, Value>,
    cells: &'a ScreenCells,
}

impl Serialize for Body<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let cells = self.cells;
        let mut map = serializer.serialize_map(None)?;
        for (name, value) in self.members {
            map.serialize_entry(name, value)?;
        }
        map.serialize_entry("number", &cells.screen.number)?;
        map.serialize_entry("base", &cells.previous.is_none())?;
        match cells.previous {
            None => map.serialize_entry("cells", &Flat(&cells.screen.lines))?,
            Some(previous) => {
                map.serialize_entry("previous", &previous)?;
                map.serialize_entry("changed", &Changed(cells))?;
            }
        }
        map.end()
    }
}

/// The rows as one array of cells in row-major order.
struct Flat<'a>(&'a [Vec<Value>]);

impl Serialize for Flat<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.iter().map(Vec::len).sum()))?;
        for cell in self.0.iter().flatten() {
            seq.serialize_element(cell)?;
        }
        seq.end()
    }
}

/// The changed rows as `{row, cells}`.
struct Changed<'a>(&'a ScreenCells);

impl Serialize for Changed<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.changed.len()))?;
        for &row in &self.0.changed {
            seq.serialize_element(&Row {
                row,
                cells: &self.0.screen.lines[row],
            })?;
        }
        seq.end()
    }
}

struct Row<'a> {
    row: usize,
    cells: &'a [Value],
}

impl Serialize for Row<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("row", &self.row)?;
        map.serialize_entry("cells", self.cells)?;
        map.end()
    }
}
