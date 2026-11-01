// The cells of a screen message are owned once, so encoding its line must not allocate another cell tree.
#[path = "support/allocations_test.rs"]
mod allocations_test;

use allocations_test::ALLOCATIONS;
use serde_json::{Map, Value, json};
use soksak_sidecar_vt_core::screen_stream::{ScreenStream, screen_cells};
use std::sync::atomic::Ordering;

#[test]
fn encoding_a_screen_line_does_not_copy_each_cell() {
    let cell = json!({"grapheme": "a", "width": 1, "fg": null, "bg": null, "attributes": 0});
    let mut members = Map::new();
    members.insert("event".to_string(), json!("screen"));
    members.insert("rows".to_string(), json!(64));
    members.insert("cols".to_string(), json!(128));
    let cells = screen_cells(
        ScreenStream::default().order(),
        64,
        128,
        vec![cell.clone(); 8192],
    );
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let line = cells.line("s1", &members).expect("encode screen");
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    println!("{allocations} allocations encode an owned 8192-cell screen message");
    let decoded: Value = serde_json::from_str(&line).expect("decode screen");
    assert_eq!(decoded["surface"], "s1");
    assert_eq!(decoded["body"]["cells"], json!(vec![cell; 8192]));
    assert_eq!(decoded["body"]["event"], "screen");
    assert!(
        allocations <= 64,
        "{allocations} allocations encoded an already normalized 8192-cell message"
    );
    let text = screen_cells(
        ScreenStream::default().order(),
        1,
        1,
        vec![json!({"grapheme": "é\"\\"})],
    );
    assert_eq!(
        text.line("s1", &Map::new()).unwrap(),
        r#"{"body":{"number":1,"base":true,"cells":[{"grapheme":"é\"\\"}]},"surface":"s1"}"#
    );
}
