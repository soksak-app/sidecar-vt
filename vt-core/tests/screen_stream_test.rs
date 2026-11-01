// The order of the screen messages of a surface (core docs/spec/persistent-services.md#screen-message): each message has a
// number, and a message carries only the rows that changed since the sent message that it names, except where the full form
// is required.

use serde_json::{Map, Value, json};
use soksak_sidecar_vt_core::screen_stream::{ScreenStream, screen_cells};

fn cell(grapheme: &str) -> Value {
    json!({"grapheme": grapheme, "width": 1, "fg": null, "bg": null, "attributes": 0})
}

/// The cells of a screen of `rows` rows and two columns whose rows hold the given graphemes.
fn grid(rows: &[&str]) -> Vec<Value> {
    rows.iter().flat_map(|row| [cell(row), cell("")]).collect()
}

/// Builds the next message of the stream for the rows and returns its members; `send` states whether the message was sent.
fn message(stream: &mut ScreenStream, rows: &[&str], cols: u16, send: bool) -> Map<String, Value> {
    let mut body = Map::new();
    let order = stream.order();
    let cells = if cols == 2 {
        grid(rows)
    } else {
        rows.iter()
            .flat_map(|row| std::iter::once(cell(row)).chain((1..cols).map(|_| cell(""))))
            .collect()
    };
    let cells = screen_cells(order, rows.len() as u16, cols, cells);
    cells.insert_into(&mut body);
    if send {
        stream.sent(cells.sent());
    }
    body
}

fn form(body: &Map<String, Value>) -> Value {
    let mut members = Map::new();
    for name in ["number", "base", "previous", "changed"] {
        if let Some(value) = body.get(name) {
            members.insert(name.to_string(), value.clone());
        }
    }
    members.insert("cells".to_string(), json!(body.contains_key("cells")));
    Value::Object(members)
}

#[test]
fn the_first_message_is_the_full_form_and_a_later_one_carries_the_changed_rows() {
    let mut stream = ScreenStream::default();
    let first = message(&mut stream, &["a", "b", "c", "d"], 2, true);
    assert_eq!(
        form(&first),
        json!({"number": 1, "base": true, "cells": true})
    );
    assert_eq!(first["cells"], json!(grid(&["a", "b", "c", "d"])));
    let second = message(&mut stream, &["a", "x", "c", "d"], 2, true);
    assert_eq!(
        form(&second),
        json!({"number": 2, "base": false, "previous": 1, "changed": [{"row": 1, "cells": [cell("x"), cell("")]}], "cells": false})
    );
    let third = message(&mut stream, &["a", "x", "c", "d"], 2, true);
    assert_eq!(
        form(&third),
        json!({"number": 3, "base": false, "previous": 2, "changed": [], "cells": false}),
        "a message without a changed row carries an empty change"
    );
}

#[test]
fn more_than_half_of_the_rows_changed_gives_the_full_form() {
    let mut stream = ScreenStream::default();
    message(&mut stream, &["a", "b", "c", "d"], 2, true);
    let half = message(&mut stream, &["x", "y", "c", "d"], 2, true);
    assert_eq!(half["base"], json!(false), "half of the rows is a change");
    let more = message(&mut stream, &["1", "2", "3", "d"], 2, true);
    assert_eq!(
        form(&more),
        json!({"number": 3, "base": true, "cells": true})
    );
}

#[test]
fn a_resize_a_message_not_sent_and_a_forgotten_screen_give_the_full_form() {
    let mut stream = ScreenStream::default();
    message(&mut stream, &["a", "b", "c", "d"], 2, true);
    let resized = message(&mut stream, &["a", "b", "c", "d"], 3, true);
    assert_eq!(
        form(&resized),
        json!({"number": 2, "base": true, "cells": true}),
        "a resize"
    );
    message(&mut stream, &["a", "b", "c", "e"], 3, false);
    let after_unsent = message(&mut stream, &["a", "b", "c", "e"], 3, true);
    assert_eq!(
        form(&after_unsent),
        json!({"number": 4, "base": true, "cells": true}),
        "the message after one that was not sent"
    );
    stream.forget();
    let after_forget = message(&mut stream, &["a", "b", "c", "e"], 3, true);
    assert_eq!(
        form(&after_forget),
        json!({"number": 5, "base": true, "cells": true}),
        "the message after a screen read"
    );
}

#[test]
fn a_change_of_a_link_alone_is_a_changed_row() {
    let mut stream = ScreenStream::default();
    message(&mut stream, &["a", "b", "c", "d"], 2, true);
    let mut body = Map::new();
    let mut cells = grid(&["a", "b", "c", "d"]);
    cells[4]["link"] = json!("https://example.test/");
    let message = screen_cells(stream.order(), 4, 2, cells.clone());
    message.insert_into(&mut body);
    stream.sent(message.sent());
    assert_eq!(
        body["changed"],
        json!([{"row": 2, "cells": [cells[4], cells[5]]}])
    );
}

#[test]
fn the_line_of_a_message_holds_the_members_that_insert_into_gives() {
    let mut stream = ScreenStream::default();
    let mut members = Map::new();
    members.insert("event".to_string(), json!("screen"));
    for rows in [["a", "b", "c", "d"], ["a", "x", "c", "d"]] {
        let message = screen_cells(stream.order(), 4, 2, grid(&rows));
        let line: Value = serde_json::from_str(&message.line("s1", &members).unwrap()).unwrap();
        let mut body = members.clone();
        message.insert_into(&mut body);
        assert_eq!(line, json!({"surface": "s1", "body": body}), "{rows:?}");
        stream.sent(message.sent());
    }
}
