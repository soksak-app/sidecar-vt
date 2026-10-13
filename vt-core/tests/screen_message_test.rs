// The `screen` message of the service carries the members of the screen hash (core docs/spec/terminal-runtime.md#screen-message):
// for each vector of the golden file the message that the service builds from the screen of the vector has the members of the
// golden message and the hash of the vector. The golden file is owned by core (tests/fixtures/log-contract/README.md).
use serde_json::{Value, json};
use soksak_sidecar_vt_core::protocol::{Cell, Cursor, Screen, screen_message};
use soksak_sidecar_vt_core::screen_hash::screen_hash_json;

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

/// The screen of the engine for a golden message, or None when the engine cannot hold it: the engine resolves an indexed color to
/// a true color, and its cells state bold, italic, underline and inverse only.
fn screen_of(message: &Value) -> Option<Screen> {
    let rows = message["rows"].as_u64().unwrap() as u16;
    let cols = message["cols"].as_u64().unwrap() as u16;
    let mut lines = vec![Vec::new(); rows as usize];
    for (index, cell) in message["cells"].as_array().unwrap().iter().enumerate() {
        let color = |value: &Value| match value {
            Value::Null => Some(None),
            Value::String(text) => Some(Some(text.clone())),
            _ => None,
        };
        let attributes = cell["attributes"].as_u64().unwrap();
        if attributes & !0x2d != 0 {
            return None;
        }
        let grapheme = cell["grapheme"].as_str().unwrap();
        lines[index / cols as usize].push(Cell {
            ch: (!grapheme.is_empty()).then(|| grapheme.to_string()),
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
            &screen,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(message["event"], "screen", "{name}");
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
    let error = screen_message("b1", 3, &screen).unwrap_err();
    assert!(error.contains("outside the screen"), "{error}");
}
