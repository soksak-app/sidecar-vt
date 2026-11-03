// The screen of the engine is built for every drawn frame, so its cost grows with the cells only by
// copying them: a colored cell and its character take no allocation of their own.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

#[path = "../../vt-core/tests/support/allocations_test.rs"]
mod allocations_test;

use allocations_test::ALLOCATIONS;
use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::Engine;
use std::sync::atomic::Ordering;

#[test]
fn building_a_screen_allocates_per_line_not_per_cell() {
    let (cols, rows) = (100u16, 24u16);
    let mut engine = AlacrittyEngine::new();
    engine.resize(cols, rows);
    // Every cell has a true foreground, an indexed background and a character.
    let mut output = Vec::new();
    for row in 0..rows {
        output.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        for col in 0..cols {
            output.extend_from_slice(
                format!(
                    "\x1b[38;2;{};{};9m\x1b[48;5;{}m{}",
                    row,
                    col,
                    col % 200 + 16,
                    (b'a' + (col % 26) as u8) as char
                )
                .as_bytes(),
            );
        }
    }
    engine.feed(&output);
    engine.screen();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let screen = engine.screen();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    println!("{allocations} allocations built a {rows}x{cols} screen");
    assert_eq!(
        screen.lines.iter().map(Vec::len).sum::<usize>(),
        usize::from(cols) * usize::from(rows)
    );
    let bound = usize::from(rows) * 4 + 64;
    assert!(
        allocations <= bound,
        "{allocations} allocations built a {rows}x{cols} screen; the bound is {bound}"
    );
}
