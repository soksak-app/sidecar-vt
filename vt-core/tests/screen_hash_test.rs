// The screen hash reproduces the golden vectors of the replay contract (core docs/spec/replay.md#screen-hash): the canonical
// bytes and the hash of every screen, and the error text for every screen that the hash refuses.
use serde_json::Value;
use soksak_sidecar_vt_core::screen_hash::{screen_bytes_json, screen_hash_json};

fn vectors() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/log-contract/screen-hash.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    // A Rust string cannot hold a lone surrogate, so the case that fixes the error text for one cannot be written in this
    // language; the escape is replaced by a character that parses, and the case is skipped by name below.
    serde_json::from_str(&text.replace("\\ud800", "\u{fffd}"))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn the_canonical_bytes_and_the_hash_equal_every_golden_vector() {
    let vectors = vectors();
    let list = vectors["vectors"].as_array().unwrap();
    assert!(list.len() >= 4);
    for vector in list {
        let name = vector["name"].as_str().unwrap();
        let bytes =
            screen_bytes_json(&vector["screen"]).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(hex(&bytes), vector["bytes_hex"].as_str().unwrap(), "{name}");
        assert_eq!(
            screen_hash_json(&vector["screen"]).unwrap(),
            vector["hash"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn every_screen_that_the_hash_refuses_gets_the_error_text_of_the_golden_file() {
    let vectors = vectors();
    let mut checked = 0;
    for case in vectors["rejected"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if name == "grapheme with a lone surrogate" {
            continue;
        }
        let error = screen_hash_json(&case["screen"]).expect_err(name);
        assert_eq!(error, case["error"].as_str().unwrap(), "{name}");
        checked += 1;
    }
    assert!(checked >= 6, "{checked}");
}
