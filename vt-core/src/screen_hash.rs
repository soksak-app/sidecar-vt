//! The hash of a terminal screen (core docs/spec/replay.md#screen-hash): the SHA-256, as 64 lowercase hexadecimal characters,
//! of the canonical bytes of the screen. A sidecar stores the hash of each screen it produces, and a replay compares it with
//! the hash of the screen that the stored inputs rebuild.

use crate::protocol::Screen;
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAGIC: &[u8] = b"SKSCR1";

/// The cell that stands for a cell the engine leaves out.
static BLANK: crate::protocol::Cell = crate::protocol::Cell {
    ch: None,
    width: 1,
    fg: None,
    bg: None,
    bold: false,
    italic: false,
    underline: false,
    inverse: false,
    link: None,
};

/// A color of a cell: the default color, an index of the palette, or a true color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Color {
    Default,
    Indexed(u8),
    True(u8, u8, u8),
}

impl Color {
    /// The color in the JSON form of the contract.
    fn json(self) -> Value {
        match self {
            Color::Default => Value::Null,
            Color::Indexed(index) => Value::from(index),
            Color::True(red, green, blue) => {
                Value::from(format!("#{red:02x}{green:02x}{blue:02x}"))
            }
        }
    }

    fn bytes(self) -> [u8; 4] {
        match self {
            Color::Default => [0, 0, 0, 0],
            Color::Indexed(index) => [1, 0, 0, index],
            Color::True(red, green, blue) => [2, red, green, blue],
        }
    }

    /// `#rrggbb` in lowercase; None for any other text.
    fn parse_true(text: &str) -> Option<Color> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6
            || !digits
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        // default: the six hexadecimal digits were checked above, so the conversion cannot fail.
        let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
        Some(Color::True(channel(0)?, channel(2)?, channel(4)?))
    }
}

struct HashCell<'a> {
    grapheme: &'a str,
    width: u8,
    fg: Color,
    bg: Color,
    attributes: u8,
}

/// The canonical bytes of a screen of `rows` × `cols` cells.
fn canonical(rows: u32, cols: u32, cursor: (bool, u32, u32), cells: &[HashCell<'_>]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(23 + cells.len() * 14);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&rows.to_be_bytes());
    bytes.extend_from_slice(&cols.to_be_bytes());
    bytes.push(u8::from(cursor.0));
    bytes.extend_from_slice(&cursor.1.to_be_bytes());
    bytes.extend_from_slice(&cursor.2.to_be_bytes());
    for cell in cells {
        bytes.extend_from_slice(&(cell.grapheme.len() as u32).to_be_bytes());
        bytes.extend_from_slice(cell.grapheme.as_bytes());
        bytes.push(cell.width);
        bytes.extend_from_slice(&cell.fg.bytes());
        bytes.extend_from_slice(&cell.bg.bytes());
        bytes.push(cell.attributes);
    }
    bytes
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn refuse<T>(reason: String) -> Result<T, String> {
    Err(format!("screen hash: {reason}"))
}

fn integer(name: &str, value: &Value, min: u64, max: u64) -> Result<u64, String> {
    match value.as_u64() {
        Some(number) if number >= min && number <= max => Ok(number),
        _ => refuse(format!(
            "{name} must be an integer from {min} to {max}, found {value}"
        )),
    }
}

fn json_color(name: &str, value: &Value) -> Result<Color, String> {
    match value {
        Value::Null => return Ok(Color::Default),
        Value::Number(number) => {
            if let Some(index) = number.as_u64().filter(|index| *index <= 255) {
                return Ok(Color::Indexed(index as u8));
            }
        }
        Value::String(text) => {
            if let Some(color) = Color::parse_true(text) {
                return Ok(color);
            }
        }
        _ => {}
    }
    refuse(format!(
        "{name} must be null, an integer from 0 to 255 or \"#rrggbb\", found {value}"
    ))
}

/// The canonical bytes of a screen in the JSON form of the contract (`{rows, cols, cursor, cells}`). The error names the member
/// that is malformed.
pub fn screen_bytes_json(screen: &Value) -> Result<Vec<u8>, String> {
    let Some(screen) = screen.as_object() else {
        return refuse(format!("screen must be an object, found {screen}"));
    };
    // default: a member that is absent is null, and the check that follows refuses null with an error that names the member.
    let get = |name: &str| screen.get(name).unwrap_or(&Value::Null);
    let mut sizes = [0u32; 2];
    for (index, name) in ["rows", "cols"].into_iter().enumerate() {
        match get(name)
            .as_u64()
            .filter(|value| (1..=u64::from(u32::MAX)).contains(value))
        {
            Some(value) => sizes[index] = value as u32,
            None => {
                return refuse(format!(
                    "{name} must be a positive integer, found {}",
                    get(name)
                ));
            }
        }
    }
    let [rows, cols] = sizes;
    let Some(cursor) = get("cursor").as_object() else {
        return refuse(format!("cursor must be an object, found {}", get("cursor")));
    };
    let Some(visible) = cursor.get("visible").and_then(Value::as_bool) else {
        return refuse(format!(
            "cursor.visible must be true or false, found {}",
            // default: an absent member is null in the text of the error that names it.
            cursor.get("visible").unwrap_or(&Value::Null)
        ));
    };
    let null = Value::Null;
    let row = integer(
        "cursor.row",
        // default: a member that is absent is null, and the check that follows refuses null with an error that names the member.
        cursor.get("row").unwrap_or(&null),
        0,
        u64::from(rows) - 1,
    )?;
    let col = integer(
        "cursor.col",
        // default: a member that is absent is null, and the check that follows refuses null with an error that names the member.
        cursor.get("col").unwrap_or(&null),
        0,
        u64::from(cols) - 1,
    )?;
    let expected = rows as usize * cols as usize;
    let cells = match get("cells").as_array() {
        Some(cells) if cells.len() == expected => cells,
        Some(cells) => {
            return refuse(format!(
                "cells must hold {expected} cells (rows {rows} × cols {cols}), found {}",
                cells.len()
            ));
        }
        None => {
            return refuse(format!(
                "cells must hold {expected} cells (rows {rows} × cols {cols}), found {}",
                get("cells")
            ));
        }
    };
    let mut hashed = Vec::with_capacity(cells.len());
    for (index, cell) in cells.iter().enumerate() {
        let at = format!("cells[{index}]");
        let Some(cell) = cell.as_object() else {
            return refuse(format!("{at} must be an object, found {cell}"));
        };
        // default: a member that is absent is null, and the check that follows refuses null with an error that names the member.
        let field = |name: &str| cell.get(name).unwrap_or(&null);
        let Some(grapheme) = field("grapheme").as_str() else {
            return refuse(format!(
                "{at}.grapheme must be a string, found {}",
                field("grapheme")
            ));
        };
        let width = match field("width").as_u64() {
            Some(width @ 0..=2) => width as u8,
            _ => {
                return refuse(format!(
                    "{at}.width must be 0, 1 or 2, found {}",
                    field("width")
                ));
            }
        };
        let fg = json_color(&format!("{at}.fg"), field("fg"))?;
        let bg = json_color(&format!("{at}.bg"), field("bg"))?;
        let attributes = integer(&format!("{at}.attributes"), field("attributes"), 0, 255)? as u8;
        hashed.push(HashCell {
            grapheme,
            width,
            fg,
            bg,
            attributes,
        });
    }
    Ok(canonical(
        rows,
        cols,
        (visible, row as u32, col as u32),
        &hashed,
    ))
}

/// The hash of a screen in the JSON form of the contract.
pub fn screen_hash_json(screen: &Value) -> Result<String, String> {
    screen_bytes_json(screen).map(|bytes| digest(&bytes))
}

/// The hash of the screen of the engine. A row or a cell that the engine leaves out is a blank cell; the colors of a cell are
/// the true colors that the engine resolved (`#rrggbb`) or the default color. The attributes are those that a cell of the
/// engine states: bold `01`, italic `04`, underline `08` and inverse `20`.
pub fn screen_hash(screen: &Screen) -> Result<String, String> {
    let cells = screen_cells(screen)?;
    Ok(digest(&canonical(
        u32::from(screen.rows),
        u32::from(screen.cols),
        (
            screen.cursor.visible,
            u32::from(screen.cursor.row),
            u32::from(screen.cursor.col),
        ),
        &cells,
    )))
}

/// The cells of the message `screen` for the screen of the engine, in the form of core (docs/spec/terminal-runtime.md#screen-message):
/// `rows` × `cols` objects with `grapheme`, `width`, `fg`, `bg` (null, an integer or `#rrggbb`), `attributes`, and `link` when the
/// cell has one. They are the cells that [`screen_hash`] covers, so a page that hashes the message gets the hash of the screen.
pub fn message_cells(screen: &Screen) -> Result<Vec<Value>, String> {
    let cells = screen_cells(screen)?;
    let mut values = Vec::with_capacity(cells.len());
    for (index, cell) in cells.iter().enumerate() {
        let mut value = serde_json::json!({
            "grapheme": cell.grapheme,
            "width": cell.width,
            "fg": cell.fg.json(),
            "bg": cell.bg.json(),
            "attributes": cell.attributes,
        });
        let (row, col) = (
            index / usize::from(screen.cols),
            index % usize::from(screen.cols),
        );
        if let Some(link) = screen
            .lines
            .get(row)
            .and_then(|line| line.get(col))
            .and_then(|cell| cell.link.as_ref())
        {
            value["link"] = serde_json::json!(link);
        }
        values.push(value);
    }
    Ok(values)
}

/// The cells of the screen of the engine, `rows` × `cols`, with the checks of the size and of the cursor.
fn screen_cells(screen: &Screen) -> Result<Vec<HashCell<'_>>, String> {
    let (rows, cols) = (u32::from(screen.rows), u32::from(screen.cols));
    if rows == 0 || cols == 0 {
        return refuse(format!("the screen is {rows}x{cols}"));
    }
    if screen.cursor.row >= screen.rows || screen.cursor.col >= screen.cols {
        return refuse(format!(
            "the cursor ({}, {}) is outside the screen {rows}x{cols}",
            screen.cursor.row, screen.cursor.col
        ));
    }
    let color = |name: &str, color: &Option<String>| match color {
        None => Ok(Color::Default),
        Some(text) => Color::parse_true(text)
            .ok_or_else(|| format!("screen hash: {name} must be #rrggbb, found {text:?}")),
    };
    let mut cells = Vec::with_capacity(rows as usize * cols as usize);
    for row in 0..rows as usize {
        for col in 0..cols as usize {
            let cell = screen
                .lines
                .get(row)
                .and_then(|line| line.get(col))
                // default: a cell that the engine leaves out is a blank cell.
                .unwrap_or(&BLANK);
            cells.push(HashCell {
                // default: a blank cell has no grapheme.
                grapheme: cell.ch.as_deref().unwrap_or(""),
                width: cell.width,
                fg: color(&format!("cell ({row}, {col}) fg"), &cell.fg)?,
                bg: color(&format!("cell ({row}, {col}) bg"), &cell.bg)?,
                attributes: u8::from(cell.bold)
                    | u8::from(cell.italic) << 2
                    | u8::from(cell.underline) << 3
                    | u8::from(cell.inverse) << 5,
            });
        }
    }
    Ok(cells)
}
