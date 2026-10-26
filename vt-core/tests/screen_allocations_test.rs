// The normalized screen body is owned, so encoding must not allocate another cell tree.
#[path = "support/allocations_test.rs"]
mod allocations_test;

use allocations_test::ALLOCATIONS;
use serde_json::{Value, json};
use soksak_sidecar_vt_core::protocol::screen_line;
use std::sync::atomic::Ordering;

#[test]
fn encoding_a_screen_envelope_does_not_copy_each_cell() {
    let body = json!({
        "event": "screen", "rows": 64, "cols": 128,
        "cells": vec![json!({"grapheme": "a", "width": 1,
            "fg": null, "bg": null, "attributes": 0}); 8192],
    });
    let expected = body.clone();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let line = screen_line("s1", body).expect("encode screen");
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    println!("{allocations} allocations encode an owned 8192-cell screen body");
    let decoded: Value = serde_json::from_str(&line).expect("decode screen");
    assert_eq!(decoded["surface"], "s1");
    assert_eq!(decoded["body"], expected);
    assert!(
        allocations <= 64,
        "{allocations} allocations encoded an already normalized 8192-cell body"
    );
    assert_eq!(
        screen_line("s1", json!({"text": "é\"\\"})).unwrap(),
        r#"{"body":{"text":"é\"\\"},"surface":"s1"}"#
    );
}
