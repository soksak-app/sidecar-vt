// The `screen` message of the service carries the members of the screen hash (core docs/spec/terminal-runtime.md#screen-message):
// for each vector of the golden file the message that the service builds from the screen of the vector has the members of the
// golden message and the hash of the vector. The golden file is owned by core (tests/fixtures/log-contract/README.md).
use serde_json::{Value, json};
use soksak_sidecar_vt_core::protocol::{Cell, Cursor, JsonRange, Preedit, Screen, screen_message};
use soksak_sidecar_vt_core::screen_hash::{screen_hash, screen_hash_json};
use soksak_sidecar_vt_core::screen_stream::ScreenStream;

fn golden(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/log-contract")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    // A Rust string cannot hold a lone surrogate; the escape of the refused cases is replaced by a character that parses.
    serde_json::from_str(&text.replace("\\ud800", "\u{fffd}"))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn accepted_frame(sequence: u64) -> Value {
    json!({"generation": 1, "raster": 1, "sequence": sequence})
}

/// The screen of the engine for a golden message, or None when the engine cannot hold it: the engine resolves an indexed color to
/// a true color, and its cells state bold, italic, underline and inverse only.
fn screen_of(message: &Value) -> Option<Screen> {
    let rows = message["rows"].as_u64().unwrap() as u16;
    let cols = message["cols"].as_u64().unwrap() as u16;
    let mut lines = vec![Vec::new(); rows as usize];
    for (index, cell) in message["cells"].as_array().unwrap().iter().enumerate() {
        let color = |value: &Value| match value {
            Value::Null => Some(None),
            Value::String(text) => soksak_sidecar_vt_core::palette::parse_hex(text)
                .map(|rgb| Some(soksak_sidecar_vt_core::Rgb(rgb))),
            _ => None,
        };
        let attributes = cell["attributes"].as_u64().unwrap();
        if attributes & !0x2d != 0 {
            return None;
        }
        let grapheme = cell["grapheme"].as_str().unwrap();
        lines[index / cols as usize].push(Cell {
            ch: (!grapheme.is_empty()).then(|| grapheme.into()),
            width: cell["width"].as_u64().unwrap() as u8,
            fg: color(&cell["fg"])?,
            bg: color(&cell["bg"])?,
            bold: attributes & 0x01 != 0,
            italic: attributes & 0x04 != 0,
            underline: attributes & 0x08 != 0,
            inverse: attributes & 0x20 != 0,
            link: cell["link"].as_str().map(str::to_string),
        });
    }
    let cursor = &message["cursor"];
    let mut screen_cursor: Cursor = serde_json::from_value(json!({
        "col": cursor["col"], "row": cursor["row"], "visible": cursor["visible"]
    }))
    .unwrap();
    screen_cursor.focused = true;
    Some(Screen {
        cols,
        rows,
        cursor: screen_cursor,
        scrollback: Default::default(),
        background: "#000000".to_string(),
        lines,
    })
}

#[test]
fn screen_message_reports_the_same_wrapped_preedit_caret_without_mutating_engine_cursor() {
    let screen = Screen {
        cols: 6,
        rows: 2,
        cursor: Cursor {
            col: 5,
            row: 0,
            shape: Default::default(),
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: Some(Preedit {
                text: "한".into(),
                marked_range: Some(JsonRange {
                    location: 0,
                    length: 1,
                }),
                selected_range: Some(JsonRange {
                    location: 1,
                    length: 0,
                }),
                replacement_range: None,
                attributed: false,
            }),
        },
        scrollback: Default::default(),
        background: "#1e1e1e".into(),
        lines: vec![vec![Cell::default(); 6]; 2],
    };
    let engine_cursor = (screen.cursor.row, screen.cursor.col);

    let message = screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        accepted_frame(1),
        Value::Null,
        &screen,
        true,
    )
    .map(|(message, _)| message)
    .expect("screen message");

    assert_eq!(
        (
            message["cursor"]["row"].as_u64(),
            message["cursor"]["col"].as_u64()
        ),
        (Some(0), Some(5))
    );
    assert_eq!(message["cursor"]["caret"], json!({"row": 1, "col": 2}));
    assert_eq!((screen.cursor.row, screen.cursor.col), engine_cursor);
    assert_eq!(
        screen_hash_json(&message).unwrap(),
        screen_hash(&screen).unwrap()
    );

    let mut cleared = screen.clone();
    cleared.cursor.preedit = None;
    let cleared_message = screen_message(
        "boot",
        2,
        ScreenStream::default().order(),
        accepted_frame(2),
        Value::Null,
        &cleared,
        true,
    )
    .map(|(message, _)| message)
    .expect("cleared screen");
    assert_eq!(
        cleared_message["cursor"]["caret"],
        json!({"row": cleared.cursor.row, "col": cleared.cursor.col})
    );
    assert_eq!(
        screen_hash_json(&cleared_message).unwrap(),
        screen_hash(&cleared).unwrap()
    );
}

#[test]
fn screen_message_reports_caret_in_the_cropped_preedit_viewport() {
    let screen = Screen {
        cols: 4,
        rows: 2,
        cursor: Cursor {
            col: 3,
            row: 0,
            shape: Default::default(),
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: Some(Preedit {
                text: "ABCDEFGH".into(),
                marked_range: Some(JsonRange {
                    location: 0,
                    length: 8,
                }),
                selected_range: Some(JsonRange {
                    location: 8,
                    length: 0,
                }),
                replacement_range: None,
                attributed: false,
            }),
        },
        scrollback: Default::default(),
        background: "#1e1e1e".into(),
        lines: vec![vec![Cell::default(); 4]; 2],
    };

    let message = screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        accepted_frame(1),
        Value::Null,
        &screen,
        true,
    )
    .map(|(message, _)| message)
    .expect("cropped preedit screen message");

    assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    assert_eq!(
        (
            message["cursor"]["row"].as_u64(),
            message["cursor"]["col"].as_u64()
        ),
        (Some(0), Some(3))
    );
    assert_eq!(message["cursor"]["caret"], json!({"row": 1, "col": 3}));
    assert_eq!(message["cursor"]["preedit"]["text"], "ABCDEFGH");
    assert_eq!(
        message["cursor"]["preedit"]["selectedRange"],
        json!({"location": 8, "length": 0})
    );
    assert!(screen_hash_json(&message).is_ok());
}

#[test]
fn an_end_boundary_after_a_full_row_paints_on_the_last_occupied_cell() {
    let screen = Screen {
        cols: 4,
        rows: 2,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: Default::default(),
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: Some(Preedit {
                text: "ABCD".into(),
                marked_range: Some(JsonRange {
                    location: 0,
                    length: 4,
                }),
                selected_range: Some(JsonRange {
                    location: 4,
                    length: 0,
                }),
                replacement_range: None,
                attributed: false,
            }),
        },
        scrollback: Default::default(),
        background: "#1e1e1e".into(),
        lines: vec![vec![Cell::default(); 4]; 2],
    };

    let message = screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        accepted_frame(1),
        Value::Null,
        &screen,
        true,
    )
    .map(|(message, _)| message)
    .expect("screen message at a wrapped boundary");

    assert_eq!(message["cursor"]["row"], json!(0));
    assert_eq!(message["cursor"]["col"], json!(0));
    assert_eq!(message["cursor"]["caret"], json!({"row": 0, "col": 3}));
    assert!(screen_hash_json(&message).is_ok());
}

#[test]
fn a_boundary_inside_preedit_maps_to_the_next_grapheme_row() {
    let screen = Screen {
        cols: 4,
        rows: 2,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: Default::default(),
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: Some(Preedit {
                text: "ABCDE".into(),
                marked_range: Some(JsonRange {
                    location: 0,
                    length: 5,
                }),
                selected_range: Some(JsonRange {
                    location: 4,
                    length: 0,
                }),
                replacement_range: None,
                attributed: false,
            }),
        },
        scrollback: Default::default(),
        background: "#1e1e1e".into(),
        lines: vec![vec![Cell::default(); 4]; 2],
    };

    let message = screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        accepted_frame(1),
        Value::Null,
        &screen,
        true,
    )
    .map(|(message, _)| message)
    .expect("screen message at an internal wrap boundary");

    assert_eq!(message["cursor"]["caret"], json!({"row": 1, "col": 0}));
}

#[test]
fn a_one_row_wrap_pending_wide_end_boundary_keeps_raw_cursor_and_hash() {
    let screen = Screen {
        cols: 1,
        rows: 1,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: Default::default(),
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: Some(Preedit {
                text: "한".into(),
                marked_range: Some(JsonRange {
                    location: 0,
                    length: 1,
                }),
                selected_range: Some(JsonRange {
                    location: 1,
                    length: 0,
                }),
                replacement_range: None,
                attributed: false,
            }),
        },
        scrollback: Default::default(),
        background: "#1e1e1e".into(),
        lines: vec![vec![Cell::default(); 1]],
    };

    let message = screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        accepted_frame(1),
        Value::Null,
        &screen,
        true,
    )
    .map(|(message, _)| message)
    .expect("one-cell wrap-pending screen message");

    assert_eq!(
        (
            message["cursor"]["row"].as_u64(),
            message["cursor"]["col"].as_u64()
        ),
        (Some(0), Some(0))
    );
    assert_eq!(message["cursor"]["caret"], json!({"row": 0, "col": 0}));
    assert_eq!(message["cursor"]["preedit"]["text"], "한");
    assert_eq!(
        screen_hash_json(&message).unwrap(),
        screen_hash(&screen).unwrap()
    );
}

#[test]
fn a_screen_without_an_accepted_frame_has_no_native_caret() {
    let screen = screen_of(&golden("screen-message.json")["vectors"][0]["message"]).unwrap();

    let message = screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        Value::Null,
        Value::Null,
        &screen,
        false,
    )
    .map(|(message, _)| message)
    .expect("headless screen message");

    assert!(message["frame"].is_null());
    assert!(message["cursor"]["caret"].is_null());
    assert_eq!(
        screen_hash_json(&message).unwrap(),
        screen_hash(&screen).unwrap()
    );
}

#[test]
fn a_dirty_future_frame_keeps_its_identity_without_a_native_caret() {
    let screen = screen_of(&golden("screen-message.json")["vectors"][0]["message"]).unwrap();
    let future_frame = accepted_frame(2);

    let message = screen_message(
        "boot",
        2,
        ScreenStream::default().order(),
        future_frame.clone(),
        Value::Null,
        &screen,
        false,
    )
    .map(|(message, _)| message)
    .expect("dirty future frame message");

    assert_eq!(message["frame"], future_frame);
    assert!(message["cursor"]["caret"].is_null());
    assert_eq!(
        screen_hash_json(&message).unwrap(),
        screen_hash(&screen).unwrap()
    );
}

#[test]
fn the_message_for_the_screen_of_each_vector_has_its_members_and_its_hash() {
    let vectors = golden("screen-message.json");
    let mut built = 0;
    for vector in vectors["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let expected = &vector["message"];
        // The members of the golden message hash to the hash of the vector (the hash function of the service reads a message).
        let hashes = golden("screen-hash.json");
        let hash = hashes["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["name"] == vector["screen_vector"])
            .unwrap()["hash"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            screen_hash_json(expected).unwrap(),
            hash,
            "{name}: golden message"
        );
        let Some(screen) = screen_of(expected) else {
            continue;
        };
        let message = screen_message(
            expected["segment"].as_str().unwrap(),
            expected["sequence"].as_u64().unwrap(),
            ScreenStream::default().order(),
            expected["frame"].clone(),
            serde_json::Value::Null,
            &screen,
            !expected["frame"].is_null(),
        )
        .map(|(message, _)| message)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(message["event"], "screen", "{name}");
        assert!(
            message.get("render").is_some(),
            "{name}: render member is required"
        );
        if message["frame"].is_null() {
            assert!(message["render"].is_null(), "{name}: render without frame");
            assert!(
                message["cursor"]["caret"].is_null(),
                "{name}: caret without frame"
            );
        } else {
            assert!(
                message["cursor"]["caret"].is_object(),
                "{name}: frame without caret"
            );
        }
        for member in ["sequence", "segment", "rows", "cols"] {
            assert_eq!(message[member], expected[member], "{name}: {member}");
        }
        for member in ["visible", "row", "col"] {
            assert_eq!(
                message["cursor"][member], expected["cursor"][member],
                "{name}: cursor.{member}"
            );
        }
        for (index, cell) in expected["cells"].as_array().unwrap().iter().enumerate() {
            for member in ["grapheme", "width", "fg", "bg", "attributes"] {
                assert_eq!(
                    message["cells"][index][member], cell[member],
                    "{name}: cells[{index}].{member}"
                );
            }
            assert!(
                message["cells"][index].get("ch").is_none()
                    && message["cells"][index].get("bold").is_none(),
                "{name}"
            );
        }
        assert_eq!(
            screen_hash_json(&message).unwrap(),
            hash,
            "{name}: message of the service"
        );
        built += 1;
    }
    // The vector with an indexed color and the attributes that a cell of the engine lacks cannot be held by a screen of the
    // engine; the others can.
    assert!(built >= 5, "{built}");
}

#[test]
fn a_screen_that_the_hash_refuses_gets_no_message() {
    let mut screen = screen_of(&golden("screen-message.json")["vectors"][1]["message"]).unwrap();
    screen.cursor.row = 5;
    let error = screen_message(
        "b1",
        3,
        ScreenStream::default().order(),
        serde_json::Value::Null,
        serde_json::Value::Null,
        &screen,
        false,
    )
    .map(|(message, _)| message)
    .unwrap_err();
    assert!(error.contains("outside the screen"), "{error}");
}

#[test]
fn a_render_record_for_another_frame_is_rejected() {
    let screen = screen_of(&golden("screen-message.json")["vectors"][0]["message"]).unwrap();
    let frame = json!({ "generation": 1, "raster": 2, "sequence": 3 });
    let render = json!({
        "frame": { "generation": 1, "raster": 2, "sequence": 2 },
        "glyphs": []
    });
    let error = soksak_sidecar_vt_core::protocol::screen_message(
        "boot",
        1,
        ScreenStream::default().order(),
        frame,
        render,
        &screen,
        true,
    )
    .map(|(message, _)| message)
    .unwrap_err();
    assert!(error.contains("render.frame must match frame"), "{error}");
}

// The service reproduces each change sequence of the core fixture: the screens of the messages, given to one stream, give the same
// members of the full and the change form (core docs/spec/persistent-services.md#screen-message).
#[test]
fn the_messages_of_each_change_sequence_are_reproduced_by_one_stream() {
    let fixture = golden("screen-message.json");
    let sequences = fixture["changes"].as_array().expect("change sequences");
    assert!(!sequences.is_empty(), "the fixture has change sequences");
    for sequence in sequences {
        let name = sequence["name"].as_str().unwrap();
        let messages = sequence["messages"].as_array().unwrap();
        let mut stream = ScreenStream::default();
        let mut cells = messages[0]["cells"].as_array().unwrap().clone();
        let cols = messages[0]["cols"].as_u64().unwrap() as usize;
        for (index, expected) in messages.iter().enumerate() {
            for change in expected["changed"].as_array().into_iter().flatten() {
                let row = change["row"].as_u64().unwrap() as usize;
                for (col, cell) in change["cells"].as_array().unwrap().iter().enumerate() {
                    cells[row * cols + col] = cell.clone();
                }
            }
            let mut full = expected.clone();
            full["cells"] = Value::Array(cells.clone());
            let screen = screen_of(&full)
                .unwrap_or_else(|| panic!("{name}: message {index} is a screen of the engine"));
            let (message, sent) = screen_message(
                expected["segment"].as_str().unwrap(),
                expected["sequence"].as_u64().unwrap(),
                stream.order(),
                Value::Null,
                Value::Null,
                &screen,
                false,
            )
            .unwrap_or_else(|error| panic!("{name}: message {index}: {error}"));
            stream.sent(sent);
            for member in [
                "number", "base", "previous", "changed", "cells", "rows", "cols", "sequence",
                "segment",
            ] {
                assert_eq!(
                    message.get(member),
                    expected.get(member),
                    "{name}: message {index}: {member}"
                );
            }
            assert_eq!(
                screen_hash(&screen).unwrap(),
                sequence["hashes"][index].as_str().unwrap(),
                "{name}: message {index}: hash"
            );
        }
    }
}
